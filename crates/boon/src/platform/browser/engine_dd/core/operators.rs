//! Differential Dataflow runtime for Boon
//!
//! This module provides a minimal wrapper around Timely/DD for running
//! reactive dataflows in the browser. It uses a single-threaded worker
//! with the simplest possible configuration.
//!
//! # Architecture
//!
//! ```text
//! Browser Events → inject_event() → DD Worker → inspect() callbacks → DOM
//! ```
//!
//! # Phase 1 Goals
//!
//! - Get DD compiling to WASM ✓
//! - Run a simple counter example ✓
//! - Measure WASM size impact ✓
//!
//! # Phase 2 Goals
//!
//! - Implement HOLD operator using DD's unary operator
//! - Test stateful accumulation patterns

use std::sync::{Arc, Mutex};

use differential_dataflow::collection::{AsCollection, VecCollection};
use differential_dataflow::input::Input;
use differential_dataflow::lattice::Lattice;
use differential_dataflow::operators::Count;
use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::Operator;
use timely::dataflow::Scope;

/// A minimal DD runtime for browser execution.
///
/// Uses a single-threaded Timely worker (no parallelism needed for browser).
/// Events are injected via `inject_event()` and processed via `step()`.
pub struct DdRuntime {
    /// The current logical time (increments on each event batch)
    current_time: u64,
}

impl DdRuntime {
    /// Create a new DD runtime.
    pub fn new() -> Self {
        Self {
            current_time: 0,
        }
    }

    /// Get the current logical time.
    pub fn current_time(&self) -> u64 {
        self.current_time
    }

    /// Advance the logical time and return the new time.
    pub fn advance_time(&mut self) -> u64 {
        self.current_time += 1;
        self.current_time
    }
}

impl Default for DdRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a simple counter dataflow as a proof of concept.
///
/// This demonstrates:
/// 1. Creating a DD collection from an input
/// 2. Using `count()` to track the number of items
/// 3. Inspecting output changes
///
/// # Example
///
/// ```ignore
/// let outputs = run_counter_dataflow(vec![
///     (1, true),   // Insert item 1
///     (2, true),   // Insert item 2
///     (1, false),  // Remove item 1
/// ]);
/// // outputs: [(0, 1), (1, 2), (2, 1)] - count after each event
/// ```
pub fn run_counter_dataflow(events: Vec<(i32, bool)>) -> Vec<(u64, isize)> {
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let outputs_clone = outputs.clone();

    // Run a single-threaded Timely computation
    timely::execute_directly(move |worker| {
        // Create input handle and probe for tracking progress
        let (mut input, probe) = worker.dataflow::<u64, _, _>(|scope| {
            // Create an input collection
            let (input_handle, collection) = scope.new_collection::<i32, isize>();

            // Count the total number of items in the collection
            // count() returns ((element, occurrence_count), time, diff)
            // To get total count, we map to unit key first, then count
            let total_count = collection
                .map(|_item| ())  // Map all items to unit key ()
                .count();         // Now count gives us ((), total_count)

            let outputs = outputs_clone.clone();
            total_count.inspect(move |(((), count), time, diff)| {
                // () is the key (unit), count is the total count
                if *diff > 0 {
                    outputs.lock().unwrap().push((*time, *count));
                }
            });

            // Create a probe to track progress
            let probe = total_count.probe();

            (input_handle, probe)
        });

        // Process events one at a time
        for (time, (item, is_insert)) in events.into_iter().enumerate() {
            let time = time as u64;

            if is_insert {
                input.insert(item);
            } else {
                input.remove(item);
            }

            // Advance to next time and flush
            input.advance_to(time + 1);
            input.flush();

            // Step until we've processed up to this time
            while probe.less_than(&(time + 1)) {
                worker.step();
            }
        }
    });

    // Return collected outputs
    Arc::try_unwrap(outputs)
        .expect("outputs still borrowed")
        .into_inner()
        .unwrap()
}

