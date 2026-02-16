//! Custom DD operators for the Boon engine.
//!
//! Only operators that can't be expressed as standard DD combinators
//! or standard library Boon functions go here.
//!
//! Standard DD operators used directly in this file:
//! - `.filter()` — SKIP, List/retain (simple predicate)
//! - `.flat_map()` — WHEN (pattern matching)
//! - `.concat()` — LATEST (merge sources), List/append
//! - `.join()` — List/retain reactive, TEXT interpolation (2 deps)
//! - `.map()` — field access, pure transforms
//! - `.negate()` — List/remove (retraction)
//!
//! DD operators available but not currently used:
//! - `.reduce()` — aggregations (not needed; custom operators handle state)
//! - `.arrange()` — indexed collections (implicit in .join())
//! - `.count()` — element counting (custom list_count is more efficient for scalars)
//! - `.distinct()` — deduplication
//! - `.iterate()` — fixed-point loops

use super::types::ListKey;
use super::value::Value;
use differential_dataflow::collection::AsCollection;
use differential_dataflow::operators::join::Join;
use differential_dataflow::VecCollection;
use timely::container::CapacityContainerBuilder;
use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::generic::operator::Operator;
use timely::dataflow::Scope;

// ---------------------------------------------------------------------------
// Custom stateful operators (can't be expressed as standard DD combinators)
// ---------------------------------------------------------------------------

/// HOLD state operator.
///
/// Maintains a single-element collection representing the current state.
/// On each event (positive diff), applies the transform function to produce
/// new state, retracting the old and inserting the new.
///
/// `initial_collection` should contain exactly one element: the initial state.
/// `events` is the stream of events that trigger state updates.
/// `transform` is called with (current_state, event) -> new_state.
///
/// Returns: collection that always contains exactly one element (current state).
pub fn hold_state<G>(
    initial_collection: &VecCollection<G, Value, isize>,
    events: &VecCollection<G, Value, isize>,
    initial_value: Value,
    transform: impl Fn(&Value, &Value) -> Value + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    let mut state = initial_value;

    // Process events and produce retract-old / insert-new diffs
    let changes = events
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "HoldState",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut session = output.session(&time);
                        for (event, ts, diff) in data.drain(..) {
                            if diff > 0 {
                                let old = state.clone();
                                let new_state = transform(&old, &event);
                                if new_state != old {
                                    session.give((old, ts, -1isize));
                                    session.give((new_state.clone(), ts, 1isize));
                                    state = new_state;
                                }
                            }
                        }
                    });
                }
            },
        )
        .as_collection();

    // Merge initial value with state changes
    initial_collection.concat(&changes)
}

/// LATEST operator.
///
/// Takes a concatenated collection (from multiple LATEST inputs) and
/// maintains only the most recently changed value. Always contains
/// exactly one element.
pub fn hold_latest<G>(
    source: &VecCollection<G, Value, isize>,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    let mut current: Option<Value> = None;

    source
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "HoldLatest",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut session = output.session(&time);

                        // Find the last positive insertion in this batch
                        let mut latest_new: Option<(Value, u64)> = None;
                        for &(ref value, ref ts, ref diff) in data.iter() {
                            if *diff > 0 {
                                latest_new = Some((value.clone(), *ts));
                            }
                        }

                        if let Some((new_val, ts)) = latest_new {
                            // Retract old value if we had one
                            if let Some(old) = current.take() {
                                session.give((old, ts, -1isize));
                            }
                            // Insert new value
                            session.give((new_val.clone(), ts, 1isize));
                            current = Some(new_val);
                        }
                    });
                }
            },
        )
        .as_collection()
}

// ---------------------------------------------------------------------------
// WHEN — frozen pattern match (uses flat_map)
// ---------------------------------------------------------------------------

