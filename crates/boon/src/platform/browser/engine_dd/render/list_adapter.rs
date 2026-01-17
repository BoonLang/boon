//! Phase 12: DD Collection to VecDiff Stream Adapter
//!
//! This module converts DD collection diffs `(Value, time, +1/-1)` to
//! `VecDiff<Value>` for Zoon's incremental DOM rendering via `children_signal_vec()`.
//!
//! # Architecture
//!
//! ```text
//! DD Dataflow (pure)
//!   └── collection with diffs: (Value, time, +1/-1)
//!       Outputs via capture() → Vec<(Value, time, diff)>
//!                ↓
//! DD-to-VecDiff Adapter (this module)
//!   └── stream::unfold() converts batches to VecDiff stream
//!       Tracks current list state locally (in unfold closure)
//!       +1 diff → VecDiff::Push
//!       -1 diff → VecDiff::RemoveAt (finds index by value)
//!                ↓
//! Zoon children_signal_vec()
//!   └── Receives VecDiff, updates DOM incrementally
//!       Each VecDiff → ONE element operation
//! ```
//!
//! # Design: Stream-Based (Like Actors Engine)
//!
//! This follows the same pattern as the actors engine's `list_change_to_vec_diff_stream`:
//! - Uses `stream::scan()` to track current list state
//! - Emits `VecDiff` variants directly (no intermediate MutableVec)
//! - Pure stream transformation, no globals, no spawned tasks
//!
//! # Usage
//!
//! ```rust,ignore
//! // Convert DD captured outputs to VecDiff stream:
//! let diff_stream = dd_diffs_to_vec_diff_stream(captured_batches);
//!
//! // In bridge.rs rendering code:
//! El::new().children_signal_vec(
//!     diff_stream.to_signal_vec()
//!         .map(|item| render_item(&item))
//! )
//! ```

use super::super::core::value::Value;
use zoon::futures_util::stream::{self, StreamExt};
use zoon::futures_util::Stream;
use zoon::futures_signals::signal_vec::VecDiff;
use zoon::future;

/// Diff type from DD's capture() output.
/// +1 = insert, -1 = remove
pub type DdDiff = isize;

/// A single diff entry from DD collection output.
/// Format: (Value, timestamp, diff)
pub type DdDiffEntry = (Value, u64, DdDiff);

/// A batch of diffs from DD collection output.
/// Format: Vec<(Value, timestamp, diff)>
pub type DdDiffBatch = Vec<DdDiffEntry>;

// ═══════════════════════════════════════════════════════════════════════════
// Primary API: Convert DD diff batches to VecDiff stream
// ═══════════════════════════════════════════════════════════════════════════

/// Converts a stream of DD diff batches to a stream of VecDiff.
///
/// This is the core adapter that bridges DD's diff-based collections
/// with Zoon's SignalVec-based incremental rendering.
///
/// # Pattern: Same as Actors Engine
///
/// This follows the actors engine's `list_change_to_vec_diff_stream` pattern:
/// - Uses `stream::scan()` to track current list state in closure
/// - Emits `VecDiff` variants directly
/// - Pure stream transformation, no globals, no spawned tasks
///
/// # Arguments
///
/// * `diff_stream` - Stream of DD diff batches
/// * `initial_items` - Initial items (e.g., from persisted state)
///
/// # Returns
///
/// A stream of `VecDiff<Value>` that can be converted to SignalVec.
///
/// # Example
///
/// ```rust,ignore
/// // Convert DD captured outputs to VecDiff stream:
/// let captured_batches = outputs_rx.extract();
/// let diff_stream = stream::iter(captured_batches.into_iter().map(|(_t, b)| b));
///
/// let vec_diff_stream = dd_diffs_to_vec_diff_stream(diff_stream, vec![]);
///
/// // In bridge.rs:
/// El::new().children_signal_vec(
///     vec_diff_stream.to_signal_vec()
///         .map(|item| render_item(&item))
/// )
/// ```
pub fn dd_diffs_to_vec_diff_stream(
    diff_stream: impl Stream<Item = DdDiffBatch>,
    initial_items: Vec<Value>,
) -> impl Stream<Item = VecDiff<Value>> {
    // Start with initial Replace if we have items
    let initial_diff = if initial_items.is_empty() {
        None
    } else {
        Some(VecDiff::Replace { values: initial_items.clone() })
    };

    // Use scan to track current list state and emit VecDiff
    let converted = diff_stream
        .flat_map(move |batch| {
            // Convert each diff in the batch to a VecDiff
            stream::iter(batch.into_iter())
        })
        .scan(initial_items, |items, (value, _timestamp, diff)| {
            let vec_diff = match diff {
                // +1 = Insert: Push to end
                1 => {
                    items.push(value.clone());
                    VecDiff::Push { value }
                }
                // -1 = Remove: Find index by value and remove
                -1 => {
                    if let Some(index) = items.iter().position(|v| v == &value) {
                        items.remove(index);
                        VecDiff::RemoveAt { index }
                    } else {
                        // Value not found - emit Replace with current state
                        // This shouldn't happen in normal DD operation
                        #[cfg(debug_assertions)]
                        zoon::println!("[DD list_adapter] Remove for unknown value, emitting Replace");
                        VecDiff::Replace { values: items.clone() }
                    }
                }
                // Other diffs (shouldn't happen)
                _ => {
                    #[cfg(debug_assertions)]
                    zoon::println!("[DD list_adapter] Unexpected diff value: {}", diff);
                    // Emit no-op Replace
                    VecDiff::Replace { values: items.clone() }
                }
            };
            future::ready(Some(vec_diff))
        });

    // Prepend initial Replace if needed
    match initial_diff {
        Some(replace) => stream::once(future::ready(replace))
            .chain(converted)
            .left_stream(),
        None => converted.right_stream(),
    }
}

