use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, Wake, Waker};

use super::evaluator::{FunctionRegistry, evaluate_static_expression_with_registry};
use boon::parser;
use boon::parser::SourceCode;
use boon::parser::static_expression;

use ulid::Ulid;

use zoon::IntoCowStr;
use zoon::future;
use zoon::futures_channel::{mpsc, oneshot};
use zoon::futures_util::SinkExt;
use zoon::futures_util::future::{FutureExt, LocalBoxFuture};
use zoon::futures_util::select;
use zoon::futures_util::stream::{self, LocalBoxStream, Stream, StreamExt};
use zoon::{Deserialize, DeserializeOwned, Serialize, serde, serde_json};
use zoon::{Task, TaskHandle};
use zoon::{WebStorage, local_storage};

use smallvec::SmallVec;
use std::cell::{Cell, RefCell};

// --- Live Actor Count (always-on, not behind feature flag) ---
// Used to detect actor leaks when switching examples.
thread_local! {
    static LIVE_ACTOR_COUNT: Cell<u64> = const { Cell::new(0) };
    static PENDING_SCOPE_DESTROYS: std::cell::RefCell<Vec<ScopeId>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn live_actor_count() -> u64 {
    LIVE_ACTOR_COUNT.with(|c| c.get())
}

fn list_instance_key(list: &Arc<List>) -> usize {
    Arc::as_ptr(list) as usize
}

// --- Performance Metrics ---
//
// Compile-time instrumentation counters for profiling the actors engine.
// Enabled via `--features actors-metrics`. Compiles to no-ops when disabled.

/// Increment a metric counter. No-op when `actors-metrics` feature is disabled.
#[cfg(feature = "actors-metrics")]
macro_rules! inc_metric {
    ($counter:ident) => {
        $crate::engine::metrics::$counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    };
    ($counter:ident, $amount:expr) => {
        $crate::engine::metrics::$counter.fetch_add($amount, std::sync::atomic::Ordering::Relaxed);
    };
}

#[cfg(not(feature = "actors-metrics"))]
macro_rules! inc_metric {
    ($counter:ident) => {};
    ($counter:ident, $amount:expr) => {};
}

pub(crate) use inc_metric;

#[cfg(feature = "actors-metrics")]
pub mod metrics {
    use std::sync::atomic::{AtomicU64, Ordering};

    // Bridge events
    pub static CHANGE_EVENTS_CONSTRUCTED: AtomicU64 = AtomicU64::new(0);
    pub static KEYDOWN_EVENTS_CONSTRUCTED: AtomicU64 = AtomicU64::new(0);
    pub static HOVER_EVENTS_EMITTED: AtomicU64 = AtomicU64::new(0);
    pub static HOVER_EVENTS_DEDUPED: AtomicU64 = AtomicU64::new(0);
    pub static CHANGE_EVENTS_DEDUPED: AtomicU64 = AtomicU64::new(0);

    // List path
    pub static REPLACE_PAYLOADS_SENT: AtomicU64 = AtomicU64::new(0);
    pub static REPLACE_PAYLOAD_TOTAL_ITEMS: AtomicU64 = AtomicU64::new(0);
    pub static REPLACE_FANOUT_SENDS: AtomicU64 = AtomicU64::new(0);
    pub static RETAIN_PREDICATE_REBUILDS: AtomicU64 = AtomicU64::new(0);
    pub static RETAIN_PREDICATE_ITEMS: AtomicU64 = AtomicU64::new(0);
    pub static REMOVE_TASKS_SPAWNED: AtomicU64 = AtomicU64::new(0);
    pub static REMOVE_TASKS_COMPLETED: AtomicU64 = AtomicU64::new(0);

    // Actor lifecycle
    pub static ACTORS_CREATED: AtomicU64 = AtomicU64::new(0);
    pub static ACTORS_DROPPED: AtomicU64 = AtomicU64::new(0);
    pub static CHANNEL_DROPS: AtomicU64 = AtomicU64::new(0);

    // Evaluator
    pub static SLOTS_ALLOCATED: AtomicU64 = AtomicU64::new(0);
    pub static REGISTRY_CLONES: AtomicU64 = AtomicU64::new(0);
    pub static REGISTRY_CLONE_ENTRIES: AtomicU64 = AtomicU64::new(0);

    // Persistence
    pub static PERSISTENCE_WRITES: AtomicU64 = AtomicU64::new(0);

    /// Dump all counters to the browser console.
    pub fn dump_to_console() {
        zoon::println!("[actors-metrics] === Performance Counters ===");
        zoon::println!(
            "[actors-metrics] Bridge: change_constructed={}, keydown_constructed={}, hover_emitted={}, hover_deduped={}, change_deduped={}",
            CHANGE_EVENTS_CONSTRUCTED.load(Ordering::Relaxed),
            KEYDOWN_EVENTS_CONSTRUCTED.load(Ordering::Relaxed),
            HOVER_EVENTS_EMITTED.load(Ordering::Relaxed),
            HOVER_EVENTS_DEDUPED.load(Ordering::Relaxed),
            CHANGE_EVENTS_DEDUPED.load(Ordering::Relaxed),
        );
        zoon::println!(
            "[actors-metrics] List: replace_sent={}, replace_items={}, fanout_sends={}, retain_rebuilds={}, retain_items={}, remove_spawned={}, remove_completed={}",
            REPLACE_PAYLOADS_SENT.load(Ordering::Relaxed),
            REPLACE_PAYLOAD_TOTAL_ITEMS.load(Ordering::Relaxed),
            REPLACE_FANOUT_SENDS.load(Ordering::Relaxed),
            RETAIN_PREDICATE_REBUILDS.load(Ordering::Relaxed),
            RETAIN_PREDICATE_ITEMS.load(Ordering::Relaxed),
            REMOVE_TASKS_SPAWNED.load(Ordering::Relaxed),
            REMOVE_TASKS_COMPLETED.load(Ordering::Relaxed),
        );
        zoon::println!(
            "[actors-metrics] Actors: created={}, dropped={}, channel_drops={}",
            ACTORS_CREATED.load(Ordering::Relaxed),
            ACTORS_DROPPED.load(Ordering::Relaxed),
            CHANNEL_DROPS.load(Ordering::Relaxed),
        );
        zoon::println!(
            "[actors-metrics] Evaluator: slots_allocated={}, registry_clones={}, registry_clone_entries={}",
            SLOTS_ALLOCATED.load(Ordering::Relaxed),
            REGISTRY_CLONES.load(Ordering::Relaxed),
            REGISTRY_CLONE_ENTRIES.load(Ordering::Relaxed),
        );
        zoon::println!(
            "[actors-metrics] Persistence: writes={}",
            PERSISTENCE_WRITES.load(Ordering::Relaxed),
        );
    }

    /// Reset all counters to zero.
    pub fn reset() {
        CHANGE_EVENTS_CONSTRUCTED.store(0, Ordering::Relaxed);
        KEYDOWN_EVENTS_CONSTRUCTED.store(0, Ordering::Relaxed);
        HOVER_EVENTS_EMITTED.store(0, Ordering::Relaxed);
        HOVER_EVENTS_DEDUPED.store(0, Ordering::Relaxed);
        CHANGE_EVENTS_DEDUPED.store(0, Ordering::Relaxed);
        REPLACE_PAYLOADS_SENT.store(0, Ordering::Relaxed);
        REPLACE_PAYLOAD_TOTAL_ITEMS.store(0, Ordering::Relaxed);
        REPLACE_FANOUT_SENDS.store(0, Ordering::Relaxed);
        RETAIN_PREDICATE_REBUILDS.store(0, Ordering::Relaxed);
        RETAIN_PREDICATE_ITEMS.store(0, Ordering::Relaxed);
        REMOVE_TASKS_SPAWNED.store(0, Ordering::Relaxed);
        REMOVE_TASKS_COMPLETED.store(0, Ordering::Relaxed);
        ACTORS_CREATED.store(0, Ordering::Relaxed);
        ACTORS_DROPPED.store(0, Ordering::Relaxed);
        CHANNEL_DROPS.store(0, Ordering::Relaxed);
        SLOTS_ALLOCATED.store(0, Ordering::Relaxed);
        REGISTRY_CLONES.store(0, Ordering::Relaxed);
        REGISTRY_CLONE_ENTRIES.store(0, Ordering::Relaxed);
        PERSISTENCE_WRITES.store(0, Ordering::Relaxed);
    }
}

// --- Channel Capacity Constants (A3) ---
//
// Named constants for all NamedChannel and mpsc::channel capacities.
// Grouped by component for easy tuning.

// Lazy value actor
const LAZY_ACTOR_REQUEST_CAPACITY: usize = 16;

// List channels
const LIST_CHANGE_CAPACITY: usize = 64;

// Infrastructure channels
const CONSTRUCT_STORAGE_INSERTER_CAPACITY: usize = 32;
const CONSTRUCT_STORAGE_GETTER_CAPACITY: usize = 32;
const CALL_RECORDER_CAPACITY: usize = 64;
const OUTPUT_VALVE_IMPULSE_CAPACITY: usize = 1;
const LINK_VALUE_CAPACITY: usize = 128;
const REFERENCE_CONNECTOR_INSERTER_CAPACITY: usize = 64;
const REFERENCE_CONNECTOR_GETTER_CAPACITY: usize = 64;
const LINK_CONNECTOR_INSERTER_CAPACITY: usize = 64;
const LINK_CONNECTOR_GETTER_CAPACITY: usize = 64;
const PASS_THROUGH_OPS_CAPACITY: usize = 32;
const PASS_THROUGH_GETTER_CAPACITY: usize = 32;
const PASS_THROUGH_SENDER_GETTER_CAPACITY: usize = 32;
const VIRTUAL_FILESYSTEM_REQUEST_CAPACITY: usize = 32;

// Module loader
pub const MODULE_LOADER_REQUEST_CAPACITY: usize = 16;

// Bridge DOM event channels
pub const BRIDGE_HOVER_CAPACITY: usize = 2;
pub const BRIDGE_PRESS_EVENT_CAPACITY: usize = 8;
pub const BRIDGE_TEXT_CHANGE_CAPACITY: usize = 16;
pub const BRIDGE_KEY_DOWN_CAPACITY: usize = 32;
pub const BRIDGE_BLUR_CAPACITY: usize = 8;
pub const BRIDGE_FOCUS_CAPACITY: usize = 8;

// Bridge pending event buffer caps
pub const BRIDGE_PENDING_KEY_DOWN_CAP: usize = 64;
pub const BRIDGE_PENDING_BLUR_CAP: usize = 16;
pub const BRIDGE_PENDING_FOCUS_CAP: usize = 16;

// --- Emission Sequence Clock ---
//
// The Actors runtime exposes only "emission sequence" semantics here:
// values get a monotonic per-runtime sequence number and consumers compare
// those sequence numbers for ordering and late-subscription filtering.

thread_local! {
    /// Thread-local monotonic emission counter for the current execution context.
    /// In browser WASM, there's only one thread so this behaves as one runtime-local counter.
    static LOCAL_CLOCK: Cell<u64> = const { Cell::new(0) };
}

/// Advance the local clock and return the next emission sequence.
pub fn next_emission_seq() -> EmissionSeq {
    LOCAL_CLOCK.with(|c| {
        let new_time = c.get() + 1;
        c.set(new_time);
        new_time
    })
}

/// Observe an externally captured emission sequence and advance the local floor.
pub fn observe_emission_seq(received_seq: EmissionSeq) -> EmissionSeq {
    LOCAL_CLOCK.with(|c| {
        let new_time = c.get().max(received_seq) + 1;
        c.set(new_time);
        new_time
    })
}

/// Get the current emission sequence floor without advancing it.
pub fn current_emission_seq() -> EmissionSeq {
    LOCAL_CLOCK.with(|c| c.get())
}

// --- TimestampedEvent ---
//
// Wrapper type that enforces external-source emission capture on DOM events.
// Using this type instead of raw data prevents future bugs where
// developers forget to capture timestamps.

/// DOM event data with a captured emission sequence.
/// The sequence is captured at the moment the DOM callback fires,
/// BEFORE the event is sent through channels. This ensures correct
/// happened-before ordering even when `select!` processes events
/// out of order.
#[derive(Debug, Clone)]
pub struct TimestampedEvent<T> {
    pub data: T,
    pub emission_seq: EmissionSeq,
}

impl<T> TimestampedEvent<T> {
    /// Create a timestamped event - captures the current emission sequence.
    /// Call this in DOM callbacks, BEFORE sending to channels.
    pub fn now(data: T) -> Self {
        Self {
            data,
            emission_seq: next_emission_seq(),
        }
    }
}

// --- List Item Origin Tracking ---
//
// Each list item carries its own origin info (no global registry).
// When List/remove removes an item, it uses the origin to update
// its branch-local removed set in localStorage.

/// Origin info for items created by persisted List/append.
/// Each item carries this to enable removal tracking.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListItemOrigin {
    /// The localStorage key for the source list's recorded calls
    /// e.g., "list_calls:list_append_Span(123:456)"
    pub source_storage_key: String,
    /// Stable identifier for this item's call (survives restores)
    /// e.g., "call_0", "call_1"
    pub call_id: String,
}

/// Add an item's call_id to a branch's removed set.
/// Each List/remove site maintains its own removed set.
/// This is idempotent (adding twice is safe).
pub fn add_to_removed_set(removed_set_key: &str, call_id: &str) {
    // Load existing removed set (or empty)
    let mut removed: Vec<String> = match local_storage().get(removed_set_key) {
        Some(Ok(set)) => set,
        _ => Vec::new(),
    };

    // Add if not already present (idempotent)
    if !removed.contains(&call_id.to_string()) {
        removed.push(call_id.to_string());
        if let Err(e) = local_storage().insert(removed_set_key, &removed) {
            zoon::eprintln!("[DEBUG] Failed to save removed set: {:#}", e);
        } else if LOG_DEBUG {
            zoon::println!(
                "[DEBUG] Added {} to removed set {}, now {} items",
                call_id,
                removed_set_key,
                removed.len()
            );
        }
    }
}

/// Load a branch's removed set from storage.
/// Returns empty vec if not found or error.
pub fn load_removed_set(removed_set_key: &str) -> Vec<String> {
    match local_storage().get(removed_set_key) {
        Some(Ok(set)) => set,
        _ => Vec::new(),
    }
}

// --- NamedChannel ---

/// Error returned by `NamedChannel::send()`.
/// Note: The value is lost because it was consumed by the send future.
#[derive(Debug)]
pub enum ChannelError {
    /// Channel is closed (receiver dropped).
    Closed,
    /// Send timed out (only in debug-channels mode).
    #[cfg(feature = "debug-channels")]
    Timeout,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Closed => write!(f, "channel closed"),
            #[cfg(feature = "debug-channels")]
            ChannelError::Timeout => write!(f, "send timeout"),
        }
    }
}

/// Wrapper around mpsc::Sender with debugging capabilities.
///
/// Provides named identification, backpressure logging, and optional timeouts.
/// All inter-actor communication should use this wrapper for observability.
///
/// # Features
/// - **Named identification**: Know which channel is stuck/dropping
/// - **Backpressure logging**: Log when events are dropped
/// - **Debug timeouts**: Detect infinite waits in debug builds (feature flag)
///
/// # Usage
/// ```ignore
/// // Create named bounded channel
/// let (tx, rx) = NamedChannel::new("value_actor.messages", 16);
///
/// // Async send with debug timeout
/// tx.send(value).await.ok();
///
/// // Fire-and-forget (for DOM events)
/// tx.send_or_drop(event);
/// ```
pub struct NamedChannel<T> {
    inner: mpsc::Sender<T>,
    name: &'static str,
    capacity: usize,
}

impl<T> Clone for NamedChannel<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            name: self.name,
            capacity: self.capacity,
        }
    }
}

impl<T> NamedChannel<T> {
    /// Create a named bounded channel with the specified capacity.
    ///
    /// # Arguments
    /// * `name` - Static string identifying this channel (e.g., "value_actor.messages")
    /// * `capacity` - Maximum number of pending messages before backpressure kicks in
    ///
    /// # Returns
    /// Tuple of (sender, receiver)
    pub fn new(name: &'static str, capacity: usize) -> (Self, mpsc::Receiver<T>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                inner: tx,
                name,
                capacity,
            },
            rx,
        )
    }

    /// Get the channel name (for debugging/logging).
    #[allow(dead_code)]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Get the channel capacity.
    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Check if the receiver has been dropped.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Async send with optional debug timeout.
    ///
    /// In production (no debug-channels feature): blocks until space is available.
    /// In debug mode (debug-channels feature): times out after 5 seconds with error log.
    #[cfg(feature = "debug-channels")]
    pub async fn send(&self, value: T) -> Result<(), ChannelError> {
        use zoon::Timer;

        // 5 second timeout for debug mode
        const TIMEOUT_MS: u32 = 5000;

        let mut sender = self.inner.clone();
        let send_fut = sender.send(value);

        select! {
            result = send_fut.fuse() => {
                result.map_err(|_e| ChannelError::Closed)
            }
            _ = Timer::sleep(TIMEOUT_MS).fuse() => {
                // The value is lost - it was consumed by the send future.
                // Log the error - this is a potential deadlock
                zoon::eprintln!(
                    "[CHANNEL TIMEOUT] '{}' blocked for {}ms (capacity: {}) - possible deadlock!",
                    self.name, TIMEOUT_MS, self.capacity
                );
                Err(ChannelError::Timeout)
            }
        }
    }

    /// Async send (production mode - no timeout).
    #[cfg(not(feature = "debug-channels"))]
    pub async fn send(&self, value: T) -> Result<(), ChannelError> {
        self.inner
            .clone()
            .send(value)
            .await
            .map_err(|_e| ChannelError::Closed)
    }

    /// Fire-and-forget send - logs if dropped (for DOM events and other droppable events).
    ///
    /// Use this when dropping events is acceptable (user input, periodic updates).
    /// Always logs when events are dropped for observability.
    pub fn send_or_drop(&self, value: T) {
        if let Err(e) = self.inner.clone().try_send(value) {
            inc_metric!(CHANNEL_DROPS);
            if LOG_ACTOR_FLOW {
                zoon::eprintln!(
                    "[FLOW] send_or_drop FAILED on '{}' (full or disconnected, capacity: {})",
                    self.name,
                    self.capacity
                );
            }
            #[cfg(feature = "debug-channels")]
            if e.is_full() {
                zoon::eprintln!(
                    "[BACKPRESSURE DROP] '{}' channel FULL (capacity: {})",
                    self.name,
                    self.capacity
                );
            } else {
                zoon::eprintln!(
                    "[BACKPRESSURE DROP] '{}' channel DISCONNECTED (receiver dropped)",
                    self.name,
                );
            }
        } else if LOG_ACTOR_FLOW && self.name == "value_actor.subscriptions" {
            zoon::println!(
                "[FLOW] send_or_drop OK on '{}' (subscription sent)",
                self.name
            );
        }
    }

    /// Try send with explicit result (for sync contexts).
    ///
    /// Returns error if channel is full or closed.
    /// Logs backpressure events for observability.
    #[allow(dead_code)]
    pub fn try_send(&self, value: T) -> Result<(), mpsc::TrySendError<T>> {
        let result = self.inner.clone().try_send(value);
        #[cfg(feature = "debug-channels")]
        if let Err(ref e) = result {
            if e.is_full() {
                zoon::eprintln!(
                    "[BACKPRESSURE] '{}' channel full (capacity: {})",
                    self.name,
                    self.capacity
                );
            }
            // Don't log disconnected - it's expected during WHILE arm switches
        }
        result
    }
}

/// Debug flag to trace actor flow: stream subscriptions, value broadcasts,
/// and loop starts/ends.
const LOG_ACTOR_FLOW: bool = false;

/// Master debug logging flag for the engine.
/// When enabled, prints detailed information about:
/// - LINK_ACTOR: Link actor creation and events
/// - VAR_REF: Variable reference resolution
/// - FORWARDING/FWD2: Stream forwarding operations
/// - DEBUG: General debug information
/// - LAZY_ACTOR: Lazy actor lifecycle
/// - BACKPRESSURE: Backpressure system
/// - VFS: Virtual filesystem operations
pub const LOG_DEBUG: bool = false;

// --- constant ---

// --- Error Types ---

/// Error returned by `current_value()`.
///
/// Distinguishes between "no value stored yet" (actor is alive but waiting)
/// and "actor was dropped" (actor is gone).
#[derive(Debug, Clone)]
pub enum CurrentValueError {
    /// Actor exists but has no stored value yet.
    /// This happens for LINK variables waiting for user interaction (click, input, etc.).
    NoValueYet,
    /// Actor was dropped (navigation, WHILE branch switch, etc.).
    /// The actor is gone and will never produce a value.
    ActorDropped,
}

/// Error returned by `value()`.
///
/// Since `value()` waits for a value, the only failure mode is the actor dying
/// before producing one.
#[derive(Debug, Clone)]
pub enum ValueError {
    /// Actor was dropped before or while waiting for a value.
    /// This can happen when:
    /// - User navigates away without triggering the event (LINK)
    /// - WHILE branch switches, dropping the old branch's actors
    /// - Parent actor is dropped
    ActorDropped,
}

// --- switch_map ---

/// Applies a function to each value from the outer stream, creating an inner stream.
/// When a new outer value arrives, the previous inner stream is cancelled immediately
/// and a new inner stream is started.
///
/// This is the "switch" behavior from RxJS - only the most recent inner stream is active.
/// Previous inner streams are dropped, cancelling their subscriptions.
///
/// # Example
/// For a path like `event.key_down.key`:
/// - When `key_down` emits a new KeyboardEvent, we start subscribing to its `.key` field
/// - When `key_down` emits another KeyboardEvent, we DROP the old `.key` subscription
///   and start a new one on the new event
///
/// This is essential for LINK-based paths where each LINK emission is a discrete event
/// and we shouldn't continue processing fields from old events.
///
/// # Implementation
/// Uses `stream::unfold()` for a pure demand-driven stream (no Task spawn).
/// State is threaded through the unfold closure, making this a pure stream combinator.
pub fn switch_map<S, F, U>(outer: S, f: F) -> LocalBoxStream<'static, U::Item>
where
    S: Stream + 'static,
    F: Fn(S::Item) -> U + 'static,
    U: Stream + 'static,
    U::Item: 'static,
{
    use zoon::futures_util::stream::FusedStream;

    // Use type aliases to avoid complex generic inference issues
    type FusedOuter<T> = stream::Fuse<LocalBoxStream<'static, T>>;
    type FusedInner<T> = stream::Fuse<LocalBoxStream<'static, T>>;

    // State as tuple: (outer_stream, inner_stream_opt, map_fn)
    let initial: (FusedOuter<S::Item>, Option<FusedInner<U::Item>>, F) =
        (outer.boxed_local().fuse(), None, f);

    stream::unfold(initial, |state| async move {
        use zoon::futures_util::future::Either;

        let (mut outer_stream, mut inner_opt, map_fn) = state;

        loop {
            match &mut inner_opt {
                Some(inner) if !inner.is_terminated() => {
                    // Both streams active - race between them
                    let outer_fut = outer_stream.next();
                    let inner_fut = inner.next();

                    match future::select(pin!(outer_fut), pin!(inner_fut)).await {
                        Either::Left((outer_opt, _inner_fut_incomplete)) => {
                            match outer_opt {
                                Some(new_outer_value) => {
                                    // True switch semantics: cancel the old inner stream immediately.
                                    drop(inner_opt.take());
                                    inner_opt = Some(map_fn(new_outer_value).boxed_local().fuse());
                                }
                                None => {
                                    // Outer ended - drain inner then finish
                                    while let Some(item) = inner.next().await {
                                        return Some((item, (outer_stream, inner_opt, map_fn)));
                                    }
                                    return None;
                                }
                            }
                        }
                        Either::Right((inner_opt_val, _)) => {
                            match inner_opt_val {
                                Some(item) => {
                                    return Some((item, (outer_stream, inner_opt, map_fn)));
                                }
                                None => {
                                    // Inner ended - clear it
                                    inner_opt = None;
                                }
                            }
                        }
                    }
                }
                _ => {
                    // No active inner stream - wait for outer value
                    match outer_stream.next().await {
                        Some(value) => {
                            inner_opt = Some(map_fn(value).boxed_local().fuse());
                        }
                        None => {
                            return None; // Outer ended
                        }
                    }
                }
            }
        }
    })
    .boxed_local()
}

// --- Coalesce ---

/// Collects all immediately-available items from a stream into batches.
///
/// When the source emits an item, this combinator polls repeatedly until
/// the source would block (Poll::Pending), collecting all ready items.
/// It then yields a Vec containing the batch.
///
/// # Use Cases
/// - Batching rapid predicate updates in List/retain, List/every, List/any
/// - Coalescing multiple simultaneous events into a single processing pass
///
/// # Zero Latency
/// If only one item is ready, yields immediately with a single-element Vec.
/// No timer or delay is involved - batching is purely opportunistic.
///
/// # Hardware Mapping
/// This is equivalent to "drain FIFO until empty" - a standard hardware pattern.
pub fn coalesce<S>(source: S) -> impl Stream<Item = Vec<S::Item>>
where
    S: Stream + 'static,
    S::Item: 'static,
{
    use std::task::Poll;
    use zoon::futures_util::stream::FusedStream;

    let fused_source = source.boxed_local().fuse();

    stream::unfold(fused_source, |mut source| async move {
        // 1. Wait for at least one item (this is the blocking point)
        let first = source.next().await?;
        let mut batch = vec![first];

        // 2. Non-blocking: drain all IMMEDIATELY ready items
        loop {
            if source.is_terminated() {
                break;
            }

            let maybe_item = std::future::poll_fn(|cx| {
                match Pin::new(&mut source).poll_next(cx) {
                    Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
                    Poll::Ready(None) => Poll::Ready(None), // Stream ended
                    Poll::Pending => Poll::Ready(None),     // Would block - stop draining
                }
            })
            .await;

            match maybe_item {
                Some(item) => batch.push(item),
                None => break,
            }
        }

        // 3. Emit the batch
        Some((batch, source))
    })
}

// --- BackpressuredStream ---

/// Stream combinator with demand-based backpressure.
/// Consumer controls pace by signaling readiness via the demand channel.
/// Inner stream is only polled when demand signal is received.
///
/// Uses a bounded(1) channel - single pending demand signal is sufficient.
///
/// # Usage
/// ```ignore
/// let backpressured = BackpressuredStream::new(source_stream);
/// let demand_tx = backpressured.demand_sender();
/// backpressured.signal_initial_demand(); // Allow first value
///
/// // In processing loop, after handling each value:
/// let _ = demand_tx.try_send(()); // Signal ready for next
/// ```
#[pin_project::pin_project]
pub struct BackpressuredStream<S> {
    #[pin]
    inner: S,
    #[pin]
    demand_rx: mpsc::Receiver<()>,
    demand_tx: mpsc::Sender<()>,
    awaiting_demand: bool,
}

impl<S> BackpressuredStream<S> {
    pub fn new(inner: S) -> Self {
        let (demand_tx, demand_rx) = mpsc::channel(1);
        Self {
            inner,
            demand_rx,
            demand_tx,
            awaiting_demand: true,
        }
    }

    /// Get a cloneable sender to signal demand.
    /// Call `demand_sender.try_send(())` to allow next value through.
    pub fn demand_sender(&self) -> mpsc::Sender<()> {
        self.demand_tx.clone()
    }

    /// Signal initial demand (call once after creation to start flow).
    pub fn signal_initial_demand(&self) {
        // Bounded(1) channel - if full, demand already signaled
        if let Err(e) = self.demand_tx.clone().try_send(()) {
            zoon::println!("[BACKPRESSURED_STREAM] Initial demand signal failed: {e}");
        }
    }
}

impl<S: Stream> Stream for BackpressuredStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // Wait for demand signal before polling inner
        if *this.awaiting_demand {
            match this.demand_rx.poll_next(cx) {
                Poll::Ready(Some(())) => *this.awaiting_demand = false,
                Poll::Ready(None) => return Poll::Ready(None), // Demand channel closed
                Poll::Pending => return Poll::Pending,
            }
        }

        // Poll inner stream
        match this.inner.poll_next(cx) {
            Poll::Ready(Some(value)) => {
                *this.awaiting_demand = true; // Need demand for next value
                Poll::Ready(Some(value))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// --- BackpressureCoordinator ---

/// Inner state of BackpressureCoordinator, held in Arc for cloning.
struct BackpressureCoordinatorInner {
    /// True when the next acquire can proceed immediately.
    available: Cell<bool>,
    /// FIFO queue of acquires waiting for the permit to be released.
    waiters: RefCell<VecDeque<oneshot::Sender<()>>>,
}

/// Queue-based backpressure coordination for HOLD/THEN synchronization.
///
/// # How it works:
/// 1. HOLD creates coordinator
/// 2. THEN calls `acquire().await` which either takes the permit immediately or waits in a queue
/// 3. THEN evaluates body and emits result
/// 4. HOLD callback calls `release()`
/// 5. Release wakes the next queued waiter or makes the permit immediately available
///
/// This keeps HOLD/THEN sequential without an internal sidecar task.
#[derive(Clone)]
pub struct BackpressureCoordinator {
    inner: Arc<BackpressureCoordinatorInner>,
}

impl BackpressureCoordinator {
    /// Create a new coordinator.
    ///
    /// The coordinator starts ready to grant one permit (equivalent to initial=1).
    /// After each acquire/release cycle, it's ready for the next.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(BackpressureCoordinatorInner {
                available: Cell::new(true),
                waiters: RefCell::new(VecDeque::new()),
            }),
        }
    }

    fn acquire_or_queue(&self, waiter: oneshot::Sender<()>) -> bool {
        if self.inner.available.replace(false) {
            true
        } else {
            self.inner.waiters.borrow_mut().push_back(waiter);
            false
        }
    }

    /// Acquire a permit asynchronously.
    /// Called by THEN before each body evaluation.
    pub async fn acquire(&self) {
        let (grant_tx, grant_rx) = oneshot::channel();
        if self.acquire_or_queue(grant_tx) {
            return;
        }

        if grant_rx.await.is_err() {
            zoon::println!("[BACKPRESSURE] Grant channel closed during acquire");
        }
    }

    /// Release the permit, allowing the next acquire to proceed.
    ///
    /// This is synchronous (non-blocking) so it can be called from callbacks.
    /// Called by HOLD after updating state.
    pub fn release(&self) {
        loop {
            let Some(waiter) = self.inner.waiters.borrow_mut().pop_front() else {
                if self.inner.available.replace(true) {
                    zoon::eprintln!(
                        "[BACKPRESSURE BUG] release() called while permit was already available"
                    );
                }
                return;
            };

            if waiter.send(()).is_ok() {
                return;
            }
        }
    }
}

// Type alias kept while call sites converge on the coordinator name.
pub type BackpressurePermit = BackpressureCoordinator;

// --- LazyValueActor ---

/// Request from subscriber to LazyValueActor for the next value.
struct LazyValueRequest {
    subscriber_id: usize,
    start_cursor: usize,
    response_tx: oneshot::Sender<Option<Value>>,
}

/// A demand-driven actor that only polls its source stream when subscribers request values.
///
/// Unlike the regular ValueActor which eagerly polls its source stream and broadcasts values,
/// LazyValueActor only pulls values from the source when a subscriber asks for one.
///
/// This is essential for sequential state updates in HOLD bodies:
/// ```boon
/// [count: 0] |> HOLD state {
///     3 |> Stream/pulses() |> THEN { [count: state.count + 1] }
/// }
/// ```
/// With lazy actors, HOLD pulls one value at a time, updates state, then pulls the next.
/// Each THEN evaluation sees the updated state from the previous iteration.
///
/// # Architecture
/// - Internal loop handles requests via channel (no Mutex/RwLock)
/// - Buffer + cursor enables multiple subscribers to replay values
/// - Threshold-based cleanup prevents unbounded memory growth
pub struct LazyValueActor {
    /// Channel for subscribers to request the next value.
    /// Bounded(16) - demand-driven requests from subscribers.
    request_tx: NamedChannel<LazyValueRequest>,
    /// Counter for unique subscriber IDs
    subscriber_counter: std::sync::atomic::AtomicUsize,
}

impl LazyValueActor {
    fn new_unstarted() -> (Self, mpsc::Receiver<LazyValueRequest>) {
        let (request_tx, request_rx) =
            NamedChannel::new("lazy_value_actor.requests", LAZY_ACTOR_REQUEST_CAPACITY);

        (
            Self {
                request_tx,
                subscriber_counter: std::sync::atomic::AtomicUsize::new(0),
            },
            request_rx,
        )
    }

    /// The internal loop that handles demand-driven value delivery.
    ///
    /// This loop owns all state (buffer, cursors) - no locks needed.
    /// Values are only pulled from the source when a subscriber requests one.
    async fn internal_loop<S: Stream<Item = Value> + 'static>(
        source_stream: S,
        mut request_rx: mpsc::Receiver<LazyValueRequest>,
        stored_state: ActorStoredState,
    ) {
        let mut source = Box::pin(source_stream);
        let mut buffer: Vec<Value> = Vec::new();
        let mut buffer_offset = 0usize;
        let mut cursors: HashMap<usize, usize> = HashMap::new();
        let mut source_exhausted = false;
        const CLEANUP_THRESHOLD: usize = 100;

        while let Some(request) = request_rx.next().await {
            let cursor = cursors
                .entry(request.subscriber_id)
                .or_insert_with(|| request.start_cursor.max(buffer_offset));

            if *cursor < buffer_offset {
                *cursor = buffer_offset;
            }

            let value = if *cursor < buffer_offset + buffer.len() {
                // Return buffered value (replay for this subscriber)
                Some(buffer[*cursor - buffer_offset].clone())
            } else if source_exhausted {
                // Source is exhausted, no more values
                None
            } else {
                // Poll source for next value (demand-driven pull!)
                match source.next().await {
                    Some(value) => {
                        stored_state.store(value.clone());
                        buffer.push(value.clone());
                        Some(value)
                    }
                    None => {
                        source_exhausted = true;
                        None
                    }
                }
            };

            if value.is_some() {
                *cursor += 1;
            }

            // Buffer cleanup when all subscribers have advanced past threshold
            if !cursors.is_empty() {
                let min_cursor = *cursors.values().min().unwrap_or(&buffer_offset);
                let drain_count = min_cursor.saturating_sub(buffer_offset);
                if drain_count >= CLEANUP_THRESHOLD {
                    // Remove consumed values from buffer
                    buffer.drain(0..drain_count);
                    buffer_offset = min_cursor;
                }
            }

            // Send response to subscriber (subscriber may have dropped)
            if request.response_tx.send(value).is_err() {
                zoon::println!("[LAZY_ACTOR] Subscriber dropped before receiving value");
            }
        }
    }

    /// Subscribe to this lazy actor's values.
    ///
    /// Returns a stream that pulls values on demand.
    /// Each call to .next() on the stream will request the next value from the actor.
    ///
    /// Takes ownership of the Arc to keep the actor alive for the subscription lifetime.
    /// Callers should use `.clone().stream()` if they need to retain a reference.
    pub fn stream(self: Arc<Self>) -> LazySubscription {
        self.stream_from_cursor(0)
    }

    fn stream_from_cursor(self: Arc<Self>, start_cursor: usize) -> LazySubscription {
        let subscriber_id = self.subscriber_counter.fetch_add(1, Ordering::SeqCst);
        LazySubscription {
            actor: self,
            subscriber_id,
            start_cursor,
            pending_send: None,
            pending_response: None,
        }
    }
}

/// Subscription to a LazyValueActor.
///
/// Each .next() call sends a request to the actor and awaits the response.
/// This implements pull-based (demand-driven) semantics.
pub struct LazySubscription {
    actor: Arc<LazyValueActor>,
    subscriber_id: usize,
    start_cursor: usize,
    /// In-flight async send when the bounded request channel applies backpressure.
    pending_send: Option<(
        LocalBoxFuture<'static, Result<(), ChannelError>>,
        oneshot::Receiver<Option<Value>>,
    )>,
    /// Stores the pending response receiver when a request is in-flight.
    /// This prevents creating duplicate requests on repeated polls.
    pending_response: Option<oneshot::Receiver<Option<Value>>>,
}

fn lazy_request_send_future(
    request_tx: NamedChannel<LazyValueRequest>,
    request: LazyValueRequest,
) -> LocalBoxFuture<'static, Result<(), ChannelError>> {
    async move {
        request_tx
            .inner
            .clone()
            .send(request)
            .await
            .map_err(|_| ChannelError::Closed)
    }
    .boxed_local()
}

impl Stream for LazySubscription {
    type Item = Value;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            if let Some(ref mut response_rx) = this.pending_response {
                match Pin::new(response_rx).poll(cx) {
                    Poll::Ready(Ok(value)) => {
                        this.pending_response = None;
                        return Poll::Ready(value);
                    }
                    Poll::Ready(Err(_)) => {
                        this.pending_response = None;
                        return Poll::Ready(None);
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            if let Some((ref mut send_fut, _)) = this.pending_send {
                match Pin::new(send_fut).poll(cx) {
                    Poll::Ready(Ok(())) => {
                        let (_, response_rx) = this
                            .pending_send
                            .take()
                            .expect("pending send should exist after successful poll");
                        this.pending_response = Some(response_rx);
                        continue;
                    }
                    Poll::Ready(Err(_)) => {
                        this.pending_send = None;
                        return Poll::Ready(None);
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            let (response_tx, response_rx) = oneshot::channel();
            let request = LazyValueRequest {
                subscriber_id: this.subscriber_id,
                start_cursor: this.start_cursor,
                response_tx,
            };
            this.pending_send = Some((
                lazy_request_send_future(this.actor.request_tx.clone(), request),
                response_rx,
            ));
        }
    }
}

pub type EmissionSeq = u64;

// --- VirtualFilesystem ---

/// Request types for VirtualFilesystem actor
/// Actor-based virtual filesystem for module loading.
/// Keeps file contents in direct shared state while preserving the async API shape.
#[derive(Clone)]
pub struct VirtualFilesystem {
    files: Arc<RefCell<HashMap<String, String>>>,
}

impl VirtualFilesystem {
    pub fn new() -> Self {
        Self::with_files(HashMap::new())
    }

    /// Create a VirtualFilesystem pre-populated with files
    pub fn with_files(initial_files: HashMap<String, String>) -> Self {
        Self {
            files: Arc::new(RefCell::new(initial_files)),
        }
    }

    /// Read text content from a file (async)
    pub async fn read_text(&self, path: &str) -> Option<String> {
        let normalized = Self::normalize_path(path);
        self.files.borrow().get(&normalized).cloned()
    }

    /// Write text content to a file (fire-and-forget)
    pub fn write_text(&self, path: &str, content: String) {
        let normalized = Self::normalize_path(path);
        self.files.borrow_mut().insert(normalized, content);
    }

    /// List entries in a directory (async)
    pub async fn list_directory(&self, path: &str) -> Vec<String> {
        let normalized = Self::normalize_path(path);
        let prefix = if normalized.is_empty() || normalized == "/" {
            String::new()
        } else if normalized.ends_with('/') {
            normalized.clone()
        } else {
            format!("{}/", normalized)
        };

        let mut entries: Vec<String> = self
            .files
            .borrow()
            .keys()
            .filter_map(|file_path| {
                if prefix.is_empty() {
                    file_path.split('/').next().map(|s| s.to_string())
                } else if file_path.starts_with(&prefix) {
                    let remainder = &file_path[prefix.len()..];
                    remainder.split('/').next().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();
        entries.sort();
        entries.dedup();
        entries
    }

    /// Check if a file exists (async)
    pub async fn exists(&self, path: &str) -> bool {
        let normalized = Self::normalize_path(path);
        self.files.borrow().contains_key(&normalized)
    }

    /// Delete a file (async)
    pub async fn delete(&self, path: &str) -> bool {
        let normalized = Self::normalize_path(path);
        self.files.borrow_mut().remove(&normalized).is_some()
    }

    /// Normalize path by removing leading/trailing slashes and "./" prefixes
    fn normalize_path(path: &str) -> String {
        let path = path.trim();
        let path = path.strip_prefix("./").unwrap_or(path);
        let path = path.strip_prefix('/').unwrap_or(path);
        let path = path.strip_suffix('/').unwrap_or(path);
        path.to_string()
    }
}

// --- ConstructContext ---

#[derive(Clone)]
pub struct ConstructContext {
    pub construct_storage: Arc<ConstructStorage>,
    pub virtual_fs: VirtualFilesystem,
    /// Registry scope ID for bridge-created actors (event value actors).
    /// Set by the evaluator from the root scope.
    pub bridge_scope_id: Option<ScopeId>,
    /// Type-erased scene context for physical rendering.
    /// Set by the bridge when the root object uses `scene:` instead of `document:`.
    /// Bridge code downcasts this to `SceneContext` via `Rc::downcast_ref()`.
    pub scene_ctx: Option<Rc<dyn std::any::Any>>,
    // NOTE: `previous_actors` was intentionally REMOVED from here.
    // It should NOT be part of ConstructContext because:
    // 1. It gets captured in stream closures, causing memory leaks
    // 2. It's not scalable (can't send huge actor maps to web workers/nodes)
    // 3. Actors should be addressable by ID via a registry, not passed around
    //
    // For actor reuse during hot-reload, use the evaluator's separate
    // `previous_actors` parameter instead.
}

// --- ConstructStorage ---

/// Actor for persistent storage operations.
/// Uses a sidecar task internally to encapsulate the async work.
pub struct ConstructStorage {
    persistence_disabled: bool,
    states_local_storage_key: Option<Cow<'static, str>>,
    states: RefCell<BTreeMap<String, serde_json::Value>>,
}

// @TODO Replace LocalStorage with IndexedDB
// - https://crates.io/crates/indexed_db
// - https://developer.mozilla.org/en-US/docs/Web/API/IndexedDB_API
// - https://blog.openreplay.com/the-ultimate-guide-to-browser-side-storage/
impl ConstructStorage {
    pub fn new(states_local_storage_key: impl Into<Cow<'static, str>>) -> Self {
        let states_local_storage_key = states_local_storage_key.into();
        let persistence_disabled = states_local_storage_key.is_empty();
        if persistence_disabled {
            return Self {
                persistence_disabled,
                states_local_storage_key: None,
                states: RefCell::new(BTreeMap::new()),
            };
        }

        let states = match local_storage()
            .get::<BTreeMap<String, serde_json::Value>>(&states_local_storage_key)
        {
            None => BTreeMap::new(),
            Some(Ok(states)) => states,
            Some(Err(error)) => panic!("Failed to deserialize states: {error:#}"),
        };

        Self {
            persistence_disabled,
            states_local_storage_key: Some(states_local_storage_key),
            states: RefCell::new(states),
        }
    }

    #[cfg(test)]
    pub(crate) fn in_memory_for_tests(states: BTreeMap<String, serde_json::Value>) -> Self {
        Self {
            persistence_disabled: false,
            states_local_storage_key: None,
            states: RefCell::new(states),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.persistence_disabled
    }

    pub(crate) fn load_state_now<T: DeserializeOwned>(
        &self,
        persistence_id: parser::PersistenceId,
    ) -> Option<T> {
        if self.persistence_disabled {
            return None;
        }

        let key = persistence_id.to_string();
        let json_value = self.states.borrow().get(&key).cloned()?;
        match serde_json::from_value(json_value) {
            Ok(state) => Some(state),
            Err(error) => {
                panic!("Failed to load state: {error:#}");
            }
        }
    }

    /// Save state to persistent storage (fire-and-forget).
    ///
    /// This is synchronous - the actor persists asynchronously.
    /// Uses send_or_drop() which logs in debug mode if channel is full.
    pub fn save_state<T: Serialize>(&self, persistence_id: parser::PersistenceId, state: &T) {
        if self.persistence_disabled {
            return;
        }

        let json_value = match serde_json::to_value(state) {
            Ok(json_value) => json_value,
            Err(error) => {
                zoon::eprintln!("Failed to serialize state: {error:#}");
                return;
            }
        };
        let key = persistence_id.to_string();
        let mut states = self.states.borrow_mut();
        states.insert(key, json_value);
        if let Some(states_local_storage_key) = &self.states_local_storage_key {
            inc_metric!(PERSISTENCE_WRITES);
            if let Err(error) = local_storage().insert(states_local_storage_key, &*states) {
                zoon::eprintln!("Failed to save states: {error:#}");
            }
        }
    }

    // @TODO is &self enough?
    pub async fn load_state<T: DeserializeOwned>(
        self: Arc<Self>,
        persistence_id: parser::PersistenceId,
    ) -> Option<T> {
        self.load_state_now(persistence_id)
    }
}

// --- CapturedValue ---

/// Value captured for persistence (hybrid model).
/// Primitives are stored directly, persisting actors are stored as references.
/// Used for recording function call inputs for restoration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CapturedValue {
    // Primitives - stored directly
    Nothing,
    Number(f64),
    Text(String),
    Tag(String),
    Bool(bool),

    // Composites - recursive
    List(Vec<CapturedValue>),
    Object(BTreeMap<String, CapturedValue>),

    // Actor reference - for persisting actors (HOLD, persisted LIST)
    // Resolved against graph on restore
    ActorRef { persistence_id: u128, scope: String },
}

impl CapturedValue {
    /// Capture a Value for persistence.
    /// Primitives are captured directly. For TodoMVC, this means capturing
    /// "Buy milk" (the title text), not the complex todo object.
    ///
    /// Note: Object/TaggedObject/List contain Variables (actors) and would need
    /// async snapshot - for now we capture them as Nothing with a warning.
    pub fn capture(value: &Value) -> Self {
        match value {
            Value::Text(text_arc, _) => CapturedValue::Text(text_arc.text().to_string()),
            Value::Number(number_arc, _) => CapturedValue::Number(number_arc.number()),
            Value::Tag(tag_arc, _) => CapturedValue::Tag(tag_arc.tag().to_string()),
            Value::Object(_, _) => {
                // Objects contain Variables (actors) - need async to snapshot
                // For now, capture as Nothing. TODO: Implement async capture
                CapturedValue::Nothing
            }
            Value::TaggedObject(_, _) => {
                // TaggedObjects contain Variables (actors) - need async to snapshot
                CapturedValue::Nothing
            }
            Value::List(_, _) => {
                // Lists contain items that may have actors - need async to snapshot
                CapturedValue::Nothing
            }
            Value::Flushed(inner, _) => CapturedValue::capture(inner),
        }
    }

    /// Convert captured value back to a Value for restoration.
    /// This requires ConstructInfo and ConstructContext - must be called from evaluator context.
    /// For now, returns None. The actual restoration will recreate values via evaluation.
    pub fn restore_with_context(
        &self,
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
    ) -> Option<Value> {
        match self {
            CapturedValue::Nothing => None,
            CapturedValue::Text(s) => Some(Text::new_value(
                construct_info,
                construct_context,
                ValueIdempotencyKey::new(),
                s.clone(),
            )),
            CapturedValue::Number(n) => Some(Number::new_value(
                construct_info,
                construct_context,
                ValueIdempotencyKey::new(),
                *n,
            )),
            CapturedValue::Tag(t) => Some(Tag::new_value(
                construct_info,
                construct_context,
                ValueIdempotencyKey::new(),
                t.clone(),
            )),
            CapturedValue::Bool(b) => Some(Tag::new_value(
                construct_info,
                construct_context,
                ValueIdempotencyKey::new(),
                if *b { "True" } else { "False" },
            )),
            CapturedValue::Object(_) | CapturedValue::List(_) | CapturedValue::ActorRef { .. } => {
                // Complex types need full evaluation context - not supported in simple restore
                None
            }
        }
    }
}

/// A recorded function call for scope-based persistence.
/// When restoring, these calls are replayed with stored inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedCall {
    /// Stable identifier for this call (e.g., "call_0", "call_1").
    /// Used for removal tracking - survives restores.
    pub id: String,
    /// Function path (e.g., ["new_todo"])
    pub path: Vec<String>,
    /// Captured inputs at call time
    pub inputs: CapturedValue,
}

// --- SubscriptionScope ---

/// Scope for tracking subscription lifecycle within WHILE arms.
/// When a WHILE arm switches, the old arm's scope is cancelled,
/// which terminates all streams created within that scope.
#[derive(Clone)]
pub struct SubscriptionScope {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl SubscriptionScope {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Cancel this scope, causing all streams checking it to terminate.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Check if this scope has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for SubscriptionScope {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that cancels a SubscriptionScope when dropped.
/// Used to tie scope lifecycle to stream lifecycle - when the stream is dropped
/// (e.g., by switch_map switching to a new inner stream), the scope is cancelled.
pub struct ScopeGuard {
    scope: Arc<SubscriptionScope>,
}

impl ScopeGuard {
    pub fn new(scope: Arc<SubscriptionScope>) -> Self {
        Self { scope }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

/// Guard that destroys a registry scope when dropped.
/// Used to tie a registry scope's lifetime to a stream or async block:
/// when the stream is dropped (e.g., by switch_map switching arms),
/// the scope and all its actors/child scopes are destroyed.
pub struct ScopeDestroyGuard {
    scope_id: Option<ScopeId>,
}

impl ScopeDestroyGuard {
    pub fn new(scope_id: ScopeId) -> Self {
        Self {
            scope_id: Some(scope_id),
        }
    }

    /// Prevent destruction on drop (e.g., when transferring ownership).
    pub fn defuse(&mut self) {
        self.scope_id = None;
    }
}

impl Drop for ScopeDestroyGuard {
    fn drop(&mut self) {
        if let Some(scope_id) = self.scope_id {
            if let Some((before, after)) = destroy_scope_tree_now(scope_id) {
                if LOG_DEBUG {
                    zoon::println!(
                        "[actors] ScopeDestroyGuard: destroyed scope {:?} immediately, actors {} → {} (freed {})",
                        scope_id,
                        before,
                        after,
                        before - after
                    );
                }
            } else {
                PENDING_SCOPE_DESTROYS.with(|pending| pending.borrow_mut().push(scope_id));
                if LOG_DEBUG {
                    zoon::println!(
                        "[actors] ScopeDestroyGuard: queued scope {:?} for deferred destroy",
                        scope_id
                    );
                }
            }
        }
    }
}

fn destroy_scope_tree_now(scope_id: ScopeId) -> Option<(u64, u64)> {
    let before = live_actor_count();
    let dropped_actors = REGISTRY.with(|reg| {
        let mut reg = reg.try_borrow_mut().ok()?;
        Some(reg.take_scope_tree(scope_id))
    })?;
    drop(dropped_actors);
    let after = live_actor_count();
    Some((before, after))
}

fn drain_pending_scope_destroys() {
    loop {
        let Some(scope_id) = PENDING_SCOPE_DESTROYS.with(|pending| pending.borrow_mut().pop())
        else {
            break;
        };

        let Some((before, after)) = destroy_scope_tree_now(scope_id) else {
            PENDING_SCOPE_DESTROYS.with(|pending| pending.borrow_mut().push(scope_id));
            break;
        };

        if LOG_DEBUG {
            zoon::println!(
                "[actors] ScopeDestroyGuard: destroyed deferred scope {:?}, actors {} → {} (freed {})",
                scope_id,
                before,
                after,
                before - after
            );
        }
    }
}

pub fn create_registry_scope(parent: Option<ScopeId>) -> ScopeId {
    let scope_id = REGISTRY.with(|reg| reg.borrow_mut().create_scope(parent));
    drain_pending_scope_destroys();
    scope_id
}

fn insert_actor_and_drain(scope_id: ScopeId, owned_actor: OwnedActor) -> ActorId {
    let actor_id = REGISTRY.with(|reg| reg.borrow_mut().insert_actor(scope_id, owned_actor));
    drain_pending_scope_destroys();
    actor_id
}

// --- ActorContext ---

#[derive(Default, Clone)]
pub struct ActorContext {
    pub output_valve_signal: Option<Arc<ActorOutputValveSignal>>,
    /// The piped value from `|>` operator.
    /// Set when evaluating `x |> expr` - the `x` becomes `piped` for `expr`.
    /// Used by function calls to prepend as first argument.
    /// Also used by THEN/WHEN/WHILE/LinkSetter to process the piped stream.
    pub piped: Option<ActorHandle>,
    /// The PASSED context - implicit context passed through function calls.
    /// Set when calling a function with `PASS: something` argument.
    /// Accessible inside the function via `PASSED` or `PASSED.field`.
    /// Propagates automatically through nested function calls.
    pub passed: Option<ActorHandle>,
    /// Function parameter bindings - maps parameter names to their values.
    /// Set when calling a user-defined function.
    /// e.g., `fn(param: x)` binds "param" -> x's ValueActor
    pub parameters: Arc<HashMap<String, ActorHandle>>,
    /// When true, THEN/WHEN process events sequentially (one body completes before next starts).
    /// Set by HOLD to ensure state consistency in accumulator patterns.
    /// This prevents race conditions where multiple parallel body evaluations read stale state.
    pub sequential_processing: bool,
    /// Backpressure permit for HOLD/THEN synchronization.
    /// When set, THEN must acquire permit before each body evaluation,
    /// and HOLD releases permit after updating state.
    /// This ensures state is updated before next body starts.
    pub backpressure_permit: Option<BackpressurePermit>,
    /// Callback for THEN to update HOLD's state synchronously after body evaluation.
    /// This enables synchronous processing of all pulses during eager polling.
    /// The callback receives the body result and:
    /// 1. Updates state_actor.store_value_directly()
    /// 2. Releases the backpressure permit
    /// Without this, THEN would block on permit.acquire() for the second pulse
    /// because permit release normally happens in HOLD's Task which hasn't run yet.
    /// Uses Arc for thread-safety (WebWorkers, future HVM targets).
    pub hold_state_update_callback: Option<Arc<dyn Fn(Value)>>,
    /// When true, expression evaluation creates LazyValueActors instead of eager ValueActors.
    /// Lazy actors only poll their source stream when a subscriber requests values (demand-driven).
    /// This is set by HOLD for body evaluation to ensure sequential state updates:
    /// each pulse from Stream/pulses is pulled one at a time, state is updated between each.
    /// Default: false (normal eager evaluation).
    pub use_lazy_actors: bool,
    /// When true, code should use `.value()` instead of `.stream()` for subscriptions.
    ///
    /// This flag propagates through function calls:
    /// - THEN/WHEN bodies set this to `true` (value/snapshot context)
    /// - WHILE bodies keep this `false` (streaming context)
    /// - User-defined function bodies inherit caller's context
    ///
    /// Code that needs values (variable references, API functions, operators) checks
    /// this flag and calls `.value()` or `.stream()` accordingly.
    ///
    /// Default: false (streaming context - continuous updates).
    pub is_snapshot_context: bool,
    /// Object-local variables - maps span to actor for sibling field access.
    /// When evaluating expressions inside an Object, sibling variables should
    /// check this map first before falling back to ReferenceConnector.
    /// This prevents span collisions when multiple Objects are created from
    /// the same function definition (they would otherwise share the same spans
    /// and overwrite each other in the global ReferenceConnector).
    pub object_locals: Arc<HashMap<parser::Span, ActorHandle>>,
    /// Scope context for Variables - either Root (top-level) or Nested (inside List/map).
    ///
    /// **IMPORTANT: When to create a new scope:**
    /// A new scope must be created when evaluating code from the SAME source position
    /// but for DIFFERENT runtime instances. This includes:
    /// - LIST items (each item needs unique LINK registrations)
    /// - List/map items (each transformed item needs unique LINK registrations)
    /// - Function calls that create multiple instances with LINK fields
    ///
    /// Use `with_child_scope()` to create a properly isolated child scope.
    /// Without proper scoping, LINK variables would collide and events would be
    /// routed to wrong elements.
    pub scope: parser::Scope,
    /// Subscription scope for WHILE arm lifecycle management.
    /// When a WHILE arm switches, its scope is cancelled, terminating all streams
    /// created within that arm. This prevents inactive arms from processing updates
    /// and interfering with active arms (e.g., overwriting LINK Variables).
    pub subscription_scope: Option<Arc<SubscriptionScope>>,
    /// Sender for recording function calls that produce stateful values.
    /// When set, evaluate_function_call sends RecordedCall here for calls
    /// whose result contains actors (stateful).
    /// The receiver is held by the parent container (List/Map) for persistence.
    pub call_recorder: Option<NamedChannel<RecordedCall>>,
    /// True when restoring from persisted state.
    /// When set, function calls check storage for results before executing.
    /// This prevents re-execution of side effects and ensures stable impure values.
    pub is_restoring: bool,
    /// Storage key for list append recording.
    /// When set, function calls generate ListItemOrigin for each recorded call.
    /// Used to attach origin info to list items for removal persistence.
    pub list_append_storage_key: Option<String>,
    /// Counter for generating stable call_ids during recording.
    /// Incremented for each recorded call to ensure unique, sequential IDs.
    /// Used together with list_append_storage_key for ListItemOrigin.
    pub recording_counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Emission-sequence floor captured when this context subscribed.
    ///
    /// Used by THEN/WHEN-style flows to ignore values that were already present
    /// before the subscriber existed.
    ///
    /// - `None` = no filtering (streaming context, accept all values)
    /// - `Some(seq)` = ignore values emitted at or before that sequence
    pub subscription_after_seq: Option<EmissionSeq>,
    /// Source emission sequence for snapshot evaluation contexts.
    ///
    /// When THEN/WHEN evaluate a body for a triggering emission, sibling consumers of the
    /// same source can update shared state in parallel. Snapshot readers use this sequence
    /// to request the latest value that existed before the current emission, rather than
    /// whatever sibling happened to store first.
    pub snapshot_emission_seq: Option<EmissionSeq>,
    /// Registry scope ID for deterministic actor ownership.
    ///
    /// When set, actors created in this context are registered under this scope.
    /// When the scope is destroyed, all actors within it are dropped.
    /// Used by WHILE arms, List items, and program teardown.
    ///
    /// `None` means actors are managed by Arc reference counting (legacy behavior).
    pub registry_scope_id: Option<ScopeId>,
}

impl ActorContext {
    /// Get the registry scope ID, panicking if not set.
    /// All actors should be created within a scope after evaluation starts.
    pub fn scope_id(&self) -> ScopeId {
        self.registry_scope_id
            .expect("Bug: no registry scope - all actors should be created within a scope")
    }

    /// Creates a child context with a new unique nested scope.
    ///
    /// Use this when evaluating code that needs isolation from siblings:
    /// - LIST items
    /// - List/map transformed items
    /// - Any context where the same source code position is evaluated multiple times
    ///   for different runtime instances
    ///
    /// The `scope_id` should be unique among siblings (e.g., "list_item_0", "map_item_abc123").
    pub fn with_child_scope(&self, scope_id: &str) -> Self {
        let new_prefix = match &self.scope {
            parser::Scope::Root => scope_id.to_string(),
            parser::Scope::Nested(existing) => format!("{}:{}", existing, scope_id),
        };
        Self {
            scope: parser::Scope::Nested(new_prefix),
            // Don't propagate call_recorder to child - each persisting scope has its own
            call_recorder: None,
            // Clear list append tracking - not applicable in child scope
            list_append_storage_key: None,
            recording_counter: None,
            ..self.clone()
        }
    }

    /// Creates a child context with a persisting scope that records function calls.
    ///
    /// Use this when evaluating code that needs persistence:
    /// - List/append items
    /// - Map/insert entries
    /// - Any container item that needs to be restored on reload
    ///
    /// The `storage_key` is used for list append recording to attach origin info to items.
    ///
    /// Returns (child_context, receiver) where receiver collects RecordedCalls.
    pub fn with_persisting_child_scope(
        &self,
        scope_id: &str,
        storage_key: String,
    ) -> (Self, mpsc::Receiver<RecordedCall>) {
        let new_prefix = match &self.scope {
            parser::Scope::Root => scope_id.to_string(),
            parser::Scope::Nested(existing) => format!("{}:{}", existing, scope_id),
        };
        // Use static name for channel - the actual scope identity is in self.scope
        let (call_recorder, receiver) = NamedChannel::new("call_recorder", CALL_RECORDER_CAPACITY);
        (
            Self {
                scope: parser::Scope::Nested(new_prefix),
                call_recorder: Some(call_recorder),
                is_restoring: false,
                list_append_storage_key: Some(storage_key),
                recording_counter: Some(Arc::new(std::sync::atomic::AtomicUsize::new(0))),
                ..self.clone()
            },
            receiver,
        )
    }

    /// Creates a child context for restoration (replaying recorded calls).
    ///
    /// Use this when restoring items from persisted state.
    /// Function calls will check storage before executing.
    pub fn with_restoring_child_scope(&self, scope_id: &str) -> Self {
        let new_prefix = match &self.scope {
            parser::Scope::Root => scope_id.to_string(),
            parser::Scope::Nested(existing) => format!("{}:{}", existing, scope_id),
        };
        Self {
            scope: parser::Scope::Nested(new_prefix),
            call_recorder: None,
            is_restoring: true,
            // Clear list append tracking - restoration doesn't record new calls
            list_append_storage_key: None,
            recording_counter: None,
            ..self.clone()
        }
    }
}

// --- ActorOutputValveSignal ---

/// Actor for broadcasting impulses to multiple subscribers.
/// Impulse channels are bounded(1) - a single pending signal is sufficient.
pub struct ActorOutputValveSignal {
    active: Rc<Cell<bool>>,
    impulse_senders: Rc<RefCell<SmallVec<[mpsc::Sender<()>; 4]>>>,
    _task: TaskHandle,
}

fn broadcast_output_valve_impulse(impulse_senders: &RefCell<SmallVec<[mpsc::Sender<()>; 4]>>) {
    impulse_senders.borrow_mut().retain_mut(|impulse_sender| {
        // try_send for bounded(1) - drop if already signaled
        impulse_sender.try_send(()).is_ok()
    });
}

fn close_output_valve_subscribers(
    active: &Cell<bool>,
    impulse_senders: &RefCell<SmallVec<[mpsc::Sender<()>; 4]>>,
) {
    active.set(false);
    impulse_senders.borrow_mut().clear();
}

fn subscribe_output_valve_impulses(
    active: &Cell<bool>,
    impulse_senders: &RefCell<SmallVec<[mpsc::Sender<()>; 4]>>,
) -> mpsc::Receiver<()> {
    let (impulse_sender, impulse_receiver) = mpsc::channel(OUTPUT_VALVE_IMPULSE_CAPACITY);
    if active.get() {
        impulse_senders.borrow_mut().push(impulse_sender);
    }
    impulse_receiver
}

impl ActorOutputValveSignal {
    pub fn new(impulse_stream: impl Stream<Item = ()> + 'static) -> Self {
        let active = Rc::new(Cell::new(true));
        let impulse_senders = Rc::new(RefCell::new(SmallVec::<[mpsc::Sender<()>; 4]>::new()));
        let active_for_task = active.clone();
        let impulse_senders_for_task = impulse_senders.clone();
        Self {
            active,
            impulse_senders,
            _task: Task::start_droppable(async move {
                let mut impulse_stream = pin!(impulse_stream.fuse());
                while impulse_stream.next().await.is_some() {
                    broadcast_output_valve_impulse(impulse_senders_for_task.as_ref());
                }
                close_output_valve_subscribers(
                    active_for_task.as_ref(),
                    impulse_senders_for_task.as_ref(),
                );
            }),
        }
    }

    pub fn stream(&self) -> mpsc::Receiver<()> {
        subscribe_output_valve_impulses(self.active.as_ref(), self.impulse_senders.as_ref())
    }
}

// --- ConstructInfo ---

pub struct ConstructInfo {
    id: ConstructId,
    // @TODO remove Option in the future once Persistence is created also inside API functions?
    persistence: Option<parser::Persistence>,
    description: Cow<'static, str>,
}

impl ConstructInfo {
    pub fn new(
        id: impl Into<ConstructId>,
        persistence: Option<parser::Persistence>,
        description: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            id: id.into(),
            persistence,
            description: description.into(),
        }
    }

    pub fn complete(self, r#type: ConstructType) -> ConstructInfoComplete {
        ConstructInfoComplete {
            r#type,
            id: self.id,
            persistence: self.persistence,
            description: self.description,
        }
    }
}

// --- ConstructInfoComplete ---

#[derive(Clone, Debug)]
pub struct ConstructInfoComplete {
    r#type: ConstructType,
    id: ConstructId,
    persistence: Option<parser::Persistence>,
    description: Cow<'static, str>,
}

impl ConstructInfoComplete {
    pub fn id(&self) -> ConstructId {
        self.id.clone()
    }

    pub fn persistence(&self) -> Option<&parser::Persistence> {
        self.persistence.as_ref()
    }
}

impl std::fmt::Display for ConstructInfoComplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({:?} {:?} '{}')",
            self.r#type, self.id.ids, self.description
        )
    }
}

// --- ConstructType ---

#[derive(Debug, Clone, Copy)]
pub enum ConstructType {
    Variable,
    LinkVariable,
    VariableOrArgumentReference,
    FunctionCall,
    LatestCombinator,
    ValueActor,
    LazyValueActor,
    Object,
    TaggedObject,
    Text,
    Tag,
    Number,
    List,
}

// --- ConstructId ---

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(crate = "serde")]
pub struct ConstructId {
    ids: Arc<Vec<Cow<'static, str>>>,
}

impl ConstructId {
    pub fn new(id: impl IntoCowStr<'static>) -> Self {
        Self {
            ids: Arc::new(vec![id.into_cow_str()]),
        }
    }

    pub fn with_child_id(&self, child: impl IntoCowStr<'static>) -> Self {
        let mut ids = Vec::clone(&self.ids);
        ids.push(child.into_cow_str());
        Self { ids: Arc::new(ids) }
    }
}

impl<T: IntoCowStr<'static>> From<T> for ConstructId {
    fn from(id: T) -> Self {
        ConstructId::new(id)
    }
}

// --- Variable ---

pub struct Variable {
    construct_info: ConstructInfoComplete,
    /// Persistence identity from parser - REQUIRED for all Variables.
    /// Combined with scope using `persistence_id.in_scope(&scope)` to create unique keys.
    persistence_id: parser::PersistenceId,
    /// Scope context from List/map evaluation - either Root or Nested with a unique prefix.
    /// Combined with persistence_id to create a unique key per list item.
    /// This is how we distinguish item1.completed from item2.completed.
    scope: parser::Scope,
    name: Cow<'static, str>,
    value_actor: ActorHandle,
    link_value_sender: Option<NamedChannel<Value>>,
}

impl Variable {
    pub fn new(
        construct_info: ConstructInfo,
        name: impl Into<Cow<'static, str>>,
        value_actor: ActorHandle,
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Variable),
            persistence_id,
            scope,
            name: name.into(),
            value_actor,
            link_value_sender: None,
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        name: impl Into<Cow<'static, str>>,
        value_actor: ActorHandle,
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info,
            name,
            value_actor,
            persistence_id,
            scope,
        ))
    }

    pub fn persistence_id(&self) -> parser::PersistenceId {
        self.persistence_id
    }

    pub fn scope(&self) -> &parser::Scope {
        &self.scope
    }

    pub fn new_link_arc(
        construct_info: ConstructInfo,
        name: impl Into<Cow<'static, str>>,
        actor_context: ActorContext,
        persistence_id: parser::PersistenceId,
    ) -> Arc<Self> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: variable_description,
        } = construct_info;
        // Clone description for logging before moving it
        let variable_description_for_log = variable_description.clone();
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Variable"),
            persistence,
            variable_description,
        );
        let (link_value_sender, link_value_receiver) =
            NamedChannel::new("link.values", LINK_VALUE_CAPACITY);
        // Capture the scope before actor_context is moved
        let scope = actor_context.scope.clone();
        let scope_id = actor_context.scope_id();

        // Wrap the receiver stream with logging to trace values reaching the link_value_actor
        // We'll capture the actor's address after Arc creation for correlation
        let desc_for_closure = variable_description_for_log.clone();
        let logged_receiver = link_value_receiver.map(move |value| {
            if LOG_DEBUG {
                let value_desc = match &value {
                    Value::Tag(tag, _) => format!("Tag({})", tag.tag()),
                    Value::Object(_, _) => "Object".to_string(),
                    _ => "Other".to_string(),
                };
                zoon::println!(
                    "[LINK_ACTOR] Received from channel: {} for {}",
                    value_desc,
                    desc_for_closure
                );
            }
            value
        });

        if LOG_DEBUG {
            zoon::println!(
                "[LINK_ACTOR] Created link_value_actor pid={} scope={:?} for {}",
                persistence_id,
                scope,
                variable_description_for_log
            );
        }

        let value_actor = create_actor(logged_receiver, persistence_id, scope_id);
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            scope,
            name: name.into(),
            value_actor,
            link_value_sender: Some(link_value_sender),
        })
    }

    /// Create a new LINK variable with a forwarding actor for sibling field access.
    /// This is used when a LINK is referenced by another field in the same Object.
    /// The forwarding actor was pre-created and registered with ReferenceConnector,
    /// so sibling fields will find it.
    pub fn new_link_arc_with_forwarding_actor(
        construct_info: ConstructInfo,
        name: impl Into<Cow<'static, str>>,
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
        forwarding_actor: ActorHandle,
        link_value_sender: NamedChannel<Value>,
    ) -> Arc<Self> {
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            scope,
            name: name.into(),
            // Use forwarding_actor so sibling field lookups work correctly
            value_actor: forwarding_actor,
            // Keep the sender so elements can send events - this is the original sender
            link_value_sender: Some(link_value_sender),
        })
    }

    /// Subscribe to this variable's value stream.
    ///
    /// This method keeps the Variable alive for the lifetime of the returned stream.
    /// This is important because:
    /// - LINK variables have a `link_value_sender` that must stay alive
    /// - The Variable may be the only reference keeping dependent actors alive
    ///
    /// Takes ownership of the Arc to keep the Variable alive for the subscription lifetime.
    /// Callers should use `.clone().stream()` if they need to retain a reference.
    ///
    /// This is synchronous and cancellation-safe.
    pub fn stream(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        let subscription = self.value_actor.clone().stream();
        // Subscription keeps the actor alive; we also need to keep the Variable alive
        stream::unfold(
            (subscription, self),
            |(mut subscription, variable)| async move {
                subscription
                    .next()
                    .await
                    .map(|value| (value, (subscription, variable)))
            },
        )
        .boxed_local()
    }

    /// Subscribe to future values only - skips historical replay.
    ///
    /// Use this for event triggers (THEN, WHEN, List/remove) where historical
    /// events should NOT be replayed.
    ///
    /// Takes ownership of the Arc to keep the Variable alive for the subscription lifetime.
    ///
    /// This is synchronous and cancellation-safe.
    pub fn stream_from_now(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        let subscription = self.value_actor.clone().stream_from_now();
        // Subscription keeps the actor alive; we also need to keep the Variable alive
        stream::unfold(
            (subscription, self),
            |(mut subscription, variable)| async move {
                subscription
                    .next()
                    .await
                    .map(|value| (value, (subscription, variable)))
            },
        )
        .boxed_local()
    }

    pub fn value_actor(&self) -> ActorHandle {
        self.value_actor.clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn link_value_sender(&self) -> Option<NamedChannel<Value>> {
        self.link_value_sender.clone()
    }

    pub fn expect_link_value_sender(&self) -> NamedChannel<Value> {
        if let Some(link_value_sender) = self.link_value_sender.clone() {
            link_value_sender
        } else {
            panic!(
                "Failed to get expected link value sender from {}",
                self.construct_info
            );
        }
    }
}

// --- VariableOrArgumentReference ---

pub struct VariableOrArgumentReference {}

impl VariableOrArgumentReference {
    pub fn new_arc_value_actor(
        actor_context: ActorContext,
        alias: static_expression::Alias,
        root_value_actor: impl Future<Output = ActorHandle> + 'static,
    ) -> ActorHandle {
        // Capture context flags before closures
        let use_snapshot = actor_context.is_snapshot_context;
        let subscription_after_seq = actor_context.subscription_after_seq;
        let mut skip_alias_parts = 0;
        let alias_parts = match alias {
            static_expression::Alias::WithoutPassed {
                parts,
                referenced_span: _,
            } => {
                skip_alias_parts = 1;
                parts
            }
            static_expression::Alias::WithPassed { extra_parts } => extra_parts,
        };
        let scope_id = actor_context.scope_id();
        let parts_vec: Vec<_> = alias_parts.into_iter().skip(skip_alias_parts).collect();
        let mut root_value_actor = Box::pin(root_value_actor);
        let mut ready_root_actor = None;

        if let Poll::Ready(actor) = poll_future_once(root_value_actor.as_mut()) {
            if use_snapshot {
                if let Some(value) = try_resolve_snapshot_alias_value_now(&actor, &parts_vec) {
                    return create_constant_actor(parser::PersistenceId::new(), value, scope_id);
                }
            }
            ready_root_actor = Some(actor);
        }

        // For snapshot context (THEN/WHEN bodies), we get a single value.
        // For streaming context, we get continuous updates.
        let mut value_stream: LocalBoxStream<'static, Value> = if use_snapshot {
            match ready_root_actor {
                Some(actor) => stream::once(async move { actor.value().await })
                    .filter_map(|v| future::ready(v.ok()))
                    .boxed_local(),
                None => stream::once(async move {
                    let actor = root_value_actor.await;
                    actor.value().await
                })
                .filter_map(|v| future::ready(v.ok()))
                .boxed_local(),
            }
        } else {
            match ready_root_actor {
                Some(actor) => actor.stream(),
                None => stream::once(async move { root_value_actor.await })
                    .then(move |actor| async move { actor.stream() })
                    .flatten()
                    .boxed_local(),
            }
        };
        let num_parts = parts_vec.len();
        let alias_path_debug = parts_vec
            .iter()
            .map(|part| part.to_string())
            .collect::<Vec<_>>()
            .join(".");
        // Log subscription_after_seq for debugging
        if LOG_DEBUG {
            if let Some(sub_time) = subscription_after_seq {
                zoon::println!(
                    "[VAR_REF] Creating path subscription with subscription_after_seq={}",
                    sub_time
                );
            }
        }

        for (idx, alias_part) in parts_vec.into_iter().enumerate() {
            let alias_part = alias_part.to_string();
            let _is_last = idx == num_parts - 1;
            let step_idx = idx;
            let alias_path_debug_for_step = alias_path_debug.clone();

            // Process each field in the path using switch_map.
            // switch_map switches to a new inner stream whenever the outer emits.
            // It drains any in-flight items before switching to prevent losing events.
            //
            // WHILE re-render works because:
            // 1. When WHILE switches away from an arm, old Variables are dropped
            // 2. Subscription stream ends (receiver gone)
            // 3. switch_map creates new subscription when outer emits again
            // 4. When WHILE switches back, new Variables get new subscription
            let alias_part_for_log = alias_part.clone();
            value_stream = switch_map(value_stream, move |value| {
                let alias_part = alias_part.clone();
                let alias_part_log = alias_part_for_log.clone();
                let _alias_path_debug = alias_path_debug_for_step.clone();
                if LOG_DEBUG {
                    let value_type = match &value {
                        Value::Object(_, _) => "Object",
                        Value::TaggedObject(tagged, _) => tagged.tag(),
                        Value::Tag(tag, _) => tag.tag(),
                        _ => "Other",
                    };
                    zoon::println!(
                        "[VAR_REF] step {} outer received {} for field '{}'",
                        step_idx,
                        value_type,
                        alias_part_log
                    );
                }
                match value {
                    Value::Object(object, _) => {
                        let variable = object.expect_variable(&alias_part);
                        let variable_actor = variable.value_actor();
                        // Use value() or stream() based on context - type-safe subscription
                        if use_snapshot {
                            // Snapshot: get current value once
                            stream::once(async move {
                                let value = variable_actor.value().await;
                                // Keepalive - prevent drop until Future completes
                                drop((object, variable));
                                value
                            })
                            .filter_map(|v| future::ready(v.ok()))
                            .boxed_local()
                        } else {
                            // Streaming: continuous updates
                            // Always use stream() - stale events are filtered by emission sequence.
                            // This replaces the old hardcoded is_event_link pattern.
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), object, variable, subscription_after_seq),
                                move |(subscription_opt, actor_opt, object, variable, sub_time)| {
                                    async move {
                                        let mut subscription = match subscription_opt {
                                            Some(s) => s,
                                            None => {
                                                let actor = actor_opt.unwrap();
                                                actor.stream()
                                            }
                                        };
                                        // Poll until we get a non-stale value (or stream ends)
                                        loop {
                                            match subscription.next().await {
                                                Some(value) => {
                                                    observe_emission_seq(value.emission_seq());

                                                    // Filter stale values based on subscription_after_seq
                                                    if let Some(time) = sub_time {
                                                        if value.is_emitted_at_or_before(time) {
                                                            // Skip stale value, keep polling
                                                            if LOG_DEBUG {
                                                                let value_type = match &value {
                                                                    Value::Object(_, _) => "Object",
                                                                    Value::TaggedObject(tagged, _) => tagged.tag(),
                                                                    Value::Tag(tag, _) => tag.tag(),
                                                                    _ => "Other",
                                                                };
                                                                zoon::println!("[ALIAS_PATH] FILTERED stale {} (value.seq={:?} <= after_seq={})",
                                                                    value_type, value.emission_seq(), time);
                                                            }
                                                            continue;
                                                        }
                                                    }
                                                    // Fresh value - emit it
                                                    if LOG_DEBUG {
                                                        let emit_value_type = match &value {
                                                            Value::Object(_, _) => "Object",
                                                            Value::TaggedObject(tagged, _) => tagged.tag(),
                                                            Value::Tag(tag, _) => tag.tag(),
                                                            _ => "Other",
                                                        };
                                                        zoon::println!("[VAR_REF] Emitting {} (seq={:?}, after_seq={:?})",
                                                            emit_value_type, value.emission_seq(), sub_time);
                                                    }
                                                    return Some((value, (Some(subscription), None, object, variable, sub_time)));
                                                }
                                                None => {
                                                    // Stream ended
                                                    return None;
                                                }
                                            }
                                        }
                                    }
                                }
                            ).boxed_local()
                        }
                    }
                    Value::TaggedObject(tagged_object, _) => {
                        let variable = tagged_object.expect_variable(&alias_part);
                        let variable_actor = variable.value_actor();
                        // Use value() or stream() based on context - type-safe subscription
                        if use_snapshot {
                            // Snapshot: get current value once
                            stream::once(async move {
                                let value = variable_actor.value().await;
                                // Keepalive - prevent drop until Future completes
                                drop((tagged_object, variable));
                                value
                            })
                            .filter_map(|v| future::ready(v.ok()))
                            .boxed_local()
                        } else {
                            // Streaming: continuous updates
                            // Always use stream() - stale events are filtered by emission sequence.
                            // This replaces the old hardcoded is_event_link pattern.
                            stream::unfold(
                                (
                                    None::<LocalBoxStream<'static, Value>>,
                                    Some(variable_actor),
                                    tagged_object,
                                    variable,
                                    subscription_after_seq,
                                ),
                                move |(
                                    subscription_opt,
                                    actor_opt,
                                    tagged_object,
                                    variable,
                                    sub_time,
                                )| {
                                    async move {
                                        let mut subscription = match subscription_opt {
                                            Some(s) => s,
                                            None => {
                                                let actor = actor_opt.unwrap();
                                                actor.stream()
                                            }
                                        };
                                        // Poll until we get a non-stale value (or stream ends)
                                        loop {
                                            match subscription.next().await {
                                                Some(value) => {
                                                    observe_emission_seq(value.emission_seq());

                                                    // Filter stale values based on subscription_after_seq
                                                    if let Some(time) = sub_time {
                                                        if value.is_emitted_at_or_before(time) {
                                                            // Skip stale value, keep polling
                                                            continue;
                                                        }
                                                    }
                                                    // Fresh value - emit it
                                                    return Some((
                                                        value,
                                                        (
                                                            Some(subscription),
                                                            None,
                                                            tagged_object,
                                                            variable,
                                                            sub_time,
                                                        ),
                                                    ));
                                                }
                                                None => {
                                                    // Stream ended
                                                    return None;
                                                }
                                            }
                                        }
                                    }
                                },
                            )
                            .boxed_local()
                        }
                    }
                    // Tag, Number, Text, Boolean, etc. don't have fields.
                    // This occurs transiently when e.g. WHEN arm pattern tags flow through
                    // a field access chain. Emit nothing for this inner stream and let
                    // switch_map stay parked on the outer stream until the next Object arrives.
                    _non_object => stream::empty::<Value>().boxed_local(),
                }
            });
        }
        // Subscription-based streams are infinite (subscriptions never terminate first)
        create_actor(value_stream, parser::PersistenceId::new(), scope_id)
    }
}

fn poll_future_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    future.as_mut().poll(&mut cx)
}

fn poll_stream_once<S: Stream + ?Sized>(mut stream: Pin<&mut S>) -> Poll<Option<S::Item>> {
    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    stream.as_mut().poll_next(&mut cx)
}

fn try_resolve_snapshot_alias_value_now(
    root_actor: &ActorHandle,
    alias_parts: &[parser::StrSlice],
) -> Option<Value> {
    let mut current_value = root_actor.current_value().ok()?;

    for alias_part in alias_parts {
        let field_name = alias_part.as_str();
        current_value = match current_value {
            Value::Object(object, _) => object
                .expect_variable(field_name)
                .value_actor()
                .current_value()
                .ok()?,
            Value::TaggedObject(tagged_object, _) => tagged_object
                .expect_variable(field_name)
                .value_actor()
                .current_value()
                .ok()?,
            _ => return None,
        };
    }

    Some(current_value)
}

// --- ReferenceConnector ---

/// Actor for connecting references to actors by span.
/// Uses a sidecar task internally to encapsulate the async work.
pub struct ReferenceConnector {
    referenceables: RefCell<HashMap<parser::Span, ActorHandle>>,
    pending_referenceables: RefCell<HashMap<parser::Span, Vec<oneshot::Sender<ActorHandle>>>>,
}

impl ReferenceConnector {
    pub fn new() -> Self {
        Self {
            referenceables: RefCell::new(HashMap::new()),
            pending_referenceables: RefCell::new(HashMap::new()),
        }
    }

    pub fn register_referenceable(&self, span: parser::Span, actor: ActorHandle) {
        if let Some(waiters) = self.pending_referenceables.borrow_mut().remove(&span) {
            for waiter in waiters {
                if waiter.send(actor.clone()).is_err() {
                    zoon::eprintln!("Failed to send referenceable actor from reference connector");
                }
            }
        }
        self.referenceables.borrow_mut().insert(span, actor);
    }

    // @TODO is &self enough?
    pub async fn referenceable(self: Arc<Self>, span: parser::Span) -> ActorHandle {
        if let Some(actor) = self.referenceables.borrow().get(&span).cloned() {
            return actor;
        }

        let (referenceable_sender, referenceable_receiver) = oneshot::channel();
        self.pending_referenceables
            .borrow_mut()
            .entry(span)
            .or_default()
            .push(referenceable_sender);
        referenceable_receiver
            .await
            .expect("Failed to get referenceable from ReferenceConnector")
    }
}

// --- LinkConnector ---

/// Key for LinkConnector that includes both span (source position) and scope.
/// This ensures LINK bindings at the same source position but in different scopes
/// (e.g., different list items created by List/map) have unique identities.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopedSpan {
    pub span: parser::Span,
    pub scope: parser::Scope,
}

impl ScopedSpan {
    pub fn new(span: parser::Span, scope: parser::Scope) -> Self {
        Self { span, scope }
    }
}

/// Actor for connecting LINK variables with their setters.
/// Similar to ReferenceConnector but stores mpsc senders for LINK variables.
/// Uses a sidecar task internally to encapsulate the async work.
///
/// IMPORTANT: Uses ScopedSpan (span + scope) as the key to ensure LINK bindings
/// inside function calls (like new_todo() in List/map) get unique identities
/// per list item, not just per source position.
pub struct LinkConnector {
    links: RefCell<HashMap<ScopedSpan, NamedChannel<Value>>>,
    pending_links: RefCell<HashMap<ScopedSpan, Vec<oneshot::Sender<NamedChannel<Value>>>>>,
}

impl LinkConnector {
    pub fn new() -> Self {
        Self {
            links: RefCell::new(HashMap::new()),
            pending_links: RefCell::new(HashMap::new()),
        }
    }

    /// Register a LINK variable's sender with its span and scope.
    /// The scope is critical for distinguishing LINK bindings at the same source position
    /// but in different contexts (e.g., different list items).
    pub fn register_link(
        &self,
        span: parser::Span,
        scope: parser::Scope,
        sender: NamedChannel<Value>,
    ) {
        let scoped_span = ScopedSpan::new(span, scope);
        if let Some(waiters) = self.pending_links.borrow_mut().remove(&scoped_span) {
            for waiter in waiters {
                if waiter.send(sender.clone()).is_err() {
                    zoon::eprintln!("Failed to send link sender from link connector");
                }
            }
        }
        self.links.borrow_mut().insert(scoped_span, sender);
    }

    /// Get a LINK variable's sender by its span and scope.
    pub async fn link_sender(
        self: Arc<Self>,
        span: parser::Span,
        scope: parser::Scope,
    ) -> NamedChannel<Value> {
        let scoped_span = ScopedSpan::new(span, scope);
        if let Some(sender) = self.links.borrow().get(&scoped_span).cloned() {
            return sender;
        }
        let (link_sender, link_receiver) = oneshot::channel();
        self.pending_links
            .borrow_mut()
            .entry(scoped_span)
            .or_default()
            .push(link_sender);
        link_receiver
            .await
            .expect("Failed to get link sender from LinkConnector")
    }
}

// --- PassThroughConnector ---

/// Actor for connecting LINK pass-through actors across re-evaluations.
/// When `element |> LINK { alias }` re-evaluates, this ensures the same
/// pass-through ValueActor receives the new value instead of creating a new one.
pub struct PassThroughConnector {
    /// Stored pass-through state: sender, actor, and forwarders retained for lifetime.
    pass_throughs:
        RefCell<HashMap<PassThroughKey, (mpsc::Sender<Value>, ActorHandle, Vec<ActorHandle>)>>,
}

/// Key for identifying pass-throughs: persistence_id + scope
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct PassThroughKey {
    pub persistence_id: parser::PersistenceId,
    pub scope: parser::Scope,
}

impl PassThroughConnector {
    pub fn new() -> Self {
        Self {
            pass_throughs: RefCell::new(HashMap::new()),
        }
    }

    fn remove_if_closed(
        &self,
        key: &PassThroughKey,
    ) -> Option<(mpsc::Sender<Value>, ActorHandle, Vec<ActorHandle>)> {
        let is_closed = self
            .pass_throughs
            .borrow()
            .get(key)
            .map(|(sender, _, _)| sender.is_closed())
            .unwrap_or(false);
        if is_closed {
            self.pass_throughs.borrow_mut().remove(key)
        } else {
            None
        }
    }

    /// Register a new pass-through actor
    pub fn register(
        &self,
        key: PassThroughKey,
        value_sender: mpsc::Sender<Value>,
        actor: ActorHandle,
    ) {
        self.pass_throughs
            .borrow_mut()
            .insert(key, (value_sender, actor, Vec::new()));
    }

    /// Forward a value to an existing pass-through
    pub fn forward(&self, key: PassThroughKey, value: Value) {
        let _ = self.remove_if_closed(&key);
        if let Some((sender, _, _)) = self.pass_throughs.borrow_mut().get_mut(&key) {
            if let Err(e) = sender.try_send(value) {
                zoon::println!("[PASS_THROUGH] Forward failed for key {:?}: {e}", key);
            }
        }
    }

    /// Add a forwarder to keep alive for an existing pass-through
    pub fn add_forwarder(&self, key: PassThroughKey, forwarder: ActorHandle) {
        let _ = self.remove_if_closed(&key);
        if let Some((_, _, forwarders)) = self.pass_throughs.borrow_mut().get_mut(&key) {
            forwarders.push(forwarder);
        }
    }

    /// Get an existing pass-through actor if it exists
    pub async fn get(&self, key: PassThroughKey) -> Option<ActorHandle> {
        let _ = self.remove_if_closed(&key);
        self.pass_throughs
            .borrow()
            .get(&key)
            .map(|(_, actor, _)| actor.clone())
    }

    /// Get the value sender for an existing pass-through
    pub async fn get_sender(&self, key: PassThroughKey) -> Option<mpsc::Sender<Value>> {
        let _ = self.remove_if_closed(&key);
        self.pass_throughs
            .borrow()
            .get(&key)
            .map(|(sender, _, _)| sender.clone())
    }
}

// --- FunctionCall ---

pub struct FunctionCall {}

impl FunctionCall {
    pub fn new_arc_value_actor<FR: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        definition: impl Fn(
            Arc<Vec<ActorHandle>>,
            ConstructId,
            parser::PersistenceId,
            ConstructContext,
            ActorContext,
        ) -> FR
        + 'static,
        arguments: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        use zoon::futures_util::stream::StreamExt;

        let construct_info = construct_info.complete(ConstructType::FunctionCall);
        let arguments = Arc::new(arguments.into());

        // FLUSHED bypass logic: If any argument emits a FLUSHED value,
        // bypass the function and emit that FLUSHED value immediately.
        // This implements fail-fast error handling per FLUSH.md specification.
        //
        // Implementation:
        // 1. Subscribe to all arguments and merge their streams
        // 2. If any value is FLUSHED, emit it and don't call the function for that cycle
        // 3. If all values are non-FLUSHED, proceed with normal function processing
        //
        // For simplicity, we use a hybrid approach:
        // - Call the function normally
        // - Wrap the result stream to also listen to arguments for FLUSHED values
        // - If any argument emits FLUSHED before/during function processing, bypass

        // If persistence is None (e.g., for dynamically evaluated expressions),
        // generate a fresh persistence ID at runtime
        let persistence_id = construct_info
            .persistence
            .map(|p| p.id)
            .unwrap_or_else(parser::PersistenceId::new);

        let value_stream = definition(
            arguments.clone(),
            construct_info.id(),
            persistence_id,
            construct_context,
            actor_context.clone(),
        );

        // Create a stream that monitors arguments for FLUSHED values
        // and bypasses the function when FLUSHED is detected
        let arguments_for_flushed = arguments.clone();
        let flushed_bypass_stream = if arguments_for_flushed.is_empty() {
            // No arguments - no FLUSHED bypass needed
            zoon::futures_util::stream::empty().boxed_local()
        } else {
            // Subscribe to all arguments and filter for FLUSHED values only
            let flushed_streams: Vec<_> = arguments_for_flushed
                .iter()
                .map(|arg| {
                    arg.clone().stream().filter(|v| {
                        let is_flushed = v.is_flushed();
                        std::future::ready(is_flushed)
                    })
                })
                .collect();
            zoon::futures_util::stream::select_all(flushed_streams).boxed_local()
        };

        // Select between normal function output and FLUSHED bypass
        // FLUSHED values from arguments take priority
        let combined_stream = zoon::futures_util::stream::select(
            flushed_bypass_stream,
            value_stream.map(|v| {
                // If the function itself produces FLUSHED, pass it through
                v
            }),
        );

        // Combined stream is infinite (subscriptions never terminate first)

        // In lazy mode, use LazyValueActor for demand-driven evaluation.
        // This is critical for HOLD body context where sequential state updates are needed.
        if actor_context.use_lazy_actors {
            create_actor_lazy(
                combined_stream,
                parser::PersistenceId::new(),
                actor_context.scope_id(),
            )
        } else {
            let scope_id = actor_context.scope_id();
            create_actor(combined_stream, parser::PersistenceId::new(), scope_id)
        }
    }
}

// --- LatestCombinator ---

pub struct LatestCombinator {}

impl LatestCombinator {
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        inputs: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        #[derive(Default, Clone, Serialize, Deserialize)]
        #[serde(crate = "serde")]
        struct State {
            input_emission_keys: BTreeMap<usize, EmissionSeq>,
        }

        let construct_info = construct_info.complete(ConstructType::LatestCombinator);
        let inputs: Vec<ActorHandle> = inputs.into();
        // If persistence is None (e.g., for dynamically evaluated expressions),
        // generate a fresh persistence ID at runtime
        let persistent_id = construct_info
            .persistence
            .map(|p| p.id)
            .unwrap_or_else(parser::PersistenceId::new);
        let storage = construct_context.construct_storage.clone();

        // Track the newest value per input and select the globally newest candidate
        // on each emission. This matches kernel LATEST semantics more closely than
        // forwarding whichever input happened to arrive last. In particular, it
        // prevents an older late-arriving pulse from overwriting a newer candidate.
        let input_count = inputs.len();
        let value_stream =
            stream::select_all(inputs.iter().enumerate().map(|(index, value_actor)| {
                // Use stream() to properly handle lazy actors in HOLD body context
                value_actor
                    .clone()
                    .stream()
                    .map(move |value| (index, value))
                    .boxed_local()
            }))
            .scan(true, {
                let storage = storage.clone();
                move |first_run, (index, value)| {
                    let storage = storage.clone();
                    let previous_first_run = *first_run;
                    *first_run = false;
                    async move {
                        if previous_first_run {
                            Some((
                                storage.clone().load_state::<State>(persistent_id).await,
                                index,
                                value,
                            ))
                        } else {
                            Some((None, index, value))
                        }
                    }
                }
            })
            .scan(
                (State::default(), vec![None::<Value>; input_count]),
                move |(state, latest_values), (new_state, index, value)| {
                    if let Some(new_state) = new_state {
                        *state = new_state;
                    }
                    let emission = value.emission_seq();
                    let skip_value =
                        state
                            .input_emission_keys
                            .get(&index)
                            .is_some_and(|previous_emission| {
                                *previous_emission == emission
                                    && latest_values[index]
                                        .as_ref()
                                        .is_some_and(|previous| values_equal(previous, &value))
                            });
                    state.input_emission_keys.insert(index, emission);
                    if skip_value {
                        return future::ready(Some(None));
                    }

                    if latest_values[index]
                        .as_ref()
                        .is_some_and(|previous| previous.emission_seq() > emission)
                    {
                        return future::ready(Some(None));
                    }
                    latest_values[index] = Some(value);

                    let selected = latest_values
                        .iter()
                        .enumerate()
                        .filter_map(|(input_index, current)| {
                            current.as_ref().map(|current| (input_index, current))
                        })
                        .max_by(|(lhs_idx, lhs), (rhs_idx, rhs)| {
                            lhs.emission_seq()
                                .cmp(&rhs.emission_seq())
                                .then_with(|| rhs_idx.cmp(lhs_idx))
                        })
                        .map(|(_, selected)| selected.clone());

                    storage.save_state(persistent_id, &*state);
                    future::ready(Some(selected))
                },
            )
            .filter_map(future::ready);

        // Subscription-based streams are infinite (subscriptions never terminate first)
        let scope_id = actor_context.scope_id();
        create_actor(value_stream, parser::PersistenceId::new(), scope_id)
    }
}

// --- BinaryOperatorCombinator ---

/// Combines two value streams using a binary operation.
/// Used for comparators (==, <, >, etc.) and arithmetic (+, -, *, /).
pub struct BinaryOperatorCombinator {}

impl BinaryOperatorCombinator {
    /// Creates a ValueActor that combines two operands using the given operation.
    /// The operation receives both values and returns a new Value.
    pub fn new_arc_value_actor<F>(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
        operation: F,
    ) -> ActorHandle
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Value + 'static,
    {
        // Merge both operand streams, tracking which operand changed.
        // Use stream() to properly handle lazy actors in HOLD body context.
        // Sort by emission sequence to restore source ordering when both
        // operands have values ready at the same time.
        let a_stream = operand_a.stream().map(|v| (0usize, v));
        let b_stream = operand_b.stream().map(|v| (1usize, v));
        let value_stream = stream::select_all([a_stream.boxed_local(), b_stream.boxed_local()])
            .ready_chunks(4) // Buffer concurrent values
            .flat_map(|mut chunk| {
                chunk.sort_by_key(|(_, value)| value.emission_seq());
                stream::iter(chunk)
            })
            .scan(
                (None::<Value>, None::<Value>),
                move |(latest_a, latest_b), (index, value)| {
                    match index {
                        0 => *latest_a = Some(value),
                        1 => *latest_b = Some(value),
                        _ => unreachable!(),
                    }
                    let result = match (latest_a.clone(), latest_b.clone()) {
                        (Some(a), Some(b)) => Some((a, b)),
                        _ => None,
                    };
                    future::ready(Some(result))
                },
            )
            .filter_map(future::ready)
            .map({
                let construct_context = construct_context.clone();
                move |(a, b)| {
                    let idempotency_key = ValueIdempotencyKey::new();
                    operation(a, b, construct_context.clone(), idempotency_key)
                }
            });

        // Subscription-based streams are infinite (subscriptions never terminate first)
        let scope_id = actor_context.scope_id();
        create_actor(value_stream, parser::PersistenceId::new(), scope_id)
    }

    /// Async variant used when combining values requires awaiting nested state.
    pub fn new_arc_value_actor_async<F, Fut>(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
        operation: F,
    ) -> ActorHandle
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Fut + 'static,
        Fut: Future<Output = Value> + 'static,
    {
        let a_stream = operand_a.stream().map(|v| (0usize, v));
        let b_stream = operand_b.stream().map(|v| (1usize, v));
        let value_stream = stream::select_all([a_stream.boxed_local(), b_stream.boxed_local()])
            .ready_chunks(4)
            .flat_map(|mut chunk| {
                chunk.sort_by_key(|(_, value)| value.emission_seq());
                stream::iter(chunk)
            })
            .scan(
                (None::<Value>, None::<Value>),
                move |(latest_a, latest_b), (index, value)| {
                    match index {
                        0 => *latest_a = Some(value),
                        1 => *latest_b = Some(value),
                        _ => unreachable!(),
                    }
                    let result = match (latest_a.clone(), latest_b.clone()) {
                        (Some(a), Some(b)) => Some((a, b)),
                        _ => None,
                    };
                    future::ready(Some(result))
                },
            )
            .filter_map(future::ready)
            .then({
                let construct_context = construct_context.clone();
                move |(a, b)| {
                    let idempotency_key = ValueIdempotencyKey::new();
                    operation(a, b, construct_context.clone(), idempotency_key)
                }
            });

        let scope_id = actor_context.scope_id();
        create_actor(value_stream, parser::PersistenceId::new(), scope_id)
    }
}

// --- ComparatorCombinator ---

/// Helper for creating comparison combinators.
pub struct ComparatorCombinator {}

impl ComparatorCombinator {
    pub fn new_equal(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor_async(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| async move {
                let result = values_equal_async(&a, &b).await;
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "== result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_not_equal(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor_async(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| async move {
                let result = !values_equal_async(&a, &b).await;
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "=/= result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_greater(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_gt()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "> result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_greater_or_equal(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_ge()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, ">= result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_less(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_lt()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "< result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_less_or_equal(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_le()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "<= result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }
}

/// Fast-path equality for scalar values and identical object/list instances.
pub(super) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(n1, _), Value::Number(n2, _)) => n1.number() == n2.number(),
        (Value::Text(t1, _), Value::Text(t2, _)) => t1.text() == t2.text(),
        (Value::Tag(tag1, _), Value::Tag(tag2, _)) => tag1.tag() == tag2.tag(),
        (Value::TaggedObject(to1, _), Value::TaggedObject(to2, _)) => Arc::ptr_eq(to1, to2),
        (Value::Object(o1, _), Value::Object(o2, _)) => Arc::ptr_eq(o1, o2),
        (Value::List(l1, _), Value::List(l2, _)) => Arc::ptr_eq(l1, l2),
        (Value::Flushed(inner_a, _), Value::Flushed(inner_b, _)) => values_equal(inner_a, inner_b),
        _ => false, // Different types are not equal
    }
}

pub(super) async fn values_equal_async(a: &Value, b: &Value) -> bool {
    if values_equal(a, b) {
        return true;
    }

    match (a, b) {
        (Value::Flushed(inner_a, _), Value::Flushed(inner_b, _)) => {
            Box::pin(values_equal_async(inner_a, inner_b)).await
        }
        (Value::Flushed(inner_a, _), other) => Box::pin(values_equal_async(inner_a, other)).await,
        (other, Value::Flushed(inner_b, _)) => Box::pin(values_equal_async(other, inner_b)).await,
        (Value::Object(a_obj, _), Value::Object(b_obj, _)) => {
            object_shapes_equal_async(a_obj.variables(), b_obj.variables()).await
        }
        (Value::TaggedObject(a_obj, _), Value::TaggedObject(b_obj, _)) => {
            a_obj.tag() == b_obj.tag()
                && object_shapes_equal_async(a_obj.variables(), b_obj.variables()).await
        }
        (Value::List(a_list, _), Value::List(b_list, _)) => lists_equal_async(a_list, b_list).await,
        _ => false,
    }
}

async fn object_shapes_equal_async(
    a_variables: &[Arc<Variable>],
    b_variables: &[Arc<Variable>],
) -> bool {
    let a_effective = effective_variables(a_variables);
    let b_effective = effective_variables(b_variables);

    if a_effective.len() != b_effective.len() {
        return false;
    }

    for (field_name, a_var) in a_effective {
        let Some(b_var) = b_effective.get(field_name) else {
            return false;
        };

        let a_value = match a_var.value_actor().current_value() {
            Ok(value) => value,
            Err(_) => return a_var.value_actor().actor_id() == b_var.value_actor().actor_id(),
        };
        let b_value = match b_var.value_actor().current_value() {
            Ok(value) => value,
            Err(_) => return false,
        };

        if !Box::pin(values_equal_async(&a_value, &b_value)).await {
            return false;
        }
    }

    true
}

fn effective_variables<'a>(variables: &'a [Arc<Variable>]) -> HashMap<&'a str, &'a Arc<Variable>> {
    let mut effective = HashMap::new();
    for variable in variables.iter().rev() {
        effective.entry(variable.name()).or_insert(variable);
    }
    effective
}

async fn lists_equal_async(a_list: &Arc<List>, b_list: &Arc<List>) -> bool {
    let a_snapshot = a_list.snapshot().await;
    let b_snapshot = b_list.snapshot().await;

    if a_snapshot.len() != b_snapshot.len() {
        return false;
    }

    for ((_, a_actor), (_, b_actor)) in a_snapshot.iter().zip(b_snapshot.iter()) {
        let a_value = match a_actor.current_value() {
            Ok(value) => value,
            Err(_) => return a_actor.actor_id() == b_actor.actor_id(),
        };
        let b_value = match b_actor.current_value() {
            Ok(value) => value,
            Err(_) => return false,
        };

        if !Box::pin(values_equal_async(&a_value, &b_value)).await {
            return false;
        }
    }

    true
}

/// Compare two Values for ordering. Returns None if types are incompatible.
fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(n1, _), Value::Number(n2, _)) => n1.number().partial_cmp(&n2.number()),
        (Value::Text(t1, _), Value::Text(t2, _)) => Some(t1.text().cmp(t2.text())),
        _ => None,
    }
}

// --- ArithmeticCombinator ---

/// Helper for creating arithmetic combinators.
pub struct ArithmeticCombinator {}

impl ArithmeticCombinator {
    pub fn new_add(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) + get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "+ result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }

    pub fn new_subtract(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) - get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "- result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }

    pub fn new_multiply(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) * get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "* result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }

    pub fn new_divide(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) / get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "/ result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }
}

/// Extract a number from a Value, panicking if not a Number.
fn get_number(value: &Value) -> f64 {
    match value {
        Value::Number(n, _) => n.number(),
        other => panic!(
            "Expected Number for arithmetic operation, got {}",
            other.construct_info()
        ),
    }
}

// --- ValueHistory ---

/// Ring buffer for storing recent values with their versions.
/// This enables subscriptions to retrieve all values since a given version,
/// preventing message loss when values emit faster than subscribers poll.
///
/// # Design
/// - Stores up to `max_entries` (version, value) pairs
/// - When buffer is full, oldest entries are dropped
/// - Subscribers can pull all values since their `last_seen_version`
///
/// # Memory
/// Default 64 entries × sizeof(Value) per ValueActor
#[derive(Default)]
pub struct ValueHistory {
    values: VecDeque<(u64, Value)>,
    max_entries: usize,
}

impl ValueHistory {
    /// Create a new ValueHistory with specified capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(max_entries),
            max_entries,
        }
    }

    /// Add a value at the given version.
    pub fn add(&mut self, version: u64, value: Value) {
        self.values.push_back((version, value));
        // Trim old entries if over capacity
        while self.values.len() > self.max_entries {
            self.values.pop_front();
        }
    }

    /// Get all values since the given version.
    pub fn get_values_since(&self, since_version: u64) -> Vec<Value> {
        self.values
            .iter()
            .filter(|(v, _)| *v > since_version)
            .map(|(_, val)| val.clone())
            .collect()
    }

    /// Get the latest value in the history.
    pub fn get_latest(&self) -> Option<Value> {
        self.values.back().map(|(_, v)| v.clone())
    }

    /// Get the latest value that existed before the given source emission.
    pub fn get_latest_before_emission(&self, emission_seq: EmissionSeq) -> Option<Value> {
        self.values.iter().rev().find_map(|(_, value)| {
            let metadata = value.metadata();
            let is_before = metadata.emission_seq < emission_seq;
            is_before.then(|| value.clone())
        })
    }
}

/// Actor-local stored state that can be read directly by handles without routing
/// through a dedicated query channel.
#[derive(Clone)]
struct ActorStoredState {
    inner: Rc<ActorStoredStateInner>,
}

struct ActorStoredStateInner {
    is_alive: Cell<bool>,
    version: Cell<u64>,
    history: std::cell::RefCell<ValueHistory>,
    update_waiters: std::cell::RefCell<Vec<oneshot::Sender<()>>>,
}

impl ActorStoredState {
    fn new(max_entries: usize) -> Self {
        Self {
            inner: Rc::new(ActorStoredStateInner {
                is_alive: Cell::new(true),
                version: Cell::new(0),
                history: std::cell::RefCell::new(ValueHistory::new(max_entries)),
                update_waiters: std::cell::RefCell::new(Vec::new()),
            }),
        }
    }

    fn with_initial_value(max_entries: usize, initial_value: Value) -> Self {
        let state = Self::new(max_entries);
        state.store(initial_value);
        state
    }

    fn mark_dropped(&self) {
        self.inner.is_alive.set(false);
        self.inner.update_waiters.borrow_mut().clear();
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive.get()
    }

    fn version(&self) -> u64 {
        self.inner.version.get()
    }

    fn store(&self, value: Value) -> u64 {
        let new_version = self.version() + 1;
        self.inner.version.set(new_version);
        self.inner.history.borrow_mut().add(new_version, value);
        for waiter in self.inner.update_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
        new_version
    }

    fn get_values_since(&self, since_version: u64) -> Vec<Value> {
        self.inner.history.borrow().get_values_since(since_version)
    }

    fn latest(&self) -> Option<Value> {
        self.inner.history.borrow().get_latest()
    }

    fn latest_before_emission(&self, emission_seq: EmissionSeq) -> Option<Value> {
        self.inner
            .history
            .borrow()
            .get_latest_before_emission(emission_seq)
    }

    fn wait_for_update_after(&self, version: u64) -> Option<oneshot::Receiver<()>> {
        if self.version() > version {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.inner.update_waiters.borrow_mut().push(tx);
        Some(rx)
    }

    async fn wait_for_first_value_after(&self, version: u64) -> Result<Value, ValueError> {
        loop {
            if let Some(value) = self.get_values_since(version).into_iter().next() {
                return Ok(value);
            }

            if !self.is_alive() {
                return Err(ValueError::ActorDropped);
            }

            let Some(waiter) = self.wait_for_update_after(version) else {
                continue;
            };

            match waiter.await {
                Ok(()) => {}
                Err(_) if !self.is_alive() => return Err(ValueError::ActorDropped),
                Err(_) => {}
            }
        }
    }

    fn stream_from_version(&self, last_seen_version: u64) -> LocalBoxStream<'static, Value> {
        stream::unfold(
            (self.clone(), last_seen_version, VecDeque::<Value>::new()),
            |(stored_state, mut last_seen_version, mut buffered_values)| async move {
                loop {
                    if let Some(value) = buffered_values.pop_front() {
                        return Some((value, (stored_state, last_seen_version, buffered_values)));
                    }

                    let values = stored_state.get_values_since(last_seen_version);
                    if !values.is_empty() {
                        last_seen_version = stored_state.version();
                        buffered_values = values.into_iter().collect();
                        continue;
                    }

                    if !stored_state.is_alive() {
                        return None;
                    }

                    let Some(waiter) = stored_state.wait_for_update_after(last_seen_version) else {
                        continue;
                    };

                    match waiter.await {
                        Ok(()) => {}
                        Err(_) if !stored_state.is_alive() => return None,
                        Err(_) => {}
                    }
                }
            },
        )
        .boxed_local()
    }
}

// --- Scope-Based Generational Arena (Track D) ---
//
// Replaces Arc<ValueActor> with scope-based ownership.
// OwnedActor holds the heavy parts (retained runtime task, construct_info).
// ActorHandle is a lightweight clone-able reference (channel senders only).
// The registry owns all actors; scopes manage hierarchical lifetimes.

/// Unique identifier for an actor in the registry.
/// Uses generational indexing to detect use-after-free.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ActorId {
    index: u32,
    generation: u32,
}

/// Unique identifier for a scope in the registry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ScopeId {
    index: u32,
    generation: u32,
}

/// The owned part of an actor - lives in the registry.
/// Contains everything that doesn't need to be cloned for subscriptions.
pub struct OwnedActor {
    retained_tasks: SmallVec<[TaskHandle; 2]>,
    retained_actors: SmallVec<[ActorHandle; 2]>,
    stored_state: ActorStoredState,
    /// Lazy delegate (kept alive by OwnedActor, referenced by ActorHandle for routing).
    lazy_delegate: Option<Arc<LazyValueActor>>,
}

impl Drop for OwnedActor {
    fn drop(&mut self) {
        self.stored_state.mark_dropped();
        inc_metric!(ACTORS_DROPPED);
        LIVE_ACTOR_COUNT.with(|c| c.set(c.get() - 1));
    }
}

/// A scope groups actors with a common lifetime.
/// When a scope is destroyed, all its actors and child scopes are destroyed.
struct Scope {
    parent: Option<ScopeId>,
    actors: Vec<ActorId>,
    children: Vec<ScopeId>,
}

/// Slot in a generational arena. Either occupied with data and its generation,
/// or free with the next generation to use when the slot is reused.
enum Slot<T> {
    Occupied { generation: u32, value: T },
    Free { next_generation: u32 },
}

/// Arena-based actor registry with generational indices.
/// Thread-local - single-threaded by construction (WASM target).
///
/// Note on RefCell: This is accessed via thread_local!, so it's
/// single-threaded by construction. The CLAUDE.md rule "No Rc<RefCell>"
/// targets shared mutable state between actors. The registry is
/// infrastructure accessed synchronously during actor creation/destruction,
/// not actor-to-actor communication.
pub struct ActorRegistry {
    actors: Vec<Slot<OwnedActor>>,
    actor_free_list: Vec<u32>,
    scopes: Vec<Slot<Scope>>,
    scope_free_list: Vec<u32>,
}

thread_local! {
    pub static REGISTRY: RefCell<ActorRegistry> = RefCell::new(ActorRegistry::new());
}

impl ActorRegistry {
    pub fn new() -> Self {
        Self {
            actors: Vec::new(),
            actor_free_list: Vec::new(),
            scopes: Vec::new(),
            scope_free_list: Vec::new(),
        }
    }

    /// Create a new scope, optionally as a child of an existing scope.
    pub fn create_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let scope = Scope {
            parent,
            actors: Vec::new(),
            children: Vec::new(),
        };
        let (index, generation) = if let Some(free_idx) = self.scope_free_list.pop() {
            let idx = usize::try_from(free_idx).unwrap();
            let next_gen = match &self.scopes[idx] {
                Slot::Free { next_generation } => *next_generation,
                Slot::Occupied { .. } => unreachable!("free list points to occupied slot"),
            };
            self.scopes[idx] = Slot::Occupied {
                generation: next_gen,
                value: scope,
            };
            (free_idx, next_gen)
        } else {
            let idx = u32::try_from(self.scopes.len()).unwrap();
            self.scopes.push(Slot::Occupied {
                generation: 0,
                value: scope,
            });
            (idx, 0)
        };
        let scope_id = ScopeId { index, generation };
        // Register as child of parent
        if let Some(parent_id) = parent {
            if let Some(parent_scope) = self.get_scope_mut(parent_id) {
                parent_scope.children.push(scope_id);
            }
        }
        scope_id
    }

    /// Detach a scope subtree from the registry and return its owned actors.
    ///
    /// The returned actors must be dropped only after the registry borrow is released,
    /// because actor drop can re-enter the registry through nested ScopeDestroyGuards.
    pub fn take_scope_tree(&mut self, scope_id: ScopeId) -> Vec<OwnedActor> {
        let mut dropped_actors = Vec::new();
        self.take_scope_tree_into(scope_id, &mut dropped_actors);
        dropped_actors
    }

    /// Destroy a scope and all its actors and child scopes (recursive).
    pub fn destroy_scope(&mut self, scope_id: ScopeId) {
        drop(self.take_scope_tree(scope_id));
    }

    fn take_scope_tree_into(&mut self, scope_id: ScopeId, dropped_actors: &mut Vec<OwnedActor>) {
        let scope_idx = usize::try_from(scope_id.index).unwrap();
        let Some(scope) = self.get_scope_mut(scope_id) else {
            return;
        };

        // Take ownership of children and actors before freeing the scope slot.
        let parent = scope.parent;
        let children = std::mem::take(&mut scope.children);
        let actors = std::mem::take(&mut scope.actors);
        if LOG_ACTOR_FLOW {
            zoon::println!(
                "[FLOW] destroy_scope({:?}): {} children, {} actors: {:?}",
                scope_id,
                children.len(),
                actors.len(),
                actors
            );
        }

        // Free the scope slot with incremented generation
        let old_gen = scope_id.generation;
        self.scopes[scope_idx] = Slot::Free {
            next_generation: old_gen + 1,
        };
        self.scope_free_list.push(scope_id.index);

        // Remove the destroyed child from its parent so repeated arm/list-item scope
        // churn does not leave stale child ids hanging off long-lived parent scopes.
        // Without this, paths like repeated Cells edit-mode toggles accumulate dead
        // scope ids and later recursive destroys can explode in time/memory.
        if let Some(parent_id) = parent {
            if let Some(parent_scope) = self.get_scope_mut(parent_id) {
                parent_scope
                    .children
                    .retain(|child_id| *child_id != scope_id);
            }
        }

        // Destroy child scopes recursively
        for child_id in children {
            self.take_scope_tree_into(child_id, dropped_actors);
        }

        // Remove actors
        for actor_id in actors {
            if let Some(actor) = self.take_actor(actor_id) {
                dropped_actors.push(actor);
            }
        }
    }

    /// Insert an actor into the registry under a given scope.
    pub fn insert_actor(&mut self, scope_id: ScopeId, actor: OwnedActor) -> ActorId {
        let (index, generation) = if let Some(free_idx) = self.actor_free_list.pop() {
            let idx = usize::try_from(free_idx).unwrap();
            let next_gen = match &self.actors[idx] {
                Slot::Free { next_generation } => *next_generation,
                Slot::Occupied { .. } => unreachable!("free list points to occupied slot"),
            };
            self.actors[idx] = Slot::Occupied {
                generation: next_gen,
                value: actor,
            };
            (free_idx, next_gen)
        } else {
            let idx = u32::try_from(self.actors.len()).unwrap();
            self.actors.push(Slot::Occupied {
                generation: 0,
                value: actor,
            });
            (idx, 0)
        };
        let actor_id = ActorId { index, generation };
        // Register in scope
        if let Some(scope) = self.get_scope_mut(scope_id) {
            scope.actors.push(actor_id);
        }
        actor_id
    }

    /// Remove an actor from the registry.
    pub fn remove_actor(&mut self, actor_id: ActorId) {
        drop(self.take_actor(actor_id));
    }

    fn take_actor(&mut self, actor_id: ActorId) -> Option<OwnedActor> {
        let actor_idx = usize::try_from(actor_id.index).unwrap();
        match &self.actors.get(actor_idx) {
            Some(Slot::Occupied { generation, .. }) if *generation == actor_id.generation => {
                if LOG_ACTOR_FLOW {
                    zoon::println!("[FLOW] remove_actor({:?}): dropping actor", actor_id);
                }
            }
            _ => return None,
        }
        let old_gen = actor_id.generation;
        let actor_slot = std::mem::replace(
            &mut self.actors[actor_idx],
            Slot::Free {
                next_generation: old_gen + 1,
            },
        );
        self.actor_free_list.push(actor_id.index);
        match actor_slot {
            Slot::Occupied { value, .. } => Some(value),
            Slot::Free { .. } => unreachable!("validated occupied actor slot before take_actor"),
        }
    }

    /// Get a reference to an owned actor.
    pub fn get_actor(&self, actor_id: ActorId) -> Option<&OwnedActor> {
        let actor_idx = usize::try_from(actor_id.index).ok()?;
        match self.actors.get(actor_idx)? {
            Slot::Occupied { generation, value } if *generation == actor_id.generation => {
                Some(value)
            }
            _ => None,
        }
    }

    /// Get a mutable reference to an owned actor.
    pub fn get_actor_mut(&mut self, actor_id: ActorId) -> Option<&mut OwnedActor> {
        let actor_idx = usize::try_from(actor_id.index).ok()?;
        match self.actors.get_mut(actor_idx)? {
            Slot::Occupied { generation, value } if *generation == actor_id.generation => {
                Some(value)
            }
            _ => None,
        }
    }

    /// Get a reference to a scope.
    fn get_scope(&self, scope_id: ScopeId) -> Option<&Scope> {
        let idx = usize::try_from(scope_id.index).ok()?;
        match self.scopes.get(idx)? {
            Slot::Occupied { generation, value } if *generation == scope_id.generation => {
                Some(value)
            }
            _ => None,
        }
    }

    /// Get a mutable reference to a scope.
    fn get_scope_mut(&mut self, scope_id: ScopeId) -> Option<&mut Scope> {
        let idx = usize::try_from(scope_id.index).ok()?;
        match self.scopes.get_mut(idx)? {
            Slot::Occupied { generation, value } if *generation == scope_id.generation => {
                Some(value)
            }
            _ => None,
        }
    }
}

// --- ActorHandle ---

/// Lightweight, clone-able handle for interacting with an actor.
///
/// Unlike `Arc<ValueActor>`, cloning an ActorHandle does NOT keep the actor alive.
/// The `ActorRegistry` owns the actor (via `OwnedActor`). The handle only holds
/// direct state and immutable metadata needed to read or publish values.
///
/// This separates "how to talk to an actor" (ActorHandle) from
/// "who owns the actor" (registry scope).
#[derive(Clone)]
pub struct ActorHandle {
    actor_id: ActorId,

    /// Directly readable actor state for current-value and version lookups.
    stored_state: ActorStoredState,

    /// Persistence ID for this actor.
    persistence_id: parser::PersistenceId,

    /// Whether this handle represents an immutable constant actor.
    is_constant: bool,

    /// Origin info for items created by persisted List/append.
    list_item_origin: Option<Arc<ListItemOrigin>>,
}

impl ActorHandle {
    pub fn actor_id(&self) -> ActorId {
        self.actor_id
    }

    pub fn persistence_id(&self) -> parser::PersistenceId {
        self.persistence_id
    }

    pub fn list_item_origin(&self) -> Option<&ListItemOrigin> {
        self.list_item_origin.as_deref()
    }

    pub fn version(&self) -> u64 {
        self.stored_state.version()
    }

    pub fn has_lazy_delegate(&self) -> bool {
        self.lazy_delegate().is_some()
    }

    fn lazy_delegate(&self) -> Option<Arc<LazyValueActor>> {
        REGISTRY.with(|reg| {
            reg.borrow()
                .get_actor(self.actor_id)
                .and_then(|owned_actor| owned_actor.lazy_delegate.clone())
        })
    }

    fn constant_value(&self) -> Option<Value> {
        self.is_constant
            .then(|| self.stored_state.latest())
            .flatten()
    }

    /// Directly store a value, bypassing the async input stream.
    pub fn store_value_directly(&self, value: Value) {
        if !self.is_constant && self.lazy_delegate().is_none() && self.stored_state.is_alive() {
            publish_value_to_actor_state(&self.stored_state, value);
        }
    }

    /// Get the current stored value from direct actor state.
    pub fn current_value(&self) -> Result<Value, CurrentValueError> {
        if let Some(value) = self.constant_value() {
            return Ok(value);
        }

        if !self.stored_state.is_alive() {
            return Err(CurrentValueError::ActorDropped);
        }

        self.stored_state
            .latest()
            .ok_or(CurrentValueError::NoValueYet)
    }

    pub fn current_value_before_emission(
        &self,
        emission_seq: EmissionSeq,
    ) -> Result<Value, CurrentValueError> {
        if let Some(value) = self.constant_value() {
            return Ok(value);
        }

        if !self.stored_state.is_alive() {
            return Err(CurrentValueError::ActorDropped);
        }

        self.stored_state
            .latest_before_emission(emission_seq)
            .ok_or(CurrentValueError::NoValueYet)
    }

    /// Subscribe to continuous stream of all values from version 0.
    ///
    /// The registry scope owns the actor's lifetime.
    pub fn stream(&self) -> LocalBoxStream<'static, Value> {
        self.subscription_stream_from_version(0)
    }

    /// Subscribe starting from current version - only future values.
    pub fn stream_from_now(&self) -> LocalBoxStream<'static, Value> {
        self.subscription_stream_from_version(self.version())
    }

    /// Emit the current value once if present (or wait for the first one), then future updates.
    ///
    /// This deduplicates the common "seed with direct state, then subscribe from now" pattern
    /// used across the classic runtime and bridge layers.
    pub fn current_or_future_stream(&self) -> LocalBoxStream<'static, Value> {
        if let Some(value) = self.constant_value() {
            return stream::once(future::ready(value)).boxed_local();
        }

        match self.current_value() {
            Ok(value) => {
                let last_seen_version = self.version();
                stream::once(future::ready(value))
                    .chain(self.subscription_stream_from_version(last_seen_version))
                    .boxed_local()
            }
            Err(CurrentValueError::NoValueYet) => {
                let last_seen_version = self.version();
                self.subscription_stream_from_version(last_seen_version)
            }
            Err(CurrentValueError::ActorDropped) => stream::empty().boxed_local(),
        }
    }

    /// Get exactly ONE value - waiting if necessary.
    pub async fn value(&self) -> Result<Value, ValueError> {
        match self.current_value() {
            Ok(value) => return Ok(value),
            Err(CurrentValueError::ActorDropped) => return Err(ValueError::ActorDropped),
            Err(CurrentValueError::NoValueYet) => {}
        }

        if let Some(lazy_delegate) = self.lazy_delegate() {
            return lazy_delegate
                .stream_from_cursor(self.version() as usize)
                .next()
                .await
                .ok_or(ValueError::ActorDropped);
        }

        self.stored_state
            .wait_for_first_value_after(self.version())
            .await
    }

    fn subscription_stream_from_version(
        &self,
        last_seen_version: u64,
    ) -> LocalBoxStream<'static, Value> {
        if let Some(lazy_delegate) = self.lazy_delegate() {
            if LOG_ACTOR_FLOW && last_seen_version == 0 {
                zoon::println!("[FLOW] stream() on {:?} → lazy delegate", self.actor_id);
            }
            return lazy_delegate
                .stream_from_cursor(last_seen_version as usize)
                .boxed_local();
        }

        if self.is_constant {
            return if last_seen_version == 0 {
                self.constant_value()
                    .map(|value| stream::once(future::ready(value)).boxed_local())
                    .unwrap_or_else(|| stream::empty().boxed_local())
            } else {
                stream::empty().boxed_local()
            };
        }

        self.stored_state.stream_from_version(last_seen_version)
    }
}

/// Create an actor in the registry and return a handle to interact with it.
///
/// The retained runtime task goes into the registry under `scope_id`.
/// The returned `ActorHandle` holds only direct state metadata — cloning it does NOT keep the actor alive.
pub fn create_actor<S>(
    value_stream: S,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    S: Stream<Item = Value> + 'static,
{
    match plan_actor_stream_creation(value_stream) {
        ActorStreamCreationPlan::Constant(value) => {
            create_constant_actor(persistence_id, value, scope_id)
        }
        ActorStreamCreationPlan::Stream(value_stream) => {
            create_stream_driven_actor_arc_info(persistence_id, scope_id, None, value_stream)
        }
    }
}

pub fn create_constant_actor(
    persistence_id: parser::PersistenceId,
    constant_value: Value,
    scope_id: ScopeId,
) -> ActorHandle {
    create_constant_actor_arc_info(persistence_id, constant_value, scope_id, None)
}

pub fn create_constant_actor_with_origin(
    persistence_id: parser::PersistenceId,
    constant_value: Value,
    origin: ListItemOrigin,
    scope_id: ScopeId,
) -> ActorHandle {
    create_constant_actor_arc_info(
        persistence_id,
        constant_value,
        scope_id,
        Some(Arc::new(origin)),
    )
}

fn publish_value_to_actor_state(stored_state: &ActorStoredState, new_value: Value) {
    let new_version = stored_state.store(new_value.clone());
    if LOG_ACTOR_FLOW {
        zoon::println!("[FLOW] actor state produced v{new_version}");
    }
}

fn insert_owned_actor_handle(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    stored_state: ActorStoredState,
    lazy_delegate: Option<Arc<LazyValueActor>>,
    is_constant: bool,
    list_item_origin: Option<Arc<ListItemOrigin>>,
) -> ActorHandle {
    inc_metric!(ACTORS_CREATED);
    LIVE_ACTOR_COUNT.with(|c| c.set(c.get() + 1));

    let actor_id = insert_actor_and_drain(
        scope_id,
        OwnedActor {
            retained_tasks: SmallVec::new(),
            retained_actors: SmallVec::new(),
            stored_state: stored_state.clone(),
            lazy_delegate: lazy_delegate.clone(),
        },
    );

    ActorHandle {
        actor_id,
        stored_state,
        persistence_id,
        is_constant,
        list_item_origin,
    }
}

fn create_constant_actor_arc_info(
    persistence_id: parser::PersistenceId,
    constant_value: Value,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
) -> ActorHandle {
    let stored_state = ActorStoredState::with_initial_value(1, constant_value);
    insert_owned_actor_handle(
        persistence_id,
        scope_id,
        stored_state,
        None,
        true,
        list_item_origin,
    )
}

fn create_direct_state_actor_arc_info(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
) -> ActorHandle {
    let stored_state = ActorStoredState::new(64);
    insert_owned_actor_handle(
        persistence_id,
        scope_id,
        stored_state,
        None,
        false,
        list_item_origin,
    )
}

fn create_stream_driven_actor_arc_info<S>(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
    value_stream: S,
) -> ActorHandle
where
    S: Stream<Item = Value> + 'static,
{
    let actor = create_direct_state_actor_arc_info(persistence_id, scope_id, list_item_origin);
    let mut value_stream = value_stream.boxed_local();

    loop {
        match poll_stream_once(value_stream.as_mut()) {
            Poll::Ready(Some(value)) => actor.store_value_directly(value),
            Poll::Ready(None) => return actor,
            Poll::Pending => break,
        }
    }

    let actor_for_task = actor.clone();
    start_retained_actor_task(&actor, async move {
        if LOG_ACTOR_FLOW {
            zoon::println!("[FLOW] Actor stream task STARTED");
        }
        drain_value_stream_to_actor_state(actor_for_task, value_stream).await;
    });
    actor
}

fn create_lazy_stream_driven_actor_arc_info<S>(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    value_stream: S,
) -> ActorHandle
where
    S: Stream<Item = Value> + 'static,
{
    let stored_state = ActorStoredState::new(64);
    let stored_state_for_loop = stored_state.clone();
    let (lazy_actor, request_rx) = LazyValueActor::new_unstarted();
    let lazy_actor = Arc::new(lazy_actor);
    let actor = insert_owned_actor_handle(
        persistence_id,
        scope_id,
        stored_state,
        Some(lazy_actor.clone()),
        false,
        None,
    );
    start_retained_actor_task(
        &actor,
        LazyValueActor::internal_loop(value_stream, request_rx, stored_state_for_loop),
    );
    actor
}

enum ActorStreamCreationPlan {
    Constant(Value),
    Stream(LocalBoxStream<'static, Value>),
}

fn plan_actor_stream_creation<S>(value_stream: S) -> ActorStreamCreationPlan
where
    S: Stream<Item = Value> + 'static,
{
    let mut value_stream = value_stream.boxed_local();
    match poll_stream_once(value_stream.as_mut()) {
        Poll::Ready(Some(first_value)) => match poll_stream_once(value_stream.as_mut()) {
            Poll::Ready(None) => ActorStreamCreationPlan::Constant(first_value),
            Poll::Ready(Some(second_value)) => ActorStreamCreationPlan::Stream(
                stream::once(future::ready(first_value))
                    .chain(stream::once(future::ready(second_value)))
                    .chain(value_stream)
                    .boxed_local(),
            ),
            Poll::Pending => ActorStreamCreationPlan::Stream(
                stream::once(future::ready(first_value))
                    .chain(value_stream)
                    .boxed_local(),
            ),
        },
        Poll::Ready(None) | Poll::Pending => ActorStreamCreationPlan::Stream(value_stream),
    }
}

pub fn create_actor_from_future<F>(
    value_future: F,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    F: Future<Output = Value> + 'static,
{
    create_actor(
        stream::once(async move { value_future.await }),
        persistence_id,
        scope_id,
    )
}

pub fn create_actor_lazy_from_future<F>(
    value_future: F,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    F: Future<Output = Value> + 'static,
{
    create_actor_lazy(
        stream::once(async move { value_future.await }),
        persistence_id,
        scope_id,
    )
}

pub fn create_actor_forwarding_from_future_source<F>(
    source_future: F,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    F: Future<Output = Option<ActorHandle>> + 'static,
{
    let forwarding_actor = create_actor_forwarding(persistence_id, scope_id);
    let mut source_future = Box::pin(source_future);
    match poll_future_once(source_future.as_mut()) {
        Poll::Ready(Some(source_actor)) => {
            connect_forwarding_current_and_future(forwarding_actor.clone(), source_actor);
        }
        Poll::Ready(None) => {}
        Poll::Pending => {
            let forwarding_actor_for_task = forwarding_actor.clone();
            start_retained_actor_task(&forwarding_actor, async move {
                let Some(source_actor) = source_future.await else {
                    return;
                };
                forward_current_and_future_from_source(forwarding_actor_for_task, source_actor)
                    .await;
            });
        }
    }
    forwarding_actor
}

/// Create a forwarding actor backed only by direct stored state.
pub fn create_actor_forwarding(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    create_direct_state_actor_arc_info(persistence_id, scope_id, None)
}

/// Create an actor with list item origin metadata.
pub fn create_actor_with_origin<S: Stream<Item = Value> + 'static>(
    value_stream: S,
    persistence_id: parser::PersistenceId,
    origin: ListItemOrigin,
    scope_id: ScopeId,
) -> ActorHandle {
    let origin = Arc::new(origin);
    match plan_actor_stream_creation(value_stream) {
        ActorStreamCreationPlan::Constant(value) => {
            create_constant_actor_arc_info(persistence_id, value, scope_id, Some(origin))
        }
        ActorStreamCreationPlan::Stream(value_stream) => create_stream_driven_actor_arc_info(
            persistence_id,
            scope_id,
            Some(origin),
            value_stream,
        ),
    }
}

async fn drain_value_stream_to_actor_state<S>(target_actor: ActorHandle, value_stream: S)
where
    S: Stream<Item = Value> + 'static,
{
    let mut value_stream = pin!(value_stream);
    while let Some(value) = value_stream.next().await {
        if !target_actor.stored_state.is_alive() {
            break;
        }
        target_actor.store_value_directly(value);
    }
}

pub fn retain_actor_task(actor: &ActorHandle, task: TaskHandle) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        if let Some(owned_actor) = reg.get_actor_mut(actor.actor_id) {
            owned_actor.retained_tasks.push(task);
        }
    });
}

pub fn start_retained_actor_task<F>(actor: &ActorHandle, future: F)
where
    F: Future<Output = ()> + 'static,
{
    retain_actor_task(actor, Task::start_droppable(future));
}

pub fn retain_actor_handle(actor: &ActorHandle, retained: ActorHandle) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        if let Some(owned_actor) = reg.get_actor_mut(actor.actor_id) {
            owned_actor.retained_actors.push(retained);
        }
    });
}

pub fn retain_actor_handles<I>(actor: &ActorHandle, retained: I)
where
    I: IntoIterator<Item = ActorHandle>,
{
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        if let Some(owned_actor) = reg.get_actor_mut(actor.actor_id) {
            owned_actor.retained_actors.extend(retained);
        }
    });
}

/// Create a lazy actor for demand-driven evaluation (used in HOLD body context).
pub fn create_actor_lazy<S: Stream<Item = Value> + 'static>(
    value_stream: S,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    match plan_actor_stream_creation(value_stream) {
        ActorStreamCreationPlan::Constant(value) => {
            create_constant_actor(persistence_id, value, scope_id)
        }
        ActorStreamCreationPlan::Stream(value_stream) => {
            create_lazy_stream_driven_actor_arc_info(persistence_id, scope_id, value_stream)
        }
    }
}

enum ForwardingSubscriptionPlan {
    NoFutureSubscription,
    SubscribeAfterVersion(u64),
}

fn seed_forwarding_actor_from_source_now(
    forwarding_actor: &ActorHandle,
    source_actor: &ActorHandle,
) -> ForwardingSubscriptionPlan {
    if let Some(value) = source_actor.constant_value() {
        forwarding_actor.store_value_directly(value);
        return ForwardingSubscriptionPlan::NoFutureSubscription;
    }

    match source_actor.current_value() {
        Ok(value) => {
            forwarding_actor.store_value_directly(value);
            ForwardingSubscriptionPlan::SubscribeAfterVersion(source_actor.version())
        }
        Err(CurrentValueError::NoValueYet) => {
            ForwardingSubscriptionPlan::SubscribeAfterVersion(source_actor.version())
        }
        Err(CurrentValueError::ActorDropped) => ForwardingSubscriptionPlan::NoFutureSubscription,
    }
}

fn seed_forwarding_replay_all_from_source_now(
    forwarding_actor: &ActorHandle,
    source_actor: &ActorHandle,
) -> ForwardingSubscriptionPlan {
    if let Some(value) = source_actor.constant_value() {
        forwarding_actor.store_value_directly(value);
        return ForwardingSubscriptionPlan::NoFutureSubscription;
    }

    let buffered_values = source_actor.stored_state.get_values_since(0);
    for value in buffered_values {
        forwarding_actor.store_value_directly(value);
    }

    if source_actor.stored_state.is_alive() {
        ForwardingSubscriptionPlan::SubscribeAfterVersion(source_actor.version())
    } else {
        ForwardingSubscriptionPlan::NoFutureSubscription
    }
}

async fn forward_source_updates_after_version(
    forwarding_actor: ActorHandle,
    source_actor: ActorHandle,
    last_seen_version: u64,
) {
    drain_value_stream_to_actor_state(
        forwarding_actor,
        source_actor.subscription_stream_from_version(last_seen_version),
    )
    .await;
}

async fn forward_current_and_future_from_source(
    forwarding_actor: ActorHandle,
    source_actor: ActorHandle,
) {
    let last_seen_version =
        match seed_forwarding_actor_from_source_now(&forwarding_actor, &source_actor) {
            ForwardingSubscriptionPlan::NoFutureSubscription => return,
            ForwardingSubscriptionPlan::SubscribeAfterVersion(last_seen_version) => {
                last_seen_version
            }
        };

    forward_source_updates_after_version(forwarding_actor, source_actor, last_seen_version).await;
}

/// Forward the source actor's current value once and then only future updates.
pub fn connect_forwarding_current_and_future(
    forwarding_actor: ActorHandle,
    source_actor: ActorHandle,
) {
    let last_seen_version =
        match seed_forwarding_actor_from_source_now(&forwarding_actor, &source_actor) {
            ForwardingSubscriptionPlan::NoFutureSubscription => return,
            ForwardingSubscriptionPlan::SubscribeAfterVersion(last_seen_version) => {
                last_seen_version
            }
        };

    let forwarding_actor_for_task = forwarding_actor.clone();
    start_retained_actor_task(&forwarding_actor, async move {
        if LOG_DEBUG {
            zoon::println!("[FWD2] connect_forwarding loop STARTED");
        }
        if LOG_DEBUG {
            zoon::println!("[FORWARDING] Subscribed, entering forwarding loop");
        }
        forward_source_updates_after_version(
            forwarding_actor_for_task,
            source_actor,
            last_seen_version,
        )
        .await;
        if LOG_DEBUG {
            zoon::println!("[FORWARDING] Forwarding loop ENDED");
        }
    });
}

/// Forward all currently buffered and future values from the source actor.
pub fn connect_forwarding_replay_all(forwarding_actor: ActorHandle, source_actor: ActorHandle) {
    let last_seen_version =
        match seed_forwarding_replay_all_from_source_now(&forwarding_actor, &source_actor) {
            ForwardingSubscriptionPlan::NoFutureSubscription => return,
            ForwardingSubscriptionPlan::SubscribeAfterVersion(last_seen_version) => {
                last_seen_version
            }
        };

    let forwarding_actor_for_task = forwarding_actor.clone();
    start_retained_actor_task(&forwarding_actor, async move {
        drain_value_stream_to_actor_state(
            forwarding_actor_for_task,
            source_actor.subscription_stream_from_version(last_seen_version),
        )
        .await;
    });
}

// --- ValueIdempotencyKey ---

pub type ValueIdempotencyKey = parser::PersistenceId;

// --- ValueMetadata ---

#[derive(Clone, Copy)]
pub struct ValueMetadata {
    pub idempotency_key: ValueIdempotencyKey,
    /// Runtime-local emission sequence when this value was created.
    pub emission_seq: EmissionSeq,
}

impl ValueMetadata {
    /// Create new metadata with a fresh emission sequence.
    pub fn new(idempotency_key: ValueIdempotencyKey) -> Self {
        Self {
            idempotency_key,
            emission_seq: next_emission_seq(),
        }
    }

    /// Create metadata with a pre-captured emission sequence from an external source.
    /// Used when the sequence was captured earlier (e.g., in a DOM event handler)
    /// and needs to be preserved through async processing.
    pub fn with_emission_seq(
        idempotency_key: ValueIdempotencyKey,
        emission_seq: EmissionSeq,
    ) -> Self {
        observe_emission_seq(emission_seq);
        Self {
            idempotency_key,
            emission_seq,
        }
    }
}

// --- Value ---

#[derive(Clone)]
pub enum Value {
    Object(Arc<Object>, ValueMetadata),
    TaggedObject(Arc<TaggedObject>, ValueMetadata),
    Text(Arc<Text>, ValueMetadata),
    Tag(Arc<Tag>, ValueMetadata),
    Number(Arc<Number>, ValueMetadata),
    List(Arc<List>, ValueMetadata),
    /// FLUSHED[value] - internal wrapper for fail-fast error handling
    /// Created by FLUSH { value }, propagates transparently through pipelines,
    /// and unwraps at boundaries (variable bindings, function returns, BLOCK returns)
    Flushed(Box<Value>, ValueMetadata),
}

impl Value {
    pub fn construct_info(&self) -> &ConstructInfoComplete {
        match self {
            Self::Object(object, _) => &object.construct_info,
            Self::TaggedObject(tagged_object, _) => &tagged_object.construct_info,
            Self::Text(text, _) => &text.construct_info,
            Self::Tag(tag, _) => &tag.construct_info,
            Self::Number(number, _) => &number.construct_info,
            Self::List(list, _) => &list.construct_info,
            Self::Flushed(inner, _) => inner.construct_info(),
        }
    }

    pub fn metadata(&self) -> ValueMetadata {
        match self {
            Self::Object(_, metadata) => *metadata,
            Self::TaggedObject(_, metadata) => *metadata,
            Self::Text(_, metadata) => *metadata,
            Self::Tag(_, metadata) => *metadata,
            Self::Number(_, metadata) => *metadata,
            Self::List(_, metadata) => *metadata,
            Self::Flushed(_, metadata) => *metadata,
        }
    }
    pub fn metadata_mut(&mut self) -> &mut ValueMetadata {
        match self {
            Self::Object(_, metadata) => metadata,
            Self::TaggedObject(_, metadata) => metadata,
            Self::Text(_, metadata) => metadata,
            Self::Tag(_, metadata) => metadata,
            Self::Number(_, metadata) => metadata,
            Self::List(_, metadata) => metadata,
            Self::Flushed(_, metadata) => metadata,
        }
    }

    pub fn set_emission_seq(&mut self, emission_seq: EmissionSeq) {
        self.metadata_mut().emission_seq = emission_seq;
    }

    /// Get the emission sequence of when this value was created.
    pub fn emission_seq(&self) -> EmissionSeq {
        self.metadata().emission_seq
    }

    pub fn is_emitted_at_or_before(&self, seq: EmissionSeq) -> bool {
        self.emission_seq() <= seq
    }

    pub fn is_emitted_after(&self, seq: EmissionSeq) -> bool {
        self.emission_seq() > seq
    }

    /// Check if this value is a FLUSHED wrapper
    pub fn is_flushed(&self) -> bool {
        matches!(self, Self::Flushed(_, _))
    }

    /// Unwrap FLUSHED to get the inner value (for boundary unwrapping)
    /// Returns self unchanged if not FLUSHED
    pub fn unwrap_flushed(self) -> Value {
        match self {
            Self::Flushed(inner, _) => *inner,
            other => other,
        }
    }

    /// Create a FLUSHED wrapper around this value
    pub fn into_flushed(self) -> Value {
        let metadata = ValueMetadata::new(parser::PersistenceId::new());
        Value::Flushed(Box::new(self), metadata)
    }

    /// Check if this value might contain internal actors (stateful components).
    ///
    /// Returns true for complex types that might have internal state:
    /// - Object: might contain HOLD fields
    /// - TaggedObject: might contain HOLD fields
    /// - List: has internal state (items)
    ///
    /// Returns false for simple values that are just data:
    /// - Text, Number, Tag: no internal state
    ///
    /// Used by scope-based persistence to decide which function calls to record.
    /// If a function returns a value that contains_actors(), its inputs should
    /// be recorded so the value can be recreated on restoration.
    pub fn contains_actors(&self) -> bool {
        match self {
            Self::Object(_, _) => true,
            Self::TaggedObject(_, _) => true,
            Self::List(_, _) => true,
            Self::Text(_, _) => false,
            Self::Tag(_, _) => false,
            Self::Number(_, _) => false,
            Self::Flushed(inner, _) => inner.contains_actors(),
        }
    }

    pub fn idempotency_key(&self) -> ValueIdempotencyKey {
        self.metadata().idempotency_key
    }

    pub fn set_idempotency_key(&mut self, key: ValueIdempotencyKey) {
        self.metadata_mut().idempotency_key = key;
    }

    pub fn expect_object(self) -> Arc<Object> {
        let Self::Object(object, _) = self else {
            panic!(
                "Failed to get expected Object: The Value has a different type {}",
                self.construct_info()
            )
        };
        object
    }

    pub fn expect_tagged_object(self, tag: &str) -> Arc<TaggedObject> {
        let Self::TaggedObject(tagged_object, _) = self else {
            panic!("Failed to get expected TaggedObject: The Value has a different type")
        };
        let found_tag = &tagged_object.tag;
        if found_tag != tag {
            panic!(
                "Failed to get expected TaggedObject: Expected tag: '{tag}', found tag: '{found_tag}'"
            )
        }
        tagged_object
    }

    pub fn expect_text(self) -> Arc<Text> {
        let Self::Text(text, _) = self else {
            panic!("Failed to get expected Text: The Value has a different type")
        };
        text
    }

    pub fn expect_tag(self) -> Arc<Tag> {
        let Self::Tag(tag, _) = self else {
            panic!("Failed to get expected Tag: The Value has a different type")
        };
        tag
    }

    pub fn expect_number(self) -> Arc<Number> {
        let Self::Number(number, _) = self else {
            panic!("Failed to get expected Number: The Value has a different type")
        };
        number
    }

    pub fn expect_list(self) -> Arc<List> {
        let Self::List(list, _) = self else {
            panic!("Failed to get expected List: The Value has a different type")
        };
        list
    }

    /// Serializes this Value to a JSON representation.
    /// This is an async function because it needs to subscribe to streaming values.
    pub async fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Text(text, _) => serde_json::Value::String(text.text().to_string()),
            Value::Tag(tag, _) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "_tag".to_string(),
                    serde_json::Value::String(tag.tag().to_string()),
                );
                serde_json::Value::Object(obj)
            }
            Value::Number(number, _) => {
                serde_json::json!(number.number())
            }
            Value::Object(object, _) => {
                let mut obj = serde_json::Map::new();
                for variable in object.variables() {
                    let value = variable.value_actor().current_value().ok();
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::TaggedObject(tagged_object, _) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "_tag".to_string(),
                    serde_json::Value::String(tagged_object.tag().to_string()),
                );
                for variable in tagged_object.variables() {
                    let value = variable.value_actor().current_value().ok();
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::List(list, _) => {
                let mut json_items = Vec::new();
                for (_item_id, item) in list.snapshot().await {
                    let value = item.current_value().ok();
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        json_items.push(json_value);
                    }
                }
                serde_json::Value::Array(json_items)
            }
            Value::Flushed(inner, _) => {
                // Serialize FLUSHED values with a wrapper to preserve the flushed state
                let mut obj = serde_json::Map::new();
                obj.insert("_flushed".to_string(), serde_json::Value::Bool(true));
                obj.insert("value".to_string(), Box::pin(inner.to_json()).await);
                serde_json::Value::Object(obj)
            }
        }
    }

    /// Deserializes a JSON value into a Value (not wrapped in ValueActor).
    /// This is used internally by `value_actor_from_json`.
    pub fn from_json(
        json: &serde_json::Value,
        construct_id: ConstructId,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
    ) -> Value {
        match json {
            serde_json::Value::String(s) => {
                let construct_info = ConstructInfo::new(construct_id, None, "Text from JSON");
                Text::new_value(
                    construct_info,
                    construct_context,
                    idempotency_key,
                    s.clone(),
                )
            }
            serde_json::Value::Number(n) => {
                let construct_info = ConstructInfo::new(construct_id, None, "Number from JSON");
                let number = n.as_f64().unwrap_or(0.0);
                Number::new_value(construct_info, construct_context, idempotency_key, number)
            }
            serde_json::Value::Object(obj) => {
                if let Some(serde_json::Value::String(tag)) = obj.get("_tag") {
                    // TaggedObject or Tag
                    let other_fields: Vec<_> = obj.iter().filter(|(k, _)| *k != "_tag").collect();

                    if other_fields.is_empty() {
                        // Just a Tag
                        let construct_info =
                            ConstructInfo::new(construct_id, None, "Tag from JSON");
                        Tag::new_value(
                            construct_info,
                            construct_context,
                            idempotency_key,
                            tag.clone(),
                        )
                    } else {
                        // TaggedObject
                        let construct_info = ConstructInfo::new(
                            construct_id.clone(),
                            None,
                            "TaggedObject from JSON",
                        );
                        let variables: Vec<Arc<Variable>> = other_fields
                            .iter()
                            .map(|(name, value)| {
                                let var_construct_info = ConstructInfo::new(
                                    construct_id.with_child_id(format!("var_{name}")),
                                    None,
                                    "Variable from JSON",
                                );
                                let value_actor = value_actor_from_json(
                                    value,
                                    construct_id.with_child_id(format!("value_{name}")),
                                    construct_context.clone(),
                                    parser::PersistenceId::new(),
                                    actor_context.clone(),
                                );
                                Variable::new_arc(
                                    var_construct_info,
                                    (*name).clone(),
                                    value_actor,
                                    parser::PersistenceId::new(),
                                    actor_context.scope.clone(),
                                )
                            })
                            .collect();
                        TaggedObject::new_value(
                            construct_info,
                            construct_context,
                            idempotency_key,
                            tag.clone(),
                            variables,
                        )
                    }
                } else {
                    // Regular Object
                    let construct_info =
                        ConstructInfo::new(construct_id.clone(), None, "Object from JSON");
                    let variables: Vec<Arc<Variable>> = obj
                        .iter()
                        .map(|(name, value)| {
                            let var_construct_info = ConstructInfo::new(
                                construct_id.with_child_id(format!("var_{name}")),
                                None,
                                "Variable from JSON",
                            );
                            let value_actor = value_actor_from_json(
                                value,
                                construct_id.with_child_id(format!("value_{name}")),
                                construct_context.clone(),
                                parser::PersistenceId::new(),
                                actor_context.clone(),
                            );
                            Variable::new_arc(
                                var_construct_info,
                                name.clone(),
                                value_actor,
                                parser::PersistenceId::new(),
                                actor_context.scope.clone(),
                            )
                        })
                        .collect();
                    Object::new_value(
                        construct_info,
                        construct_context,
                        idempotency_key,
                        variables,
                    )
                }
            }
            serde_json::Value::Array(arr) => {
                let construct_info =
                    ConstructInfo::new(construct_id.clone(), None, "List from JSON");
                let items: Vec<ActorHandle> = arr
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        value_actor_from_json(
                            item,
                            construct_id.with_child_id(format!("item_{i}")),
                            construct_context.clone(),
                            parser::PersistenceId::new(),
                            actor_context.clone(),
                        )
                    })
                    .collect();
                List::new_value(
                    construct_info,
                    construct_context,
                    idempotency_key,
                    actor_context,
                    items,
                )
            }
            serde_json::Value::Bool(b) => {
                // Represent booleans as tags
                let construct_info = ConstructInfo::new(construct_id, None, "Tag from JSON bool");
                let tag = if *b { "True" } else { "False" };
                Tag::new_value(construct_info, construct_context, idempotency_key, tag)
            }
            serde_json::Value::Null => {
                // Represent null as a tag
                let construct_info = ConstructInfo::new(construct_id, None, "Tag from JSON null");
                Tag::new_value(construct_info, construct_context, idempotency_key, "None")
            }
        }
    }
}

/// Creates a ValueActor from a JSON value.
pub fn value_actor_from_json(
    json: &serde_json::Value,
    construct_id: ConstructId,
    construct_context: ConstructContext,
    idempotency_key: ValueIdempotencyKey,
    actor_context: ActorContext,
) -> ActorHandle {
    let value = Value::from_json(
        json,
        construct_id.clone(),
        construct_context,
        idempotency_key,
        actor_context.clone(),
    );
    let scope_id = actor_context.scope_id();
    create_constant_actor(parser::PersistenceId::new(), value, scope_id)
}

/// Saves a list of ValueActors to JSON for persistence.
/// Used by List persistence functions.
pub async fn save_list_items_to_json(items: &[ActorHandle]) -> Vec<serde_json::Value> {
    let mut json_items = Vec::new();
    for item in items {
        if let Ok(value) = item.current_value() {
            json_items.push(value.to_json().await);
        }
    }
    json_items
}

async fn save_list_stored_snapshot_to_storage(
    stored_state: &ListStoredState,
    storage: &ConstructStorage,
    persistence_id: parser::PersistenceId,
) {
    let items = stored_state
        .snapshot()
        .into_iter()
        .map(|(_, item_actor)| item_actor)
        .collect::<Vec<_>>();
    let json_items = save_list_items_to_json(&items).await;
    storage.save_state(persistence_id, &json_items);
}

async fn save_persistent_list_snapshot_after_next_update(
    stored_state: &ListStoredState,
    storage: &ConstructStorage,
    persistence_id: parser::PersistenceId,
    last_saved_version: u64,
) -> u64 {
    loop {
        let current_version = stored_state.version();
        if current_version > last_saved_version {
            save_list_stored_snapshot_to_storage(stored_state, storage, persistence_id).await;
            return current_version;
        }

        let Some(waiter) = stored_state.wait_for_update_after(last_saved_version) else {
            continue;
        };
        if waiter.await.is_err() {
            continue;
        }
    }
}

async fn save_or_watch_persistent_list(
    list_arc: Arc<List>,
    storage: Arc<ConstructStorage>,
    persistence_id: parser::PersistenceId,
) {
    if storage.is_disabled() {
        return;
    }

    let stored_state = list_arc.stored_state.clone();
    let last_saved_version = save_persistent_list_snapshot_after_next_update(
        &stored_state,
        storage.as_ref(),
        persistence_id,
        0,
    )
    .await;

    if !stored_state.has_future_state_updates() {
        return;
    }

    list_arc.start_retained_task(async move {
        let mut last_saved_version = last_saved_version;
        loop {
            last_saved_version = save_persistent_list_snapshot_after_next_update(
                &stored_state,
                storage.as_ref(),
                persistence_id,
                last_saved_version,
            )
            .await;
        }
    });
}

async fn apply_change_and_persist_items(
    change: &ListChange,
    current_items: &mut Vec<ActorHandle>,
    storage: &ConstructStorage,
    persistence_id: parser::PersistenceId,
) {
    change.clone().apply_to_vec(current_items);
    let json_items = save_list_items_to_json(current_items).await;
    storage.save_state(persistence_id, &json_items);
}

/// Materialize a Value by eagerly evaluating all lazy variable streams.
///
/// This is essential for HOLD with Object state. When THEN body produces an Object,
/// its variables contain lazy ValueActors that reference the state stream. If we
/// don't materialize, future state accesses will try to subscribe to old lazy actors
/// that depend on previous state, creating circular dependencies.
///
/// This function:
/// - For Object/TaggedObject: awaits each variable's value, recursively materializes,
///   and creates new Variables with constant ValueActors holding the materialized values
/// - For List: materializes each item
/// - For Flushed: materializes inner value
/// - For scalars (Number, Text, Tag): returns as-is
pub async fn materialize_value(
    value: Value,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Value {
    if let Some(materialized_now) = try_materialize_value_now(
        value.clone(),
        construct_context.clone(),
        actor_context.clone(),
    ) {
        return materialized_now;
    }

    let scope_id = actor_context.scope_id();
    match value {
        Value::Object(object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in object.variables() {
                // Await the variable's first concrete value; freshly created HOLD body fields
                // may not have a current stored value yet even though they are about to emit.
                let var_value = variable.value_actor().value().await.ok();
                if let Some(var_value) = var_value {
                    // Recursively materialize nested values
                    let materialized = Box::pin(materialize_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    ))
                    .await;
                    // Create a constant ValueActor for the materialized value
                    let value_actor =
                        create_constant_actor(parser::PersistenceId::new(), materialized, scope_id);
                    // Create new Variable with the constant actor
                    let new_var = Variable::new_arc(
                        ConstructInfo::new(
                            format!("materialized_var_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ),
                        variable.name().to_string(),
                        value_actor,
                        parser::PersistenceId::new(),
                        actor_context.scope.clone(),
                    );
                    materialized_vars.push(new_var);
                }
            }
            let new_object = Object::new_arc(
                ConstructInfo::new("materialized_object", None, "Materialized object"),
                construct_context,
                materialized_vars,
            );
            Value::Object(new_object, metadata)
        }
        Value::TaggedObject(tagged_object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in tagged_object.variables() {
                let var_value = variable.value_actor().value().await.ok();
                if let Some(var_value) = var_value {
                    let materialized = Box::pin(materialize_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    ))
                    .await;
                    let value_actor =
                        create_constant_actor(parser::PersistenceId::new(), materialized, scope_id);
                    let new_var = Variable::new_arc(
                        ConstructInfo::new(
                            format!("materialized_var_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ),
                        variable.name().to_string(),
                        value_actor,
                        parser::PersistenceId::new(),
                        actor_context.scope.clone(),
                    );
                    materialized_vars.push(new_var);
                }
            }
            let new_tagged_object = TaggedObject::new_arc(
                ConstructInfo::new(
                    "materialized_tagged_object",
                    None,
                    "Materialized tagged object",
                ),
                construct_context,
                tagged_object.tag().to_string(),
                materialized_vars,
            );
            Value::TaggedObject(new_tagged_object, metadata)
        }
        Value::List(list, metadata) => {
            // Wait until the list is initialized, then materialize from the current snapshot
            // rather than only the first change. Lists created through `List/append` /
            // `List/remove` may already have applied follow-up incremental changes by this
            // point, and taking only the first Replace would silently drop them.
            let items = list
                .snapshot()
                .await
                .into_iter()
                .map(|(_item_id, item_actor)| item_actor)
                .collect::<Vec<_>>();
            let mut materialized_items = Vec::with_capacity(items.len());
            for (_index, item_actor) in items.into_iter().enumerate() {
                let item_value = item_actor.value().await.ok();
                if let Some(item_value) = item_value {
                    let materialized = Box::pin(materialize_value(
                        item_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    ))
                    .await;
                    let value_actor =
                        create_constant_actor(parser::PersistenceId::new(), materialized, scope_id);
                    materialized_items.push(value_actor);
                }
            }
            let new_list = List::new_arc(
                ConstructInfo::new("materialized_list", None, "Materialized list"),
                construct_context,
                actor_context,
                materialized_items,
            );
            Value::List(new_list, metadata)
        }
        Value::Flushed(inner, metadata) => {
            let materialized_inner =
                Box::pin(materialize_value(*inner, construct_context, actor_context)).await;
            Value::Flushed(Box::new(materialized_inner), metadata)
        }
        // Scalars don't need materialization
        other => other,
    }
}

pub(crate) fn try_materialize_value_now(
    value: Value,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Option<Value> {
    let scope_id = actor_context.scope_id();
    match value {
        Value::Object(object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in object.variables() {
                let var_value = variable.value_actor().current_value().ok()?;
                let materialized = try_materialize_value_now(
                    var_value,
                    construct_context.clone(),
                    actor_context.clone(),
                )?;
                let value_actor =
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id);
                let new_var = Variable::new_arc(
                    ConstructInfo::new(
                        format!("materialized_var_{}", variable.name()),
                        None,
                        format!("Materialized variable {}", variable.name()),
                    ),
                    variable.name().to_string(),
                    value_actor,
                    parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                );
                materialized_vars.push(new_var);
            }
            let new_object = Object::new_arc(
                ConstructInfo::new("materialized_object", None, "Materialized object"),
                construct_context,
                materialized_vars,
            );
            Some(Value::Object(new_object, metadata))
        }
        Value::TaggedObject(tagged_object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in tagged_object.variables() {
                let var_value = variable.value_actor().current_value().ok()?;
                let materialized = try_materialize_value_now(
                    var_value,
                    construct_context.clone(),
                    actor_context.clone(),
                )?;
                let value_actor =
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id);
                let new_var = Variable::new_arc(
                    ConstructInfo::new(
                        format!("materialized_var_{}", variable.name()),
                        None,
                        format!("Materialized variable {}", variable.name()),
                    ),
                    variable.name().to_string(),
                    value_actor,
                    parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                );
                materialized_vars.push(new_var);
            }
            let new_tagged_object = TaggedObject::new_arc(
                ConstructInfo::new(
                    "materialized_tagged_object",
                    None,
                    "Materialized tagged object",
                ),
                construct_context,
                tagged_object.tag().to_string(),
                materialized_vars,
            );
            Some(Value::TaggedObject(new_tagged_object, metadata))
        }
        Value::List(list, metadata) => {
            let items = list
                .snapshot_now()?
                .into_iter()
                .map(|(_item_id, item_actor)| item_actor)
                .collect::<Vec<_>>();
            let mut materialized_items = Vec::with_capacity(items.len());
            for item_actor in items {
                let item_value = item_actor.current_value().ok()?;
                let materialized = try_materialize_value_now(
                    item_value,
                    construct_context.clone(),
                    actor_context.clone(),
                )?;
                let value_actor =
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id);
                materialized_items.push(value_actor);
            }
            let new_list = List::new_arc(
                ConstructInfo::new("materialized_list", None, "Materialized list"),
                construct_context,
                actor_context,
                materialized_items,
            );
            Some(Value::List(new_list, metadata))
        }
        Value::Flushed(inner, metadata) => {
            let materialized_inner =
                try_materialize_value_now(*inner, construct_context, actor_context)?;
            Some(Value::Flushed(Box::new(materialized_inner), metadata))
        }
        other => Some(other),
    }
}

/// Snapshot-oriented materialization that never waits for missing values.
///
/// This is used when freezing THEN/WHEN parameters at trigger time. Regular
/// `materialize_value` can intentionally wait for first emissions, which is
/// correct for some eager state paths but can deadlock snapshotting when an
/// object contains unresolved LINK fields that are not relevant to the body.
pub async fn materialize_snapshot_value(
    value: Value,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Value {
    async fn snapshot_actor_value(
        actor: &ActorHandle,
        actor_context: &ActorContext,
    ) -> Result<Value, CurrentValueError> {
        if let Some(emission_seq) = actor_context.snapshot_emission_seq {
            actor.current_value_before_emission(emission_seq)
        } else {
            actor.current_value()
        }
    }

    let scope_id = actor_context.scope_id();
    match value {
        Value::Object(object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in object.variables() {
                let value_actor = if let Ok(var_value) =
                    snapshot_actor_value(&variable.value_actor(), &actor_context).await
                {
                    let materialized = Box::pin(materialize_snapshot_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    ))
                    .await;
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id)
                } else {
                    // Snapshot freezing must not block waiting for a first value. Nested
                    // LINK-only structures need to remain traversable even when they never emit
                    // a normal value stream item, so preserve the original actor unchanged.
                    variable.value_actor().clone()
                };
                let new_var = Variable::new_arc(
                    ConstructInfo::new(
                        format!("materialized_snapshot_var_{}", variable.name()),
                        None,
                        format!("Materialized snapshot variable {}", variable.name()),
                    ),
                    variable.name().to_string(),
                    value_actor,
                    parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                );
                materialized_vars.push(new_var);
            }
            let new_object = Object::new_arc(
                ConstructInfo::new(
                    "materialized_snapshot_object",
                    None,
                    "Materialized snapshot object",
                ),
                construct_context,
                materialized_vars,
            );
            Value::Object(new_object, metadata)
        }
        Value::TaggedObject(tagged_object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in tagged_object.variables() {
                let value_actor = if let Ok(var_value) =
                    snapshot_actor_value(&variable.value_actor(), &actor_context).await
                {
                    let materialized = Box::pin(materialize_snapshot_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    ))
                    .await;
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id)
                } else {
                    variable.value_actor().clone()
                };
                let new_var = Variable::new_arc(
                    ConstructInfo::new(
                        format!("materialized_snapshot_var_{}", variable.name()),
                        None,
                        format!("Materialized snapshot variable {}", variable.name()),
                    ),
                    variable.name().to_string(),
                    value_actor,
                    parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                );
                materialized_vars.push(new_var);
            }
            let new_tagged_object = TaggedObject::new_arc(
                ConstructInfo::new(
                    "materialized_snapshot_tagged_object",
                    None,
                    "Materialized snapshot tagged object",
                ),
                construct_context,
                tagged_object.tag().to_string(),
                materialized_vars,
            );
            Value::TaggedObject(new_tagged_object, metadata)
        }
        Value::List(list, metadata) => {
            let items = list
                .snapshot()
                .await
                .into_iter()
                .map(|(_item_id, item_actor)| item_actor)
                .collect::<Vec<_>>();
            let mut materialized_items = Vec::with_capacity(items.len());
            for (_index, item_actor) in items.into_iter().enumerate() {
                let value_actor = if let Ok(item_value) =
                    snapshot_actor_value(&item_actor, &actor_context).await
                {
                    let materialized = Box::pin(materialize_snapshot_value(
                        item_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    ))
                    .await;
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id)
                } else {
                    item_actor
                };
                materialized_items.push(value_actor);
            }
            let new_list = List::new_arc(
                ConstructInfo::new(
                    "materialized_snapshot_list",
                    None,
                    "Materialized snapshot list",
                ),
                construct_context,
                actor_context,
                materialized_items,
            );
            Value::List(new_list, metadata)
        }
        Value::Flushed(inner, metadata) => {
            let materialized_inner = Box::pin(materialize_snapshot_value(
                *inner,
                construct_context,
                actor_context,
            ))
            .await;
            Value::Flushed(Box::new(materialized_inner), metadata)
        }
        other => other,
    }
}

pub(crate) fn try_materialize_snapshot_value_now(
    value: Value,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Option<Value> {
    fn snapshot_actor_value_now(
        actor: &ActorHandle,
        actor_context: &ActorContext,
    ) -> Result<Value, CurrentValueError> {
        if let Some(emission_seq) = actor_context.snapshot_emission_seq {
            actor.current_value_before_emission(emission_seq)
        } else {
            actor.current_value()
        }
    }

    let scope_id = actor_context.scope_id();
    match value {
        Value::Object(object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in object.variables() {
                let value_actor = if let Ok(var_value) =
                    snapshot_actor_value_now(&variable.value_actor(), &actor_context)
                {
                    let materialized = try_materialize_snapshot_value_now(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )?;
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id)
                } else {
                    variable.value_actor().clone()
                };
                let new_var = Variable::new_arc(
                    ConstructInfo::new(
                        format!("materialized_snapshot_var_{}", variable.name()),
                        None,
                        format!("Materialized snapshot variable {}", variable.name()),
                    ),
                    variable.name().to_string(),
                    value_actor,
                    parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                );
                materialized_vars.push(new_var);
            }
            let new_object = Object::new_arc(
                ConstructInfo::new(
                    "materialized_snapshot_object",
                    None,
                    "Materialized snapshot object",
                ),
                construct_context,
                materialized_vars,
            );
            Some(Value::Object(new_object, metadata))
        }
        Value::TaggedObject(tagged_object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in tagged_object.variables() {
                let value_actor = if let Ok(var_value) =
                    snapshot_actor_value_now(&variable.value_actor(), &actor_context)
                {
                    let materialized = try_materialize_snapshot_value_now(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )?;
                    create_constant_actor(parser::PersistenceId::new(), materialized, scope_id)
                } else {
                    variable.value_actor().clone()
                };
                let new_var = Variable::new_arc(
                    ConstructInfo::new(
                        format!("materialized_snapshot_var_{}", variable.name()),
                        None,
                        format!("Materialized snapshot variable {}", variable.name()),
                    ),
                    variable.name().to_string(),
                    value_actor,
                    parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                );
                materialized_vars.push(new_var);
            }
            let new_tagged_object = TaggedObject::new_arc(
                ConstructInfo::new(
                    "materialized_snapshot_tagged_object",
                    None,
                    "Materialized snapshot tagged object",
                ),
                construct_context,
                tagged_object.tag().to_string(),
                materialized_vars,
            );
            Some(Value::TaggedObject(new_tagged_object, metadata))
        }
        Value::List(list, metadata) => {
            let items = list
                .snapshot_now()?
                .into_iter()
                .map(|(_item_id, item_actor)| item_actor)
                .collect::<Vec<_>>();
            let mut materialized_items = Vec::with_capacity(items.len());
            for item_actor in items {
                let value_actor =
                    if let Ok(item_value) = snapshot_actor_value_now(&item_actor, &actor_context) {
                        let materialized = try_materialize_snapshot_value_now(
                            item_value,
                            construct_context.clone(),
                            actor_context.clone(),
                        )?;
                        create_constant_actor(parser::PersistenceId::new(), materialized, scope_id)
                    } else {
                        item_actor
                    };
                materialized_items.push(value_actor);
            }
            let new_list = List::new_arc(
                ConstructInfo::new(
                    "materialized_snapshot_list",
                    None,
                    "Materialized snapshot list",
                ),
                construct_context,
                actor_context,
                materialized_items,
            );
            Some(Value::List(new_list, metadata))
        }
        Value::Flushed(inner, metadata) => {
            let materialized_inner =
                try_materialize_snapshot_value_now(*inner, construct_context, actor_context)?;
            Some(Value::Flushed(Box::new(materialized_inner), metadata))
        }
        other => Some(other),
    }
}

// --- Object ---

pub struct Object {
    construct_info: ConstructInfoComplete,
    variables: Vec<Arc<Variable>>,
}

impl Object {
    pub fn new(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Object),
            variables: variables.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, variables))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Value {
        Value::Object(
            Self::new_arc(construct_info, construct_context, variables),
            ValueMetadata::new(idempotency_key),
        )
    }

    /// Create a Value with a pre-captured emission sequence from an external source.
    pub fn new_value_with_emission_seq(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        emission_seq: EmissionSeq,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Value {
        Value::Object(
            Self::new_arc(construct_info, construct_context, variables),
            ValueMetadata::with_emission_seq(idempotency_key, emission_seq),
        )
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> ActorHandle {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: object_description,
        } = construct_info;

        // Create the wrapped Object construct_info
        let object_construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Object"),
            persistence.clone(),
            object_description,
        );
        // Create the initial value first
        let initial_value = Self::new_value(
            object_construct_info,
            construct_context,
            idempotency_key,
            variables.into(),
        );

        // Use a constant actor so the value is immediately available
        // This is critical for WASM single-threaded runtime where spawned tasks
        // don't run until we yield - subscriptions need immediate access to values.
        let scope_id = actor_context.scope_id();
        create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id)
    }

    /// Look up a variable by name.
    /// Uses rposition (last match) so that explicit fields override spread fields
    /// when both have the same name — spread variables come first in the Vec.
    pub fn variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .iter()
            .rposition(|variable| variable.name == name)
            .map(|index| self.variables[index].clone())
    }

    pub fn expect_variable(&self, name: &str) -> Arc<Variable> {
        self.variable(name).unwrap_or_else(|| {
            panic!(
                "Failed to get expected Variable '{name}' from {}",
                self.construct_info
            )
        })
    }

    pub fn variables(&self) -> &[Arc<Variable>] {
        &self.variables
    }
}

// --- TaggedObject ---

pub struct TaggedObject {
    construct_info: ConstructInfoComplete,
    tag: Cow<'static, str>,
    variables: Vec<Arc<Variable>>,
}

impl TaggedObject {
    pub fn new(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::TaggedObject),
            tag: tag.into(),
            variables: variables.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, tag, variables))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Value {
        Value::TaggedObject(
            Self::new_arc(construct_info, construct_context, tag, variables),
            ValueMetadata::new(idempotency_key),
        )
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> ActorHandle {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: tagged_object_description,
        } = construct_info;

        // Create the wrapped TaggedObject construct_info
        let tagged_object_construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped TaggedObject"),
            persistence.clone(),
            tagged_object_description,
        );
        // Create the initial value first
        let initial_value = Self::new_value(
            tagged_object_construct_info,
            construct_context,
            idempotency_key,
            tag.into(),
            variables.into(),
        );

        // Use a constant actor so the value is immediately available
        // This is critical for WASM single-threaded runtime where spawned tasks
        // don't run until we yield - subscriptions need immediate access to values.
        let scope_id = actor_context.scope_id();
        create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id)
    }

    /// Look up a variable by name.
    /// Uses rposition (last match) so that explicit fields override spread fields.
    pub fn variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .iter()
            .rposition(|variable| variable.name == name)
            .map(|index| self.variables[index].clone())
    }

    pub fn expect_variable(&self, name: &str) -> Arc<Variable> {
        self.variable(name).unwrap_or_else(|| {
            panic!(
                "Failed to get expected Variable '{name}' from {}",
                self.construct_info
            )
        })
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn variables(&self) -> &[Arc<Variable>] {
        &self.variables
    }
}

// --- Text ---

pub struct Text {
    construct_info: ConstructInfoComplete,
    text: Cow<'static, str>,
}

impl Text {
    pub fn new(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        text: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Text),
            text: text.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        text: impl Into<Cow<'static, str>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, text))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        text: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Text(
            Self::new_arc(construct_info, construct_context, text),
            ValueMetadata::new(idempotency_key),
        )
    }

    /// Create a Value with a pre-captured emission sequence from an external source.
    pub fn new_value_with_emission_seq(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        emission_seq: EmissionSeq,
        text: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Text(
            Self::new_arc(construct_info, construct_context, text),
            ValueMetadata::with_emission_seq(idempotency_key, emission_seq),
        )
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        text: impl Into<Cow<'static, str>>,
    ) -> ActorHandle {
        let text: Cow<'static, str> = text.into();
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: text_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Text"),
            persistence,
            text_description,
        );
        // Create the initial value directly (avoid cloning ConstructInfo)
        let initial_value = Value::Text(
            Self::new_arc(construct_info, construct_context, text.clone()),
            ValueMetadata::new(idempotency_key),
        );
        // Use a constant actor so the initial value is immediately available.
        let scope_id = actor_context.scope_id();
        create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Create a Value using a pre-built ConstructInfoComplete.
    /// Avoids ConstructInfo::new() → complete() allocation chain.
    /// Used for hot paths in bridge.rs where the same event type is created repeatedly.
    pub fn new_value_cached(
        construct_info: ConstructInfoComplete,
        idempotency_key: ValueIdempotencyKey,
        text: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Text(
            Arc::new(Self {
                construct_info,
                text: text.into(),
            }),
            ValueMetadata::new(idempotency_key),
        )
    }

    /// Create a Value with cached info and a pre-captured emission sequence.
    pub fn new_value_cached_with_emission_seq(
        construct_info: ConstructInfoComplete,
        idempotency_key: ValueIdempotencyKey,
        emission_seq: EmissionSeq,
        text: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Text(
            Arc::new(Self {
                construct_info,
                text: text.into(),
            }),
            ValueMetadata::with_emission_seq(idempotency_key, emission_seq),
        )
    }
}

// --- Tag ---

pub struct Tag {
    construct_info: ConstructInfoComplete,
    tag: Cow<'static, str>,
}

impl Tag {
    pub fn new(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Tag),
            tag: tag.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, tag))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Tag(
            Self::new_arc(construct_info, construct_context, tag),
            ValueMetadata::new(idempotency_key),
        )
    }

    /// Create a Value with a pre-captured emission sequence from an external source.
    pub fn new_value_with_emission_seq(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        emission_seq: EmissionSeq,
        tag: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Tag(
            Self::new_arc(construct_info, construct_context, tag),
            ValueMetadata::with_emission_seq(idempotency_key, emission_seq),
        )
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        tag: impl Into<Cow<'static, str>>,
    ) -> ActorHandle {
        let tag: Cow<'static, str> = tag.into();
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: tag_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Tag"),
            persistence,
            tag_description,
        );
        // Create the initial value directly (avoid cloning ConstructInfo)
        let initial_value = Value::Tag(
            Self::new_arc(construct_info, construct_context, tag.clone()),
            ValueMetadata::new(idempotency_key),
        );
        // Use a constant actor so the initial value is immediately available.
        let scope_id = actor_context.scope_id();
        create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id)
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Create a Value using a pre-built ConstructInfoComplete.
    /// Avoids ConstructInfo::new() → complete() allocation chain.
    /// Used for hot paths in bridge.rs where the same event type is created repeatedly.
    pub fn new_value_cached(
        construct_info: ConstructInfoComplete,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Tag(
            Arc::new(Self {
                construct_info,
                tag: tag.into(),
            }),
            ValueMetadata::new(idempotency_key),
        )
    }

    /// Create a Value with cached info and a pre-captured emission sequence.
    pub fn new_value_cached_with_emission_seq(
        construct_info: ConstructInfoComplete,
        idempotency_key: ValueIdempotencyKey,
        emission_seq: EmissionSeq,
        tag: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Tag(
            Arc::new(Self {
                construct_info,
                tag: tag.into(),
            }),
            ValueMetadata::with_emission_seq(idempotency_key, emission_seq),
        )
    }
}

// --- Number ---

pub struct Number {
    construct_info: ConstructInfoComplete,
    number: f64,
}

impl Number {
    pub fn new(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        number: impl Into<f64>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Number),
            number: number.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        number: impl Into<f64>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, number))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        number: impl Into<f64>,
    ) -> Value {
        Value::Number(
            Self::new_arc(construct_info, construct_context, number),
            ValueMetadata::new(idempotency_key),
        )
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        number: impl Into<f64>,
    ) -> ActorHandle {
        let number = number.into();
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: number_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Number"),
            persistence,
            number_description,
        );
        // Create the initial value directly (avoid cloning ConstructInfo)
        let initial_value = Value::Number(
            Self::new_arc(construct_info, construct_context, number),
            ValueMetadata::new(idempotency_key),
        );
        // Use a constant actor so the initial value is immediately available.
        let scope_id = actor_context.scope_id();
        create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id)
    }

    pub fn number(&self) -> f64 {
        self.number
    }
}

// --- List ---

#[derive(Clone)]
struct ListStoredState {
    inner: Rc<ListStoredStateInner>,
}

struct ListStoredStateInner {
    initialized: Cell<bool>,
    has_future_state_updates: Cell<bool>,
    diff_history: RefCell<DiffHistory>,
    initialization_waiters: RefCell<Vec<oneshot::Sender<()>>>,
    update_waiters: RefCell<Vec<oneshot::Sender<()>>>,
    change_subscribers: RefCell<SmallVec<[NamedChannel<ListChange>; 4]>>,
    pending_change_subscribers: RefCell<SmallVec<[NamedChannel<ListChange>; 2]>>,
}

impl ListStoredState {
    fn new(config: DiffHistoryConfig) -> Self {
        Self {
            inner: Rc::new(ListStoredStateInner {
                initialized: Cell::new(false),
                has_future_state_updates: Cell::new(false),
                diff_history: RefCell::new(DiffHistory::new(config)),
                initialization_waiters: RefCell::new(Vec::new()),
                update_waiters: RefCell::new(Vec::new()),
                change_subscribers: RefCell::new(SmallVec::new()),
                pending_change_subscribers: RefCell::new(SmallVec::new()),
            }),
        }
    }

    fn is_initialized(&self) -> bool {
        self.inner.initialized.get()
    }

    fn set_has_future_state_updates(&self, has_future_state_updates: bool) {
        self.inner
            .has_future_state_updates
            .set(has_future_state_updates);
    }

    fn has_future_state_updates(&self) -> bool {
        self.inner.has_future_state_updates.get()
    }

    fn mark_initialized(&self) {
        if self.inner.initialized.replace(true) {
            return;
        }
        for waiter in self.inner.initialization_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
    }

    fn wait_until_initialized(&self) -> Option<oneshot::Receiver<()>> {
        if self.is_initialized() {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.inner.initialization_waiters.borrow_mut().push(tx);
        Some(rx)
    }

    fn wait_for_update_after(&self, version: u64) -> Option<oneshot::Receiver<()>> {
        if self.version() > version {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.inner.update_waiters.borrow_mut().push(tx);
        Some(rx)
    }

    fn record_change(&self, change: &ListChange) -> u64 {
        let mut diff_history = self.inner.diff_history.borrow_mut();
        let diff = change.to_diff(diff_history.snapshot());
        diff_history.add(diff);
        let version = diff_history.version();
        drop(diff_history);
        for waiter in self.inner.update_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
        version
    }

    fn version(&self) -> u64 {
        self.inner.diff_history.borrow().version()
    }

    fn get_update_since(&self, subscriber_version: u64) -> Vec<Arc<ListDiff>> {
        self.inner
            .diff_history
            .borrow_mut()
            .get_update_since(subscriber_version)
    }

    fn snapshot(&self) -> Vec<(ItemId, ActorHandle)> {
        self.inner.diff_history.borrow().snapshot().to_vec()
    }

    fn register_change_subscriber(&self, change_sender: NamedChannel<ListChange>) {
        if !self.is_initialized() {
            self.inner
                .pending_change_subscribers
                .borrow_mut()
                .push(change_sender);
            return;
        }

        let snapshot_items = Arc::from(
            self.inner
                .diff_history
                .borrow()
                .snapshot()
                .iter()
                .map(|(_, actor)| actor.clone())
                .collect::<Vec<_>>(),
        );
        inc_metric!(REPLACE_PAYLOADS_SENT);
        let first_change_to_send = ListChange::Replace {
            items: snapshot_items,
        };

        match change_sender.try_send(first_change_to_send) {
            Ok(()) => self
                .inner
                .change_subscribers
                .borrow_mut()
                .push(change_sender),
            Err(error) if !error.is_disconnected() => {
                self.inner
                    .change_subscribers
                    .borrow_mut()
                    .push(change_sender);
            }
            Err(_) => {}
        }
    }

    fn activate_pending_change_subscribers(&self, items: Arc<[ActorHandle]>) {
        let mut pending_subscribers = self.inner.pending_change_subscribers.borrow_mut();
        let mut change_subscribers = self.inner.change_subscribers.borrow_mut();

        for pending_sender in pending_subscribers.drain(..) {
            inc_metric!(REPLACE_PAYLOADS_SENT);
            let first_change_to_send = ListChange::Replace {
                items: items.clone(),
            };
            match pending_sender.try_send(first_change_to_send) {
                Ok(()) => change_subscribers.push(pending_sender),
                Err(error) if !error.is_disconnected() => change_subscribers.push(pending_sender),
                Err(_) => {}
            }
        }
    }

    fn broadcast_change(&self, change: &ListChange) {
        self.inner
            .change_subscribers
            .borrow_mut()
            .retain(|change_sender| {
                if let ListChange::Replace {
                    items: replace_items,
                } = change
                {
                    let _ = replace_items.len();
                    inc_metric!(REPLACE_FANOUT_SENDS);
                    inc_metric!(REPLACE_PAYLOAD_TOTAL_ITEMS, replace_items.len() as u64);
                }
                match change_sender.try_send(change.clone()) {
                    Ok(()) => true,
                    Err(error) => !error.is_disconnected(),
                }
            });
    }

    fn broadcast_snapshot(&self, items: Arc<[ActorHandle]>) {
        self.inner
            .change_subscribers
            .borrow_mut()
            .retain(|change_sender| {
                inc_metric!(REPLACE_FANOUT_SENDS);
                let change_to_send = ListChange::Replace {
                    items: items.clone(),
                };
                match change_sender.try_send(change_to_send) {
                    Ok(()) => true,
                    Err(error) => !error.is_disconnected(),
                }
            });
    }
}

pub struct List {
    construct_info: ConstructInfoComplete,
    /// Direct list-owned diff/snapshot state for current reads.
    stored_state: ListStoredState,
    /// Retained runtime tasks owned by the list for its full lifetime.
    retained_tasks: RefCell<SmallVec<[TaskHandle; 2]>>,
}

impl List {
    fn new_static(construct_info: ConstructInfoComplete, items: Vec<ActorHandle>) -> Self {
        let stored_state = ListStoredState::new(DiffHistoryConfig::default());
        let items: Arc<[ActorHandle]> = Arc::from(items);
        stored_state.record_change(&ListChange::Replace {
            items: items.clone(),
        });
        stored_state.mark_initialized();

        Self {
            construct_info,
            stored_state,
            retained_tasks: RefCell::new(SmallVec::new()),
        }
    }

    fn new_static_with_optional_output_valve_stream(
        construct_info: ConstructInfoComplete,
        items: Vec<ActorHandle>,
        output_valve_impulse_stream: Option<LocalBoxStream<'static, ()>>,
    ) -> Self {
        let list = Self::new_static(construct_info, items);
        if let Some(output_valve_impulse_stream) = output_valve_impulse_stream {
            let stored_state = list.stored_state.clone();
            let construct_info = list.construct_info.clone();
            let initial_items: Vec<_> = stored_state
                .snapshot()
                .into_iter()
                .map(|(_, actor)| actor)
                .collect();
            let initial_items_arc = Arc::from(initial_items.as_slice());
            list.start_retained_task(async move {
                run_live_list_change_loop_core(
                    construct_info,
                    stored_state,
                    stream::empty::<ListChange>(),
                    Some(output_valve_impulse_stream),
                    Some(initial_items),
                    Some(initial_items_arc),
                    (),
                )
                .await;
            });
        }
        list
    }

    pub fn new(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        actor_context: ActorContext,
        items: impl Into<Vec<ActorHandle>>,
    ) -> Self {
        let construct_info = construct_info.complete(ConstructType::List);
        let items = items.into();
        let output_valve_impulse_stream = actor_context
            .output_valve_signal
            .map(|output_valve_signal| output_valve_signal.stream().boxed_local());
        Self::new_static_with_optional_output_valve_stream(
            construct_info,
            items,
            output_valve_impulse_stream,
        )
    }

    pub fn new_with_change_stream<EOD: 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        change_stream: impl Stream<Item = ListChange> + 'static,
        extra_owned_data: EOD,
    ) -> Self {
        let construct_info = construct_info.complete(ConstructType::List);
        Self::new_with_change_stream_complete(
            construct_info,
            actor_context,
            change_stream,
            extra_owned_data,
        )
    }

    fn new_with_change_stream_complete<EOD: 'static>(
        construct_info: ConstructInfoComplete,
        actor_context: ActorContext,
        change_stream: impl Stream<Item = ListChange> + 'static,
        extra_owned_data: EOD,
    ) -> Self {
        // Direct list-owned state for versions, snapshots, and diff history.
        let stored_state = ListStoredState::new(DiffHistoryConfig::default());
        let list = Self {
            construct_info,
            stored_state,
            retained_tasks: RefCell::new(SmallVec::new()),
        };

        let construct_info = list.construct_info.clone();
        let output_valve_signal = actor_context.output_valve_signal.clone();
        let mut change_stream = change_stream.boxed_local();
        let (ready_drain_status, initial_items) = drain_ready_list_change_prefix_now(
            &construct_info,
            &list.stored_state,
            &mut change_stream,
        );
        list.stored_state
            .set_has_future_state_updates(has_future_state_updates_after_ready_drain(
                ready_drain_status,
            ));

        match initial_items {
            Some(initial_items) => match ready_drain_status {
                ReadyListChangeDrainStatus::Ended => {
                    if let Some(output_valve_impulse_stream) = output_valve_signal
                        .map(|output_valve_signal| output_valve_signal.stream().boxed_local())
                    {
                        let stored_state = list.stored_state.clone();
                        let initial_items_for_task = initial_items.clone();
                        list.start_retained_task(async move {
                            run_live_list_change_loop_core(
                                construct_info,
                                stored_state,
                                stream::empty::<ListChange>(),
                                Some(output_valve_impulse_stream),
                                Some(initial_items_for_task.to_vec()),
                                Some(initial_items),
                                extra_owned_data,
                            )
                            .await;
                        });
                    }
                }
                ReadyListChangeDrainStatus::Pending => {
                    let stored_state = list.stored_state.clone();
                    let output_valve_impulse_stream = output_valve_signal
                        .map(|output_valve_signal| output_valve_signal.stream().boxed_local());
                    list.start_retained_task(async move {
                        run_live_list_change_loop_core(
                            construct_info,
                            stored_state,
                            change_stream,
                            output_valve_impulse_stream,
                            Some(initial_items.to_vec()),
                            Some(initial_items),
                            extra_owned_data,
                        )
                        .await;
                    });
                }
            },
            None => {
                let stored_state = list.stored_state.clone();
                let output_valve_impulse_stream = output_valve_signal
                    .map(|output_valve_signal| output_valve_signal.stream().boxed_local());
                list.start_retained_task(async move {
                    run_live_list_change_loop_core(
                        construct_info,
                        stored_state,
                        change_stream,
                        output_valve_impulse_stream,
                        None,
                        None,
                        extra_owned_data,
                    )
                    .await;
                });
            }
        }

        list
    }

    /// Retain a runtime task for the list's lifetime.
    fn retain_task(&self, task: TaskHandle) {
        self.retained_tasks.borrow_mut().push(task);
    }

    fn start_retained_task<F>(&self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.retain_task(Task::start_droppable(future));
    }

    /// Get current version.
    fn version(&self) -> u64 {
        self.stored_state.version()
    }

    /// Get optimal update for subscriber at given version.
    /// Returns diffs if subscriber is close, snapshot if too far behind.
    fn get_update_since(&self, subscriber_version: u64) -> Vec<Arc<ListDiff>> {
        self.stored_state.get_update_since(subscriber_version)
    }

    /// Get current snapshot of items with their stable IDs (async).
    pub async fn snapshot(&self) -> Vec<(ItemId, ActorHandle)> {
        if let Some(waiter) = self.stored_state.wait_until_initialized() {
            let _ = waiter.await;
        }
        self.stored_state.snapshot()
    }

    pub(crate) fn snapshot_now(&self) -> Option<Vec<(ItemId, ActorHandle)>> {
        self.stored_state
            .is_initialized()
            .then(|| self.stored_state.snapshot())
    }

    /// Wait for the next optimal update after `last_seen_version`.
    ///
    /// Returns either incremental diffs or a snapshot-style replacement diff.
    async fn next_update_after(&self, last_seen_version: &mut u64) -> Option<Vec<Arc<ListDiff>>> {
        loop {
            let current = self.version();
            if current > *last_seen_version {
                break;
            }
            if let Some(waiter) = self.stored_state.wait_for_update_after(*last_seen_version) {
                if waiter.await.is_err() {
                    return None;
                }
            }
        }

        let update = self.get_update_since(*last_seen_version);
        *last_seen_version = self.version();
        Some(update)
    }

    /// Stream list updates with diff support.
    ///
    /// Takes ownership of the Arc to keep the list alive for the subscription lifetime.
    /// Callers should use `.clone().diff_stream(...)` if they need to retain a reference.
    fn diff_stream(
        self: Arc<Self>,
        last_seen_version: u64,
    ) -> LocalBoxStream<'static, Vec<Arc<ListDiff>>> {
        stream::unfold(
            (self, last_seen_version),
            |(list, mut last_seen_version)| async move {
                let update = list.next_update_after(&mut last_seen_version).await?;
                Some((update, (list, last_seen_version)))
            },
        )
        .boxed_local()
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        items: impl Into<Vec<ActorHandle>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info,
            construct_context,
            actor_context,
            items,
        ))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<ActorHandle>>,
    ) -> Value {
        Value::List(
            Self::new_arc(construct_info, construct_context, actor_context, items),
            ValueMetadata::new(idempotency_key),
        )
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: list_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped List"),
            persistence,
            list_description,
        );
        let initial_value = Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            actor_context.clone(),
            items.into(),
        );
        let scope_id = actor_context.scope_id();
        create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id)
    }

    /// Subscribe to this list's changes.
    ///
    /// The returned stream keeps the list alive for the
    /// duration of the subscription. When the stream is dropped, the
    /// list reference is released.
    ///
    /// Takes ownership of the Arc to keep the list alive for the subscription lifetime.
    /// Callers should use `.clone().stream()` if they need to retain a reference.
    pub fn stream(self: Arc<Self>) -> LocalBoxStream<'static, ListChange> {
        // ListChange events use bounded channel with backpressure - keeps senders that are just full
        let (change_sender, change_receiver) =
            NamedChannel::new("list.changes", LIST_CHANGE_CAPACITY);
        self.stored_state.register_change_subscriber(change_sender);
        stream::unfold((change_receiver, self), |(mut receiver, list)| async move {
            receiver
                .next()
                .await
                .map(|change| (change, (receiver, list)))
        })
        .boxed_local()
    }

    /// Creates a wrapper List with persistence support.
    /// Used by List/append and List/clear to persist their final state.
    ///
    /// Unlike new_arc_value_actor_with_persistence (for LIST {}), this:
    /// - Receives changes from a source list via change_stream
    /// - If saved state exists: uses saved items as initial state, skips first source Replace
    /// - If no saved state: forwards all source changes normally
    /// - On any change: saves the current list state
    pub fn new_with_change_stream_and_persistence<EOD: 'static>(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        change_stream: impl Stream<Item = ListChange> + 'static,
        extra_owned_data: EOD,
        persistence_id: parser::PersistenceId,
    ) -> Self {
        let construct_storage = construct_context.construct_storage.clone();
        if construct_storage.is_disabled() {
            return Self::new_with_change_stream(
                construct_info,
                actor_context,
                change_stream,
                extra_owned_data,
            );
        }

        let construct_context_for_stream = construct_context.clone();
        let actor_context_for_stream = actor_context.clone();
        let construct_id = construct_info.id.clone();

        // State for the persistence stream
        enum PersistState {
            /// First iteration - need to check for saved state
            Init {
                storage: Arc<ConstructStorage>,
                pid: parser::PersistenceId,
                change_stream: std::pin::Pin<Box<dyn Stream<Item = ListChange>>>,
                ctx: ConstructContext,
                actor_ctx: ActorContext,
                cid: ConstructId,
            },
            /// Running normally - forward and save changes
            Running {
                pid: parser::PersistenceId,
                change_stream: std::pin::Pin<Box<dyn Stream<Item = ListChange>>>,
                current_items: Vec<ActorHandle>,
                skip_initial_source_replace: bool,
            },
        }

        let persistent_change_stream = stream::unfold(
            PersistState::Init {
                storage: construct_storage.clone(),
                pid: persistence_id,
                change_stream: Box::pin(change_stream),
                ctx: construct_context_for_stream,
                actor_ctx: actor_context_for_stream,
                cid: construct_id,
            },
            move |state| {
                let storage_for_save = construct_storage.clone();
                async move {
                    match state {
                        PersistState::Init {
                            storage,
                            pid,
                            mut change_stream,
                            ctx,
                            actor_ctx,
                            cid,
                        } => {
                            // Check for saved state
                            let loaded_items: Option<Vec<serde_json::Value>> =
                                storage.clone().load_state(pid).await;

                            // Only restore simple values from JSON - complex Objects with nested
                            // fields (like TodoMVC items) don't survive JSON restoration because
                            // they lose their Variable structure and reactive connections.
                            // For complex items, use recorded-call restoration via List/append instead.
                            let should_restore = loaded_items.as_ref().map_or(false, |items| {
                                items.iter().all(|json| {
                                    // Only restore if ALL items are simple (not nested Objects)
                                    match json {
                                        serde_json::Value::Object(obj) => {
                                            // Allow Tagged values (have "$Tag" field and one other field)
                                            // but NOT complex objects with multiple fields or nested objects
                                            if obj.contains_key("$Tag") && obj.len() <= 2 {
                                                true
                                            } else if obj.len() > 1 {
                                                // Complex object - skip restoration
                                                if LOG_DEBUG { zoon::println!("[DEBUG] Skipping JSON restoration: complex object with {} fields", obj.len()); }
                                                false
                                            } else {
                                                true
                                            }
                                        }
                                        _ => true, // Numbers, strings, etc. are fine
                                    }
                                })
                            });

                            if should_restore {
                                if let Some(json_items) = loaded_items {
                                    // Restore from saved state (simple values only)
                                    let items: Vec<ActorHandle> = json_items
                                        .iter()
                                        .enumerate()
                                        .map(|(i, json)| {
                                            value_actor_from_json(
                                                json,
                                                cid.with_child_id(format!("restored_item_{i}")),
                                                ctx.clone(),
                                                parser::PersistenceId::new(),
                                                actor_ctx.clone(),
                                            )
                                        })
                                        .collect();

                                    // C4: Build Arc first, then derive Vec from it to avoid double allocation
                                    let items_arc: Arc<[ActorHandle]> = Arc::from(items);
                                    let current_items = items_arc.to_vec();
                                    let restored_change = ListChange::Replace { items: items_arc };
                                    return Some((
                                        restored_change,
                                        PersistState::Running {
                                            pid,
                                            change_stream,
                                            current_items,
                                            skip_initial_source_replace: true,
                                        },
                                    ));
                                }
                            } else if loaded_items.is_some() && LOG_DEBUG {
                                zoon::println!(
                                    "[DEBUG] Skipping JSON restoration for list with complex objects - use recorded-call restoration instead"
                                );
                            }

                            // No saved state OR skipped restoration - forward first change from source
                            if let Some(change) = change_stream.next().await {
                                let mut items = Vec::new();
                                apply_change_and_persist_items(
                                    &change,
                                    &mut items,
                                    storage_for_save.as_ref(),
                                    pid,
                                )
                                .await;

                                Some((
                                    change,
                                    PersistState::Running {
                                        pid,
                                        change_stream,
                                        current_items: items,
                                        skip_initial_source_replace: false,
                                    },
                                ))
                            } else {
                                None
                            }
                        }
                        PersistState::Running {
                            pid,
                            mut change_stream,
                            mut current_items,
                            mut skip_initial_source_replace,
                        } => loop {
                            let Some(change) = change_stream.next().await else {
                                return None;
                            };

                            if skip_initial_source_replace {
                                skip_initial_source_replace = false;
                                if matches!(&change, ListChange::Replace { .. }) {
                                    continue;
                                }
                            }

                            apply_change_and_persist_items(
                                &change,
                                &mut current_items,
                                storage_for_save.as_ref(),
                                pid,
                            )
                            .await;

                            return Some((
                                change,
                                PersistState::Running {
                                    pid,
                                    change_stream,
                                    current_items,
                                    skip_initial_source_replace: false,
                                },
                            ));
                        },
                    }
                }
            },
        );

        // Create the list with the persistent change stream
        Self::new_with_change_stream(
            construct_info,
            actor_context,
            persistent_change_stream,
            extra_owned_data,
        )
    }

    fn new_persistent_list_value(
        actor_id: ConstructId,
        persistence_data: parser::Persistence,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: Vec<ActorHandle>,
    ) -> Value {
        let inner_construct_info = ConstructInfo::new(
            actor_id.with_child_id("persistent_list"),
            Some(persistence_data),
            "Persistent List",
        )
        .complete(ConstructType::List);
        let output_valve_impulse_stream = actor_context
            .output_valve_signal
            .map(|output_valve_signal| output_valve_signal.stream().boxed_local());
        let list = Arc::new(List::new_static_with_optional_output_valve_stream(
            inner_construct_info,
            items,
            output_valve_impulse_stream,
        ));
        Value::List(list, ValueMetadata::new(idempotency_key))
    }

    /// Creates a List with persistence support.
    /// - If saved data exists, it's loaded and used as initial items (code items are ignored)
    /// - On any change, the current list state is saved to storage
    pub fn new_arc_value_actor_with_persistence(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        code_items: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        let code_items = code_items.into();
        let persistence = construct_info.persistence;

        // If no persistence, just use the regular constructor
        let Some(persistence_data) = persistence else {
            return Self::new_arc_value_actor(
                construct_info,
                construct_context,
                idempotency_key,
                actor_context,
                code_items,
            );
        };

        let persistence_id = persistence_data.id;
        let construct_storage = construct_context.construct_storage.clone();
        if construct_storage.is_disabled() {
            let ConstructInfo { id: actor_id, .. } = construct_info;
            let value = Self::new_persistent_list_value(
                actor_id,
                persistence_data,
                idempotency_key,
                actor_context.clone(),
                code_items,
            );
            return create_constant_actor(
                parser::PersistenceId::new(),
                value,
                actor_context.scope_id(),
            );
        }

        if let Some(json_items) =
            construct_storage.load_state_now::<Vec<serde_json::Value>>(persistence_id)
        {
            let ConstructInfo { id: actor_id, .. } = construct_info;
            let code_items_len = code_items.len();
            let json_items_len = json_items.len();
            let max_len = code_items_len.max(json_items_len);

            let initial_items = (0..max_len)
                .map(|i| {
                    if i < code_items_len {
                        code_items[i].clone()
                    } else {
                        value_actor_from_json(
                            &json_items[i],
                            actor_id.with_child_id(format!("loaded_item_{i}")),
                            construct_context.clone(),
                            parser::PersistenceId::new(),
                            actor_context.clone(),
                        )
                    }
                })
                .collect();

            let value = Self::new_persistent_list_value(
                actor_id,
                persistence_data,
                idempotency_key,
                actor_context.clone(),
                initial_items,
            );
            return create_constant_actor(
                parser::PersistenceId::new(),
                value,
                actor_context.scope_id(),
            );
        }

        let ConstructInfo {
            id: actor_id,
            persistence: _,
            description: _list_description,
        } = construct_info;

        // Create a one-shot initialization future that:
        // 1. Loads initial items from storage (or uses code items if nothing is saved)
        // 2. Creates the inner list and installs its persistence sidecar task
        // 3. Emits the wrapped list value once and completes
        let construct_context_for_load = construct_context.clone();
        let actor_context_for_load = actor_context.clone();
        let actor_id_for_load = actor_id.clone();
        let value_future = async move {
            // Try to load from storage (clone the Arc since load_state takes ownership)
            let loaded_items: Option<Vec<serde_json::Value>> =
                construct_storage.clone().load_state(persistence_id).await;

            let initial_items = if let Some(json_items) = loaded_items {
                // Merge code_items with loaded_items:
                // - Use code_items for their reactivity
                // - If storage has MORE items than code, load extras from JSON
                // - If code has MORE items than storage, use the code items
                let code_items_len = code_items.len();
                let json_items_len = json_items.len();
                let max_len = code_items_len.max(json_items_len);

                (0..max_len)
                    .map(|i| {
                        if i < code_items_len {
                            // Use code item for reactivity
                            code_items[i].clone()
                        } else {
                            // Load extra items from JSON (beyond code_items)
                            value_actor_from_json(
                                &json_items[i],
                                actor_id_for_load.with_child_id(format!("loaded_item_{i}")),
                                construct_context_for_load.clone(),
                                parser::PersistenceId::new(),
                                actor_context_for_load.clone(),
                            )
                        }
                    })
                    .collect()
            } else {
                // Use code-defined items
                code_items
            };

            let Value::List(list_arc, _) = Self::new_persistent_list_value(
                actor_id_for_load,
                persistence_data,
                idempotency_key,
                actor_context_for_load.clone(),
                initial_items,
            ) else {
                unreachable!("persistent list helper must return a list value");
            };

            save_or_watch_persistent_list(list_arc.clone(), construct_storage, persistence_id)
                .await;

            Value::List(list_arc, ValueMetadata::new(idempotency_key))
        };

        let scope_id = actor_context.scope_id();
        create_actor_from_future(value_future, parser::PersistenceId::new(), scope_id)
    }
}

fn apply_list_change_to_state(
    construct_info: &ConstructInfoComplete,
    stored_state: &ListStoredState,
    list: &mut Option<Vec<ActorHandle>>,
    list_arc_cache: &mut Option<Arc<[ActorHandle]>>,
    change: &ListChange,
) {
    stored_state.record_change(change);

    if let Some(list) = list {
        change.clone().apply_to_vec(list);
        *list_arc_cache = None;
    } else if let ListChange::Replace { items } = change {
        *list = Some(items.to_vec());
        *list_arc_cache = Some(items.clone());
        stored_state.mark_initialized();
        stored_state.activate_pending_change_subscribers(items.clone());
    } else {
        panic!(
            "Failed to initialize {construct_info}: The first change has to be 'ListChange::Replace'"
        )
    }
}

fn process_live_list_change(
    construct_info: &ConstructInfoComplete,
    stored_state: &ListStoredState,
    list: &mut Option<Vec<ActorHandle>>,
    list_arc_cache: &mut Option<Arc<[ActorHandle]>>,
    change: &ListChange,
    broadcast_change: bool,
) {
    if broadcast_change {
        stored_state.broadcast_change(change);
    }
    apply_list_change_to_state(construct_info, stored_state, list, list_arc_cache, change);
}

#[derive(Clone, Copy)]
enum ReadyListChangeDrainStatus {
    Ended,
    Pending,
}

fn has_future_state_updates_after_ready_drain(status: ReadyListChangeDrainStatus) -> bool {
    matches!(status, ReadyListChangeDrainStatus::Pending)
}

fn drain_ready_list_change_prefix_now(
    construct_info: &ConstructInfoComplete,
    stored_state: &ListStoredState,
    change_stream: &mut LocalBoxStream<'static, ListChange>,
) -> (ReadyListChangeDrainStatus, Option<Arc<[ActorHandle]>>) {
    let mut list = None;
    let mut list_arc_cache = None;

    let finalized_items = |list: &Option<Vec<ActorHandle>>,
                           list_arc_cache: &Option<Arc<[ActorHandle]>>| {
        list_arc_cache
            .clone()
            .or_else(|| list.as_ref().map(|list| Arc::from(list.as_slice())))
    };

    loop {
        match poll_stream_once(change_stream.as_mut()) {
            Poll::Ready(Some(change)) => {
                process_live_list_change(
                    construct_info,
                    stored_state,
                    &mut list,
                    &mut list_arc_cache,
                    &change,
                    false,
                );
            }
            Poll::Ready(None) => {
                return (
                    ReadyListChangeDrainStatus::Ended,
                    finalized_items(&list, &list_arc_cache),
                );
            }
            Poll::Pending => {
                return (
                    ReadyListChangeDrainStatus::Pending,
                    finalized_items(&list, &list_arc_cache),
                );
            }
        }
    }
}

fn broadcast_list_snapshot_if_ready(
    stored_state: &ListStoredState,
    list: &Option<Vec<ActorHandle>>,
    list_arc_cache: &mut Option<Arc<[ActorHandle]>>,
    last_broadcast_version: &mut u64,
) {
    let current_version = stored_state.version();
    if current_version <= *last_broadcast_version {
        return;
    }

    if let Some(list) = list.as_ref() {
        let items_arc = list_arc_cache
            .get_or_insert_with(|| Arc::from(list.as_slice()))
            .clone();
        stored_state.broadcast_snapshot(items_arc);
        *last_broadcast_version = current_version;
    }
}

async fn run_live_list_change_loop_core<S, EOD>(
    construct_info: ConstructInfoComplete,
    stored_state: ListStoredState,
    change_stream: S,
    output_valve_impulse_stream: Option<LocalBoxStream<'static, ()>>,
    initial_list: Option<Vec<ActorHandle>>,
    initial_list_arc_cache: Option<Arc<[ActorHandle]>>,
    extra_owned_data: EOD,
) where
    S: Stream<Item = ListChange> + 'static,
    EOD: 'static,
{
    let mut change_stream = change_stream.boxed_local().fuse();
    let mut list = initial_list;
    let mut list_arc_cache = initial_list_arc_cache;

    if let Some(output_valve_impulse_stream) = output_valve_impulse_stream {
        let mut output_valve_impulse_stream = output_valve_impulse_stream.fuse();
        let mut last_broadcast_version = 0;

        loop {
            select! {
                change = change_stream.next() => {
                    let Some(change) = change else {
                        break;
                    };
                    process_live_list_change(
                        &construct_info,
                        &stored_state,
                        &mut list,
                        &mut list_arc_cache,
                        &change,
                        false,
                    );
                }
                impulse = output_valve_impulse_stream.next() => {
                    let Some(()) = impulse else {
                        while let Some(change) = change_stream.next().await {
                            process_live_list_change(
                                &construct_info,
                                &stored_state,
                                &mut list,
                                &mut list_arc_cache,
                                &change,
                                true,
                            );
                        }
                        drop(extra_owned_data);
                        return;
                    };
                    broadcast_list_snapshot_if_ready(
                        &stored_state,
                        &list,
                        &mut list_arc_cache,
                        &mut last_broadcast_version,
                    );
                }
            }
        }

        while let Some(()) = output_valve_impulse_stream.next().await {
            broadcast_list_snapshot_if_ready(
                &stored_state,
                &list,
                &mut list_arc_cache,
                &mut last_broadcast_version,
            );
        }
        broadcast_list_snapshot_if_ready(
            &stored_state,
            &list,
            &mut list_arc_cache,
            &mut last_broadcast_version,
        );
    } else {
        while let Some(change) = change_stream.next().await {
            process_live_list_change(
                &construct_info,
                &stored_state,
                &mut list,
                &mut list_arc_cache,
                &change,
                true,
            );
        }
    }

    drop(extra_owned_data);
}

// --- ItemId ---

/// Stable identifier for list items that survives structural changes.
/// Unlike indices which shift on insert/remove, ItemId stays constant.
/// This enables O(1) diff translation through filter chains.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ItemId(pub Ulid);

impl ItemId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

// --- ListDiff ---

/// Diff operations using stable ItemId references.
/// Unlike ListChange which uses indices, ListDiff enables O(1) filter translation.
#[derive(Clone)]
pub enum ListDiff {
    /// Insert new item after the given position (None = at start)
    Insert {
        id: ItemId,
        after: Option<ItemId>,
        value: ActorHandle,
    },
    /// Remove item by its stable ID
    Remove { id: ItemId },
    /// Update item's value (ID stays the same)
    Update { id: ItemId, value: ActorHandle },
    /// Full replacement (when diffs would be larger than snapshot)
    Replace { items: Vec<(ItemId, ActorHandle)> },
}

// --- DiffHistory ---

/// Default maximum number of diffs to keep in history ring buffer.
const DIFF_HISTORY_MAX_ENTRIES: usize = 1500;
/// Default snapshot threshold: prefer snapshot over diffs if catching up
/// requires more than this fraction of the current list length.
const DIFF_HISTORY_SNAPSHOT_THRESHOLD: f64 = 0.5;

/// Configuration for diff history ring buffer.
struct DiffHistoryConfig {
    /// Maximum number of diffs to keep before oldest are dropped
    max_entries: usize,
    /// When to prefer snapshot over diffs (if gap > threshold * current_len)
    snapshot_threshold: f64,
}

impl Default for DiffHistoryConfig {
    fn default() -> Self {
        Self {
            max_entries: DIFF_HISTORY_MAX_ENTRIES,
            snapshot_threshold: DIFF_HISTORY_SNAPSHOT_THRESHOLD,
        }
    }
}

/// Ring buffer storing recent diffs for efficient subscriber updates.
/// Subscribers close to current version get diffs; those far behind get snapshots.
struct DiffHistory {
    /// Recent diffs with their version numbers
    diffs: VecDeque<(u64, Arc<ListDiff>)>,
    /// Current items with their stable IDs
    current_snapshot: Vec<(ItemId, ActorHandle)>,
    /// Oldest version still in history (versions before this need snapshot)
    oldest_version: u64,
    /// Current version (incremented on each change)
    current_version: u64,
    /// Configuration
    config: DiffHistoryConfig,
}

impl DiffHistory {
    fn new(config: DiffHistoryConfig) -> Self {
        Self {
            diffs: VecDeque::new(),
            current_snapshot: Vec::new(),
            oldest_version: 0,
            current_version: 0,
            config,
        }
    }

    /// Add a new diff and update snapshot.
    pub fn add(&mut self, diff: ListDiff) {
        self.current_version += 1;

        // Apply diff to snapshot
        match &diff {
            ListDiff::Insert { id, after, value } => {
                let pos = match after {
                    None => 0,
                    Some(after_id) => self
                        .current_snapshot
                        .iter()
                        .position(|(id, _)| id == after_id)
                        .map(|i| i + 1)
                        .unwrap_or(self.current_snapshot.len()),
                };
                let insert_pos = pos.min(self.current_snapshot.len());
                if insert_pos != pos {
                    zoon::eprintln!(
                        "[actors-diffhistory-clamp] insert pos={} len={}",
                        pos,
                        self.current_snapshot.len()
                    );
                }
                self.current_snapshot
                    .insert(insert_pos, (*id, value.clone()));
            }
            ListDiff::Remove { id } => {
                self.current_snapshot.retain(|(item_id, _)| item_id != id);
            }
            ListDiff::Update { id, value } => {
                if let Some((_, v)) = self
                    .current_snapshot
                    .iter_mut()
                    .find(|(item_id, _)| item_id == id)
                {
                    *v = value.clone();
                }
            }
            ListDiff::Replace { items } => {
                self.current_snapshot = items.clone();
            }
        }

        // Store diff
        self.diffs.push_back((self.current_version, Arc::new(diff)));

        while self.diffs.len() > self.config.max_entries {
            if let Some((version, _)) = self.diffs.pop_front() {
                self.oldest_version = version;
            }
        }
    }

    /// Get optimal update for subscriber at given version.
    fn get_update_since(&mut self, subscriber_version: u64) -> Vec<Arc<ListDiff>> {
        if subscriber_version >= self.current_version {
            return Vec::new();
        }

        // If subscriber is too far behind or before our history, send snapshot
        if subscriber_version < self.oldest_version {
            return self.snapshot_update();
        }

        // Calculate how many diffs needed
        let diffs_needed: Vec<_> = self
            .diffs
            .iter()
            .filter(|(v, _)| *v > subscriber_version)
            .map(|(_, d)| d.clone())
            .collect();

        // Heuristic: if catching up requires more than threshold% of list, prefer snapshot
        let list_len = self.current_snapshot.len().max(1);
        let diff_cost = diffs_needed.len();
        if diff_cost as f64 > list_len as f64 * self.config.snapshot_threshold {
            return self.snapshot_update();
        }

        // Return diffs if we have any, otherwise current
        if diffs_needed.is_empty() {
            Vec::new()
        } else {
            diffs_needed
        }
    }

    fn snapshot_update(&self) -> Vec<Arc<ListDiff>> {
        // Return a Replace diff containing the full snapshot
        let items: Vec<_> = self
            .current_snapshot
            .iter()
            .map(|(id, actor)| (*id, actor.clone()))
            .collect();
        vec![Arc::new(ListDiff::Replace { items })]
    }

    /// Get current version.
    pub fn version(&self) -> u64 {
        self.current_version
    }

    /// Get current snapshot.
    pub fn snapshot(&self) -> &[(ItemId, ActorHandle)] {
        &self.current_snapshot
    }
}

// --- ListChange ---

#[derive(Clone)]
pub enum ListChange {
    Replace { items: Arc<[ActorHandle]> },
    InsertAt { index: usize, item: ActorHandle },
    UpdateAt { index: usize, item: ActorHandle },
    Remove { id: parser::PersistenceId },
    Move { old_index: usize, new_index: usize },
    Push { item: ActorHandle },
    Pop,
    Clear,
}

impl ListChange {
    pub fn apply_to_vec(self, vec: &mut Vec<ActorHandle>) {
        match self {
            Self::Replace { items } => {
                *vec = items.to_vec();
            }
            Self::InsertAt { index, item } => {
                if index <= vec.len() {
                    vec.insert(index, item);
                } else {
                    zoon::eprintln!(
                        "ListChange::InsertAt index {} out of bounds (len: {})",
                        index,
                        vec.len()
                    );
                }
            }
            Self::UpdateAt { index, item } => {
                if index < vec.len() {
                    vec[index] = item;
                } else {
                    zoon::eprintln!(
                        "ListChange::UpdateAt index {} out of bounds (len: {})",
                        index,
                        vec.len()
                    );
                }
            }
            Self::Push { item } => {
                vec.push(item);
            }
            Self::Remove { id } => {
                if let Some(pos) = vec.iter().position(|item| item.persistence_id() == id) {
                    vec.remove(pos);
                }
                // Silently ignore if item not found - it may have already been removed
            }
            Self::Move {
                old_index,
                new_index,
            } => {
                if old_index < vec.len() {
                    let item = vec.remove(old_index);
                    let insert_index = new_index.min(vec.len());
                    vec.insert(insert_index, item);
                } else {
                    zoon::eprintln!(
                        "ListChange::Move old_index {} out of bounds (len: {})",
                        old_index,
                        vec.len()
                    );
                }
            }
            Self::Pop => {
                if vec.pop().is_none() {
                    zoon::eprintln!("ListChange::Pop on empty vec");
                }
            }
            Self::Clear => {
                vec.clear();
            }
        }
    }

    /// Convert to ListDiff using current snapshot for index-to-ItemId translation.
    /// Returns the diff and a new ItemId for inserted items.
    pub fn to_diff(&self, snapshot: &[(ItemId, ActorHandle)]) -> ListDiff {
        match self {
            Self::Replace { items } => {
                // Assign new ItemIds to all items
                let items_with_ids: Vec<_> = items
                    .iter()
                    .map(|actor| (ItemId::new(), actor.clone()))
                    .collect();
                ListDiff::Replace {
                    items: items_with_ids,
                }
            }
            Self::InsertAt { index, item } => {
                let new_id = ItemId::new();
                let after = if *index == 0 {
                    None
                } else {
                    snapshot.get(index - 1).map(|(id, _)| *id)
                };
                ListDiff::Insert {
                    id: new_id,
                    after,
                    value: item.clone(),
                }
            }
            Self::UpdateAt { index, item } => {
                let id = snapshot
                    .get(*index)
                    .map(|(id, _)| *id)
                    .unwrap_or_else(ItemId::new);
                ListDiff::Update {
                    id,
                    value: item.clone(),
                }
            }
            Self::Push { item } => {
                let new_id = ItemId::new();
                let after = snapshot.last().map(|(id, _)| *id);
                ListDiff::Insert {
                    id: new_id,
                    after,
                    value: item.clone(),
                }
            }
            Self::Remove { id: persistence_id } => {
                // Find ItemId by matching PersistenceId in snapshot
                let item_id = snapshot
                    .iter()
                    .find(|(_, actor)| actor.persistence_id() == *persistence_id)
                    .map(|(id, _)| *id)
                    .unwrap_or_else(ItemId::new);
                ListDiff::Remove { id: item_id }
            }
            Self::Move {
                old_index,
                new_index,
            } => {
                // Move is Remove + Insert
                // For simplicity, we model it as Remove followed by Insert
                // The caller should handle this as two separate diffs if needed
                if let Some((id, value)) = snapshot.get(*old_index).map(|(id, v)| (*id, v.clone()))
                {
                    let after = if *new_index == 0 {
                        None
                    } else {
                        // Account for removal when calculating position
                        let adjusted_index = if *new_index > *old_index {
                            new_index - 1
                        } else {
                            *new_index - 1
                        };
                        snapshot.get(adjusted_index).map(|(id, _)| *id)
                    };
                    // Return as Insert with same ID (effectively a move)
                    ListDiff::Insert { id, after, value }
                } else {
                    zoon::eprintln!(
                        "ListChange::Move old_index {} out of bounds in to_diff (snapshot len: {})",
                        old_index,
                        snapshot.len()
                    );
                    // Return a no-op by replacing with current snapshot
                    ListDiff::Replace {
                        items: snapshot.iter().map(|(id, v)| (*id, v.clone())).collect(),
                    }
                }
            }
            Self::Pop => {
                let id = snapshot
                    .last()
                    .map(|(id, _)| *id)
                    .unwrap_or_else(ItemId::new);
                ListDiff::Remove { id }
            }
            Self::Clear => {
                // Clear is a Replace with empty list
                ListDiff::Replace { items: Vec::new() }
            }
        }
    }
}

// --- ListBindingFunction ---

use boon::parser::StrSlice;
use boon::parser::static_expression::{Expression as StaticExpression, Spanned as StaticSpanned};

/// Handles List binding functions (map, retain, every, any) that need to
/// evaluate an expression for each list item.
///
/// Uses StaticExpression which is 'static (via StrSlice into Arc<String> source)
/// and can be:
/// - Stored in async contexts without lifetime issues
/// - Sent to WebWorkers for parallel processing
/// - Cloned cheaply (just Arc increment + offset copy)
/// - Serialized for distributed evaluation
pub struct ListBindingFunction;

fn list_item_scope_id(index: usize, persistence_id: parser::PersistenceId) -> String {
    format!("list_item_{}_{}", index, persistence_id.as_u128())
}

/// Configuration for a list binding operation.
#[derive(Clone)]
pub struct ListBindingConfig {
    /// The variable name that will be bound to each list item
    pub binding_name: StrSlice,
    /// The expression to evaluate for each item (with binding_name in scope)
    pub transform_expr: StaticSpanned<StaticExpression>,
    /// The type of list operation
    pub operation: ListBindingOperation,
    /// Reference connector for looking up scope-resolved references
    pub reference_connector: Arc<ReferenceConnector>,
    /// Link connector for connecting LINK variables with their setters
    pub link_connector: Arc<LinkConnector>,
    /// Pass-through connector for stable LINK pass-throughs
    pub pass_through_connector: Arc<PassThroughConnector>,
    /// Source code for creating borrowed expressions
    pub source_code: SourceCode,
    /// Function registry snapshot for resolving user-defined functions
    pub function_registry_snapshot: Option<Arc<FunctionRegistry>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListBindingOperation {
    Map,
    Retain,
    Remove,
    Every,
    Any,
    SortBy,
}

/// A sortable key extracted from a Value for use in List/sort_by.
/// Supports comparison of Numbers, Text, and Tags.
/// Uses Cow<'static, str> to avoid allocations for static tag/text values.
#[derive(Clone, Debug)]
pub enum SortKey {
    Number(f64),
    Text(Cow<'static, str>),
    Tag(Cow<'static, str>),
    /// Fallback for unsupported types - sorts last
    Unsupported,
}

impl SortKey {
    /// Extract a sortable key from a Value
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Number(num, _) => SortKey::Number(num.number()),
            Value::Text(text, _) => {
                // Reuse Cow directly when the Text has a borrowed static str
                match &text.text {
                    Cow::Borrowed(s) => SortKey::Text(Cow::Borrowed(s)),
                    Cow::Owned(s) => SortKey::Text(Cow::Owned(s.clone())),
                }
            }
            Value::Tag(tag, _) => {
                // Reuse Cow directly when the Tag has a borrowed static str
                match &tag.tag {
                    Cow::Borrowed(s) => SortKey::Tag(Cow::Borrowed(s)),
                    Cow::Owned(s) => SortKey::Tag(Cow::Owned(s.clone())),
                }
            }
            Value::Flushed(inner, _) => SortKey::from_value(inner),
            _ => SortKey::Unsupported,
        }
    }
}

impl PartialEq for SortKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (SortKey::Number(a), SortKey::Number(b)) => {
                // Handle NaN properly
                if a.is_nan() && b.is_nan() {
                    true
                } else {
                    a == b
                }
            }
            (SortKey::Text(a), SortKey::Text(b)) => a == b,
            (SortKey::Tag(a), SortKey::Tag(b)) => a == b,
            (SortKey::Unsupported, SortKey::Unsupported) => true,
            _ => false,
        }
    }
}

impl Eq for SortKey {}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (SortKey::Number(a), SortKey::Number(b)) => {
                // Handle NaN: NaN sorts last
                match (a.is_nan(), b.is_nan()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
                }
            }
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),
            (SortKey::Tag(a), SortKey::Tag(b)) => a.cmp(b),
            // Different types sort by type priority: Number < Text < Tag < Unsupported
            (SortKey::Number(_), _) => Ordering::Less,
            (_, SortKey::Number(_)) => Ordering::Greater,
            (SortKey::Text(_), _) => Ordering::Less,
            (_, SortKey::Text(_)) => Ordering::Greater,
            (SortKey::Tag(_), _) => Ordering::Less,
            (_, SortKey::Tag(_)) => Ordering::Greater,
            (SortKey::Unsupported, SortKey::Unsupported) => Ordering::Equal,
        }
    }
}

/// B5: Compute incremental Remove+InsertAt operations to transform
/// old_sorted_indices into new_sorted_indices.
/// Both arrays map sorted positions to item indices.
/// Walks through target positions and moves misplaced items.
fn compute_sort_diff(
    items: &[ActorHandle],
    old_order: &[usize],
    new_order: &[usize],
) -> Vec<ListChange> {
    let mut changes = Vec::new();
    let mut current = old_order.to_vec();

    for target_pos in 0..new_order.len() {
        if current[target_pos] == new_order[target_pos] {
            continue;
        }

        // Find the target item in current order
        let item_idx = new_order[target_pos];
        let current_pos = current
            .iter()
            .position(|&x| x == item_idx)
            .expect("Bug: item missing from current sorted order");

        // Remove from current position
        let item = &items[item_idx];
        changes.push(ListChange::Remove {
            id: item.persistence_id(),
        });
        current.remove(current_pos);

        // Insert at target position
        changes.push(ListChange::InsertAt {
            index: target_pos,
            item: item.clone(),
        });
        let insert_pos = target_pos.min(current.len());
        if insert_pos != target_pos {
            zoon::eprintln!(
                "[actors-sort-clamp] target_pos={} len={}",
                target_pos,
                current.len()
            );
        }
        current.insert(insert_pos, item_idx);
    }

    changes
}

impl ListBindingFunction {
    fn source_list_current_and_future_values(
        source_list_actor: ActorHandle,
        subscription_scope: Option<Arc<SubscriptionScope>>,
    ) -> LocalBoxStream<'static, Arc<List>> {
        source_list_actor
            .current_or_future_stream()
            .take_while(move |_| {
                let is_active = subscription_scope
                    .as_ref()
                    .map_or(true, |s| !s.is_cancelled());
                future::ready(is_active)
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::List(list, _) => Some(list),
                    _ => None,
                })
            })
            // Deduplicate only identical runtime list instances.
            .scan(None, |prev_key: &mut Option<usize>, list| {
                let list_key = list_instance_key(&list);
                if prev_key.as_ref() == Some(&list_key) {
                    future::ready(Some(None))
                } else {
                    *prev_key = Some(list_key);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready)
            .boxed_local()
    }

    /// Creates a new ValueActor for a List binding function.
    ///
    /// For List/map(old, new: expr):
    /// - Subscribes to the source list
    /// - For each item, evaluates transform_expr with 'old' bound to the item
    /// - Produces the transformed list
    ///
    /// The StaticExpression is 'static, so it can be used in async handlers
    /// and potentially sent to WebWorkers for parallel processing.
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: ListBindingConfig,
        persistence_id: Option<parser::PersistenceId>,
    ) -> ActorHandle {
        let construct_info = construct_info.complete(ConstructType::FunctionCall);
        let config = Arc::new(config);

        match config.operation {
            ListBindingOperation::Map => Self::create_map_actor(
                construct_info,
                construct_context,
                actor_context,
                source_list_actor,
                config,
            ),
            ListBindingOperation::Retain => Self::create_retain_actor(
                construct_info,
                construct_context,
                actor_context,
                source_list_actor,
                config,
            ),
            ListBindingOperation::Remove => Self::create_remove_actor(
                construct_info,
                construct_context,
                actor_context,
                source_list_actor,
                config,
                persistence_id,
            ),
            ListBindingOperation::Every => {
                Self::create_every_any_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                    true, // is_every
                )
            }
            ListBindingOperation::Any => {
                Self::create_every_any_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                    false, // is_every (false = any)
                )
            }
            ListBindingOperation::SortBy => Self::create_sort_by_actor(
                construct_info,
                construct_context,
                actor_context,
                source_list_actor,
                config,
            ),
        }
    }

    /// Creates a map actor that transforms each list item.
    fn create_map_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
    ) -> ActorHandle {
        let config_for_stream = config.clone();
        let construct_context_for_stream = construct_context.clone();
        let actor_context_for_stream = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // LINK connections unnecessarily.
        let change_stream = switch_map(
            Self::source_list_current_and_future_values(
                source_list_actor.clone(),
                subscription_scope,
            ),
            move |list| {
                use std::collections::HashMap;
                let config = config_for_stream.clone();
                let construct_context = construct_context_for_stream.clone();
                let actor_context = actor_context_for_stream.clone();

                // Track length, PersistenceId mapping, current transformed items, and item order.
                // The transformed-item map is only used for removal/pop bookkeeping and move fallback.
                // Item order (Vec) is needed to clean transformed items on Pop (we need to know which item was last).
                type MapState = (
                    usize,
                    HashMap<parser::PersistenceId, parser::PersistenceId>,
                    HashMap<parser::PersistenceId, ActorHandle>, // transformed actors by source PersistenceId
                    Vec<parser::PersistenceId>,                  // Item order for Pop handling
                );
                list.stream().scan(
                    (0usize, HashMap::new(), HashMap::new(), Vec::new()),
                    move |state: &mut MapState, change| {
                        let (length, pid_map, transformed_items_by_pid, item_order) = state;
                        let (transformed_change, new_length) =
                            Self::transform_list_change_for_map_with_tracking(
                                change,
                                *length,
                                pid_map,
                                transformed_items_by_pid,
                                item_order,
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                        *length = new_length;
                        future::ready(Some(transformed_change))
                    },
                )
            },
        );

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/map result",
            ),
            actor_context.clone(),
            change_stream,
            source_list_actor.clone(),
        );

        let scope_id = actor_context.scope_id();
        create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ),
            scope_id,
        )
    }

    async fn rebuild_retain_state(
        items: &[ActorHandle],
        previous_results: &HashMap<parser::PersistenceId, bool>,
        config: &Arc<ListBindingConfig>,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> (
        HashMap<parser::PersistenceId, bool>,
        Option<Pin<Box<dyn Stream<Item = Vec<(parser::PersistenceId, bool)>>>>>,
        Vec<ActorHandle>,
        Vec<parser::PersistenceId>,
    ) {
        let mut predicate_results = HashMap::new();
        let mut predicate_streams = Vec::new();

        for (idx, item) in items.iter().enumerate() {
            let pid = item.persistence_id();
            let pred = Self::transform_item(
                item.clone(),
                idx,
                config,
                construct_context.clone(),
                actor_context.clone(),
            );
            let fallback = previous_results.get(&pid).copied().unwrap_or(true);
            let is_true = match pred.current_value() {
                Ok(value) => matches!(&value, Value::Tag(tag, _) if tag.tag() == "True"),
                Err(_) => fallback,
            };
            predicate_results.insert(pid.clone(), is_true);
            predicate_streams.push(
                pred.stream_from_now()
                    .map(move |v| {
                        let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                        (pid.clone(), is_true)
                    })
                    .scan(None::<bool>, |last_bool, (pid, is_true)| {
                        if Some(is_true) == *last_bool {
                            future::ready(Some(None))
                        } else {
                            *last_bool = Some(is_true);
                            future::ready(Some(Some((pid, is_true))))
                        }
                    })
                    .filter_map(future::ready),
            );
        }

        let merged_predicates = if predicate_streams.is_empty() {
            None
        } else {
            Some(Box::pin(coalesce(stream::select_all(predicate_streams)))
                as Pin<
                    Box<dyn Stream<Item = Vec<(parser::PersistenceId, bool)>>>,
                >)
        };

        let filtered: Vec<_> = items
            .iter()
            .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
            .cloned()
            .collect();
        let current_pids = filtered.iter().map(|item| item.persistence_id()).collect();

        (predicate_results, merged_predicates, filtered, current_pids)
    }

    /// Creates a retain actor that filters items based on predicate evaluation.
    /// Uses pure stream combinators (no spawned tasks, no channels) following
    /// the same pattern as `create_remove_actor`.
    fn create_retain_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
    ) -> ActorHandle {
        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling
        let value_stream = switch_map(
            Self::source_list_current_and_future_values(
                source_list_actor.clone(),
                subscription_scope,
            ),
            move |list| {
                let config = config.clone();
                let construct_context = construct_context.clone();
                let actor_context = actor_context.clone();

                // Use stream::unfold with select for fine-grained control
                // Uses PersistenceId-based tracking to avoid index shift bugs on Remove/Pop

                // State: (items, predicate_results, list_stream, merged_predicate_stream)
                // Note: Using HashMap keyed by PersistenceId for predicate results
                // to avoid index misalignment when items are removed
                type RetainState = (
                    Vec<ActorHandle>,                        // items (order matters for output)
                    HashMap<parser::PersistenceId, bool>,    // predicate_results by PersistenceId
                    Pin<Box<dyn Stream<Item = ListChange>>>, // list_stream
                    Option<Pin<Box<dyn Stream<Item = Vec<(parser::PersistenceId, bool)>>>>>, // merged predicates
                );

                let list_stream: Pin<Box<dyn Stream<Item = ListChange>>> = Box::pin(list.stream());

                stream::unfold(
                (
                    Vec::<ActorHandle>::new(),
                    HashMap::<parser::PersistenceId, bool>::new(),
                    list_stream,
                    // A3: Coalesced predicate stream yields batches instead of single items
                    None::<Pin<Box<dyn Stream<Item = Vec<(parser::PersistenceId, bool)>>>>>,
                    // A2: Track last emitted PersistenceIds for output deduplication
                    Vec::<parser::PersistenceId>::new(),
                    config,
                    construct_context,
                    actor_context,
                ),
                move |(mut items, mut predicate_results, mut list_stream, mut merged_predicates, mut last_emitted_pids, config, construct_context, actor_context)| {
                    async move {
                        loop {
                            // If we have predicate streams, race between list changes and predicate updates
                            if let Some(ref mut pred_stream) = merged_predicates {
                                // Use select to race between list changes and predicate updates
                                use zoon::futures_util::future::Either;

                                let list_next = list_stream.next();
                                let pred_next = pred_stream.next();

                                match future::select(pin!(list_next), pin!(pred_next)).await {
                                    Either::Left((Some(change), _)) => {
                                        // List structure changed
                                        match change {
                                            ListChange::Replace { items: new_items } => {
                                                items = new_items.to_vec();
                                                if items.is_empty() {
                                                    predicate_results.clear();
                                                    merged_predicates = None;
                                                    last_emitted_pids.clear(); // A2: Clear for empty list
                                                    return Some((
                                                        Some(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) }),
                                                        (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                } else {
                                                    let (
                                                        new_predicate_results,
                                                        new_merged_predicates,
                                                        filtered,
                                                        new_last_emitted_pids,
                                                    ) = Self::rebuild_retain_state(
                                                        &items,
                                                        &predicate_results,
                                                        &config,
                                                        construct_context.clone(),
                                                        actor_context.clone(),
                                                    )
                                                    .await;
                                                    predicate_results = new_predicate_results;
                                                    merged_predicates = new_merged_predicates;
                                                    last_emitted_pids = new_last_emitted_pids;

                                                    return Some((
                                                        Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                        (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                }
                                            }
                                            ListChange::Push { item } => {
                                                items.push(item);
                                                let (
                                                    new_predicate_results,
                                                    new_merged_predicates,
                                                    filtered,
                                                    current_pids,
                                                ) = Self::rebuild_retain_state(
                                                    &items,
                                                    &predicate_results,
                                                    &config,
                                                    construct_context.clone(),
                                                    actor_context.clone(),
                                                )
                                                .await;
                                                predicate_results = new_predicate_results;
                                                merged_predicates = new_merged_predicates;

                                                // A2: Output deduplication - skip if same as last emitted
                                                if current_pids == last_emitted_pids {
                                                    continue; // Skip redundant emission (pushed item was filtered out)
                                                }
                                                last_emitted_pids = current_pids;

                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                    (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                ));
                                            }
                                            ListChange::Remove { id } => {
                                                items.retain(|item| item.persistence_id() != id);
                                                let (filtered, current_pids) = if items.is_empty() {
                                                    predicate_results.clear();
                                                    merged_predicates = None;
                                                    (Vec::new(), Vec::new())
                                                } else {
                                                    let (
                                                        new_predicate_results,
                                                        new_merged_predicates,
                                                        filtered,
                                                        current_pids,
                                                    ) = Self::rebuild_retain_state(
                                                        &items,
                                                        &predicate_results,
                                                        &config,
                                                        construct_context.clone(),
                                                        actor_context.clone(),
                                                    )
                                                    .await;
                                                    predicate_results = new_predicate_results;
                                                    merged_predicates = new_merged_predicates;
                                                    (filtered, current_pids)
                                                };

                                                if current_pids == last_emitted_pids {
                                                    continue; // Skip redundant emission (removed item was filtered out)
                                                }
                                                last_emitted_pids = current_pids;

                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                    (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                ));
                                            }
                                            ListChange::Pop => {
                                                if items.pop().is_some() {
                                                    let (filtered, current_pids) = if items.is_empty() {
                                                        predicate_results.clear();
                                                        merged_predicates = None;
                                                        (Vec::new(), Vec::new())
                                                    } else {
                                                        let (
                                                            new_predicate_results,
                                                            new_merged_predicates,
                                                            filtered,
                                                            current_pids,
                                                        ) = Self::rebuild_retain_state(
                                                            &items,
                                                            &predicate_results,
                                                            &config,
                                                            construct_context.clone(),
                                                            actor_context.clone(),
                                                        )
                                                        .await;
                                                        predicate_results = new_predicate_results;
                                                        merged_predicates = new_merged_predicates;
                                                        (filtered, current_pids)
                                                    };

                                                    if current_pids == last_emitted_pids {
                                                        continue; // Skip redundant emission (popped item was filtered out)
                                                    }
                                                    last_emitted_pids = current_pids;

                                                    return Some((
                                                        Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                        (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                }
                                                continue;
                                            }
                                            ListChange::Clear => {
                                                items.clear();
                                                predicate_results.clear();
                                                merged_predicates = None;
                                                last_emitted_pids.clear(); // A2: Clear for empty list
                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) }),
                                                    (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                ));
                                            }
                                            // Explicitly handle all ListChange variants to catch future additions.
                                            // These variants are not currently generated by any List API.
                                            // If you're adding a new List operation that uses these,
                                            // you must implement proper handling here.
                                            ListChange::InsertAt { .. } => {
                                                panic!(
                                                    "List/retain received InsertAt event which is not yet implemented. \
                                                    If you added a List/insert_at API, implement handling in create_retain_actor."
                                                );
                                            }
                                            ListChange::UpdateAt { .. } => {
                                                panic!(
                                                    "List/retain received UpdateAt event which is not yet implemented. \
                                                    If you added a List/update_at API, implement handling in create_retain_actor."
                                                );
                                            }
                                            ListChange::Move { .. } => {
                                                panic!(
                                                    "List/retain received Move event which is not yet implemented. \
                                                    If you added a List/move API, implement handling in create_retain_actor."
                                                );
                                            }
                                        }
                                    }
                                    Either::Left((None, _)) => {
                                        // List stream ended
                                        return None;
                                    }
                                    Either::Right((Some(batch), _)) => {
                                        let mut visibility_changed = false;
                                        for (pid, is_true) in batch {
                                            if predicate_results.contains_key(&pid) {
                                                if predicate_results.get(&pid) != Some(&is_true) {
                                                    visibility_changed = true;
                                                }
                                                predicate_results.insert(pid, is_true);
                                            }
                                        }

                                        if !visibility_changed {
                                            continue; // No visibility changes
                                        }

                                        // Simpler and more robust than trying to synthesize a
                                        // per-batch insert/remove diff from out-of-order predicate
                                        // updates. Milestone 1 parity values correctness here.
                                        let filtered: Vec<_> = items.iter()
                                            .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                            .cloned()
                                            .collect();

                                        // A2: Output deduplication - skip if same as last emitted (order-aware)
                                        let current_pids: Vec<_> = filtered.iter()
                                            .map(|item| item.persistence_id())
                                            .collect();
                                        if current_pids == last_emitted_pids {
                                            continue; // Skip redundant emission
                                        }
                                        last_emitted_pids = current_pids;

                                        return Some((
                                            Some(ListChange::Replace { items: Arc::from(filtered) }),
                                            (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                        ));
                                    }
                                    Either::Right((None, _)) => {
                                        if predicate_results.is_empty() {
                                            merged_predicates = None;
                                        } else {
                                            let (
                                                new_predicate_results,
                                                new_merged_predicates,
                                                _filtered,
                                                _current_pids,
                                            ) = Self::rebuild_retain_state(
                                                &items,
                                                &predicate_results,
                                                &config,
                                                construct_context.clone(),
                                                actor_context.clone(),
                                            )
                                            .await;
                                            predicate_results = new_predicate_results;
                                            merged_predicates = new_merged_predicates;
                                        }
                                        continue;
                                    }
                                }
                            } else {
                                // No predicate streams yet, wait for list change
                                match list_stream.next().await {
                                    Some(ListChange::Replace { items: new_items }) => {
                                        items = new_items.to_vec();

                                        if items.is_empty() {
                                            predicate_results.clear();
                                            last_emitted_pids.clear(); // A2: Clear for empty list
                                            return Some((
                                                Some(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) }),
                                                (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                            ));
                                        } else {
                                            let (
                                                new_predicate_results,
                                                new_merged_predicates,
                                                filtered,
                                                new_last_emitted_pids,
                                            ) = Self::rebuild_retain_state(
                                                &items,
                                                &predicate_results,
                                                &config,
                                                construct_context.clone(),
                                                actor_context.clone(),
                                            )
                                            .await;
                                            predicate_results = new_predicate_results;
                                            merged_predicates = new_merged_predicates;
                                            last_emitted_pids = new_last_emitted_pids;

                                            return Some((
                                                Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                            ));
                                        }
                                    }
                                    Some(ListChange::Push { item }) => {
                                        items.push(item);
                                        let (
                                            new_predicate_results,
                                            new_merged_predicates,
                                            filtered,
                                            current_pids,
                                        ) = Self::rebuild_retain_state(
                                            &items,
                                            &predicate_results,
                                            &config,
                                            construct_context.clone(),
                                            actor_context.clone(),
                                        )
                                        .await;
                                        predicate_results = new_predicate_results;
                                        merged_predicates = new_merged_predicates;

                                        if current_pids == last_emitted_pids {
                                            continue; // Skip redundant emission
                                        }
                                        last_emitted_pids = current_pids;

                                        return Some((
                                            Some(ListChange::Replace { items: Arc::from(filtered) }),
                                            (items, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                        ));
                                    }
                                    // When merged_predicates is None, the list is empty.
                                    // These operations on an empty list are no-ops:
                                    Some(ListChange::Remove { .. }) => continue, // Can't remove from empty
                                    Some(ListChange::Pop) => continue,           // Can't pop from empty
                                    Some(ListChange::Clear) => continue,         // Already empty
                                    // These variants are not currently generated by any List API.
                                    // If you're adding a new List operation that uses these,
                                    // you must implement proper handling here.
                                    Some(ListChange::InsertAt { .. }) => {
                                        panic!(
                                            "List/retain received InsertAt event which is not yet implemented. \
                                            If you added a List/insert_at API, implement handling in create_retain_actor."
                                        );
                                    }
                                    Some(ListChange::UpdateAt { .. }) => {
                                        panic!(
                                            "List/retain received UpdateAt event which is not yet implemented. \
                                            If you added a List/update_at API, implement handling in create_retain_actor."
                                        );
                                    }
                                    Some(ListChange::Move { .. }) => {
                                        panic!(
                                            "List/retain received Move event which is not yet implemented. \
                                            If you added a List/move API, implement handling in create_retain_actor."
                                        );
                                    }
                                    None => return None,
                                }
                            }
                        }
                    }
                }
            )
            .filter_map(future::ready)
            },
        );

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/retain result",
            ),
            actor_context_for_list,
            value_stream,
            source_list_actor.clone(),
        );

        let scope_id = actor_context_for_result.scope_id();
        create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ),
            scope_id,
        )
    }

    /// Creates a remove actor that removes items when their `when` event fires.
    /// Tracks removed items by PersistenceId so they don't reappear on upstream Replace.
    fn create_remove_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
        persistence_id: Option<parser::PersistenceId>,
    ) -> ActorHandle {
        use std::collections::HashSet;

        // Storage key for this List/remove's removed set (per-branch removal tracking)
        let removed_set_key: Option<String> = persistence_id
            .as_ref()
            .map(|pid| format!("list_removed:{}", pid));

        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();
        let construct_context_for_persistence = construct_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling
        let value_stream = switch_map(
            Self::source_list_current_and_future_values(
                source_list_actor.clone(),
                subscription_scope,
            ),
            move |list| {
                let config = config.clone();
                let construct_context = construct_context.clone();
                let actor_context = actor_context.clone();
                let removed_set_key = removed_set_key.clone();

                // Load persisted removed set for restoration (call_ids that were previously removed)
                let persisted_removed: HashSet<String> = removed_set_key
                    .as_ref()
                    .map(|key| load_removed_set(key).into_iter().collect())
                    .unwrap_or_default();

                // Event type for merged streams
                enum RemoveEvent {
                    ListChange(ListChange),
                    RemoveItem(parser::PersistenceId),
                }

                // State: track items and which PersistenceIds have been removed by THIS List/remove
                // removed_persistence_ids is bounded: items are removed from this set when they're
                // removed from upstream (no longer in Replace payload or via Remove { id }).
                type ItemEntry = ActorHandle;

                /// B6: Create a trigger stream for a single when_actor.
                /// Emits the persistence_id once when the when_actor produces a value, then ends.
                fn make_trigger_stream(
                    when_actor: ActorHandle,
                    persistence_id: parser::PersistenceId,
                ) -> LocalBoxStream<'static, parser::PersistenceId> {
                    when_actor
                        .current_or_future_stream()
                        .map(move |_| persistence_id.clone())
                        .take(1)
                        .boxed_local()
                }

                stream::unfold(
                    (
                        list.clone().stream().boxed_local().fuse(),
                        stream::SelectAll::<LocalBoxStream<'static, parser::PersistenceId>>::new(),
                        Vec::<ItemEntry>::new(),
                        HashSet::<parser::PersistenceId>::new(),
                        config.clone(),
                        construct_context.clone(),
                        actor_context.clone(),
                        0usize,
                        removed_set_key.clone(),
                        persisted_removed,
                        false,
                    ),
                    move |(
                        mut list_changes,
                        mut active_triggers,
                        mut items,
                        mut removed_pids,
                        config,
                        construct_context,
                        actor_context,
                        mut next_idx,
                        removed_set_key,
                        persisted_removed,
                        mut list_changes_done,
                    )| async move {
                        loop {
                            let event = if list_changes_done {
                                let Some(persistence_id) = active_triggers.next().await else {
                                    return None;
                                };
                                RemoveEvent::RemoveItem(persistence_id)
                            } else if active_triggers.is_empty() {
                                match list_changes.next().await {
                                    Some(change) => RemoveEvent::ListChange(change),
                                    None => {
                                        list_changes_done = true;
                                        continue;
                                    }
                                }
                            } else {
                                select! {
                                    change = list_changes.next().fuse() => {
                                        match change {
                                            Some(change) => RemoveEvent::ListChange(change),
                                            None => {
                                                list_changes_done = true;
                                                continue;
                                            }
                                        }
                                    }
                                    persistence_id = active_triggers.select_next_some() => {
                                        RemoveEvent::RemoveItem(persistence_id)
                                    }
                                }
                            };

                            let emitted_change = match event {
                                RemoveEvent::ListChange(change) => match change {
                                    ListChange::Replace { items: new_items } => {
                                        if LOG_DEBUG {
                                            zoon::println!(
                                                "[DEBUG] List/remove Replace: {} items incoming, persisted_removed={:?}",
                                                new_items.len(),
                                                persisted_removed
                                            );
                                        }
                                        items.clear();
                                        active_triggers = stream::SelectAll::new();
                                        let mut filtered_items = Vec::new();

                                        for item in new_items.iter() {
                                            let persistence_id = item.persistence_id();
                                            if removed_pids.contains(&persistence_id) {
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] List/remove Replace: filtering by removed_pids"
                                                    );
                                                }
                                                continue;
                                            }

                                            if let Some(origin) = item.list_item_origin() {
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] List/remove Replace: item has origin call_id={}",
                                                        origin.call_id
                                                    );
                                                }
                                                if persisted_removed.contains(&origin.call_id) {
                                                    if LOG_DEBUG {
                                                        zoon::println!(
                                                            "[DEBUG] List/remove Replace: FILTERING out call_id={}",
                                                            origin.call_id
                                                        );
                                                    }
                                                    continue;
                                                }
                                            } else {
                                                let id_str = format!("pid:{}", persistence_id);
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] List/remove Replace: item has NO origin, checking pid={}",
                                                        id_str
                                                    );
                                                }
                                                if persisted_removed.contains(&id_str) {
                                                    if LOG_DEBUG {
                                                        zoon::println!(
                                                            "[DEBUG] List/remove Replace: FILTERING out pid={}",
                                                            id_str
                                                        );
                                                    }
                                                    continue;
                                                }
                                            }

                                            let idx = next_idx;
                                            next_idx += 1;
                                            let when_actor = Self::transform_item(
                                                item.clone(),
                                                idx,
                                                &config,
                                                construct_context.clone(),
                                                actor_context.clone(),
                                            );
                                            active_triggers.push(make_trigger_stream(
                                                when_actor,
                                                persistence_id,
                                            ));
                                            items.push(item.clone());
                                            filtered_items.push(item.clone());
                                        }

                                        let upstream_pids: HashSet<_> = new_items
                                            .iter()
                                            .map(|item| item.persistence_id())
                                            .collect();
                                        removed_pids.retain(|pid| upstream_pids.contains(pid));

                                        Some(ListChange::Replace {
                                            items: Arc::from(filtered_items),
                                        })
                                    }
                                    ListChange::Push { item } => {
                                        let persistence_id = item.persistence_id();
                                        if removed_pids.contains(&persistence_id) {
                                            if LOG_DEBUG {
                                                zoon::println!(
                                                    "[DEBUG] List/remove Push: filtering by removed_pids"
                                                );
                                            }
                                            None
                                        } else if let Some(origin) = item.list_item_origin() {
                                            if LOG_DEBUG {
                                                zoon::println!(
                                                    "[DEBUG] List/remove Push: item has origin call_id={}, persisted_removed={:?}",
                                                    origin.call_id,
                                                    persisted_removed
                                                );
                                            }
                                            if persisted_removed.contains(&origin.call_id) {
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] List/remove Push: FILTERING out call_id={}",
                                                        origin.call_id
                                                    );
                                                }
                                                None
                                            } else {
                                                let idx = next_idx;
                                                next_idx += 1;
                                                let when_actor = Self::transform_item(
                                                    item.clone(),
                                                    idx,
                                                    &config,
                                                    construct_context.clone(),
                                                    actor_context.clone(),
                                                );
                                                active_triggers.push(make_trigger_stream(
                                                    when_actor,
                                                    persistence_id,
                                                ));
                                                items.push(item.clone());
                                                Some(ListChange::Push { item })
                                            }
                                        } else {
                                            let id_str = format!("pid:{}", persistence_id);
                                            if LOG_DEBUG {
                                                zoon::println!(
                                                    "[DEBUG] List/remove Push: item has NO origin, checking pid={}",
                                                    id_str
                                                );
                                            }
                                            if persisted_removed.contains(&id_str) {
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] List/remove Push: FILTERING out pid={}",
                                                        id_str
                                                    );
                                                }
                                                None
                                            } else {
                                                let idx = next_idx;
                                                next_idx += 1;
                                                let when_actor = Self::transform_item(
                                                    item.clone(),
                                                    idx,
                                                    &config,
                                                    construct_context.clone(),
                                                    actor_context.clone(),
                                                );
                                                active_triggers.push(make_trigger_stream(
                                                    when_actor,
                                                    persistence_id,
                                                ));
                                                items.push(item.clone());
                                                Some(ListChange::Push { item })
                                            }
                                        }
                                    }
                                    ListChange::Remove { id } => {
                                        removed_pids.remove(&id);
                                        if let Some(pos) = items
                                            .iter()
                                            .position(|item| item.persistence_id() == id)
                                        {
                                            items.remove(pos);
                                        }
                                        Some(ListChange::Remove { id })
                                    }
                                    ListChange::Clear => {
                                        items.clear();
                                        active_triggers = stream::SelectAll::new();
                                        removed_pids.clear();
                                        next_idx = 0;
                                        Some(ListChange::Clear)
                                    }
                                    ListChange::Pop => {
                                        if let Some(item) = items.pop() {
                                            removed_pids.remove(&item.persistence_id());
                                        }
                                        Some(ListChange::Pop)
                                    }
                                    _ => None,
                                },
                                RemoveEvent::RemoveItem(persistence_id) => {
                                    if let Some(pos) = items
                                        .iter()
                                        .position(|item| item.persistence_id() == persistence_id)
                                    {
                                        let item = &items[pos];
                                        if LOG_DEBUG {
                                            zoon::println!(
                                                "[DEBUG] RemoveItem: item has origin: {}, removed_set_key: {:?}",
                                                item.list_item_origin().is_some(),
                                                removed_set_key
                                            );
                                        }

                                        if let Some(key) = removed_set_key.as_ref() {
                                            if let Some(origin) = item.list_item_origin() {
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] Adding to removed set: key={}, call_id={}",
                                                        key,
                                                        origin.call_id
                                                    );
                                                }
                                                add_to_removed_set(key, &origin.call_id);
                                            } else {
                                                let id_str = format!("pid:{}", persistence_id);
                                                if LOG_DEBUG {
                                                    zoon::println!(
                                                        "[DEBUG] Adding to removed set (no origin): key={}, id={}",
                                                        key,
                                                        id_str
                                                    );
                                                }
                                                add_to_removed_set(key, &id_str);
                                            }
                                        }

                                        items.remove(pos);
                                        removed_pids.insert(persistence_id);
                                        Some(ListChange::Remove { id: persistence_id })
                                    } else {
                                        None
                                    }
                                }
                            };

                            if let Some(change) = emitted_change {
                                return Some((
                                    change,
                                    (
                                        list_changes,
                                        active_triggers,
                                        items,
                                        removed_pids,
                                        config,
                                        construct_context,
                                        actor_context,
                                        next_idx,
                                        removed_set_key,
                                        persisted_removed,
                                        list_changes_done,
                                    ),
                                ));
                            }
                        }
                    },
                )
            },
        );

        // Use persistence-aware list creation if persistence_id is provided
        let list = if let Some(pid) = persistence_id {
            List::new_with_change_stream_and_persistence(
                ConstructInfo::new(
                    construct_info.id.clone().with_child_id(0),
                    None,
                    "List/remove result",
                ),
                construct_context_for_persistence,
                actor_context_for_list,
                value_stream,
                (source_list_actor.clone(),),
                pid,
            )
        } else {
            List::new_with_change_stream(
                ConstructInfo::new(
                    construct_info.id.clone().with_child_id(0),
                    None,
                    "List/remove result",
                ),
                actor_context_for_list,
                value_stream,
                source_list_actor.clone(),
            )
        };

        let scope_id = actor_context_for_result.scope_id();
        create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ),
            scope_id,
        )
    }

    /// Creates an every/any actor that produces True/False based on predicates.
    fn create_every_any_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
        is_every: bool, // true = every, false = any
    ) -> ActorHandle {
        let construct_info_id = construct_info.id.clone();

        // Clone for use after the flat_map chain
        let actor_context_for_result = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // subscriptions unnecessarily.
        let value_stream = switch_map(
            Self::source_list_current_and_future_values(
                source_list_actor.clone(),
                subscription_scope,
            ),
            move |list| {
                let config = config.clone();
                let construct_context = construct_context.clone();
                let actor_context = actor_context.clone();
                let construct_info_id = construct_info_id.clone();

                // Clone for the second flat_map
                let construct_info_id_inner = construct_info_id.clone();
                let construct_context_inner = construct_context.clone();

                list.stream().scan(
                Vec::<(parser::PersistenceId, ActorHandle)>::new(),
                move |item_predicates, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    match &change {
                        ListChange::Replace { items } => {
                            *item_predicates = items.iter().enumerate().map(|(idx, item)| {
                                let predicate = Self::transform_item(
                                    item.clone(),
                                    idx,
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.persistence_id(), predicate)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let idx = item_predicates.len();
                            let predicate = Self::transform_item(
                                item.clone(),
                                idx,
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_predicates.push((item.persistence_id(), predicate));
                        }
                        ListChange::Clear => {
                            item_predicates.clear();
                        }
                        ListChange::Pop => {
                            item_predicates.pop();
                        }
                        ListChange::Remove { id } => {
                            // Find item by PersistenceId
                            if let Some(index) =
                                item_predicates.iter().position(|(pid, _)| pid == id)
                            {
                                item_predicates.remove(index);
                            }
                        }
                        // Explicitly handle all ListChange variants to catch future additions.
                        // These variants are not currently generated by any List API.
                        ListChange::InsertAt { .. } => {
                            panic!(
                                "List/every or List/any received InsertAt event which is not yet implemented. \
                                If you added a List/insert_at API, implement handling in create_every_any_actor."
                            );
                        }
                        ListChange::UpdateAt { .. } => {
                            panic!(
                                "List/every or List/any received UpdateAt event which is not yet implemented. \
                                If you added a List/update_at API, implement handling in create_every_any_actor."
                            );
                        }
                        ListChange::Move { .. } => {
                            panic!(
                                "List/every or List/any received Move event which is not yet implemented. \
                                If you added a List/move API, implement handling in create_every_any_actor."
                            );
                        }
                    }

                    future::ready(Some(item_predicates.clone()))
                }
            ).flat_map(move |item_predicates| {
                let _construct_info_id = construct_info_id_inner.clone();
                let construct_context = construct_context_inner.clone();

                if item_predicates.is_empty() {
                    // Empty list: every([]) = True, any([]) = False
                    let result = if is_every { "True" } else { "False" };
                    return stream::once(future::ready(Tag::new_value(
                        ConstructInfo::new(
                            construct_info_id.clone().with_child_id(0),
                            None,
                            if is_every { "List/every result" } else { "List/any result" },
                        ),
                        construct_context,
                        ValueIdempotencyKey::new(),
                        result.to_string(),
                    ))).boxed_local();
                }

                // Clone for the map closure
                let construct_info_id_map = construct_info_id.clone();
                let construct_context_map = construct_context.clone();

                // B2: Add boolean deduplication to each predicate stream
                let predicate_streams: Vec<_> = item_predicates.iter().enumerate().map(|(idx, (_, pred))| {
                    pred.clone().stream()
                        .map(move |value| {
                            let is_true = match &value {
                                Value::Tag(tag, _) => tag.tag() == "True",
                                _ => false,
                            };
                            (idx, is_true)
                        })
                        // B2: Deduplicate booleans - skip emission if same as previous
                        .scan(None::<bool>, |last_bool, (idx, is_true)| {
                            if Some(is_true) == *last_bool {
                                future::ready(Some(None)) // Skip duplicate
                            } else {
                                *last_bool = Some(is_true);
                                future::ready(Some(Some((idx, is_true))))
                            }
                        })
                        .filter_map(future::ready)
                }).collect();

                // A3: Coalesce to batch all synchronously-available predicate updates
                // B4: Track true_count/evaluated_count for O(1) every/any evaluation
                let total = item_predicates.len();
                coalesce(stream::select_all(predicate_streams))
                    .scan(
                        (vec![None::<bool>; total], 0usize, 0usize), // (states, true_count, evaluated_count)
                        move |(states, true_count, evaluated_count), batch| {
                            // Process entire batch of predicate updates at once
                            for (idx, is_true) in batch {
                                if idx < states.len() {
                                    match states[idx] {
                                        None => {
                                            // First evaluation of this predicate
                                            *evaluated_count += 1;
                                            if is_true { *true_count += 1; }
                                        }
                                        Some(was_true) => {
                                            // Update existing predicate
                                            if was_true && !is_true { *true_count -= 1; }
                                            if !was_true && is_true { *true_count += 1; }
                                        }
                                    }
                                    states[idx] = Some(is_true);
                                }
                            }

                            if *evaluated_count == total {
                                // O(1) check instead of O(N) scan
                                let result = if is_every {
                                    *true_count == total
                                } else {
                                    *true_count > 0
                                };
                                future::ready(Some(Some(result)))
                            } else {
                                future::ready(Some(None))
                            }
                        }
                    )
                    .filter_map(future::ready)
                    .map(move |result| {
                        let tag = if result { "True" } else { "False" };
                        Tag::new_value(
                            ConstructInfo::new(
                                construct_info_id_map.clone().with_child_id(0),
                                None,
                                if is_every { "List/every result" } else { "List/any result" },
                            ),
                            construct_context_map.clone(),
                            ValueIdempotencyKey::new(),
                            tag.to_string(),
                        )
                    })
                    .boxed_local()
            })
            },
        );

        // Deduplicate: only emit when the boolean result actually changes
        let deduplicated_stream = value_stream
            .scan(None::<bool>, |last_result, value| {
                let current_result = match &value {
                    Value::Tag(tag, _) => tag.tag() == "True",
                    _ => false,
                };
                if *last_result != Some(current_result) {
                    *last_result = Some(current_result);
                    future::ready(Some(Some(value)))
                } else {
                    future::ready(Some(None))
                }
            })
            .filter_map(future::ready);

        let scope_id = actor_context_for_result.scope_id();
        create_actor(deduplicated_stream, parser::PersistenceId::new(), scope_id)
    }

    /// Creates a sort_by actor that sorts list items based on a key expression.
    /// When any item's key changes, emits an updated sorted list.
    fn create_sort_by_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
    ) -> ActorHandle {
        let construct_info_id = construct_info.id.clone();

        // Clone for use after the flat_map chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Create a stream that:
        // 1. Subscribes to source list changes (stream() yields automatically)
        // 2. For each item, evaluates key expression and subscribes to its changes
        // 3. When list or any key changes, emits sorted Replace
        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // subscriptions unnecessarily.
        let value_stream = switch_map(
            Self::source_list_current_and_future_values(
                source_list_actor.clone(),
                subscription_scope,
            ),
            move |list| {
                let config = config.clone();
                let construct_context = construct_context.clone();
                let actor_context = actor_context.clone();
                let construct_info_id = construct_info_id.clone();

                // Clone for the second flat_map
                let construct_info_id_inner = construct_info_id.clone();

                // Track items and their keys
                list.stream().scan(
                Vec::<(ActorHandle, ActorHandle)>::new(), // (item, key_actor)
                move |item_keys, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    // Apply change and update key actors
                    match &change {
                        ListChange::Replace { items } => {
                            *item_keys = items.iter().enumerate().map(|(idx, item)| {
                                let key_actor = Self::transform_item(
                                    item.clone(),
                                    idx,
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), key_actor)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let idx = item_keys.len();
                            let key_actor = Self::transform_item(
                                item.clone(),
                                idx,
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_keys.push((item.clone(), key_actor));
                        }
                        ListChange::InsertAt { index, item } => {
                            let key_actor = Self::transform_item(
                                item.clone(),
                                *index,
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            if *index <= item_keys.len() {
                                item_keys.insert(*index, (item.clone(), key_actor));
                            }
                        }
                        ListChange::Remove { id } => {
                            // Find item by PersistenceId
                            if let Some(index) = item_keys.iter().position(|(item, _)| item.persistence_id() == *id) {
                                item_keys.remove(index);
                            }
                        }
                        ListChange::Clear => {
                            item_keys.clear();
                        }
                        ListChange::Pop => {
                            item_keys.pop();
                        }
                        // Explicitly handle all ListChange variants to catch future additions.
                        // These variants are not currently generated by any List API.
                        ListChange::UpdateAt { .. } => {
                            panic!(
                                "List/sort_by_key received UpdateAt event which is not yet implemented. \
                                If you added a List/update_at API, implement handling in create_sort_by_actor."
                            );
                        }
                        ListChange::Move { .. } => {
                            panic!(
                                "List/sort_by_key received Move event which is not yet implemented. \
                                If you added a List/move API, implement handling in create_sort_by_actor."
                            );
                        }
                    }

                    future::ready(Some(item_keys.clone()))
                }
            ).flat_map(move |item_keys| {
                let _construct_info_id = construct_info_id_inner.clone();

                if item_keys.is_empty() {
                    // Empty list - emit empty Replace
                    return stream::once(future::ready(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) })).boxed_local();
                }

                // Subscribe to all keys and emit sorted list when any changes
                // A3: Use coalesce to batch simultaneous key updates
                let key_streams: Vec<_> = item_keys.iter().enumerate().map(|(idx, (_, key_actor))| {
                    key_actor.clone().stream().map(move |value| (idx, value))
                }).collect();

                // B5: Incremental sort using BTreeMap for O(log n) key updates.
                // State: (items, key_values, sorted_tree, prev_sorted_indices, unevaluated_count)
                // BTreeMap key: (SortKey, usize) where usize is original index for stable tie-breaking.
                // BTreeMap value: usize (index into items vec).
                let item_count = item_keys.len();
                coalesce(stream::select_all(key_streams))
                    .scan(
                        (
                            item_keys.iter().map(|(item, _)| item.clone()).collect::<Vec<_>>(),
                            vec![None::<SortKey>; item_count],
                            std::collections::BTreeMap::<(SortKey, usize), usize>::new(),
                            Vec::<usize>::new(), // prev_sorted_indices
                            item_count,          // unevaluated_count
                        ),
                        move |(items, key_values, sorted_tree, prev_sorted, unevaluated), batch| {
                            // Update keys in BTreeMap
                            for (idx, value) in batch {
                                let new_key = SortKey::from_value(&value);
                                if idx < key_values.len() {
                                    // Remove old key from tree if it existed
                                    if let Some(old_key) = key_values[idx].take() {
                                        sorted_tree.remove(&(old_key, idx));
                                    } else {
                                        // First evaluation of this key
                                        *unevaluated = unevaluated.saturating_sub(1);
                                    }
                                    // Insert new key
                                    sorted_tree.insert((new_key.clone(), idx), idx);
                                    key_values[idx] = Some(new_key);
                                }
                            }

                            // Wait until all items have evaluated keys
                            if *unevaluated > 0 {
                                return future::ready(Some(None));
                            }

                            // Read sorted order from BTreeMap (already in order)
                            let new_sorted: Vec<usize> = sorted_tree.values().copied().collect();

                            if prev_sorted.is_empty() {
                                // First evaluation - emit Replace
                                let sorted_items: Vec<ActorHandle> = new_sorted.iter()
                                    .map(|&idx| items[idx].clone())
                                    .collect();
                                *prev_sorted = new_sorted;
                                future::ready(Some(Some(vec![ListChange::Replace { items: Arc::from(sorted_items) }])))
                            } else if *prev_sorted == new_sorted {
                                // No change in sorted order
                                future::ready(Some(None))
                            } else {
                                // B5: Compute incremental Remove+InsertAt operations
                                let changes = compute_sort_diff(items, prev_sorted, &new_sorted);
                                *prev_sorted = new_sorted;
                                future::ready(Some(Some(changes)))
                            }
                        }
                    )
                    .filter_map(future::ready)
                    .flat_map(|changes| stream::iter(changes))
                    .boxed_local()
            })
            },
        );

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/sort_by result",
            ),
            actor_context_for_list,
            value_stream,
            source_list_actor.clone(),
        );

        let scope_id = actor_context_for_result.scope_id();
        create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ),
            scope_id,
        )
    }

    /// Transform a single list item using the config's transform expression.
    fn transform_item(
        item_actor: ActorHandle,
        index: usize,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> ActorHandle {
        // Create a new ActorContext with the binding variable set
        let binding_name = config.binding_name.to_string();
        let mut new_params = (*actor_context.parameters).clone();

        new_params.insert(binding_name.clone(), item_actor.clone());

        // Use with_child_scope to create a properly isolated scope for this list item.
        // This ensures LINKs inside the transform expression have unique persistence IDs
        // per item, preventing cross-item contamination when List/map is used.
        //
        // IMPORTANT: Include BOTH index AND persistence_id in the scope!
        // - Index ensures uniqueness per list position (critical when multiple items have same persistence_id)
        // - persistence_id provides stability across WHILE re-renders (derived from source position)
        //
        // Without the index, items from `LIST { fn(), fn(), fn() }` all get the same scope
        // because they share the same persistence_id (from the function's return object AST position).
        // This caused the list_object_state bug where clicking any button incremented all counters.
        let scope_id = list_item_scope_id(index, item_actor.persistence_id());
        let child_scope = actor_context.with_child_scope(&scope_id);
        // Create a child registry scope for this list item so all actors created
        // within the transform expression are owned by this scope.
        // When the parent scope is destroyed (e.g., WHILE arm switch, program teardown),
        // all list item scopes are recursively destroyed.
        let item_registry_scope = child_scope
            .registry_scope_id
            .map(|parent_scope| create_registry_scope(Some(parent_scope)));
        let new_actor_context = ActorContext {
            parameters: Arc::new(new_params),
            registry_scope_id: item_registry_scope.or(child_scope.registry_scope_id),
            ..child_scope
        };

        // Evaluate the transform expression with the binding in scope
        // Pass the function registry snapshot to enable user-defined function calls
        match evaluate_static_expression_with_registry(
            &config.transform_expr,
            construct_context.clone(),
            new_actor_context.clone(),
            config.reference_connector.clone(),
            config.link_connector.clone(),
            config.pass_through_connector.clone(),
            config.source_code.clone(),
            config.function_registry_snapshot.clone(),
        ) {
            Ok(result_actor) => {
                // IMPORTANT: The result_actor's PersistenceId comes from AST position,
                // which is the same for all items mapped by the same expression.
                // We need unique PersistenceIds for identity-based Remove to work.
                // Forward the result under the ORIGINAL item's PersistenceId while preserving:
                // - the current value (some mapped expressions have one before any stream item)
                // - future updates (especially when the mapped body depends on external state)
                let original_pid = item_actor.persistence_id();
                let scope_id = new_actor_context.scope_id();
                let forwarding_actor = create_actor_forwarding(
                    original_pid, // Preserve original PersistenceId!
                    scope_id,
                );
                connect_forwarding_current_and_future(forwarding_actor.clone(), result_actor);
                forwarding_actor
            }
            Err(e) => {
                zoon::eprintln!("Error evaluating transform expression: {e}");
                // Return the original item as fallback
                item_actor
            }
        }
    }

    /// Transform a ListChange by applying the transform expression to affected items.
    /// Only used for map operation.
    ///
    /// `current_length` is the current list length before applying this change.
    /// Returns the transformed change and the new list length after applying the change.
    fn transform_list_change_for_map(
        change: ListChange,
        current_length: usize,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> (ListChange, usize) {
        match change {
            ListChange::Replace { items } => {
                let new_length = items.len();
                let transformed_items: Vec<ActorHandle> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        Self::transform_item(
                            item.clone(),
                            index,
                            config,
                            construct_context.clone(),
                            actor_context.clone(),
                        )
                    })
                    .collect();
                (
                    ListChange::Replace {
                        items: Arc::from(transformed_items),
                    },
                    new_length,
                )
            }
            ListChange::InsertAt { index, item } => {
                let insert_index = index.min(current_length);
                if insert_index != index {
                    zoon::eprintln!(
                        "[actors-map-insert-clamp] index={} len={}",
                        index,
                        current_length
                    );
                }
                let transformed_item = Self::transform_item(
                    item,
                    insert_index,
                    config,
                    construct_context,
                    actor_context,
                );
                (
                    ListChange::InsertAt {
                        index: insert_index,
                        item: transformed_item,
                    },
                    current_length + 1,
                )
            }
            ListChange::UpdateAt { index, item } => {
                let update_index = if current_length == 0 {
                    0
                } else {
                    index.min(current_length - 1)
                };
                if update_index != index {
                    zoon::eprintln!(
                        "[actors-map-update-clamp] index={} len={}",
                        index,
                        current_length
                    );
                }
                let transformed_item = Self::transform_item(
                    item,
                    update_index,
                    config,
                    construct_context,
                    actor_context,
                );
                (
                    ListChange::UpdateAt {
                        index: update_index,
                        item: transformed_item,
                    },
                    current_length,
                )
            }
            ListChange::Push { item } => {
                // Use current_length as the index for the new item.
                // This gives each pushed item a unique, stable index based on its position.
                let transformed_item = Self::transform_item(
                    item,
                    current_length,
                    config,
                    construct_context,
                    actor_context,
                );
                (
                    ListChange::Push {
                        item: transformed_item,
                    },
                    current_length + 1,
                )
            }
            // These operations don't involve new items, pass through with updated length
            ListChange::Remove { id } => {
                (ListChange::Remove { id }, current_length.saturating_sub(1))
            }
            ListChange::Move {
                old_index,
                new_index,
            } => {
                let clamped_new_index = new_index.min(current_length.saturating_sub(1));
                if clamped_new_index != new_index {
                    zoon::eprintln!(
                        "[actors-map-move-clamp] old_index={} new_index={} len={}",
                        old_index,
                        new_index,
                        current_length
                    );
                }
                (
                    ListChange::Move {
                        old_index,
                        new_index: clamped_new_index,
                    },
                    current_length,
                )
            }
            ListChange::Pop => (ListChange::Pop, current_length.saturating_sub(1)),
            ListChange::Clear => (ListChange::Clear, 0),
        }
    }

    /// Like transform_list_change_for_map but tracks the mapping from original
    /// PersistenceIds to mapped PersistenceIds. This is necessary because Remove { id }
    /// uses the original item's PersistenceId, but the output list contains mapped items
    /// with different PersistenceIds.
    fn transform_list_change_for_map_with_tracking(
        change: ListChange,
        current_length: usize,
        pid_map: &mut std::collections::HashMap<parser::PersistenceId, parser::PersistenceId>,
        transformed_items_by_pid: &mut std::collections::HashMap<
            parser::PersistenceId,
            ActorHandle,
        >,
        item_order: &mut Vec<parser::PersistenceId>,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> (ListChange, usize) {
        // Each incoming change builds a fresh mapped item set. We keep only the active transformed
        // actors needed for remove/pop bookkeeping and move fallback inside this change.
        transformed_items_by_pid.clear();

        match change {
            ListChange::Replace { items } => {
                let new_length = items.len();
                // Clear old PID mapping and item order for the fresh mapped item set.
                pid_map.clear();
                item_order.clear();

                let transformed_items: Vec<ActorHandle> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let original_pid = item.persistence_id();
                        item_order.push(original_pid.clone());
                        let transformed = Self::transform_item(
                            item.clone(),
                            index,
                            config,
                            construct_context.clone(),
                            actor_context.clone(),
                        );
                        transformed_items_by_pid.insert(original_pid.clone(), transformed.clone());

                        let mapped_pid = transformed.persistence_id();
                        pid_map.insert(original_pid, mapped_pid);
                        transformed
                    })
                    .collect();

                (
                    ListChange::Replace {
                        items: Arc::from(transformed_items),
                    },
                    new_length,
                )
            }
            ListChange::InsertAt { index, item } => {
                let original_pid = item.persistence_id();
                // Track item order
                let insert_index = index.min(item_order.len());
                if insert_index != index {
                    zoon::eprintln!(
                        "[actors-map-insert-clamp] index={} len={}",
                        index,
                        item_order.len()
                    );
                }
                item_order.insert(insert_index, original_pid.clone());
                let transformed_item =
                    Self::transform_item(item, index, config, construct_context, actor_context);
                transformed_items_by_pid.insert(original_pid.clone(), transformed_item.clone());
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (
                    ListChange::InsertAt {
                        index: insert_index,
                        item: transformed_item,
                    },
                    current_length + 1,
                )
            }
            ListChange::UpdateAt { index, item } => {
                let original_pid = item.persistence_id();
                let transformed_item =
                    Self::transform_item(item, index, config, construct_context, actor_context);
                transformed_items_by_pid.insert(original_pid.clone(), transformed_item.clone());
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (
                    ListChange::UpdateAt {
                        index,
                        item: transformed_item,
                    },
                    current_length,
                )
            }
            ListChange::Push { item } => {
                let original_pid = item.persistence_id();
                // Track item order
                item_order.push(original_pid.clone());
                let transformed_item = Self::transform_item(
                    item,
                    current_length,
                    config,
                    construct_context,
                    actor_context,
                );
                transformed_items_by_pid.insert(original_pid.clone(), transformed_item.clone());
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (
                    ListChange::Push {
                        item: transformed_item,
                    },
                    current_length + 1,
                )
            }
            ListChange::Remove { id } => {
                transformed_items_by_pid.remove(&id);
                item_order.retain(|pid| *pid != id);
                // Translate the original PersistenceId to the mapped PersistenceId
                if let Some(mapped_pid) = pid_map.remove(&id) {
                    (
                        ListChange::Remove { id: mapped_pid },
                        current_length.saturating_sub(1),
                    )
                } else {
                    // Item not found in mapping - this shouldn't happen, but pass through
                    zoon::println!(
                        "[List/map] WARNING: Remove for unknown PersistenceId {:?}",
                        id
                    );
                    (ListChange::Remove { id }, current_length.saturating_sub(1))
                }
            }
            ListChange::Move {
                old_index,
                new_index,
            } => {
                // Update item order for Move
                if old_index < item_order.len() {
                    let pid = item_order.remove(old_index);
                    let insert_index = new_index.min(item_order.len());
                    if insert_index != new_index {
                        zoon::eprintln!(
                            "[actors-map-order-clamp] old_index={} new_index={} len={}",
                            old_index,
                            new_index,
                            item_order.len()
                        );
                    }
                    item_order.insert(insert_index, pid);
                    (
                        ListChange::Move {
                            old_index,
                            new_index: insert_index,
                        },
                        current_length,
                    )
                } else {
                    zoon::eprintln!(
                        "[actors-map-move-fallback] old_index={} new_index={} len={}",
                        old_index,
                        new_index,
                        item_order.len()
                    );
                    let current_items: Vec<_> = item_order
                        .iter()
                        .filter_map(|pid| transformed_items_by_pid.get(pid).cloned())
                        .collect();
                    (
                        ListChange::Replace {
                            items: Arc::from(current_items),
                        },
                        current_length,
                    )
                }
            }
            ListChange::Pop => {
                if let Some(popped_pid) = item_order.pop() {
                    transformed_items_by_pid.remove(&popped_pid);
                    pid_map.remove(&popped_pid);
                }
                (ListChange::Pop, current_length.saturating_sub(1))
            }
            ListChange::Clear => {
                pid_map.clear();
                transformed_items_by_pid.clear();
                item_order.clear();
                (ListChange::Clear, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActorContext, ActorHandle, ActorId, ConstructContext, ConstructInfo, ConstructStorage,
        LatestCombinator, List, ListChange, NamedChannel, REGISTRY, ScopeDestroyGuard,
        TaggedObject, Text, Value, ValueIdempotencyKey, Variable, VirtualFilesystem, create_actor,
        create_actor_forwarding, create_actor_from_future, create_constant_actor,
        create_registry_scope, current_emission_seq, list_instance_key, list_item_scope_id,
        retain_actor_handle, retain_actor_handles, values_equal_async,
    };
    use crate::engine::ValueMetadata;
    use crate::engine::stream;
    use boon::parser::{
        Persistence, PersistenceId, PersistenceStatus, Scope, SourceCode, span_at,
        static_expression,
    };
    use std::borrow::Cow;
    use std::collections::{BTreeMap, HashMap};
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;
    use zoon::future;
    use zoon::futures_channel::{mpsc, oneshot};
    use zoon::futures_util::FutureExt;
    use zoon::futures_util::SinkExt;
    use zoon::futures_util::StreamExt;

    fn block_on<F: Future>(future: F) -> F::Output {
        struct ThreadWake(std::thread::Thread);

        impl Wake for ThreadWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);

        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::park_timeout(Duration::from_millis(10)),
            }
        }
    }

    fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
        struct ThreadWake(std::thread::Thread);

        impl Wake for ThreadWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        future.as_mut().poll(&mut cx)
    }

    fn poll_pinned_once<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        struct ThreadWake(std::thread::Thread);

        impl Wake for ThreadWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        future.poll(&mut cx)
    }

    fn test_actor_handle(text: &'static str, emission_seq: u64) -> ActorHandle {
        let stored_state = super::ActorStoredState::new(8);
        stored_state.store(Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    format!("test.actor.{text}"),
                    None,
                    "test actor handle value",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed(text),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), emission_seq),
        ));

        ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state,
            persistence_id: PersistenceId::new(),
            is_constant: false,
            list_item_origin: None,
        }
    }

    fn test_constant_actor_handle(text: &'static str, emission_seq: u64) -> ActorHandle {
        let value = Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    format!("test.constant_actor.{text}"),
                    None,
                    "test constant actor handle value",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed(text),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), emission_seq),
        );
        let stored_state = super::ActorStoredState::with_initial_value(1, value.clone());

        ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state,
            persistence_id: PersistenceId::new(),
            is_constant: true,
            list_item_origin: None,
        }
    }

    fn test_empty_actor_handle() -> ActorHandle {
        ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state: super::ActorStoredState::new(8),
            persistence_id: PersistenceId::new(),
            is_constant: false,
            list_item_origin: None,
        }
    }

    #[test]
    fn list_item_scope_id_is_stable_for_same_inputs() {
        let pid = PersistenceId::new();
        let first = list_item_scope_id(3, pid);
        let second = list_item_scope_id(3, pid);

        assert_eq!(first, second);
    }

    #[test]
    fn list_item_scope_id_changes_for_index_and_persistence_id() {
        let first_pid = PersistenceId::new();
        let second_pid = PersistenceId::new();

        assert_ne!(
            list_item_scope_id(0, first_pid),
            list_item_scope_id(1, first_pid)
        );
        assert_ne!(
            list_item_scope_id(0, first_pid),
            list_item_scope_id(0, second_pid)
        );
    }

    #[test]
    fn value_reports_explicit_emission_sequence() {
        let value = Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new("test.emission_seq", None, "test emission seq")
                    .complete(super::ConstructType::Text),
                text: Cow::Borrowed("hello"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 42),
        );

        assert_eq!(value.emission_seq(), 42);
        assert!(value.is_emitted_at_or_before(42));
        assert!(value.is_emitted_after(41));
        assert_eq!(value.emission_seq(), 42);
    }

    #[test]
    fn explicit_emission_sequence_advances_local_floor() {
        let floor_before = current_emission_seq();
        let explicit_seq = floor_before + 5;

        let _metadata =
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), explicit_seq);

        assert!(
            current_emission_seq() > explicit_seq,
            "capturing an explicit external seq should advance the local floor past it"
        );
    }

    #[test]
    fn current_value_before_emission_reads_by_sequence_only() {
        block_on(async move {
            let stored_state = super::ActorStoredState::new(8);
            stored_state.store(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.before_emission.first",
                        None,
                        "test before emission first",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("first"),
                }),
                super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ));
            stored_state.store(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.before_emission.second",
                        None,
                        "test before emission second",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("second"),
                }),
                super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 8),
            ));

            let actor = ActorHandle {
                actor_id: ActorId {
                    index: 0,
                    generation: 0,
                },
                stored_state,
                persistence_id: PersistenceId::new(),
                is_constant: false,
                list_item_origin: None,
            };

            let value = actor
                .current_value_before_emission(8)
                .expect("snapshot-before-emission should return the prior value");
            let Value::Text(text, _) = value else {
                panic!("expected text value from before-emission lookup");
            };
            assert_eq!(text.text(), "first");
        });
    }

    #[test]
    fn forwarding_actor_exposes_current_value_via_direct_state() {
        block_on(async move {
            let actor = test_actor_handle("ready", 7);

            let current = actor
                .current_value()
                .expect("current value should be readable from direct actor state");
            let Value::Text(text, _) = current else {
                panic!("expected text value from forwarding actor");
            };
            assert_eq!(text.text(), "ready");
            assert_eq!(actor.version(), 1);
        });
    }

    #[test]
    fn forwarding_actor_stream_replays_direct_state_without_mailbox_subscription() {
        block_on(async move {
            let actor = test_actor_handle("ready", 7);
            let mut stream = actor.stream();

            let first = stream
                .next()
                .await
                .expect("direct-state subscription should replay the stored value");
            let Value::Text(text, _) = first else {
                panic!("expected text value from direct-state stream");
            };
            assert_eq!(text.text(), "ready");
        });
    }

    #[test]
    fn current_or_future_stream_seeds_current_value_and_replays_later_updates() {
        block_on(async move {
            let stored_state = super::ActorStoredState::new(8);
            stored_state.store(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.current_or_future.initial",
                        None,
                        "test current_or_future initial",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("first"),
                }),
                super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 1),
            ));

            let actor = ActorHandle {
                actor_id: ActorId {
                    index: 0,
                    generation: 0,
                },
                stored_state: stored_state.clone(),
                persistence_id: PersistenceId::new(),
                is_constant: false,
                list_item_origin: None,
            };

            let mut stream = actor.current_or_future_stream();

            let first = stream
                .next()
                .await
                .expect("seeded stream should emit the current value immediately");
            let Value::Text(text, _) = first else {
                panic!("expected initial text value from current_or_future stream");
            };
            assert_eq!(text.text(), "first");

            stored_state.store(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.current_or_future.update",
                        None,
                        "test current_or_future update",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("second"),
                }),
                super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 2),
            ));
            stored_state.mark_dropped();

            let second = stream
                .next()
                .await
                .expect("seeded stream should replay updates after the initial value");
            let Value::Text(text, _) = second else {
                panic!("expected updated text value from current_or_future stream");
            };
            assert_eq!(text.text(), "second");
        });
    }

    #[test]
    fn forwarding_constant_source_stores_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let source = test_constant_actor_handle("ready", 7);
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        super::connect_forwarding_current_and_future(forwarded.clone(), source);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(forwarded.actor_id)
                .expect("forwarding actor should stay registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "constant source forwarding should not retain an async loop"
            );
        });

        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarded actor should expose constant value immediately")
        else {
            panic!("expected forwarded constant text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn forwarding_dropped_source_skips_retained_task_when_no_future_subscription_exists() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let stored_state = super::ActorStoredState::new(8);
        stored_state.mark_dropped();
        let source = ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state,
            persistence_id: PersistenceId::new(),
            is_constant: false,
            list_item_origin: None,
        };
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        super::connect_forwarding_current_and_future(forwarded.clone(), source);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(forwarded.actor_id)
                .expect("forwarding actor should stay registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "dropped source without a current value should not retain a forwarding task"
            );
        });

        assert!(matches!(
            forwarded.current_value(),
            Err(super::CurrentValueError::NoValueYet)
        ));
    }

    #[test]
    fn forwarding_seed_helper_uses_direct_current_value_before_future_subscription() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let source = test_actor_handle("ready", 7);
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        let plan = super::seed_forwarding_actor_from_source_now(&forwarded, &source);

        assert!(matches!(
            plan,
            super::ForwardingSubscriptionPlan::SubscribeAfterVersion(1)
        ));
        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarding seed should store the current source value immediately")
        else {
            panic!("expected forwarded text value");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn forwarding_seed_helper_leaves_empty_forwarder_when_source_has_no_value_yet() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let source = ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state: super::ActorStoredState::new(8),
            persistence_id: PersistenceId::new(),
            is_constant: false,
            list_item_origin: None,
        };
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        let plan = super::seed_forwarding_actor_from_source_now(&forwarded, &source);

        assert!(matches!(
            plan,
            super::ForwardingSubscriptionPlan::SubscribeAfterVersion(0)
        ));
        assert!(matches!(
            forwarded.current_value(),
            Err(super::CurrentValueError::NoValueYet)
        ));
    }

    #[test]
    fn replay_all_seed_helper_replays_buffered_history_before_future_subscription() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let stored_state = super::ActorStoredState::new(8);
        stored_state.store(Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    "test.replay_all.first",
                    None,
                    "test replay-all first",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed("first"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 1),
        ));
        stored_state.store(Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    "test.replay_all.second",
                    None,
                    "test replay-all second",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed("second"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 2),
        ));
        let source = ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state,
            persistence_id: PersistenceId::new(),
            is_constant: false,
            list_item_origin: None,
        };
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        let plan = super::seed_forwarding_replay_all_from_source_now(&forwarded, &source);

        assert!(matches!(
            plan,
            super::ForwardingSubscriptionPlan::SubscribeAfterVersion(2)
        ));
        assert_eq!(
            forwarded.version(),
            2,
            "replay-all seed should publish the whole buffered history into the forwarder"
        );
        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarder should expose the latest replayed value")
        else {
            panic!("expected replayed text value");
        };
        assert_eq!(text.text(), "second");
    }

    #[test]
    fn forwarding_current_and_future_helper_seeds_current_and_catches_later_updates() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let stored_state = super::ActorStoredState::new(8);
        stored_state.store(Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    "test.forwarding_helper.initial",
                    None,
                    "test forwarding helper initial",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed("first"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 1),
        ));
        let source = ActorHandle {
            actor_id: ActorId {
                index: 0,
                generation: 0,
            },
            stored_state: stored_state.clone(),
            persistence_id: PersistenceId::new(),
            is_constant: false,
            list_item_origin: None,
        };
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        let mut helper = Box::pin(super::forward_current_and_future_from_source(
            forwarded.clone(),
            source,
        ));
        let Poll::Pending = poll_pinned_once(helper.as_mut()) else {
            panic!("forwarding helper should wait for later updates after seeding current value");
        };

        stored_state.store(Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    "test.forwarding_helper.update",
                    None,
                    "test forwarding helper update",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed("second"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 2),
        ));
        stored_state.mark_dropped();

        block_on(helper);

        assert_eq!(
            forwarded.version(),
            2,
            "forwarding helper should seed the current slot and then replay later updates"
        );
        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarded actor should expose the latest replayed value")
        else {
            panic!("expected forwarded helper text value");
        };
        assert_eq!(text.text(), "second");
    }

    #[test]
    fn backpressure_coordinator_queues_second_acquire_until_release() {
        let permit = super::BackpressureCoordinator::new();

        block_on(permit.acquire());

        let second_holder = permit.clone();
        let mut second_acquire = Box::pin(second_holder.acquire());
        let Poll::Pending = poll_once(second_acquire.as_mut()) else {
            panic!("second acquire should wait until the current permit is released");
        };

        permit.release();

        block_on(second_acquire);

        let third_holder = permit.clone();
        let mut third_acquire = Box::pin(third_holder.acquire());
        let Poll::Pending = poll_once(third_acquire.as_mut()) else {
            panic!("permit should be held by the second acquire until it is released");
        };

        permit.release();
        block_on(third_acquire);
        permit.release();
    }

    #[test]
    fn reference_connector_waiter_resolves_from_direct_state() {
        let connector = Arc::new(super::ReferenceConnector::new());
        let span = span_at(7);
        let pending_connector = connector.clone();
        let mut pending_reference = Box::pin(pending_connector.referenceable(span));

        let Poll::Pending = poll_once(pending_reference.as_mut()) else {
            panic!("missing reference should stay pending until registration");
        };

        connector.register_referenceable(span, test_actor_handle("referenceable", 3));

        let actor = block_on(pending_reference);
        let Value::Text(text, _) = actor
            .current_value()
            .expect("resolved reference should expose the registered actor")
        else {
            panic!("expected text actor from reference connector");
        };
        assert_eq!(text.text(), "referenceable");
    }

    #[test]
    fn link_connector_waiter_resolves_from_direct_state() {
        let connector = Arc::new(super::LinkConnector::new());
        let span = span_at(11);
        let scope = Scope::Nested("test.link.scope".to_owned());
        let pending_connector = connector.clone();
        let mut pending_sender = Box::pin(pending_connector.link_sender(span, scope.clone()));

        let Poll::Pending = poll_once(pending_sender.as_mut()) else {
            panic!("missing link sender should stay pending until registration");
        };

        let (sender, mut receiver) = NamedChannel::new("test.link", 1);
        connector.register_link(span, scope, sender);
        let resolved_sender = block_on(pending_sender);

        let value = Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new("test.link.value", None, "test link value")
                    .complete(super::ConstructType::Text),
                text: Cow::Borrowed("linked"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 4),
        );

        block_on(async move {
            resolved_sender
                .send(value)
                .await
                .expect("resolved link sender should remain usable");
            let Value::Text(text, _) = receiver
                .next()
                .await
                .expect("link channel should receive forwarded value")
            else {
                panic!("expected text value from link sender");
            };
            assert_eq!(text.text(), "linked");
        });
    }

    #[test]
    fn virtual_filesystem_uses_direct_state_without_sidecar_task() {
        let vfs = VirtualFilesystem::with_files(HashMap::from([
            ("dir/first.txt".to_owned(), "first".to_owned()),
            ("dir/nested/second.txt".to_owned(), "second".to_owned()),
        ]));

        block_on(async {
            assert_eq!(
                vfs.read_text("./dir/first.txt").await.as_deref(),
                Some("first")
            );
            assert!(vfs.exists("/dir/first.txt").await);

            let mut entries = vfs.list_directory("dir").await;
            entries.sort();
            assert_eq!(entries, vec!["first.txt".to_owned(), "nested".to_owned()]);

            vfs.write_text("dir/third.txt", "third".to_owned());
            assert_eq!(
                vfs.read_text("dir/third.txt").await.as_deref(),
                Some("third")
            );

            assert!(vfs.delete("dir/third.txt").await);
            assert!(!vfs.exists("dir/third.txt").await);
        });
    }

    #[test]
    fn output_valve_signal_uses_direct_subscriber_state() {
        let active = std::cell::Cell::new(true);
        let impulse_senders =
            std::cell::RefCell::new(smallvec::SmallVec::<[mpsc::Sender<()>; 4]>::new());
        let mut first = super::subscribe_output_valve_impulses(&active, &impulse_senders);
        let mut second = super::subscribe_output_valve_impulses(&active, &impulse_senders);

        super::broadcast_output_valve_impulse(&impulse_senders);

        block_on(async {
            assert_eq!(
                first.next().await,
                Some(()),
                "first subscriber should receive the broadcast impulse"
            );
            assert_eq!(
                second.next().await,
                Some(()),
                "second subscriber should receive the broadcast impulse"
            );
        });

        super::close_output_valve_subscribers(&active, &impulse_senders);

        block_on(async {
            assert_eq!(
                first.next().await,
                None,
                "subscribers should close when the output valve deactivates"
            );

            let mut late = super::subscribe_output_valve_impulses(&active, &impulse_senders);
            assert_eq!(
                late.next().await,
                None,
                "late subscribers should close immediately after deactivation"
            );
        });
    }

    #[test]
    fn construct_storage_uses_direct_state_without_sidecar_task() {
        let storage = Arc::new(super::ConstructStorage::in_memory_for_tests(BTreeMap::new()));
        let persistence_id = PersistenceId::new();

        storage.save_state(persistence_id, &"stored".to_owned());

        let loaded: Option<String> = block_on(storage.clone().load_state(persistence_id));
        assert_eq!(loaded.as_deref(), Some("stored"));

        let missing: Option<String> = block_on(storage.load_state(PersistenceId::new()));
        assert_eq!(missing, None);
    }

    #[test]
    fn pass_through_connector_uses_direct_state_without_sidecar_task() {
        let connector = super::PassThroughConnector::new();
        let key = super::PassThroughKey {
            persistence_id: PersistenceId::new(),
            scope: Scope::Nested("test.pass_through.scope".to_owned()),
        };
        let actor = test_actor_handle("pass-through", 5);
        let forwarder = test_actor_handle("forwarder", 6);
        let (sender, mut receiver) = mpsc::channel::<Value>(1);

        connector.register(key.clone(), sender, actor.clone());
        connector.add_forwarder(key.clone(), forwarder);

        let stored_actor = block_on(connector.get(key.clone()))
            .expect("registered pass-through actor should be retrievable");
        assert_eq!(stored_actor.actor_id(), actor.actor_id());

        let stored_sender = block_on(connector.get_sender(key.clone()))
            .expect("registered pass-through sender should be retrievable");
        let value = Value::Text(
            Arc::new(Text {
                construct_info: ConstructInfo::new(
                    "test.pass_through.value",
                    None,
                    "test pass-through value",
                )
                .complete(super::ConstructType::Text),
                text: Cow::Borrowed("forwarded"),
            }),
            super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 7),
        );
        connector.forward(key, value.clone());

        block_on(async move {
            let Value::Text(text, _) = receiver
                .next()
                .await
                .expect("pass-through forward should publish into stored sender")
            else {
                panic!("expected forwarded text value");
            };
            assert_eq!(text.text(), "forwarded");

            stored_sender
                .clone()
                .send(value)
                .await
                .expect("retrieved sender should remain usable");
        });
    }

    #[test]
    fn constant_actor_stream_replays_once_without_pending_tail() {
        let actor = test_constant_actor_handle("ready", 7);
        let mut stream = actor.stream();

        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.next()) else {
            panic!("constant actor stream should replay its stored value immediately");
        };
        assert_eq!(text.text(), "ready");

        let Poll::Ready(None) = poll_once(stream.next()) else {
            panic!("constant actor stream should end after replaying its stored value");
        };
    }

    #[test]
    fn constant_actor_stream_from_now_is_immediately_empty() {
        let actor = test_constant_actor_handle("ready", 7);
        let mut stream = actor.stream_from_now();

        let Poll::Ready(None) = poll_once(stream.next()) else {
            panic!("constant actor stream_from_now should have no future updates");
        };
    }

    #[test]
    fn snapshot_alias_reference_uses_direct_current_value_without_stream_actor() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            is_snapshot_context: true,
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let field_actor = Text::new_arc_value_actor(
            ConstructInfo::new(
                "test.snapshot_alias.field",
                None,
                "test snapshot alias field",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            "ready",
        );
        let root_actor = create_constant_actor(
            PersistenceId::new(),
            super::Object::new_value(
                ConstructInfo::new(
                    "test.snapshot_alias.object",
                    None,
                    "test snapshot alias object",
                ),
                construct_context,
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.snapshot_alias.variable",
                        None,
                        "test snapshot alias variable",
                    ),
                    "field",
                    field_actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );

        let alias_source = SourceCode::new("root field".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4), alias_source.slice(5, 10)],
            referenced_span: None,
        };
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor },
        );

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("snapshot alias should resolve immediately from direct current values")
        else {
            panic!("snapshot alias should resolve to text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn streaming_alias_reference_ready_root_collapses_without_wrapper_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let field_actor = Text::new_arc_value_actor(
            ConstructInfo::new(
                "test.streaming_alias.field",
                None,
                "test streaming alias field",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            "ready",
        );
        let root_actor = create_constant_actor(
            PersistenceId::new(),
            super::Object::new_value(
                ConstructInfo::new(
                    "test.streaming_alias.object",
                    None,
                    "test streaming alias object",
                ),
                construct_context,
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.streaming_alias.variable",
                        None,
                        "test streaming alias variable",
                    ),
                    "field",
                    field_actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );

        let alias_source = SourceCode::new("root field".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4), alias_source.slice(5, 10)],
            referenced_span: None,
        };
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(result_actor.actor_id)
                .expect("streaming alias actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "ready streaming alias root should not retain a wrapper task"
            );
        });

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("streaming alias should expose current value immediately")
        else {
            panic!("streaming alias should resolve to text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn materialize_value_now_materializes_ready_list_without_waiting() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let item_actor = Text::new_arc_value_actor(
            ConstructInfo::new(
                "test.materialize_value_now.item",
                None,
                "test materialize_value_now item",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            "ready",
        );
        let list_value = List::new_value(
            ConstructInfo::new(
                "test.materialize_value_now.list",
                None,
                "test materialize_value_now list",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            vec![item_actor],
        );

        let Value::List(materialized_list, _) =
            super::try_materialize_value_now(list_value, construct_context, actor_context)
                .expect("ready list should materialize synchronously")
        else {
            panic!("materialized value should stay a list");
        };

        let snapshot = block_on(materialized_list.snapshot());
        assert_eq!(
            snapshot.len(),
            1,
            "materialized list should keep its single item"
        );
        let Value::Text(text, _) = snapshot[0]
            .1
            .current_value()
            .expect("materialized item should expose current value immediately")
        else {
            panic!("materialized list item should stay text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn materialize_value_uses_direct_fast_path_for_ready_list() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let item_actor = Text::new_arc_value_actor(
            ConstructInfo::new(
                "test.materialize_value_fast_path.item",
                None,
                "test materialize_value fast path item",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            "ready",
        );
        let list_value = List::new_value(
            ConstructInfo::new(
                "test.materialize_value_fast_path.list",
                None,
                "test materialize_value fast path list",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            vec![item_actor],
        );

        let Poll::Ready(Value::List(materialized_list, _)) = poll_once(super::materialize_value(
            list_value,
            construct_context,
            actor_context,
        )) else {
            panic!("ready list materialization should complete on first poll");
        };

        let snapshot = block_on(materialized_list.snapshot());
        assert_eq!(
            snapshot.len(),
            1,
            "materialized list should keep its single item"
        );
        let Value::Text(text, _) = snapshot[0]
            .1
            .current_value()
            .expect("materialized item should expose current value immediately")
        else {
            panic!("materialized list item should stay text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn lazy_request_send_future_waits_when_request_channel_is_full() {
        let (request_tx, mut request_rx) = NamedChannel::new("test.lazy.request_backpressure", 1);

        block_on(async {
            let (first_response_tx, _first_response_rx) = oneshot::channel();
            request_tx
                .send(super::LazyValueRequest {
                    subscriber_id: 0,
                    start_cursor: 0,
                    response_tx: first_response_tx,
                })
                .await
                .expect("first lazy request should fill the channel");
        });

        let (second_response_tx, _second_response_rx) = oneshot::channel();
        let pending_send = super::lazy_request_send_future(
            request_tx.clone(),
            super::LazyValueRequest {
                subscriber_id: 1,
                start_cursor: 0,
                response_tx: second_response_tx,
            },
        );

        let Poll::Pending = poll_once(pending_send) else {
            panic!("full lazy request channel should backpressure instead of looking disconnected");
        };

        block_on(async {
            request_rx
                .next()
                .await
                .expect("filled lazy request should still be queued for later drain");
        });
    }

    #[test]
    fn lazy_actor_stream_from_now_skips_buffered_history() {
        let first = test_actor_handle("first", 1)
            .current_value()
            .expect("test actor should expose first value");
        let second = test_actor_handle("second", 2)
            .current_value()
            .expect("test actor should expose second value");
        let stored_state = super::ActorStoredState::new(64);
        let (request_tx, request_rx) = NamedChannel::new("test.lazy_value_actor.requests", 8);

        block_on(async move {
            let actor_loop = super::LazyValueActor::internal_loop(
                zoon::futures_util::stream::iter(vec![first.clone(), second.clone()]),
                request_rx,
                stored_state.clone(),
            );

            let test_future = async move {
                let (first_tx, first_rx) = oneshot::channel();
                request_tx
                    .clone()
                    .send(super::LazyValueRequest {
                        subscriber_id: 0,
                        start_cursor: 0,
                        response_tx: first_tx,
                    })
                    .await
                    .expect("first lazy request should send");

                let Value::Text(first_text, _) = first_rx
                    .await
                    .expect("first lazy response channel should resolve")
                    .expect("first lazy subscriber should receive first value")
                else {
                    panic!("expected first lazy value to be text");
                };
                assert_eq!(first_text.text(), "first");

                let (future_only_tx, future_only_rx) = oneshot::channel();
                request_tx
                    .clone()
                    .send(super::LazyValueRequest {
                        subscriber_id: 1,
                        start_cursor: stored_state.version() as usize,
                        response_tx: future_only_tx,
                    })
                    .await
                    .expect("future-only lazy request should send");

                let Value::Text(future_only_text, _) = future_only_rx
                    .await
                    .expect("future-only lazy response channel should resolve")
                    .expect("future-only lazy subscriber should receive next value")
                else {
                    panic!("expected future-only lazy value to be text");
                };
                assert_eq!(
                    future_only_text.text(),
                    "second",
                    "future-only lazy subscription should skip buffered history"
                );

                let (replay_tx, replay_rx) = oneshot::channel();
                request_tx
                    .clone()
                    .send(super::LazyValueRequest {
                        subscriber_id: 0,
                        start_cursor: 0,
                        response_tx: replay_tx,
                    })
                    .await
                    .expect("existing lazy subscriber replay request should send");

                let Value::Text(replay_text, _) = replay_rx
                    .await
                    .expect("existing lazy response channel should resolve")
                    .expect("existing lazy subscriber should receive buffered next value")
                else {
                    panic!("expected replayed lazy value to be text");
                };
                assert_eq!(replay_text.text(), "second");

                drop(request_tx);
            };

            let (_actor_loop_done, _) =
                zoon::futures_util::future::join(actor_loop, test_future).await;
        });
    }

    #[test]
    fn direct_state_value_waits_for_first_store_without_subscription_stream() {
        let actor = test_empty_actor_handle();
        let actor_for_store = actor.clone();
        let ready_value = test_actor_handle("ready", 12)
            .current_value()
            .expect("test helper should provide a ready text value");

        block_on(async move {
            let (value_result, ()) = zoon::futures_util::future::join(actor.value(), async move {
                actor_for_store.store_value_directly(ready_value);
            })
            .await;

            let Value::Text(text, _) = value_result.expect("value() should resolve after store")
            else {
                panic!("expected direct-state waiter result to be text");
            };
            assert_eq!(text.text(), "ready");
        });
    }

    fn store_value_directly_publishes_into_direct_actor_state() {
        let actor = test_actor_handle("ready", 7);
        let updated = Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.direct_state.publish",
                None,
                "test direct state publish",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            9,
            "updated",
        );

        actor.store_value_directly(updated);

        let current = actor
            .current_value()
            .expect("direct-state actor should expose stored value immediately");
        let Value::Text(text, _) = current else {
            panic!("expected text value from direct-state actor");
        };
        assert_eq!(text.text(), "updated");
        assert_eq!(actor.version(), 2);
    }

    #[test]
    fn retained_actor_handles_are_stored_on_owned_actor_slot() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);

        let holder = create_actor_forwarding(PersistenceId::new(), scope_id);
        let mut retained_a = test_actor_handle("retained-a", 13);
        retained_a.actor_id = ActorId {
            index: 101,
            generation: 0,
        };
        let mut retained_b = test_actor_handle("retained-b", 14);
        retained_b.actor_id = ActorId {
            index: 102,
            generation: 0,
        };

        retain_actor_handle(&holder, retained_a.clone());
        retain_actor_handles(&holder, vec![retained_b.clone()]);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(holder.actor_id)
                .expect("holder actor should stay registered");
            assert_eq!(owned_actor.retained_actors.len(), 2);
            assert_eq!(
                owned_actor.retained_actors[0].actor_id(),
                retained_a.actor_id()
            );
            assert_eq!(
                owned_actor.retained_actors[1].actor_id(),
                retained_b.actor_id()
            );
        });
    }

    #[test]
    #[ignore = "requires wasm/js runtime; Task::start_droppable touches js-sys on host"]
    fn stream_driven_actor_reuses_direct_state_slot_with_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor(
            zoon::futures_util::stream::once(async {
                Text::new_value_with_emission_seq(
                    ConstructInfo::new(
                        "test.stream_driven_actor.value",
                        None,
                        "test stream driven actor value",
                    ),
                    ConstructContext {
                        construct_storage: Arc::new(ConstructStorage::new("")),
                        virtual_fs: VirtualFilesystem::new(),
                        bridge_scope_id: None,
                        scene_ctx: None,
                    },
                    PersistenceId::new(),
                    11,
                    "streamed",
                )
            }),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("stream-driven actor should be registered");
            assert!(
                !owned_actor.retained_tasks.is_empty(),
                "stream-driven actor should retain its feeder task on the direct-state slot"
            );
        });

        block_on(async {
            let value = actor
                .value()
                .await
                .expect("stream-driven actor should surface emitted value");
            let Value::Text(text, _) = value else {
                panic!("expected text value from stream-driven actor");
            };
            assert_eq!(text.text(), "streamed");
        });
    }

    #[test]
    fn list_direct_state_serves_snapshot_and_diffs_without_query_channel() {
        let state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);

        state.record_change(&ListChange::Replace {
            items: Arc::from(vec![first]),
        });
        assert_eq!(state.version(), 1);
        assert_eq!(state.snapshot().len(), 1);

        state.record_change(&ListChange::Push { item: second });
        assert_eq!(state.version(), 2);
        assert_eq!(state.snapshot().len(), 2);

        let diffs = state.get_update_since(1);
        assert_eq!(
            diffs.len(),
            1,
            "expected incremental diffs from direct list state"
        );
    }

    #[test]
    fn list_direct_state_snapshot_fallback_is_encoded_as_replace_diff() {
        let state = super::ListStoredState::new(super::DiffHistoryConfig {
            max_entries: 16,
            snapshot_threshold: 0.0,
        });
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);

        state.record_change(&ListChange::Replace {
            items: Arc::from(vec![first]),
        });
        state.record_change(&ListChange::Push { item: second });

        let diffs = state.get_update_since(1);
        let [diff] = diffs.as_slice() else {
            panic!("expected snapshot fallback to emit a single replace diff");
        };
        let super::ListDiff::Replace { items } = diff.as_ref() else {
            panic!("expected snapshot fallback to be encoded as replace diff");
        };
        assert_eq!(items.len(), 2, "replace diff should contain full snapshot");
    }

    #[test]
    fn diff_history_trims_to_fixed_max_entries_without_subscriber_state() {
        let mut history = super::DiffHistory::new(super::DiffHistoryConfig {
            max_entries: 2,
            snapshot_threshold: 1.0,
        });

        history.add(super::ListDiff::Replace {
            items: vec![(super::ItemId::new(), test_actor_handle("first", 1))],
        });
        history.add(super::ListDiff::Insert {
            id: super::ItemId::new(),
            after: None,
            value: test_actor_handle("second", 2),
        });
        history.add(super::ListDiff::Insert {
            id: super::ItemId::new(),
            after: None,
            value: test_actor_handle("third", 3),
        });

        assert_eq!(
            history.diffs.len(),
            2,
            "history should trim to configured cap"
        );
        assert_eq!(
            history.oldest_version, 1,
            "oldest version should advance after trimming the first diff"
        );
    }

    #[test]
    fn static_persistent_list_save_path_does_not_spawn_retained_task() {
        let list = Arc::new(List::new_static(
            ConstructInfo::new(
                "test.static.persistent.list",
                None,
                "test static persistent list",
            )
            .complete(super::ConstructType::List),
            vec![test_actor_handle("first", 1)],
        ));
        let storage = Arc::new(ConstructStorage::new(""));

        let list_for_save = list.clone();
        block_on(async move {
            super::save_or_watch_persistent_list(list_for_save, storage, PersistenceId::new())
                .await;
        });

        assert!(
            list.retained_tasks.borrow().is_empty(),
            "static persistent list should not retain a hanging save watcher"
        );
    }

    #[test]
    fn ready_list_change_drain_marks_future_updates_only_for_pending_remainder() {
        assert!(
            !super::has_future_state_updates_after_ready_drain(
                super::ReadyListChangeDrainStatus::Ended
            ),
            "ended ready drain should not keep a future-update watcher alive"
        );
        assert!(
            super::has_future_state_updates_after_ready_drain(
                super::ReadyListChangeDrainStatus::Pending
            ),
            "pending ready drain should keep future-update watching enabled"
        );
    }

    #[test]
    fn persistent_list_direct_state_saver_tracks_versioned_updates_without_stream_subscription() {
        let storage = ConstructStorage::in_memory_for_tests(BTreeMap::new());
        let persistence_id = PersistenceId::new();
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.persistent.list.direct_state_saver",
            None,
            "test persistent list direct state saver",
        )
        .complete(super::ConstructType::List);
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let mut list = None;
        let mut list_arc_cache = None;

        block_on(async {
            super::process_live_list_change(
                &construct_info,
                &stored_state,
                &mut list,
                &mut list_arc_cache,
                &ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                },
                false,
            );

            let saved_version = super::save_persistent_list_snapshot_after_next_update(
                &stored_state,
                &storage,
                persistence_id,
                0,
            )
            .await;
            assert_eq!(saved_version, 1, "replace should save version 1");
            let saved_items = storage
                .load_state_now::<Vec<zoon::serde_json::Value>>(persistence_id)
                .expect("replace should be saved to storage");
            assert_eq!(
                saved_items.len(),
                1,
                "saved snapshot should contain first item"
            );

            super::process_live_list_change(
                &construct_info,
                &stored_state,
                &mut list,
                &mut list_arc_cache,
                &ListChange::Push {
                    item: second.clone(),
                },
                false,
            );

            let saved_version = super::save_persistent_list_snapshot_after_next_update(
                &stored_state,
                &storage,
                persistence_id,
                1,
            )
            .await;
            assert_eq!(saved_version, 2, "push should save version 2");
            let saved_items = storage
                .load_state_now::<Vec<zoon::serde_json::Value>>(persistence_id)
                .expect("push should update saved snapshot");
            assert_eq!(
                saved_items.len(),
                2,
                "saved snapshot should include pushed item"
            );
        });
    }

    #[test]
    fn disabled_storage_persistent_list_actor_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..Default::default()
        };

        let value = List::new_persistent_list_value(
            "test.disabled_storage.persistent_list".into(),
            Persistence {
                id: PersistenceId::new(),
                status: PersistenceStatus::NewOrChanged,
            },
            ValueIdempotencyKey::new(),
            actor_context,
            vec![test_actor_handle("first", 1)],
        );
        let actor = create_constant_actor(PersistenceId::new(), value, scope_id);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("persistent list actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "disabled storage should not retain a one-shot persistence init task"
            );
        });

        let Value::List(list, _) = actor
            .current_value()
            .expect("disabled-storage persistent list actor should be ready immediately")
        else {
            panic!("expected wrapped list value");
        };
        assert!(
            list.retained_tasks.borrow().is_empty(),
            "disabled storage static list should not retain a persistence watcher"
        );
    }

    #[test]
    fn restored_persistent_list_actor_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let persistence_id = PersistenceId::new();
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..Default::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::from([(
                persistence_id.to_string(),
                zoon::serde_json::json!(["restored"]),
            )]))),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let actor = List::new_arc_value_actor_with_persistence(
            ConstructInfo::new(
                "test.restored_storage.persistent_list",
                Some(Persistence {
                    id: persistence_id,
                    status: PersistenceStatus::NewOrChanged,
                }),
                "test restored persistent list",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            actor_context,
            vec![test_actor_handle("code-first", 1)],
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("restored persistent list actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "restored storage should not retain a one-shot persistence init task"
            );
        });

        let Value::List(list, _) = actor
            .current_value()
            .expect("restored persistent list actor should be ready immediately")
        else {
            panic!("expected wrapped list value");
        };
        assert!(
            list.retained_tasks.borrow().is_empty(),
            "restored static list should not retain a persistence watcher"
        );
        assert_eq!(
            block_on(list.snapshot()).len(),
            1,
            "restored persistent list should expose the loaded item immediately"
        );
    }

    #[test]
    fn list_value_to_json_serializes_ready_snapshot_without_change_stream_subscription() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..Default::default()
        };

        let list_value = List::new_value(
            ConstructInfo::new("test.list.to_json", None, "test list to_json"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context,
            vec![
                test_actor_handle("first", 1),
                test_actor_handle("second", 2),
            ],
        );

        let json = block_on(list_value.to_json());
        assert_eq!(
            json,
            zoon::serde_json::json!(["first", "second"]),
            "list JSON serialization should use the current list snapshot"
        );
    }

    #[test]
    fn ready_future_actor_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor_from_future(
            future::ready(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.ready_future_actor.value",
                        None,
                        "test ready future actor value",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("ready"),
                }),
                super::ValueMetadata::new(ValueIdempotencyKey::new()),
            )),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready future actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "ready futures should not retain a one-shot stream task"
            );
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("ready future actor should expose current value immediately")
        else {
            panic!("expected ready future actor to contain text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn ready_lazy_future_actor_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = super::create_actor_lazy_from_future(
            future::ready(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.ready_lazy_future_actor.value",
                        None,
                        "test ready lazy future actor value",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("ready"),
                }),
                super::ValueMetadata::new(ValueIdempotencyKey::new()),
            )),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready lazy future actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "ready lazy future actor should not retain a feeder task"
            );
            assert!(
                owned_actor.lazy_delegate.is_none(),
                "ready lazy future actor should collapse to a constant actor"
            );
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("ready lazy future actor should expose value immediately")
        else {
            panic!("expected ready lazy future text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn ready_single_value_lazy_stream_actor_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = super::create_actor_lazy(
            stream::once(future::ready(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.ready_single_value_lazy_stream_actor.value",
                        None,
                        "test ready single-value lazy stream actor value",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("ready"),
                }),
                super::ValueMetadata::new(ValueIdempotencyKey::new()),
            ))),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready single-value lazy stream actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "ready one-shot lazy streams should not retain a feeder task"
            );
            assert!(
                owned_actor.lazy_delegate.is_none(),
                "ready one-shot lazy streams should collapse to a constant actor"
            );
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("ready single-value lazy stream actor should expose value immediately")
        else {
            panic!("expected ready single-value lazy stream text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn ready_single_value_stream_actor_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor(
            stream::once(future::ready(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.ready_single_value_stream_actor.value",
                        None,
                        "test ready single-value stream actor value",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("ready"),
                }),
                super::ValueMetadata::new(ValueIdempotencyKey::new()),
            ))),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready single-value stream actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "ready one-shot streams should not retain a stream task"
            );
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("ready single-value stream actor should expose current value immediately")
        else {
            panic!("expected ready single-value stream actor to contain text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn ready_multi_value_stream_actor_drains_prefix_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor(
            stream::iter([
                Value::Text(
                    Arc::new(Text {
                        construct_info: ConstructInfo::new(
                            "test.ready_multi_value_stream_actor.first",
                            None,
                            "test ready multi-value stream actor first",
                        )
                        .complete(super::ConstructType::Text),
                        text: Cow::Borrowed("first"),
                    }),
                    super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 1),
                ),
                Value::Text(
                    Arc::new(Text {
                        construct_info: ConstructInfo::new(
                            "test.ready_multi_value_stream_actor.second",
                            None,
                            "test ready multi-value stream actor second",
                        )
                        .complete(super::ConstructType::Text),
                        text: Cow::Borrowed("second"),
                    }),
                    super::ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 2),
                ),
            ]),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready multi-value stream actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "finite ready stream prefixes should not retain a feeder task"
            );
        });

        assert_eq!(
            actor.version(),
            2,
            "ready multi-value stream should publish both immediate values into direct state"
        );
        let Value::Text(text, _) = actor
            .current_value()
            .expect("ready multi-value stream actor should expose latest value immediately")
        else {
            panic!("expected ready multi-value stream actor to contain text");
        };
        assert_eq!(text.text(), "second");

        block_on(async move {
            let replayed: Vec<String> = actor
                .stream()
                .take(2)
                .map(|value| match value {
                    Value::Text(text, _) => text.text().to_string(),
                    _ => panic!("expected replayed text values"),
                })
                .collect()
                .await;
            assert_eq!(replayed, vec!["first".to_string(), "second".to_string()]);
        });
    }

    #[test]
    fn ready_single_value_stream_actor_with_origin_uses_constant_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = super::create_actor_with_origin(
            stream::once(future::ready(Value::Text(
                Arc::new(Text {
                    construct_info: ConstructInfo::new(
                        "test.ready_stream_with_origin.value",
                        None,
                        "test ready stream with origin value",
                    )
                    .complete(super::ConstructType::Text),
                    text: Cow::Borrowed("ready"),
                }),
                super::ValueMetadata::new(ValueIdempotencyKey::new()),
            ))),
            PersistenceId::new(),
            super::ListItemOrigin {
                source_storage_key: "test:list_origin".to_string(),
                call_id: "call_0".to_string(),
            },
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready origin actor should be registered");
            assert!(
                owned_actor.retained_tasks.is_empty(),
                "ready single-value origin stream should not retain a feeder task"
            );
        });

        let origin = actor
            .list_item_origin()
            .expect("origin should stay attached on the constant path");
        assert_eq!(origin.source_storage_key, "test:list_origin");
        assert_eq!(origin.call_id, "call_0");

        let Value::Text(text, _) = actor
            .current_value()
            .expect("ready origin actor should expose value immediately")
        else {
            panic!("expected ready origin text");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn list_direct_state_initialization_waiter_resolves_after_first_replace() {
        let state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let waiter = state
            .wait_until_initialized()
            .expect("uninitialized list should create a waiter");

        state.record_change(&ListChange::Replace {
            items: Arc::from(vec![test_actor_handle("first", 1)]),
        });
        state.mark_initialized();

        block_on(async move {
            waiter
                .await
                .expect("initialization waiter should resolve after first replace");
        });

        assert!(state.is_initialized());
        assert!(state.wait_until_initialized().is_none());
    }

    #[test]
    fn list_direct_state_update_waiter_resolves_after_change() {
        let state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        state.record_change(&ListChange::Replace {
            items: Arc::from(vec![test_actor_handle("first", 1)]),
        });
        state.mark_initialized();

        let waiter = state
            .wait_for_update_after(1)
            .expect("subscriber at current version should get an update waiter");

        state.record_change(&ListChange::Push {
            item: test_actor_handle("second", 2),
        });

        block_on(async move {
            waiter
                .await
                .expect("update waiter should resolve after a later change");
        });

        assert_eq!(state.version(), 2);
        assert!(state.wait_for_update_after(1).is_none());
    }

    #[test]
    fn list_diff_stream_replays_direct_state_without_subscription_wrapper() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let list = Arc::new(List::new_static(
            ConstructInfo::new("test.list.diff_stream", None, "test list diff stream")
                .complete(super::ConstructType::List),
            vec![first, second],
        ));

        block_on(async move {
            let mut stream = list.clone().diff_stream(0);
            let Some(diffs) = stream.next().await else {
                panic!("expected initial diff update from direct list state");
            };
            let [diff] = diffs.as_slice() else {
                panic!("expected a single replace diff");
            };
            let super::ListDiff::Replace { items } = diff.as_ref() else {
                panic!("expected replace diff for initial static snapshot");
            };
            assert_eq!(items.len(), 2, "replace diff should include current items");
        });
    }

    #[test]
    fn tagged_objects_compare_structurally_when_fields_match() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..Default::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_person_id = |label: &str, id_text: &str| {
            let id_text = id_text.to_owned();
            let id_actor = create_constant_actor(
                PersistenceId::new(),
                Text::new_value(
                    ConstructInfo::new(format!("{label}.id_value"), None, "test id value"),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    id_text,
                ),
                scope_id,
            );
            TaggedObject::new_value(
                ConstructInfo::new(format!("{label}.tagged"), None, "test tagged object"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "PersonId",
                vec![Variable::new_arc(
                    ConstructInfo::new(format!("{label}.var"), None, "test id var"),
                    "id",
                    id_actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            )
        };

        block_on(async move {
            let left = make_person_id("left", "same-id");
            let right = make_person_id("right", "same-id");
            let different = make_person_id("different", "different-id");

            assert!(values_equal_async(&left, &right).await);
            assert!(!values_equal_async(&left, &different).await);
        });
    }

    #[test]
    fn list_snapshot_and_late_stream_include_initial_items() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let list = Arc::new(List::new_static(
            ConstructInfo::new("test.list", None, "test list").complete(super::ConstructType::List),
            vec![first.clone(), second.clone()],
        ));

        block_on(async move {
            let snapshot = list.snapshot().await;
            assert_eq!(snapshot.len(), 2, "snapshot should include initial items");
            assert_eq!(
                list.version(),
                1,
                "static list should initialize immediately"
            );

            let mut stream = list.clone().stream();
            let Some(ListChange::Replace { items }) = stream.next().await else {
                panic!("late list stream should yield initial Replace");
            };
            assert_eq!(items.len(), 2, "late stream should replay current items");
            assert!(
                matches!(poll_once(stream.next()), Poll::Pending),
                "static late stream should now sit on the shared subscriber path after the initial replay"
            );
        });
    }

    #[test]
    fn list_binding_source_stream_uses_current_list_without_replaying_stale_history() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let old_list = Arc::new(List::new_static(
            ConstructInfo::new("test.list.binding.old", None, "test list binding old")
                .complete(super::ConstructType::List),
            vec![test_actor_handle("old", 1)],
        ));
        let current_list = Arc::new(List::new_static(
            ConstructInfo::new(
                "test.list.binding.current",
                None,
                "test list binding current",
            )
            .complete(super::ConstructType::List),
            vec![test_actor_handle("current", 2)],
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Value::List(
            old_list,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));
        source_actor.store_value_directly(Value::List(
            current_list.clone(),
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let mut stream = std::pin::pin!(
            super::ListBindingFunction::source_list_current_and_future_values(source_actor, None,)
        );

        let Poll::Ready(Some(emitted_list)) = poll_once(stream.as_mut().next()) else {
            panic!("list binding source stream should emit the current list immediately");
        };
        assert_eq!(
            list_instance_key(&emitted_list),
            list_instance_key(&current_list),
            "list binding source stream should skip stale buffered list history"
        );
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "list binding source stream should wait for future list replacements after the current list"
        );
    }

    #[test]
    fn list_binding_current_list_stream_replays_single_initial_replace_without_stale_history() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let old_list = Arc::new(List::new_static(
            ConstructInfo::new("test.list.map.old", None, "test list map old")
                .complete(super::ConstructType::List),
            vec![test_actor_handle("old", 1)],
        ));
        let current_list = Arc::new(List::new_static(
            ConstructInfo::new("test.list.map.current", None, "test list map current")
                .complete(super::ConstructType::List),
            vec![test_actor_handle("current", 2)],
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Value::List(
            old_list,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));
        source_actor.store_value_directly(Value::List(
            current_list.clone(),
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let Poll::Ready(Some(emitted_list)) = poll_once(
            super::ListBindingFunction::source_list_current_and_future_values(source_actor, None)
                .next(),
        ) else {
            panic!("list binding source should emit the current list immediately");
        };
        assert_eq!(
            list_instance_key(&emitted_list),
            list_instance_key(&current_list),
            "list binding source should skip stale buffered source list history"
        );

        let mut stream = emitted_list.stream();
        let Poll::Ready(Some(ListChange::Replace { items })) = poll_once(stream.next()) else {
            panic!("current list stream should replay its initial Replace once");
        };
        assert_eq!(
            items.len(),
            1,
            "current list stream should replay only the current items"
        );
        assert!(
            matches!(poll_once(stream.next()), Poll::Pending),
            "shared list stream should now wait for future updates without a duplicate initial replay"
        );
    }

    #[test]
    fn live_list_change_loop_without_output_valve_applies_incremental_changes() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new("test.live.list.loop", None, "test live list loop")
            .complete(super::ConstructType::List);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);

        block_on(async move {
            change_sender
                .send(ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                })
                .await
                .expect("initial replace should reach helper loop");
            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should reach helper loop");
            drop(change_sender);

            super::run_live_list_change_loop_core(
                construct_info,
                stored_state.clone(),
                change_receiver,
                None,
                None,
                None,
                (),
            )
            .await;

            let snapshot = stored_state.snapshot();
            assert_eq!(snapshot.len(), 2, "snapshot should include pushed item");
            assert_eq!(
                stored_state.version(),
                2,
                "replace + push should advance version"
            );

            let diffs = stored_state.get_update_since(1);
            let [diff] = diffs.as_slice() else {
                panic!("expected single incremental diff");
            };
            let super::ListDiff::Insert { value, .. } = diff.as_ref() else {
                panic!("expected push to be represented as an insert diff");
            };
            let Value::Text(text, _) = value
                .current_value()
                .expect("pushed actor should expose current value")
            else {
                panic!("expected pushed item to be a text actor");
            };
            assert_eq!(text.text(), "second");
        });
    }

    #[test]
    fn live_list_change_loop_core_continues_from_existing_state() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.live.list.drain.continuation",
            None,
            "test live list drain continuation",
        )
        .complete(super::ConstructType::List);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);

        block_on(async move {
            let mut list = None;
            let mut list_arc_cache = None;
            super::process_live_list_change(
                &construct_info,
                &stored_state,
                &mut list,
                &mut list_arc_cache,
                &ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                },
                false,
            );

            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should reach continuation helper");
            drop(change_sender);

            super::run_live_list_change_loop_core(
                construct_info,
                stored_state.clone(),
                change_receiver,
                None,
                list,
                list_arc_cache,
                (),
            )
            .await;

            let snapshot = stored_state.snapshot();
            assert_eq!(
                snapshot.len(),
                2,
                "continuation helper should keep prior list state when draining incremental changes"
            );
            assert_eq!(
                stored_state.version(),
                2,
                "replace + incremental continuation should advance version"
            );
        });
    }

    #[test]
    fn live_list_change_loop_core_drains_diffs_after_output_valve_closes() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.live.list.output.valve.close",
            None,
            "test live list output valve close",
        )
        .complete(super::ConstructType::List);
        let (subscriber_sender, mut subscriber_receiver) =
            NamedChannel::new("test.list.change_subscriber", 8);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let (impulse_sender, impulse_receiver) = mpsc::channel::<()>(8);

        stored_state.register_change_subscriber(subscriber_sender);

        block_on(async move {
            change_sender
                .send(ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                })
                .await
                .expect("initial replace should reach loop core");
            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should stay queued for loop core");
            drop(change_sender);
            drop(impulse_sender);

            super::run_live_list_change_loop_core(
                construct_info,
                stored_state.clone(),
                change_receiver,
                Some(impulse_receiver.boxed_local()),
                None,
                None,
                (),
            )
            .await;

            let Some(ListChange::Replace { items }) =
                subscriber_receiver.next().now_or_never().flatten()
            else {
                panic!("subscriber should receive replace after valve closes");
            };
            assert_eq!(items.len(), 1, "replace should keep first item snapshot");

            let Some(ListChange::Replace { items }) =
                subscriber_receiver.next().now_or_never().flatten()
            else {
                panic!("subscriber should receive final snapshot after valve closes");
            };
            assert_eq!(
                items.len(),
                2,
                "final snapshot should include queued changes after valve closes"
            );

            let snapshot = stored_state.snapshot();
            assert_eq!(
                snapshot.len(),
                2,
                "loop core should preserve full list state after valve close handoff"
            );
            assert_eq!(
                stored_state.version(),
                2,
                "replace + push should advance version after valve close handoff"
            );
        });
    }

    #[test]
    fn static_list_output_valve_watcher_rebroadcasts_snapshot_without_live_change_loop() {
        let first = test_actor_handle("first", 1);
        let construct_info = ConstructInfo::new(
            "test.static.list.output.valve",
            None,
            "test static list output valve",
        )
        .complete(super::ConstructType::List);
        let (subscriber_sender, mut subscriber_receiver) =
            NamedChannel::new("test.static.list.change_subscriber", 8);
        let (mut impulse_sender, impulse_receiver) = mpsc::channel::<()>(8);
        let list = super::List::new_static(construct_info.clone(), vec![first]);
        let stored_state = list.stored_state.clone();
        let initial_items: Vec<_> = stored_state
            .snapshot()
            .into_iter()
            .map(|(_, actor)| actor)
            .collect();
        let initial_items_arc = Arc::from(initial_items.as_slice());

        stored_state.register_change_subscriber(subscriber_sender);

        block_on(async move {
            let Some(ListChange::Replace { items }) =
                subscriber_receiver.next().now_or_never().flatten()
            else {
                panic!("subscriber should receive initial static replace immediately");
            };
            assert_eq!(
                items.len(),
                1,
                "initial replay should contain the static item"
            );

            impulse_sender
                .send(())
                .await
                .expect("impulse should reach static list watcher");
            drop(impulse_sender);

            super::run_live_list_change_loop_core(
                construct_info,
                stored_state.clone(),
                stream::empty::<ListChange>(),
                Some(impulse_receiver.boxed_local()),
                Some(initial_items),
                Some(initial_items_arc),
                (),
            )
            .await;

            let Some(ListChange::Replace { items }) =
                subscriber_receiver.next().now_or_never().flatten()
            else {
                panic!("subscriber should receive snapshot rebroadcast after impulse");
            };
            assert_eq!(
                items.len(),
                1,
                "rebroadcast should preserve the static snapshot"
            );

            assert!(
                subscriber_receiver
                    .next()
                    .now_or_never()
                    .flatten()
                    .is_none(),
                "watcher should not rebroadcast the same static snapshot again on close"
            );
        });
    }

    #[test]
    fn immediate_single_replace_change_stream_builds_static_list_without_retained_task() {
        let first = test_actor_handle("first", 1);
        let list = super::List::new_with_change_stream(
            ConstructInfo::new(
                "test.immediate.single.replace.list",
                None,
                "test immediate single replace list",
            ),
            ActorContext::default(),
            stream::once(future::ready(ListChange::Replace {
                items: Arc::from(vec![first]),
            })),
            (),
        );

        block_on(async move {
            let snapshot = list.snapshot().await;
            assert_eq!(
                snapshot.len(),
                1,
                "list should initialize from the ready replace"
            );
            assert!(
                list.retained_tasks.borrow().is_empty(),
                "single ready replace should stay on the static list path"
            );
        });
    }

    #[test]
    fn ready_change_prefix_drain_applies_multiple_immediate_changes() {
        struct ReadyListChangeStream {
            changes: std::collections::VecDeque<ListChange>,
        }

        impl zoon::Stream for ReadyListChangeStream {
            type Item = ListChange;

            fn poll_next(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                std::task::Poll::Ready(self.changes.pop_front())
            }
        }

        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.immediate.ready.change.prefix.list",
            None,
            "test immediate ready change prefix list",
        )
        .complete(super::ConstructType::List);
        let mut change_stream = ReadyListChangeStream {
            changes: std::collections::VecDeque::from(vec![
                ListChange::Replace {
                    items: Arc::from(vec![first]),
                },
                ListChange::Push { item: second },
            ]),
        }
        .boxed_local();

        block_on(async move {
            let (status, initial_items) = super::drain_ready_list_change_prefix_now(
                &construct_info,
                &stored_state,
                &mut change_stream,
            );

            assert!(matches!(status, super::ReadyListChangeDrainStatus::Ended));
            let initial_items = initial_items.expect("ready prefix should initialize list state");
            assert_eq!(
                initial_items.len(),
                2,
                "ready prefix drain should retain the full initialized list snapshot"
            );
            assert_eq!(
                stored_state.snapshot().len(),
                2,
                "ready prefix drain should apply all immediate changes to direct list state"
            );
            assert_eq!(
                stored_state.version(),
                2,
                "replace + push should advance the direct list state during sync drain"
            );
            assert!(
                super::poll_stream_once(change_stream.as_mut()).is_ready(),
                "ready prefix drain should consume the entire finite immediate stream"
            );
        });
    }

    #[test]
    fn live_list_change_loop_from_initialized_state_continues_after_ready_initial_replace() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.ready.initial.replace.list",
            None,
            "test ready initial replace list",
        )
        .complete(super::ConstructType::List);

        block_on(async move {
            let mut list = None;
            let mut list_arc_cache = None;
            super::process_live_list_change(
                &construct_info,
                &stored_state,
                &mut list,
                &mut list_arc_cache,
                &ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                },
                false,
            );

            assert_eq!(
                stored_state.snapshot().len(),
                1,
                "state should expose the ready initial replace before draining the remainder"
            );
            assert!(
                stored_state.is_initialized(),
                "initial replace should initialize state"
            );

            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should reach continuation helper");
            drop(change_sender);

            super::run_live_list_change_loop_core(
                construct_info,
                stored_state.clone(),
                change_receiver,
                None,
                Some(vec![first.clone()]),
                Some(list_arc_cache.expect("initial replace should populate list cache")),
                (),
            )
            .await;

            let snapshot = stored_state.snapshot();
            assert_eq!(
                snapshot.len(),
                2,
                "continuation helper should preserve the ready initial replace and append later changes"
            );
            assert_eq!(
                stored_state.version(),
                2,
                "replace + push should advance version in the initialized continuation path"
            );
        });
    }

    #[test]
    #[ignore = "requires wasm/js runtime; live actor loops still touch js-sys statics on host"]
    fn active_list_without_output_valve_forwards_changes_without_fake_impulse_stream() {
        let (change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let list = Arc::new(List::new_with_change_stream(
            ConstructInfo::new("test.live.list", None, "test live list"),
            ActorContext::default(),
            change_receiver,
            (),
        ));
        let mut change_sender = change_sender;
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);

        block_on(async move {
            let mut stream = list.clone().stream();

            change_sender
                .send(ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                })
                .await
                .expect("initial replace should reach live list");

            let Some(ListChange::Replace { items }) = stream.next().await else {
                panic!("live list stream should yield initial replace");
            };
            assert_eq!(items.len(), 1);

            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should reach live list");

            let Some(ListChange::Push { item }) = stream.next().await else {
                panic!("live list stream should yield incremental push");
            };
            let Value::Text(text, _) = item
                .current_value()
                .expect("pushed actor should expose current value")
            else {
                panic!("expected text actor value");
            };
            assert_eq!(text.text(), "second");
        });
    }

    #[test]
    #[ignore = "requires wasm/js runtime; host lib tests still touch js-sys statics"]
    fn latest_combinator_ignores_older_late_arrivals() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..Default::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let left_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let right_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let latest_actor = LatestCombinator::new_arc_value_actor(
            ConstructInfo::new("test.latest", None, "test latest combinator"),
            construct_context.clone(),
            actor_context,
            vec![left_actor.clone(), right_actor.clone()],
        );

        block_on(async move {
            let mut updates = latest_actor.stream_from_now();

            right_actor.store_value_directly(Text::new_value_with_emission_seq(
                ConstructInfo::new("test.value.newer", None, "newer"),
                construct_context.clone(),
                PersistenceId::new(),
                20,
                "commit",
            ));

            let first = updates
                .next()
                .await
                .expect("latest should emit the newer value");
            let Value::Text(first_text, _) = first else {
                panic!("latest should emit a text value first");
            };
            assert_eq!(first_text.text(), "commit");

            left_actor.store_value_directly(Text::new_value_with_emission_seq(
                ConstructInfo::new("test.value.older", None, "older"),
                construct_context,
                PersistenceId::new(),
                10,
                "change",
            ));

            std::thread::sleep(Duration::from_millis(50));

            let current = latest_actor
                .current_value()
                .expect("latest should keep a current value");
            let Value::Text(current_text, _) = current else {
                panic!("latest should keep a text value");
            };
            assert_eq!(current_text.text(), "commit");
        });
    }
}