/// WHEN operator: pattern match on input values.
///
/// Applies `match_fn` to each input value. If it returns Some(result),
/// the result is emitted; if None, the value is skipped (SKIP semantics).
///
/// `input |> WHEN { pattern => body, ... }` compiles to this.
pub fn when_match<G>(
    source: &VecCollection<G, Value, isize>,
    match_fn: impl Fn(Value) -> Option<Value> + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    source.flat_map(move |value| match_fn(value))
}

// ---------------------------------------------------------------------------
// SKIP — filter out values (uses filter)
// ---------------------------------------------------------------------------

/// SKIP operator: filter values from a collection.
///
/// Keeps only values where `predicate` returns true.
/// SKIP in a WHEN/THEN arm means "don't emit" — the compiler inverts
/// the predicate so this filter keeps the non-skipped values.
pub fn skip_filter<G>(
    source: &VecCollection<G, Value, isize>,
    predicate: impl Fn(&Value) -> bool + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    source.filter(move |value| predicate(value))
}

// ---------------------------------------------------------------------------
// WHILE — reactive pattern match via DD join
// ---------------------------------------------------------------------------

/// WHILE operator: reactive pattern match.
///
/// Combines `input` with `dependency` using a cross-product. When either
/// input or dependency changes, recomputes the combined result.
///
/// Uses a custom Pipeline operator (not DD's arrangement-based `join()`)
/// because scalar cross-product doesn't need arrangements, and Pipeline
/// pipelining ensures data flows within a single `worker.step()`.
pub fn while_reactive<G>(
    input: &VecCollection<G, Value, isize>,
    dependency: &VecCollection<G, Value, isize>,
    combine: impl Fn(&Value, &Value) -> Value + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    // Tag both sides: true = left (input), false = right (dependency)
    let tagged_input = input.map(|v| (true, v));
    let tagged_dep = dependency.map(|v| (false, v));
    let merged = tagged_input.concat(&tagged_dep);

    let mut left: Option<Value> = None;
    let mut right: Option<Value> = None;
    let mut last_output: Option<Value> = None;

    merged
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "WhileReactive",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        // Process all updates in this batch
                        for ((is_left, value), _ts, diff) in data.drain(..) {
                            if diff > 0 {
                                if is_left {
                                    left = Some(value);
                                } else {
                                    right = Some(value);
                                }
                            }
                        }

                        // Produce output when both sides have values
                        if let (Some(l), Some(r)) = (&left, &right) {
                            let new_output = combine(l, r);
                            if last_output.as_ref() != Some(&new_output) {
                                let mut session = output.session(&time);
                                if let Some(old) = last_output.take() {
                                    session.give((old, *time.time(), -1isize));
                                }
                                session.give((new_output.clone(), *time.time(), 1isize));
                                last_output = Some(new_output);
                            }
                        }
                    });
                }
            },
        )
        .as_collection()
}

// ---------------------------------------------------------------------------
// List operations (use DD's native collection semantics)
// ---------------------------------------------------------------------------

/// List/count: count elements in a keyed list collection.
///
/// Takes a collection of `(ListKey, Value)` pairs and returns a scalar
/// collection containing the count as `Value::number(n)`.
///
/// Uses a custom operator to maintain a running count of positive/negative
/// diffs, emitting retract-old/insert-new pairs when the count changes.
pub fn list_count<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    let mut count: i64 = 0;
    let mut has_emitted = false;

    list.inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "ListCount",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let old_count = count;
                        for (_, _, diff) in data.drain(..) {
                            count += diff as i64;
                        }
                        if count != old_count || !has_emitted {
                            let mut session = output.session(&time);
                            if has_emitted {
                                session.give((Value::number(old_count as f64), *time.time(), -1isize));
                            }
                            session.give((Value::number(count as f64), *time.time(), 1isize));
                            has_emitted = true;
                        }
                    });
                }
            },
        )
        .as_collection()
}

/// List/retain: filter list items by predicate.
///
/// Simple (non-reactive) version — uses `.filter()` directly.
/// For reactive predicates (retain + WHILE), use `list_retain_reactive`.
pub fn list_retain<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
    predicate: impl Fn(&Value) -> bool + 'static,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    list.filter(move |(_key, value)| predicate(value))
}