/// Apply a HOLD transformation to a collection.
///
/// HOLD is Boon's stateful accumulator pattern:
/// ```boon
/// initial |> HOLD state {
///     event |> THEN { transform(state) }
/// }
/// ```
///
/// In DD terms: maintains a single accumulated state value,
/// applying a transform function on each input event.
///
/// # Type Parameters
/// - `G`: The Timely scope
/// - `S`: State type (must be Data + Clone for DD compatibility)
/// - `E`: Event type (input events that trigger state updates)
/// - `F`: Transform function `(current_state, event) -> new_state`
///
/// # Returns
/// A collection containing state snapshots after each event.
/// Each output is `(state_value, time, +1)` representing the new state.
pub fn hold<G, S, E, F>(
    initial: S,
    events: &VecCollection<G, E>,
    transform: F,
) -> VecCollection<G, S>
where
    G: Scope,
    G::Timestamp: Clone,
    S: timely::Data + Clone,
    E: timely::Data,
    F: Fn(&S, &E) -> S + 'static,
{
    // Access the underlying stream of (data, time, diff) tuples
    // and apply a stateful unary operator
    events
        .inner
        .unary(Pipeline, "Hold", move |_capability, _info| {
            // State is captured in the closure - persists across batches
            let mut state = initial;

            move |input, output| {
                // Process each batch of input data
                input.for_each(|time, data| {
                    let mut session = output.session(&time);

                    for (event, _event_time, diff) in data.iter() {
                        // Only process insertions (diff > 0), not deletions
                        // This matches Boon's THEN semantics where events trigger
                        // state updates, but removals don't "un-trigger"
                        if *diff > 0 {
                            // Apply transform to get new state
                            state = transform(&state, event);
                            // Emit the new state value
                            session.give((state.clone(), time.time().clone(), 1isize));
                        }
                    }
                });
            }
        })
        .as_collection()
}

/// Run a HOLD-based counter dataflow as proof of concept.
///
/// This demonstrates:
/// 1. Using HOLD to accumulate state (counter value)
/// 2. Events trigger state transitions
/// 3. Each event increments the counter
///
/// Boon equivalent:
/// ```boon
/// 0 |> HOLD count {
///     click |> THEN { count + 1 }
/// }
/// ```
pub fn run_hold_counter_dataflow(num_clicks: usize) -> Vec<(u64, i64)> {
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let outputs_clone = outputs.clone();

    timely::execute_directly(move |worker| {
        let (mut input, probe) = worker.dataflow::<u64, _, _>(|scope| {
            // Create input collection for click events
            // Using () as the event type since we only care about event occurrence
            let (input_handle, clicks) = scope.new_collection::<(), isize>();

            // Apply HOLD to count clicks
            // Initial state: 0
            // Transform: on each click, increment count
            let count = hold(0i64, &clicks, |state, _event: &()| state + 1);

            // Capture outputs
            let outputs = outputs_clone.clone();
            count.inspect(move |(state, time, diff)| {
                if *diff > 0 {
                    outputs.lock().unwrap().push((*time, *state));
                }
            });

            let probe = count.probe();
            (input_handle, probe)
        });

        // Simulate clicks
        for time in 0..num_clicks {
            let time = time as u64;

            // Insert a click event
            input.insert(());

            input.advance_to(time + 1);
            input.flush();

            while probe.less_than(&(time + 1)) {
                worker.step();
            }
        }
    });

    Arc::try_unwrap(outputs)
        .expect("outputs still borrowed")
        .into_inner()
        .unwrap()
}

// ============================================================================
// LIST OPERATORS - O(delta) operations on DD collections
// ============================================================================
// These operators provide incremental computation over collections.
// Using DD 0.18's VecCollection<G, D> API which is Collection<G, Vec<(D, Time, Diff)>>.

use differential_dataflow::operators::Threshold;

/// Filter a collection by predicate - O(delta).
///
/// Only elements where `predicate(element)` returns true are retained.
/// DD incrementally processes only changed elements, not the entire collection.
///
/// Boon equivalent:
/// ```boon
/// list |> List/retain({ item => condition })
/// ```
pub fn list_filter<G, D, F>(
    collection: &VecCollection<G, D>,
    predicate: F,
) -> VecCollection<G, D>
where
    G: Scope,
    G::Timestamp: Clone,
    D: timely::Data,
    F: Fn(&D) -> bool + 'static,
{
    // DD's filter operation processes only deltas
    collection.filter(move |item| predicate(item))
}