/// Converts captured DD collection outputs to a VecDiff stream.
///
/// This is a convenience wrapper for the common case of processing
/// outputs from `capture().extract()`.
///
/// # Arguments
///
/// * `captured` - Output from `capture().extract()`: Vec<(time, batch)>
/// * `initial_items` - Initial items
///
/// # Example
///
/// ```rust,ignore
/// // In DD worker, after capture().extract():
/// let captured = outputs_rx.extract();
/// let vec_diff_stream = dd_captured_to_vec_diff_stream(captured, vec![]);
/// ```
pub fn dd_captured_to_vec_diff_stream(
    captured: Vec<(u64, DdDiffBatch)>,
    initial_items: Vec<Value>,
) -> impl Stream<Item = VecDiff<Value>> {
    let batches = captured.into_iter().map(|(_time, batch)| batch);
    dd_diffs_to_vec_diff_stream(stream::iter(batches), initial_items)
}

// ═══════════════════════════════════════════════════════════════════════════
// Single-batch processing (for synchronous rendering)
// ═══════════════════════════════════════════════════════════════════════════

/// Process a single diff batch into VecDiff operations.
///
/// For cases where you need synchronous processing rather than a stream.
/// Tracks state in the provided `items` vector.
///
/// # Arguments
///
/// * `items` - Current list state (mutated in place)
/// * `batch` - Batch of diffs to process
///
/// # Returns
///
/// Vector of VecDiff operations to apply.
pub fn process_diff_batch_sync(
    items: &mut Vec<Value>,
    batch: DdDiffBatch,
) -> Vec<VecDiff<Value>> {
    let mut vec_diffs = Vec::with_capacity(batch.len());

    for (value, _timestamp, diff) in batch {
        let vec_diff = match diff {
            1 => {
                items.push(value.clone());
                VecDiff::Push { value }
            }
            -1 => {
                if let Some(index) = items.iter().position(|v| v == &value) {
                    items.remove(index);
                    VecDiff::RemoveAt { index }
                } else {
                    continue; // Skip if not found
                }
            }
            _ => continue,
        };
        vec_diffs.push(vec_diff);
    }

    vec_diffs
}

// ═══════════════════════════════════════════════════════════════════════════
// Keyed adapter: O(1) removal by persistence ID
// ═══════════════════════════════════════════════════════════════════════════

/// Key-based adapter for O(1) lookups during removal.
///
/// When items have unique keys (like persistence IDs), this adapter
/// maintains a key→index mapping for O(1) removal instead of O(n) scan.
///
/// # Why Key-Based Removal?
///
/// The basic adapter finds items by value equality (O(n) scan).
/// For large lists, this can be slow. Key-based removal uses a HashMap
/// for O(1) lookup, similar to how React's key prop optimizes list diffing.
pub struct KeyedListAdapter<F>
where
    F: Fn(&Value) -> String,
{
    items: Vec<Value>,
    key_to_index: std::collections::HashMap<String, usize>,
    key_fn: F,
}