/// List/retain with reactive predicate (WHILE) — uses DD join.
///
/// Joins the list collection with a filter state collection.
/// When the filter state changes, DD incrementally recomputes
/// which items pass. This is THE core DD value proposition for TodoMVC.
///
/// `todos |> List/retain(item, if: filter |> WHILE { All => True, ... })`
pub fn list_retain_reactive<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
    filter_state: &VecCollection<G, Value, isize>,
    predicate: impl Fn(&Value, &Value) -> bool + 'static,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    // Key filter state by () for cross-product join with list
    let keyed_filter = filter_state.map(|v| ((), v));
    let keyed_list = list.map(|(key, val)| ((), (key, val)));

    // Join list × filter, then filter by predicate
    keyed_list
        .join(&keyed_filter)
        .flat_map(move |(_unit, ((key, val), filter_val))| {
            if predicate(&val, &filter_val) {
                Some((key, val))
            } else {
                None
            }
        })
}

/// List/map: transform each list item.
pub fn list_map<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
    transform: impl Fn(Value) -> Value + 'static,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    list.map(move |(key, val)| (key, transform(val)))
}

/// List/append: add an item to a list (concat with new keyed item).
pub fn list_append<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
    new_items: &VecCollection<G, (ListKey, Value), isize>,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    list.concat(new_items)
}

/// List/remove: remove items from a list by negation.
///
/// `remove_items` must contain the exact `(key, value)` pairs to retract.
/// DD consolidation cancels matching positive entries automatically.
pub fn list_remove<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
    remove_items: &VecCollection<G, (ListKey, Value), isize>,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    list.concat(&remove_items.negate())
}

/// Map scalar events to keyed pairs via a classify function.
///
/// Each scalar input value is passed to `classify`. If it returns `Some((key, event))`,
/// the pair is emitted to the keyed output. If `None`, the value is skipped.
/// Used for demuxing wildcard events into per-item keyed events.
pub fn map_to_keyed<G>(
    source: &VecCollection<G, Value, isize>,
    classify: impl Fn(&Value) -> Option<(ListKey, Value)> + 'static,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    source.flat_map(move |value| classify(&value))
}

/// Append new keyed items from a scalar trigger with auto-incrementing keys.
///
/// Each positive diff from `source` generates a new `(ListKey, item)` pair
/// with a monotonically increasing key (zero-padded 4-digit format).
/// `transform` converts the trigger value into the item value.
/// `initial_counter` sets the starting key number (to avoid collisions with initial items).
pub fn append_new_keyed<G>(
    source: &VecCollection<G, Value, isize>,
    transform: impl Fn(&Value) -> Value + 'static,
    initial_counter: usize,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    let mut counter = initial_counter;

    source
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "AppendNewKeyed",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut session = output.session(&time);
                        for (value, ts, diff) in data.drain(..) {
                            if diff > 0 {
                                let key = ListKey::new(format!("{:04}", counter));
                                counter += 1;
                                let item = transform(&value);
                                session.give(((key, item), ts, 1isize));
                            }
                        }
                    });
                }
            },
        )
        .as_collection()
}

/// Assemble keyed list items into a scalar Value::Tagged("List", BTreeMap).
///
/// Maintains an internal BTreeMap of current items. When items are added/removed/updated,
/// retracts the old assembled list and inserts the new one.
pub fn assemble_list<G>(
    keyed: &VecCollection<G, (ListKey, Value), isize>,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    let mut items: std::collections::BTreeMap<std::sync::Arc<str>, Value> =
        std::collections::BTreeMap::new();
    let mut last_emitted: Option<Value> = None;

    keyed
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "AssembleList",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut changed = false;
                        for ((key, value), _ts, diff) in data.drain(..) {
                            if diff > 0 {
                                items.insert(std::sync::Arc::from(key.0.as_ref()), value);
                                changed = true;
                            } else if diff < 0 {
                                items.remove(key.0.as_ref());
                                changed = true;
                            }
                        }

                        if changed {
                            let new_list = Value::Tagged {
                                tag: std::sync::Arc::from("List"),
                                fields: std::sync::Arc::new(items.clone()),
                            };

                            if last_emitted.as_ref() != Some(&new_list) {
                                let mut session = output.session(&time);
                                if let Some(old) = last_emitted.take() {
                                    session.give((old, *time.time(), -1isize));
                                }
                                session.give((new_list.clone(), *time.time(), 1isize));
                                last_emitted = Some(new_list);
                            }
                        }
                    });
                }
            },
        )
        .as_collection()
}

