//! Custom DD operators for the Boon engine.
//!
//! Only operators that can't be expressed as standard DD combinators
//! or standard library Boon functions go here.
//!
//! Standard DD operators used directly:
//! - `.filter()` — SKIP, List/retain (simple predicate)
//! - `.flat_map()` — WHEN (pattern matching)
//! - `.concat()` — LATEST (merge sources)
//! - `.join()` — WHILE (reactive pattern match), TEXT interpolation
//! - `.count()` — List/count
//! - `.map()` — THEN, field access, pure transforms
//! - `.reduce()` — aggregations
//! - `.arrange()` — indexed collections for join/reduce

use super::types::ListKey;
use super::value::Value;
use differential_dataflow::collection::AsCollection;
use differential_dataflow::operators::join::Join;
use differential_dataflow::operators::reduce::Count;
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
                                session.give((old, ts, -1isize));
                                session.give((new_state.clone(), ts, 1isize));
                                state = new_state;
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
/// Joins `input` with `dependency` using a unit key (cross-product for scalars).
/// When either input or dependency changes, recomputes the match.
///
/// `input |> WHILE { True => body_a, False => body_b }` where the input
/// depends on a reactive value compiles to a join.
///
/// For scalar values, both sides are single-element collections keyed by ().
pub fn while_reactive<G>(
    input: &VecCollection<G, Value, isize>,
    dependency: &VecCollection<G, Value, isize>,
    combine: impl Fn(&Value, &Value) -> Value + 'static,
) -> VecCollection<G, Value, isize>
where
    G: Scope<Timestamp = u64>,
{
    // Key both collections by () for cross-product join
    let keyed_input = input.map(|v| ((), v));
    let keyed_dep = dependency.map(|v| ((), v));

    // DD join: when either side changes, recompute
    keyed_input
        .join(&keyed_dep)
        .map(move |(_key, (input_val, dep_val))| combine(&input_val, &dep_val))
}

// ---------------------------------------------------------------------------
// List operations (use DD's native collection semantics)
// ---------------------------------------------------------------------------

/// List/count: count elements in a keyed list collection.
///
/// Takes a collection of `(ListKey, Value)` pairs and returns a scalar
/// collection containing the count.
pub fn list_count<G>(
    list: &VecCollection<G, (ListKey, Value), isize>,
) -> VecCollection<G, ((), isize), isize>
where
    G: Scope<Timestamp = u64>,
{
    // Key all items by () to count them as one group
    list.map(|(_key, _val)| ()).count()
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