impl<F> KeyedListAdapter<F>
where
    F: Fn(&Value) -> String,
{
    /// Create a new keyed adapter with initial items.
    ///
    /// `key_fn` extracts the unique key from each value.
    pub fn new(initial_items: Vec<Value>, key_fn: F) -> Self {
        let mut key_to_index = std::collections::HashMap::new();
        for (i, item) in initial_items.iter().enumerate() {
            key_to_index.insert(key_fn(item), i);
        }

        Self {
            items: initial_items,
            key_to_index,
            key_fn,
        }
    }

    /// Process a diff batch with key-based lookup.
    ///
    /// Returns VecDiff operations to apply.
    pub fn process_batch(&mut self, batch: DdDiffBatch) -> Vec<VecDiff<Value>> {
        let mut vec_diffs = Vec::with_capacity(batch.len());

        for (value, _timestamp, diff) in batch {
            let key = (self.key_fn)(&value);

            match diff {
                1 => {
                    // Insert: add to end and update index
                    let index = self.items.len();
                    self.items.push(value.clone());
                    self.key_to_index.insert(key, index);
                    vec_diffs.push(VecDiff::Push { value });
                }
                -1 => {
                    // Remove: O(1) lookup by key
                    if let Some(index) = self.key_to_index.remove(&key) {
                        self.items.remove(index);
                        // Update indices for items after the removed one
                        for (_, idx) in self.key_to_index.iter_mut() {
                            if *idx > index {
                                *idx -= 1;
                            }
                        }
                        vec_diffs.push(VecDiff::RemoveAt { index });
                    }
                }
                _ => {}
            }
        }

        vec_diffs
    }

    /// Get current items.
    pub fn items(&self) -> &[Value] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn test_dd_diffs_to_vec_diff_stream_insert() {
        let batches = vec![
            vec![
                (Value::text("a"), 1, 1),
                (Value::text("b"), 2, 1),
                (Value::text("c"), 3, 1),
            ],
        ];

        let stream = dd_diffs_to_vec_diff_stream(stream::iter(batches), vec![]);
        let diffs: Vec<_> = stream.collect().await;

        assert_eq!(diffs.len(), 3);
        assert!(matches!(&diffs[0], VecDiff::Push { value } if value == &Value::text("a")));
        assert!(matches!(&diffs[1], VecDiff::Push { value } if value == &Value::text("b")));
        assert!(matches!(&diffs[2], VecDiff::Push { value } if value == &Value::text("c")));
    }

    #[tokio::test]
    async fn test_dd_diffs_to_vec_diff_stream_with_initial() {
        let initial = vec![Value::text("x"), Value::text("y")];
        let batches = vec![
            vec![(Value::text("z"), 1, 1)],
        ];

        let stream = dd_diffs_to_vec_diff_stream(stream::iter(batches), initial.clone());
        let diffs: Vec<_> = stream.collect().await;

        assert_eq!(diffs.len(), 2);
        assert!(matches!(&diffs[0], VecDiff::Replace { values } if values == &initial));
        assert!(matches!(&diffs[1], VecDiff::Push { value } if value == &Value::text("z")));
    }

    #[tokio::test]
    async fn test_dd_diffs_to_vec_diff_stream_remove() {
        let initial = vec![Value::text("a"), Value::text("b"), Value::text("c")];
        let batches = vec![
            vec![(Value::text("b"), 4, -1)], // Remove "b"
        ];

        let stream = dd_diffs_to_vec_diff_stream(stream::iter(batches), initial.clone());
        let diffs: Vec<_> = stream.collect().await;

        assert_eq!(diffs.len(), 2);
        assert!(matches!(&diffs[0], VecDiff::Replace { values } if values == &initial));
        assert!(matches!(&diffs[1], VecDiff::RemoveAt { index } if *index == 1));
    }

    #[test]
    fn test_process_diff_batch_sync() {
        let mut items = vec![];

        // Insert a, b, c
        let diffs = process_diff_batch_sync(&mut items, vec![
            (Value::text("a"), 1, 1),
            (Value::text("b"), 2, 1),
            (Value::text("c"), 3, 1),
        ]);

        assert_eq!(items.len(), 3);
        assert_eq!(diffs.len(), 3);

        // Remove b
        let diffs = process_diff_batch_sync(&mut items, vec![
            (Value::text("b"), 4, -1),
        ]);

        assert_eq!(items.len(), 2);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(&diffs[0], VecDiff::RemoveAt { index } if *index == 1));
    }

    #[test]
    fn test_keyed_adapter() {
        // Key function extracts "id" field from object
        let key_fn = |v: &Value| {
            v.get("id")
                .and_then(|id| id.as_text())
                .unwrap_or_default()
                .to_string()
        };

        let item_a = Value::object([("id", Value::text("a")), ("name", Value::text("Alice"))]);
        let item_b = Value::object([("id", Value::text("b")), ("name", Value::text("Bob"))]);

        let mut adapter = KeyedListAdapter::new(vec![], key_fn);

        // Insert items
        let diffs = adapter.process_batch(vec![
            (item_a.clone(), 1, 1),
            (item_b.clone(), 2, 1),
        ]);
        assert_eq!(diffs.len(), 2);
        assert_eq!(adapter.items().len(), 2);

        // Remove by key (item_a)
        let diffs = adapter.process_batch(vec![
            (item_a, 3, -1),
        ]);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(&diffs[0], VecDiff::RemoveAt { index } if *index == 0));
        assert_eq!(adapter.items().len(), 1);
    }
}