/// Keyed HOLD state: per-item stateful accumulator for list elements.
///
/// Maintains a `HashMap<ListKey, Value>` of per-key state. Initial values
/// set the state for each key; events update state per-key via transform.
///
/// `initial` provides `(key, initial_value)` pairs (e.g., from ListAppend).
/// `events` provides `(key, event_value)` pairs (e.g., checkbox clicks).
/// `transform` is called per-key: `transform(current_state, event) -> new_state`.
/// `broadcasts` (optional) provides scalar events that affect all items (toggle_all, remove_completed).
/// `broadcast_handler` is called with (all_items_HashMap, broadcast_event) → per-item updates.
///
/// On new key: emits `(key, initial_value, +1)`.
/// On key removal: emits `(key, last_value, -1)`.
/// On event: emits `(key, old_state, -1)` and `(key, new_state, +1)`.
pub fn keyed_hold_state<G>(
    initial: &VecCollection<G, (ListKey, Value), isize>,
    events: &VecCollection<G, (ListKey, Value), isize>,
    transform: impl Fn(&Value, &Value) -> Value + 'static,
    broadcasts: Option<&VecCollection<G, Value, isize>>,
    broadcast_handler: Option<std::sync::Arc<dyn Fn(&std::collections::HashMap<ListKey, Value>, &Value) -> Vec<(ListKey, Option<Value>)> + 'static>>,
) -> VecCollection<G, (ListKey, Value), isize>
where
    G: Scope<Timestamp = u64>,
{
    // Tag: 0 = initial, 1 = per-key event, 2 = broadcast
    // Sort order ensures initials processed first, then events, then broadcasts.
    let sentinel_key = ListKey::new("__broadcast");
    let tagged_initial = initial.map(|(key, val)| (key, (0u8, val)));
    let tagged_events = events.map(|(key, val)| (key, (1u8, val)));
    let mut combined = tagged_initial.concat(&tagged_events);

    if let Some(bcast) = broadcasts {
        let sk = sentinel_key.clone();
        let tagged_broadcast = bcast.map(move |val| (sk.clone(), (2u8, val)));
        combined = combined.concat(&tagged_broadcast);
    }

    let mut states: std::collections::HashMap<ListKey, Value> = std::collections::HashMap::new();

    combined
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "KeyedHoldState",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut session = output.session(&time);
                        // Collect and sort by tag: initials (0) < events (1) < broadcasts (2)
                        let mut items: Vec<_> = data.drain(..).collect();
                        items.sort_by_key(|((_, (tag, _)), _, _)| *tag);

                        for ((key, (tag, value)), ts, diff) in items {
                            match tag {
                                0 => {
                                    // Initial
                                    if diff > 0 {
                                        states.insert(key.clone(), value.clone());
                                        session.give(((key, value), ts, 1isize));
                                    } else if let Some(old) = states.remove(&key) {
                                        session.give(((key, old), ts, -1isize));
                                    }
                                }
                                1 => {
                                    // Per-key event
                                    if diff > 0 {
                                        if let Some(current) = states.get(&key) {
                                            let old = current.clone();
                                            let new_val = transform(&old, &value);
                                            if new_val == Value::Unit {
                                                states.remove(&key);
                                                session.give(((key, old), ts, -1isize));
                                            } else if new_val != old {
                                                session.give(((key.clone(), old), ts, -1isize));
                                                session.give(((key.clone(), new_val.clone()), ts, 1isize));
                                                states.insert(key, new_val);
                                            }
                                        }
                                    }
                                }
                                2 => {
                                    // Broadcast event
                                    if diff > 0 {
                                        if let Some(ref handler) = broadcast_handler {
                                            let results = handler(&states, &value);
                                            for (bk, maybe_new) in results {
                                                match maybe_new {
                                                    Some(new_val) => {
                                                        if let Some(old) = states.get(&bk) {
                                                            if *old != new_val {
                                                                session.give(((bk.clone(), old.clone()), ts, -1isize));
                                                                session.give(((bk.clone(), new_val.clone()), ts, 1isize));
                                                                states.insert(bk, new_val);
                                                            }
                                                        }
                                                    }
                                                    None => {
                                                        if let Some(old) = states.remove(&bk) {
                                                            session.give(((bk, old), ts, -1isize));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    });
                }
            },
        )
        .as_collection()
}

// ---------------------------------------------------------------------------
// TEXT interpolation (uses join for reactive dependencies)
// ---------------------------------------------------------------------------

/// TEXT interpolation with reactive variable.
///
/// `TEXT { Count: {counter} }` joins the template with the reactive variable.
/// When the variable changes, the text is recomputed.
pub fn text_interpolation<G>(
    dependency: &VecCollection<G, Value, isize>,
    template: impl Fn(&Value) -> Value + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    dependency.map(move |val| template(&val))
}

/// TEXT interpolation with two reactive dependencies (uses join).
pub fn text_interpolation_join<G>(
    dep_a: &VecCollection<G, Value, isize>,
    dep_b: &VecCollection<G, Value, isize>,
    template: impl Fn(&Value, &Value) -> Value + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    let keyed_a = dep_a.map(|v| ((), v));
    let keyed_b = dep_b.map(|v| ((), v));

    keyed_a
        .join(&keyed_b)
        .map(move |(_key, (a, b))| template(&a, &b))
}

// ---------------------------------------------------------------------------
// THEN — event-triggered map (positive diffs only)
// ---------------------------------------------------------------------------

/// THEN operator: apply transform on event insertion.
///
/// Only processes positive diffs (insertions), ignoring retractions.
/// `event |> THEN { body }` evaluates body for each new event.
pub fn then<G>(
    events: &VecCollection<G, Value, isize>,
    body: impl Fn(Value) -> Value + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    events
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "Then",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut session = output.session(&time);
                        for (event, ts, diff) in data.drain(..) {
                            if diff > 0 {
                                let result = body(event);
                                session.give((result, ts, 1isize));
                            }
                        }
                    });
                }
            },
        )
        .as_collection()
}

// ---------------------------------------------------------------------------
// Stream/skip — skip first N values
// ---------------------------------------------------------------------------

/// Skip the first `count` positive-diff values from a collection.
///
/// `source |> Stream/skip(count: N)` drops the first N insertions.
/// Retractions pass through unchanged (they retract previously-emitted values).
///
/// For values that are skipped, emits `Value::Unit` so downstream can filter.
pub fn skip<G>(
    source: &VecCollection<G, Value, isize>,
    count: usize,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    let mut seen = 0usize;

    source
        .inner
        .unary::<CapacityContainerBuilder<Vec<_>>, _, _, _>(
            Pipeline,
            "Skip",
            |_cap, _info| {
                move |input, output| {
                    input.for_each(|time, data| {
                        let mut session = output.session(&time);
                        for (value, ts, diff) in data.drain(..) {
                            if diff > 0 {
                                if seen < count {
                                    seen += 1;
                                    // Skip: don't emit anything
                                } else {
                                    session.give((value, ts, 1isize));
                                }
                            } else {
                                // Retractions: pass through for values we already emitted
                                if seen > count {
                                    session.give((value, ts, diff));
                                }
                            }
                        }
                    });
                }
            },
        )
        .as_collection()
}