/// Map over a collection - O(delta).
///
/// Transforms each element using the provided function.
/// DD incrementally processes only changed elements.
///
/// Boon equivalent:
/// ```boon
/// list |> List/map({ item => transform(item) })
/// ```
pub fn list_map<G, D, D2, F>(
    collection: &VecCollection<G, D>,
    transform: F,
) -> VecCollection<G, D2>
where
    G: Scope,
    G::Timestamp: Clone,
    D: timely::Data,
    D2: timely::Data,
    F: Fn(D) -> D2 + 'static,
{
    // DD's map operation processes only deltas
    collection.map(move |item| transform(item))
}

/// Count elements in a collection - O(1) per change.
///
/// Returns a collection containing the current count.
/// Each change to the input produces an incremental count update.
///
/// Boon equivalent:
/// ```boon
/// list |> List/count()
/// ```
pub fn list_count<G, D>(
    collection: &VecCollection<G, D>,
) -> VecCollection<G, isize>
where
    G: Scope,
    G::Timestamp: Clone + Ord + Lattice,
    D: timely::Data,
{
    // Map all elements to unit key, then count
    // This gives us the total count across all elements
    collection
        .map(|_| ())
        .count()
        .map(|((), count)| count)
}

/// Count elements matching a predicate - O(delta).
///
/// Returns a collection containing the count of matching elements.
/// Only processes changed elements, not the entire collection.
///
/// Boon equivalent:
/// ```boon
/// list |> List/retain(predicate) |> List/count()
/// ```
pub fn list_count_where<G, D, F>(
    collection: &VecCollection<G, D>,
    predicate: F,
) -> VecCollection<G, isize>
where
    G: Scope,
    G::Timestamp: Clone + Ord + Lattice,
    D: timely::Data,
    F: Fn(&D) -> bool + 'static,
{
    // Filter then count - DD optimizes this incrementally
    list_count(&list_filter(collection, predicate))
}

/// Check if a collection is empty - O(1) per change.
///
/// Returns a collection containing a boolean indicating emptiness.
///
/// Boon equivalent:
/// ```boon
/// list |> List/count() |> { count => count == 0 }
/// ```
pub fn list_is_empty<G, D>(
    collection: &VecCollection<G, D>,
) -> VecCollection<G, bool>
where
    G: Scope,
    G::Timestamp: Clone + Ord + Lattice,
    D: timely::Data,
{
    list_count(collection).map(|count| count == 0)
}

/// Flat-map over a collection - O(delta).
///
/// Each input element can produce zero or more output elements.
/// DD incrementally processes only changed elements.
///
/// Boon equivalent:
/// ```boon
/// list |> List/flat_map({ item => items })
/// ```
pub fn list_flat_map<G, D, D2, I, F>(
    collection: &VecCollection<G, D>,
    transform: F,
) -> VecCollection<G, D2>
where
    G: Scope,
    G::Timestamp: Clone,
    D: timely::Data,
    D2: timely::Data,
    I: IntoIterator<Item = D2>,
    F: Fn(D) -> I + 'static,
{
    collection.flat_map(move |item| transform(item))
}

/// Concatenate two collections - O(1).
///
/// Returns a collection containing all elements from both inputs.
/// This is the DD equivalent of Boon's LATEST for collections.
///
/// Boon equivalent:
/// ```boon
/// LATEST { list1, list2 }
/// ```
pub fn list_concat<G, D>(
    collection1: &VecCollection<G, D>,
    collection2: &VecCollection<G, D>,
) -> VecCollection<G, D>
where
    G: Scope,
    G::Timestamp: Clone,
    D: timely::Data,
{
    collection1.concat(collection2)
}

/// Distinct elements in a collection - O(delta).
///
/// Removes duplicate elements, keeping only one of each.
/// DD incrementally maintains the distinct set.
///
/// Boon equivalent:
/// ```boon
/// list |> List/distinct()
/// ```
pub fn list_distinct<G, D>(
    collection: &VecCollection<G, D>,
) -> VecCollection<G, D>
where
    G: Scope,
    G::Timestamp: Clone + Ord + Lattice,
    D: timely::Data + differential_dataflow::ExchangeData + std::hash::Hash,
{
    collection.distinct()
}

/// Run a more complex HOLD example: accumulating a sum.
///
/// Boon equivalent:
/// ```boon
/// 0 |> HOLD total {
///     number |> THEN { total + number }
/// }
/// ```
pub fn run_hold_sum_dataflow(numbers: Vec<i64>) -> Vec<(u64, i64)> {
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let outputs_clone = outputs.clone();

    timely::execute_directly(move |worker| {
        let (mut input, probe) = worker.dataflow::<u64, _, _>(|scope| {
            let (input_handle, nums) = scope.new_collection::<i64, isize>();

            // HOLD that sums all incoming numbers
            let sum = hold(0i64, &nums, |total, num| total + num);

            let outputs = outputs_clone.clone();
            sum.inspect(move |(state, time, diff)| {
                if *diff > 0 {
                    outputs.lock().unwrap().push((*time, *state));
                }
            });

            let probe = sum.probe();
            (input_handle, probe)
        });

        for (time, num) in numbers.into_iter().enumerate() {
            let time = time as u64;
            input.insert(num);
            input.advance_to(time + 1);
            input.flush();

            while probe.less_than(&(time + 1)) {
                worker.step();
            }
        }
    });

    Arc::try_unwrap(outputs)
        .expect("outputs still borrowed")
        .into_inner()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_basic() {
        let outputs = run_counter_dataflow(vec![
            (1, true),   // Insert 1 -> count = 1
            (2, true),   // Insert 2 -> count = 2
            (3, true),   // Insert 3 -> count = 3
        ]);

        // Should have counts: 1, 2, 3
        assert!(!outputs.is_empty(), "Should have outputs");

        // Check final count is 3
        if let Some((_, last_count)) = outputs.last() {
            assert_eq!(*last_count, 3, "Final count should be 3");
        }
    }

    #[test]
    fn test_counter_insert_remove() {
        let outputs = run_counter_dataflow(vec![
            (1, true),   // Insert 1 -> count = 1
            (2, true),   // Insert 2 -> count = 2
            (1, false),  // Remove 1 -> count = 1
        ]);

        assert!(!outputs.is_empty(), "Should have outputs");

        // Check final count is 1
        if let Some((_, last_count)) = outputs.last() {
            assert_eq!(*last_count, 1, "Final count should be 1 after removal");
        }
    }

    #[test]
    fn test_runtime_time_advance() {
        let mut runtime = DdRuntime::new();
        assert_eq!(runtime.current_time(), 0);

        assert_eq!(runtime.advance_time(), 1);
        assert_eq!(runtime.advance_time(), 2);
        assert_eq!(runtime.current_time(), 2);
    }

    // Phase 2: HOLD operator tests

    #[test]
    fn test_hold_counter() {
        // Test: 0 |> HOLD count { click |> THEN { count + 1 } }
        let outputs = run_hold_counter_dataflow(5);

        assert_eq!(outputs.len(), 5, "Should have 5 outputs for 5 clicks");

        // Check sequence: 1, 2, 3, 4, 5
        let counts: Vec<i64> = outputs.iter().map(|(_, count)| *count).collect();
        assert_eq!(counts, vec![1, 2, 3, 4, 5], "Counter should increment");
    }

    #[test]
    fn test_hold_counter_empty() {
        let outputs = run_hold_counter_dataflow(0);
        assert!(outputs.is_empty(), "No clicks should produce no outputs");
    }

    #[test]
    fn test_hold_sum() {
        // Test: 0 |> HOLD total { number |> THEN { total + number } }
        let outputs = run_hold_sum_dataflow(vec![10, 20, 30, 40]);

        let sums: Vec<i64> = outputs.iter().map(|(_, sum)| *sum).collect();
        assert_eq!(sums, vec![10, 30, 60, 100], "Should accumulate: 10, 30, 60, 100");
    }

    #[test]
    fn test_hold_sum_with_negatives() {
        let outputs = run_hold_sum_dataflow(vec![100, -30, 50, -20]);

        let sums: Vec<i64> = outputs.iter().map(|(_, sum)| *sum).collect();
        assert_eq!(sums, vec![100, 70, 120, 100], "Should handle negatives");
    }

    #[test]
    fn test_hold_preserves_time() {
        let outputs = run_hold_counter_dataflow(3);

        // Each output should have incrementing time
        let times: Vec<u64> = outputs.iter().map(|(time, _)| *time).collect();
        assert_eq!(times, vec![0, 1, 2], "Times should increment with events");
    }
}
