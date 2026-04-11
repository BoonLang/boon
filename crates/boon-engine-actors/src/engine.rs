use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::pin::{Pin, pin};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, Wake, Waker};

use super::evaluator::{FunctionRegistry, evaluate_static_expression_with_runtime_registry};
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
    /// In wasm debug mode: times out after 5 seconds with error log.
    /// In host debug mode: falls back to the bounded send path because `zoon::Timer`
    /// is browser-backed and aborts non-wasm test runs.
    #[cfg(all(feature = "debug-channels", target_arch = "wasm32"))]
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

    /// Async send for host debug mode or production mode.
    #[cfg(any(
        all(feature = "debug-channels", not(target_arch = "wasm32")),
        not(feature = "debug-channels")
    ))]
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
    /// Actor source completed without ever producing a value.
    SourceEndedWithoutValue,
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
    /// Deferred startup hook so pending lazy actors don't retain a feeder task
    /// until the first real demand arrives.
    start_loop: Rc<RefCell<Option<Box<dyn FnOnce()>>>>,
}

impl LazyValueActor {
    fn new_unstarted() -> (Self, mpsc::Receiver<LazyValueRequest>) {
        let (request_tx, request_rx) =
            NamedChannel::new("lazy_value_actor.requests", LAZY_ACTOR_REQUEST_CAPACITY);

        (
            Self {
                request_tx,
                subscriber_counter: std::sync::atomic::AtomicUsize::new(0),
                start_loop: Rc::new(RefCell::new(None)),
            },
            request_rx,
        )
    }

    fn install_start_loop(&self, start_loop: Box<dyn FnOnce()>) {
        let previous = self.start_loop.replace(Some(start_loop));
        assert!(
            previous.is_none(),
            "lazy actor start loop should only be installed once"
        );
    }

    fn ensure_started(&self) {
        if let Some(start_loop) = self.start_loop.borrow_mut().take() {
            start_loop();
        }
    }

    /// The internal loop that handles demand-driven value delivery.
    ///
    /// This loop owns all state (buffer, cursors) - no locks needed.
    /// Values are only pulled from the source when a subscriber requests one.
    async fn internal_loop<S: Stream<Item = Value> + 'static>(
        source_stream: S,
        mut request_rx: mpsc::Receiver<LazyValueRequest>,
        runtime_target_actor_id: Option<ActorId>,
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
                        if let Some(actor_id) = runtime_target_actor_id {
                            enqueue_actor_value_on_runtime_queue_by_id(actor_id, value.clone());
                        } else {
                            stored_state.store(value.clone());
                        }
                        buffer.push(value.clone());
                        Some(value)
                    }
                    None => {
                        source_exhausted = true;
                        if let Some(actor_id) = runtime_target_actor_id {
                            enqueue_actor_source_end_on_runtime_queue_by_id(actor_id);
                        } else {
                            stored_state.set_has_future_state_updates(false);
                        }
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
            started: false,
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
    started: bool,
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

        if !this.started {
            this.actor.ensure_started();
            this.started = true;
        }

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

fn take_list_async_sources_now(list_owner_key: usize) -> bool {
    REGISTRY.with(|reg| {
        let mut reg = match reg.try_borrow_mut() {
            Ok(reg) => reg,
            Err(_) => return false,
        };
        reg.take_async_sources_for_list(list_owner_key);
        true
    })
}

fn drain_pending_list_async_source_cleanups() {
    loop {
        let Some(list_owner_key) =
            PENDING_LIST_ASYNC_SOURCE_CLEANUPS.with(|pending| pending.borrow_mut().pop())
        else {
            break;
        };

        if !take_list_async_sources_now(list_owner_key) {
            PENDING_LIST_ASYNC_SOURCE_CLEANUPS
                .with(|pending| pending.borrow_mut().push(list_owner_key));
            break;
        }
    }
}

fn drain_pending_registry_cleanups() {
    drain_pending_scope_destroys();
    drain_pending_list_async_source_cleanups();
}

pub fn create_registry_scope(parent: Option<ScopeId>) -> ScopeId {
    let scope_id = REGISTRY.with(|reg| reg.borrow_mut().create_scope(parent));
    drain_pending_registry_cleanups();
    scope_id
}

pub(crate) fn create_registry_scope_with_registry(
    reg: &mut ActorRegistry,
    parent: Option<ScopeId>,
) -> ScopeId {
    reg.create_scope(parent)
}

fn create_child_registry_scope_with_optional_registry(
    reg: Option<&mut ActorRegistry>,
    parent: Option<ScopeId>,
) -> Option<ScopeId> {
    let parent_scope = parent?;
    Some(match reg {
        Some(reg) => create_registry_scope_with_registry(reg, Some(parent_scope)),
        None => create_registry_scope(Some(parent_scope)),
    })
}

#[track_caller]
fn insert_actor_and_drain(scope_id: ScopeId, owned_actor: OwnedActor) -> ActorId {
    let caller = std::panic::Location::caller();
    let actor_id = REGISTRY.with(|reg| {
        reg.try_borrow_mut()
            .unwrap_or_else(|_| {
                panic!(
                    "insert_actor_and_drain nested REGISTRY borrow from {}",
                    caller
                )
            })
            .insert_actor(scope_id, owned_actor)
    });
    drain_pending_registry_cleanups();
    actor_id
}

fn insert_actor_with_registry(
    reg: &mut ActorRegistry,
    scope_id: ScopeId,
    owned_actor: OwnedActor,
) -> ActorId {
    reg.insert_actor(scope_id, owned_actor)
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
#[derive(Clone, Copy)]
enum OutputValveDirectEvent {
    Impulse,
    Closed,
}

type OutputValveDirectSubscriber = Rc<dyn Fn(OutputValveDirectEvent) -> bool>;

pub struct ActorOutputValveSignal {
    active: Rc<Cell<bool>>,
    impulse_senders: Rc<RefCell<SmallVec<[mpsc::Sender<()>; 4]>>>,
    direct_subscribers: Rc<RefCell<SmallVec<[OutputValveDirectSubscriber; 4]>>>,
    _task: TaskHandle,
}

fn broadcast_output_valve_impulse(impulse_senders: &RefCell<SmallVec<[mpsc::Sender<()>; 4]>>) {
    impulse_senders.borrow_mut().retain_mut(|impulse_sender| {
        // try_send for bounded(1) - drop if already signaled
        impulse_sender.try_send(()).is_ok()
    });
}

fn broadcast_output_valve_direct_subscribers(
    direct_subscribers: &RefCell<SmallVec<[OutputValveDirectSubscriber; 4]>>,
    event: OutputValveDirectEvent,
) {
    direct_subscribers
        .borrow_mut()
        .retain(|subscriber| subscriber(event));
}

fn close_output_valve_subscribers(
    active: &Cell<bool>,
    impulse_senders: &RefCell<SmallVec<[mpsc::Sender<()>; 4]>>,
    direct_subscribers: &RefCell<SmallVec<[OutputValveDirectSubscriber; 4]>>,
) {
    active.set(false);
    impulse_senders.borrow_mut().clear();
    direct_subscribers.borrow_mut().clear();
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

fn register_output_valve_direct_subscriber(
    active: &Cell<bool>,
    direct_subscribers: &RefCell<SmallVec<[OutputValveDirectSubscriber; 4]>>,
    subscriber: OutputValveDirectSubscriber,
) {
    if active.get() {
        direct_subscribers.borrow_mut().push(subscriber);
    }
}

impl ActorOutputValveSignal {
    pub fn new(impulse_stream: impl Stream<Item = ()> + 'static) -> Self {
        let active = Rc::new(Cell::new(true));
        let impulse_senders = Rc::new(RefCell::new(SmallVec::<[mpsc::Sender<()>; 4]>::new()));
        let direct_subscribers = Rc::new(RefCell::new(
            SmallVec::<[OutputValveDirectSubscriber; 4]>::new(),
        ));
        let active_for_task = active.clone();
        let impulse_senders_for_task = impulse_senders.clone();
        let direct_subscribers_for_task = direct_subscribers.clone();
        Self {
            active,
            impulse_senders,
            direct_subscribers,
            _task: Task::start_droppable(async move {
                let mut impulse_stream = pin!(impulse_stream.fuse());
                while impulse_stream.next().await.is_some() {
                    broadcast_output_valve_impulse(impulse_senders_for_task.as_ref());
                    broadcast_output_valve_direct_subscribers(
                        direct_subscribers_for_task.as_ref(),
                        OutputValveDirectEvent::Impulse,
                    );
                }
                broadcast_output_valve_direct_subscribers(
                    direct_subscribers_for_task.as_ref(),
                    OutputValveDirectEvent::Closed,
                );
                close_output_valve_subscribers(
                    active_for_task.as_ref(),
                    impulse_senders_for_task.as_ref(),
                    direct_subscribers_for_task.as_ref(),
                );
            }),
        }
    }

    pub fn stream(&self) -> mpsc::Receiver<()> {
        subscribe_output_valve_impulses(self.active.as_ref(), self.impulse_senders.as_ref())
    }

    fn register_direct_subscriber(&self, subscriber: OutputValveDirectSubscriber) {
        register_output_valve_direct_subscriber(
            self.active.as_ref(),
            self.direct_subscribers.as_ref(),
            subscriber,
        );
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
        Self::new_link_arc_impl(None, construct_info, name, actor_context, persistence_id)
    }

    pub(crate) fn new_link_arc_with_registry(
        reg: &mut ActorRegistry,
        construct_info: ConstructInfo,
        name: impl Into<Cow<'static, str>>,
        actor_context: ActorContext,
        persistence_id: parser::PersistenceId,
    ) -> Arc<Self> {
        Self::new_link_arc_impl(
            Some(reg),
            construct_info,
            name,
            actor_context,
            persistence_id,
        )
    }

    fn new_link_arc_impl(
        reg: Option<&mut ActorRegistry>,
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

        let value_actor = match reg {
            Some(reg) => create_actor_with_registry(reg, logged_receiver, persistence_id, scope_id),
            None => create_actor(logged_receiver, persistence_id, scope_id),
        };
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
    fn try_resolve_field_actor_from_value(value: &Value, field_name: &str) -> Option<ActorHandle> {
        match value {
            Value::Object(object, _) => object.variable(field_name).map(|var| var.value_actor()),
            Value::TaggedObject(tagged_object, _) => tagged_object
                .variable(field_name)
                .map(|var| var.value_actor()),
            _ => None,
        }
    }

    fn try_resolve_constant_string_field_path_actor(
        root_actor: &ActorHandle,
        field_path: &[String],
    ) -> Option<ActorHandle> {
        if !root_actor.is_constant || field_path.is_empty() {
            return None;
        }

        let mut current_actor = root_actor.clone();
        for (index, field_name) in field_path.iter().map(String::as_str).enumerate() {
            let current_value = current_actor.current_value().ok()?;
            let next_actor = Self::try_resolve_field_actor_from_value(&current_value, field_name)?;

            if index + 1 == field_path.len() {
                return Some(next_actor);
            }

            if !next_actor.is_constant {
                return None;
            }

            current_actor = next_actor;
        }

        None
    }

    fn try_resolve_constant_field_path_actor(
        root_actor: &ActorHandle,
        parts: &[parser::StrSlice],
    ) -> Option<ActorHandle> {
        let field_path = parts
            .iter()
            .map(|part| part.as_str().to_owned())
            .collect::<Vec<_>>();
        Self::try_resolve_constant_string_field_path_actor(root_actor, &field_path)
    }

    fn attach_dynamic_field_alias_source(
        reg: &mut ActorRegistry,
        forwarding_actor: &ActorHandle,
        field_actor: ActorHandle,
        generation: u64,
        active_generation: Rc<Cell<u64>>,
        subscription_after_seq: Option<EmissionSeq>,
    ) {
        let has_lazy_delegate = reg
            .get_actor(field_actor.actor_id)
            .and_then(|owned_actor| owned_actor.lazy_delegate.clone())
            .is_some();

        if !has_lazy_delegate {
            let field_state = field_actor.stored_state.clone();
            field_state.register_direct_subscriber(
                reg,
                Rc::new({
                    let forwarding_actor = forwarding_actor.clone();
                    let active_generation = active_generation.clone();
                    move |reg, value| {
                        if !forwarding_actor.stored_state.is_alive() {
                            return false;
                        }
                        if active_generation.get() != generation {
                            return false;
                        }
                        match value {
                            Some(value) => {
                                if subscription_after_seq
                                    .is_some_and(|seq| value.is_emitted_at_or_before(seq))
                                {
                                    return true;
                                }
                                reg.enqueue_actor_mailbox_value(
                                    forwarding_actor.actor_id,
                                    value.clone(),
                                )
                            }
                            None => reg.enqueue_actor_mailbox_clear(forwarding_actor.actor_id),
                        }
                        true
                    }
                }),
            );
            return;
        }

        let Some(scope_id) = reg
            .get_actor(forwarding_actor.actor_id)
            .map(|owned_actor| owned_actor.scope_id)
        else {
            return;
        };

        let forwarding_actor_for_task = forwarding_actor.clone();
        start_retained_scope_task_with_registry(reg, scope_id, async move {
            let mut field_stream = std::pin::pin!(field_actor.current_or_future_stream());
            while let Some(value) = field_stream.next().await {
                if !forwarding_actor_for_task.stored_state.is_alive()
                    || active_generation.get() != generation
                {
                    break;
                }
                if subscription_after_seq.is_some_and(|seq| value.is_emitted_at_or_before(seq)) {
                    continue;
                }
                enqueue_actor_value_on_runtime_queue(&forwarding_actor_for_task, value);
            }
        });
    }

    fn attach_alias_chain_rebuild_watcher(
        reg: &mut ActorRegistry,
        forwarding_actor: &ActorHandle,
        root_actor: &ActorHandle,
        watched_actor: ActorHandle,
        field_path: Arc<Vec<String>>,
        generation: u64,
        active_generation: Rc<Cell<u64>>,
        subscription_after_seq: Option<EmissionSeq>,
    ) {
        let has_lazy_delegate = reg
            .get_actor(watched_actor.actor_id)
            .and_then(|owned_actor| owned_actor.lazy_delegate.clone())
            .is_some();

        if !has_lazy_delegate {
            let watched_state = watched_actor.stored_state.clone();
            let registration_version = watched_actor.version();
            watched_state.register_direct_subscriber(
                reg,
                Rc::new({
                    let forwarding_actor = forwarding_actor.clone();
                    let root_actor = root_actor.clone();
                    let watched_state = watched_state.clone();
                    let active_generation = active_generation.clone();
                    move |reg, _value| {
                        if !forwarding_actor.stored_state.is_alive() {
                            return false;
                        }
                        if active_generation.get() != generation {
                            return false;
                        }
                        if watched_state.version() == registration_version {
                            return watched_state.is_alive();
                        }

                        let next_generation = active_generation.get().wrapping_add(1);
                        active_generation.set(next_generation);
                        Self::rebuild_direct_state_field_alias_chain(
                            reg,
                            &forwarding_actor,
                            &root_actor,
                            field_path.clone(),
                            next_generation,
                            active_generation.clone(),
                            subscription_after_seq,
                        );
                        false
                    }
                }),
            );
            return;
        }

        let Some(scope_id) = reg
            .get_actor(forwarding_actor.actor_id)
            .map(|owned_actor| owned_actor.scope_id)
        else {
            return;
        };
        let forwarding_actor_for_task = forwarding_actor.clone();
        let root_actor_for_task = root_actor.clone();
        start_retained_scope_task_with_registry(reg, scope_id, async move {
            let mut stream = std::pin::pin!(watched_actor.current_or_future_stream());
            let mut is_first = true;
            while stream.next().await.is_some() {
                if !forwarding_actor_for_task.stored_state.is_alive()
                    || active_generation.get() != generation
                {
                    break;
                }
                if is_first {
                    is_first = false;
                    continue;
                }
                let next_generation = active_generation.get().wrapping_add(1);
                active_generation.set(next_generation);
                REGISTRY.with(|reg| {
                    let mut reg = reg.borrow_mut();
                    Self::rebuild_direct_state_field_alias_chain(
                        &mut reg,
                        &forwarding_actor_for_task,
                        &root_actor_for_task,
                        field_path.clone(),
                        next_generation,
                        active_generation.clone(),
                        subscription_after_seq,
                    );
                    reg.drain_runtime_ready_queue();
                });
                break;
            }
        });
    }

    fn rebuild_direct_state_field_alias_chain(
        reg: &mut ActorRegistry,
        forwarding_actor: &ActorHandle,
        root_actor: &ActorHandle,
        field_path: Arc<Vec<String>>,
        generation: u64,
        active_generation: Rc<Cell<u64>>,
        subscription_after_seq: Option<EmissionSeq>,
    ) {
        let clear_forwarding = |reg: &mut ActorRegistry| {
            if forwarding_actor.stored_state.is_alive() && active_generation.get() == generation {
                reg.enqueue_actor_mailbox_clear(forwarding_actor.actor_id);
            }
        };
        let mut current_actor = root_actor.clone();

        for (index, field_name) in field_path.iter().enumerate() {
            let current_is_root_lazy =
                index == 0 && current_actor.lazy_delegate_with_registry(reg).is_some();
            if !current_is_root_lazy {
                Self::attach_alias_chain_rebuild_watcher(
                    reg,
                    forwarding_actor,
                    root_actor,
                    current_actor.clone(),
                    field_path.clone(),
                    generation,
                    active_generation.clone(),
                    subscription_after_seq,
                );
            }

            let Ok(current_value) = current_actor.current_value() else {
                clear_forwarding(reg);
                return;
            };
            if index > 0
                && subscription_after_seq
                    .is_some_and(|seq| current_value.is_emitted_at_or_before(seq))
            {
                clear_forwarding(reg);
                return;
            }
            let Some(next_actor) =
                Self::try_resolve_field_actor_from_value(&current_value, field_name)
            else {
                clear_forwarding(reg);
                return;
            };

            if index + 1 == field_path.len() {
                Self::attach_dynamic_field_alias_source(
                    reg,
                    forwarding_actor,
                    next_actor,
                    generation,
                    active_generation,
                    subscription_after_seq,
                );
                return;
            }

            current_actor = next_actor;
        }

        clear_forwarding(reg);
    }

    fn try_create_direct_state_field_alias_actor_from_field_path_with_registry(
        mut reg: Option<&mut ActorRegistry>,
        actor_context: ActorContext,
        root_actor: ActorHandle,
        field_path: Vec<String>,
    ) -> Option<ActorHandle> {
        let root_has_lazy_delegate = match reg {
            Some(ref reg) => root_actor.lazy_delegate_with_registry(reg).is_some(),
            None => root_actor.lazy_delegate().is_some(),
        };
        if field_path.is_empty() {
            return None;
        }

        let field_path = Arc::new(field_path);
        let scope_id = actor_context.scope_id();
        let subscription_after_seq = actor_context.subscription_after_seq;
        let forwarding_actor = match reg.as_deref_mut() {
            Some(reg) => {
                create_actor_forwarding_with_registry(reg, parser::PersistenceId::new(), scope_id)
            }
            None => create_actor_forwarding(parser::PersistenceId::new(), scope_id),
        };
        retain_actor_handles_with_registry(
            reg.as_deref_mut(),
            &forwarding_actor,
            std::iter::once(root_actor.clone()),
        );

        let active_generation = Rc::new(Cell::new(0u64));
        if root_has_lazy_delegate {
            let forwarding_actor_for_task = forwarding_actor.clone();
            let root_actor_for_task = root_actor.clone();
            let field_path_for_task = field_path.clone();
            let active_generation_for_task = active_generation.clone();
            let root_stream = root_actor.current_or_future_stream();
            start_retained_scope_task(scope_id, async move {
                let mut root_stream = std::pin::pin!(root_stream);
                while root_stream.next().await.is_some() {
                    if !forwarding_actor_for_task.stored_state.is_alive() {
                        break;
                    }

                    let next_generation = active_generation_for_task.get().wrapping_add(1);
                    active_generation_for_task.set(next_generation);
                    REGISTRY.with(|reg| {
                        let mut reg = reg.borrow_mut();
                        Self::rebuild_direct_state_field_alias_chain(
                            &mut reg,
                            &forwarding_actor_for_task,
                            &root_actor_for_task,
                            field_path_for_task.clone(),
                            next_generation,
                            active_generation_for_task.clone(),
                            subscription_after_seq,
                        );
                        reg.drain_runtime_ready_queue();
                    });
                }
            });

            return Some(forwarding_actor);
        }

        match reg {
            Some(reg) => {
                let next_generation = active_generation.get().wrapping_add(1);
                active_generation.set(next_generation);
                Self::rebuild_direct_state_field_alias_chain(
                    reg,
                    &forwarding_actor,
                    &root_actor,
                    field_path,
                    next_generation,
                    active_generation,
                    subscription_after_seq,
                );
                reg.drain_runtime_ready_queue();
            }
            None => REGISTRY.with(|reg| {
                let mut reg = reg.borrow_mut();
                let next_generation = active_generation.get().wrapping_add(1);
                active_generation.set(next_generation);
                Self::rebuild_direct_state_field_alias_chain(
                    &mut reg,
                    &forwarding_actor,
                    &root_actor,
                    field_path,
                    next_generation,
                    active_generation,
                    subscription_after_seq,
                );
                reg.drain_runtime_ready_queue();
            }),
        }

        Some(forwarding_actor)
    }

    fn try_create_direct_state_field_alias_actor_from_field_path(
        actor_context: ActorContext,
        root_actor: ActorHandle,
        field_path: Vec<String>,
    ) -> Option<ActorHandle> {
        Self::try_create_direct_state_field_alias_actor_from_field_path_with_registry(
            None,
            actor_context,
            root_actor,
            field_path,
        )
    }

    fn try_create_direct_state_field_alias_actor_from_field_path_with_optional_registry(
        reg: Option<&mut ActorRegistry>,
        actor_context: ActorContext,
        root_actor: ActorHandle,
        field_path: Vec<String>,
    ) -> Option<ActorHandle> {
        Self::try_create_direct_state_field_alias_actor_from_field_path_with_registry(
            reg,
            actor_context,
            root_actor,
            field_path,
        )
    }

    fn try_create_direct_state_field_alias_actor(
        actor_context: ActorContext,
        root_actor: ActorHandle,
        parts: &[parser::StrSlice],
    ) -> Option<ActorHandle> {
        Self::try_create_direct_state_field_alias_actor_from_field_path(
            actor_context,
            root_actor,
            parts
                .iter()
                .map(|part| part.as_str().to_owned())
                .collect::<Vec<_>>(),
        )
    }

    pub(crate) fn new_arc_value_actor_with_registry(
        reg: &mut ActorRegistry,
        actor_context: ActorContext,
        alias: static_expression::Alias,
        root_value_actor: impl Future<Output = ActorHandle> + 'static,
    ) -> ActorHandle {
        Self::new_arc_value_actor_impl(Some(reg), actor_context, alias, root_value_actor)
    }

    pub fn new_arc_value_actor(
        actor_context: ActorContext,
        alias: static_expression::Alias,
        root_value_actor: impl Future<Output = ActorHandle> + 'static,
    ) -> ActorHandle {
        Self::new_arc_value_actor_impl(None, actor_context, alias, root_value_actor)
    }

    fn new_arc_value_actor_impl(
        mut reg: Option<&mut ActorRegistry>,
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
        let mut root_value_actor = Some(Box::pin(root_value_actor));
        let mut ready_root_actor = None;

        if let Poll::Ready(actor) = poll_future_once(
            root_value_actor
                .as_mut()
                .expect("root future should be present")
                .as_mut(),
        ) {
            if use_snapshot {
                if let Some(value) = try_resolve_snapshot_alias_value_now(&actor, &parts_vec) {
                    return match reg.as_deref_mut() {
                        Some(reg) => create_constant_actor_with_registry(
                            reg,
                            parser::PersistenceId::new(),
                            value,
                            scope_id,
                        ),
                        None => {
                            create_constant_actor(parser::PersistenceId::new(), value, scope_id)
                        }
                    };
                }
            } else {
                if subscription_after_seq.is_none() {
                    if let Some(field_actor) =
                        Self::try_resolve_constant_field_path_actor(&actor, &parts_vec)
                    {
                        return field_actor;
                    }
                }
                if let Some(alias_actor) = (!parts_vec.is_empty())
                    .then(|| {
                        Self::try_create_direct_state_field_alias_actor_from_field_path_with_optional_registry(
                            reg.as_deref_mut(),
                            actor_context.clone(),
                            actor.clone(),
                            parts_vec
                                .iter()
                                .map(|part| part.as_str().to_owned())
                                .collect::<Vec<_>>(),
                        )
                    })
                    .flatten()
                {
                    return alias_actor;
                }
            }
            ready_root_actor = Some(actor);
            root_value_actor = None;
        }

        if !use_snapshot && parts_vec.is_empty() {
            return match ready_root_actor {
                Some(actor) => actor,
                None => match reg.as_deref_mut() {
                    Some(reg) => create_actor_forwarding_from_future_source_with_registry(
                        reg,
                        async move {
                            Some(
                                root_value_actor
                                    .expect("plain alias root future should still be pending")
                                    .await,
                            )
                        },
                        parser::PersistenceId::new(),
                        scope_id,
                    ),
                    None => create_actor_forwarding_from_future_source(
                        async move {
                            Some(
                                root_value_actor
                                    .expect("plain alias root future should still be pending")
                                    .await,
                            )
                        },
                        parser::PersistenceId::new(),
                        scope_id,
                    ),
                },
            };
        }

        if !use_snapshot && ready_root_actor.is_none() {
            let waiting_root_actor = match reg.as_deref_mut() {
                Some(reg) => create_actor_forwarding_from_future_source_with_registry(
                    reg,
                    async move {
                        Some(
                            root_value_actor
                                .take()
                                .expect("field alias root future should still be pending")
                                .await,
                        )
                    },
                    parser::PersistenceId::new(),
                    scope_id,
                ),
                None => create_actor_forwarding_from_future_source(
                    async move {
                        Some(
                            root_value_actor
                                .take()
                                .expect("field alias root future should still be pending")
                                .await,
                        )
                    },
                    parser::PersistenceId::new(),
                    scope_id,
                ),
            };
            return Self::try_create_direct_state_field_alias_actor_from_field_path_with_optional_registry(
                reg.as_deref_mut(),
                actor_context.clone(),
                waiting_root_actor,
                parts_vec
                    .iter()
                    .map(|part| part.as_str().to_owned())
                    .collect(),
            )
            .expect("late-root field aliases should always support the direct waiting path");
        }

        // For snapshot context (THEN/WHEN bodies), we get a single value.
        // For streaming context, we get continuous updates.
        let mut value_stream: LocalBoxStream<'static, Value> = if use_snapshot {
            match ready_root_actor {
                Some(actor) => stream::once(async move { actor.value().await })
                    .filter_map(|v| future::ready(v.ok()))
                    .boxed_local(),
                None => stream::once(async move {
                    let actor = root_value_actor
                        .expect("snapshot alias root future should still be pending")
                        .await;
                    actor.value().await
                })
                .filter_map(|v| future::ready(v.ok()))
                .boxed_local(),
            }
        } else {
            match ready_root_actor {
                Some(actor) => actor.stream(),
                None => stream::once(async move {
                    root_value_actor
                        .expect("streaming alias root future should still be pending")
                        .await
                })
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
        match reg.as_deref_mut() {
            Some(reg) => create_actor_with_registry(
                reg,
                value_stream,
                parser::PersistenceId::new(),
                scope_id,
            ),
            None => create_actor(value_stream, parser::PersistenceId::new(), scope_id),
        }
    }
}

pub(crate) fn create_field_path_alias_actor_with_registry(
    reg: Option<&mut ActorRegistry>,
    actor_context: ActorContext,
    root_actor: ActorHandle,
    field_path: Vec<String>,
) -> Option<ActorHandle> {
    if field_path.is_empty() {
        return Some(root_actor);
    }

    if let Some(field_actor) =
        VariableOrArgumentReference::try_resolve_constant_string_field_path_actor(
            &root_actor,
            &field_path,
        )
    {
        return Some(field_actor);
    }

    VariableOrArgumentReference::try_create_direct_state_field_alias_actor_from_field_path_with_optional_registry(
        reg,
        actor_context,
        root_actor,
        field_path,
    )
}

pub(crate) fn create_field_path_alias_actor(
    actor_context: ActorContext,
    root_actor: ActorHandle,
    field_path: Vec<String>,
) -> Option<ActorHandle> {
    create_field_path_alias_actor_with_registry(None, actor_context, root_actor, field_path)
}

fn poll_future_once<F: Future + ?Sized>(mut future: Pin<&mut F>) -> Poll<F::Output> {
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
pub struct ReferenceConnector {
    referenceables: RefCell<HashMap<parser::Span, ActorHandle>>,
    pending_referenceables: RefCell<HashMap<parser::Span, Vec<oneshot::Sender<ActorHandle>>>>,
    pending_referenceable_actors: RefCell<HashMap<parser::Span, Vec<ActorHandle>>>,
}

impl ReferenceConnector {
    pub fn new() -> Self {
        Self {
            referenceables: RefCell::new(HashMap::new()),
            pending_referenceables: RefCell::new(HashMap::new()),
            pending_referenceable_actors: RefCell::new(HashMap::new()),
        }
    }

    pub fn try_referenceable_now(&self, span: parser::Span) -> Option<ActorHandle> {
        self.referenceables.borrow().get(&span).cloned()
    }

    pub fn register_referenceable(&self, span: parser::Span, actor: ActorHandle) {
        self.register_referenceable_with_registry(None, span, actor);
    }

    pub(crate) fn register_referenceable_with_registry(
        &self,
        mut reg: Option<&mut ActorRegistry>,
        span: parser::Span,
        actor: ActorHandle,
    ) {
        if let Some(waiters) = self.pending_referenceables.borrow_mut().remove(&span) {
            for waiter in waiters {
                if waiter.send(actor.clone()).is_err() {
                    zoon::eprintln!("Failed to send referenceable actor from reference connector");
                }
            }
        }
        if let Some(waiting_actors) = self.pending_referenceable_actors.borrow_mut().remove(&span) {
            for waiting_actor in waiting_actors {
                match reg.as_deref_mut() {
                    Some(reg) => connect_forwarding_current_and_future_with_registry(
                        Some(reg),
                        waiting_actor,
                        actor.clone(),
                    ),
                    None => connect_forwarding_current_and_future(waiting_actor, actor.clone()),
                }
            }
        }
        self.referenceables.borrow_mut().insert(span, actor);
    }

    pub fn referenceable_or_waiting_actor(
        &self,
        span: parser::Span,
        scope_id: ScopeId,
    ) -> ActorHandle {
        self.referenceable_or_waiting_actor_with_registry(None, span, scope_id)
    }

    pub(crate) fn referenceable_or_waiting_actor_with_registry(
        &self,
        reg: Option<&mut ActorRegistry>,
        span: parser::Span,
        scope_id: ScopeId,
    ) -> ActorHandle {
        if let Some(actor) = self.try_referenceable_now(span) {
            return actor;
        }

        let waiting_actor = match reg {
            Some(reg) => {
                create_actor_forwarding_with_registry(reg, parser::PersistenceId::new(), scope_id)
            }
            None => create_actor_forwarding(parser::PersistenceId::new(), scope_id),
        };
        self.pending_referenceable_actors
            .borrow_mut()
            .entry(span)
            .or_default()
            .push(waiting_actor.clone());
        waiting_actor
    }

    // @TODO is &self enough?
    pub async fn referenceable(self: Arc<Self>, span: parser::Span) -> ActorHandle {
        if let Some(actor) = self.try_referenceable_now(span) {
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

// --- FunctionCall ---

pub struct FunctionCall {}

impl FunctionCall {
    pub(crate) fn new_arc_value_actor_with_registry<FR: Stream<Item = Value> + 'static>(
        reg: &mut ActorRegistry,
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
        Self::new_arc_value_actor_impl(
            Some(reg),
            construct_info,
            construct_context,
            actor_context,
            definition,
            arguments,
        )
    }

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
        Self::new_arc_value_actor_impl(
            None,
            construct_info,
            construct_context,
            actor_context,
            definition,
            arguments,
        )
    }

    fn new_arc_value_actor_impl<FR: Stream<Item = Value> + 'static>(
        reg: Option<&mut ActorRegistry>,
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
            match reg {
                Some(reg) => create_actor_lazy_with_registry(
                    reg,
                    combined_stream,
                    parser::PersistenceId::new(),
                    actor_context.scope_id(),
                ),
                None => create_actor_lazy(
                    combined_stream,
                    parser::PersistenceId::new(),
                    actor_context.scope_id(),
                ),
            }
        } else {
            let scope_id = actor_context.scope_id();
            match reg {
                Some(reg) => create_actor_with_registry(
                    reg,
                    combined_stream,
                    parser::PersistenceId::new(),
                    scope_id,
                ),
                None => create_actor(combined_stream, parser::PersistenceId::new(), scope_id),
            }
        }
    }
}

// --- LatestCombinator ---

pub struct LatestCombinator {}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(crate = "serde")]
struct LatestCombinatorState {
    input_emission_keys: BTreeMap<usize, EmissionSeq>,
}

fn select_latest_value(latest_values: &[Option<Value>]) -> Option<Value> {
    latest_values
        .iter()
        .enumerate()
        .filter_map(|(input_index, current)| current.as_ref().map(|current| (input_index, current)))
        .max_by(|(lhs_idx, lhs), (rhs_idx, rhs)| {
            lhs.emission_seq()
                .cmp(&rhs.emission_seq())
                .then_with(|| rhs_idx.cmp(lhs_idx))
        })
        .map(|(_, selected)| selected.clone())
}

fn apply_latest_input_value(
    state: &mut LatestCombinatorState,
    latest_values: &mut [Option<Value>],
    index: usize,
    value: Value,
) -> Option<Value> {
    let emission = value.emission_seq();
    let skip_value = state
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
        return None;
    }

    if latest_values[index]
        .as_ref()
        .is_some_and(|previous| previous.emission_seq() > emission)
    {
        return None;
    }
    latest_values[index] = Some(value);
    select_latest_value(latest_values)
}

impl LatestCombinator {
    fn try_create_direct_state_actor_with_registry(
        mut reg: Option<&mut ActorRegistry>,
        _construct_context: ConstructContext,
        actor_context: ActorContext,
        inputs: Vec<ActorHandle>,
        persistent_id: parser::PersistenceId,
        storage: Arc<ConstructStorage>,
    ) -> Option<ActorHandle> {
        let state = Rc::new(RefCell::new(
            storage
                .load_state_now::<LatestCombinatorState>(persistent_id)
                .unwrap_or_default(),
        ));
        let latest_values = Rc::new(RefCell::new(vec![None::<Value>; inputs.len()]));

        let mut current_values: Vec<_> = inputs
            .iter()
            .enumerate()
            .filter_map(|(index, input)| input.current_value().ok().map(|value| (index, value)))
            .collect();
        current_values.sort_by(|(lhs_idx, lhs), (rhs_idx, rhs)| {
            lhs.emission_seq()
                .cmp(&rhs.emission_seq())
                .then_with(|| lhs_idx.cmp(rhs_idx))
        });

        let mut initial_selected = None;
        for (index, value) in current_values {
            let maybe_selected = {
                let mut state_ref = state.borrow_mut();
                let mut latest_values_ref = latest_values.borrow_mut();
                apply_latest_input_value(&mut state_ref, &mut latest_values_ref, index, value)
            };
            if let Some(selected) = maybe_selected {
                storage.save_state(persistent_id, &*state.borrow());
                initial_selected = Some(selected);
            }
        }

        let result_actor = try_create_direct_state_subscriber_actor_with_registry(
            reg.as_deref_mut(),
            inputs,
            parser::PersistenceId::new(),
            actor_context.scope_id(),
            {
                let state = state.clone();
                let latest_values = latest_values.clone();
                let storage = storage.clone();
                move |index, value| {
                    let maybe_selected = {
                        let mut state_ref = state.borrow_mut();
                        let mut latest_values_ref = latest_values.borrow_mut();
                        apply_latest_input_value(
                            &mut state_ref,
                            &mut latest_values_ref,
                            index,
                            value,
                        )
                    };

                    maybe_selected.inspect(|_| {
                        storage.save_state(persistent_id, &*state.borrow());
                    })
                }
            },
        )?;

        if let Some(selected) = initial_selected {
            result_actor.store_value_directly_with_registry(reg.as_deref_mut(), selected);
        }

        Some(result_actor)
    }

    fn try_create_direct_state_actor(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        inputs: Vec<ActorHandle>,
        persistent_id: parser::PersistenceId,
        storage: Arc<ConstructStorage>,
    ) -> Option<ActorHandle> {
        Self::try_create_direct_state_actor_with_registry(
            None,
            construct_context,
            actor_context,
            inputs,
            persistent_id,
            storage,
        )
    }

    pub(crate) fn new_arc_value_actor_with_registry(
        reg: &mut ActorRegistry,
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        inputs: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        let inputs: Vec<ActorHandle> = inputs.into();
        let persistent_id = construct_info
            .persistence
            .map(|p| p.id)
            .unwrap_or_else(parser::PersistenceId::new);
        let storage = construct_context.construct_storage.clone();

        if let Some(actor) = Self::try_create_direct_state_actor_with_registry(
            Some(reg),
            construct_context.clone(),
            actor_context.clone(),
            inputs.clone(),
            persistent_id,
            storage,
        ) {
            return actor;
        }

        Self::new_arc_value_actor(construct_info, construct_context, actor_context, inputs)
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        inputs: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        let construct_info = construct_info.complete(ConstructType::LatestCombinator);
        let inputs: Vec<ActorHandle> = inputs.into();
        // If persistence is None (e.g., for dynamically evaluated expressions),
        // generate a fresh persistence ID at runtime
        let persistent_id = construct_info
            .persistence
            .map(|p| p.id)
            .unwrap_or_else(parser::PersistenceId::new);
        let storage = construct_context.construct_storage.clone();

        if let Some(actor) = Self::try_create_direct_state_actor(
            construct_context.clone(),
            actor_context.clone(),
            inputs.clone(),
            persistent_id,
            storage.clone(),
        ) {
            return actor;
        }

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
                                storage
                                    .clone()
                                    .load_state::<LatestCombinatorState>(persistent_id)
                                    .await,
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
                (
                    LatestCombinatorState::default(),
                    vec![None::<Value>; input_count],
                ),
                move |(state, latest_values), (new_state, index, value)| {
                    if let Some(new_state) = new_state {
                        *state = new_state;
                    }
                    let selected = apply_latest_input_value(state, latest_values, index, value);
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
    pub(crate) fn try_create_direct_state_actor_with_registry<F>(
        reg: Option<&mut ActorRegistry>,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
        operation: Rc<F>,
    ) -> Option<ActorHandle>
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Value + 'static,
    {
        type LatestBinaryValues = Rc<RefCell<(Option<Value>, Option<Value>)>>;
        let latest_values: LatestBinaryValues = Rc::new(RefCell::new((None, None)));
        try_create_direct_state_subscriber_actor_with_registry(
            reg,
            vec![operand_a, operand_b],
            parser::PersistenceId::new(),
            actor_context.scope_id(),
            move |operand_index, value| {
                let maybe_result = {
                    let mut latest_values = latest_values.borrow_mut();
                    match operand_index {
                        0 => latest_values.0 = Some(value),
                        1 => latest_values.1 = Some(value),
                        _ => unreachable!(),
                    }
                    match (&latest_values.0, &latest_values.1) {
                        (Some(a), Some(b)) => Some((a.clone(), b.clone())),
                        _ => None,
                    }
                };

                maybe_result.map(|(a, b)| {
                    operation(a, b, construct_context.clone(), ValueIdempotencyKey::new())
                })
            },
        )
    }

    fn try_create_direct_state_actor<F>(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
        operation: Rc<F>,
    ) -> Option<ActorHandle>
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Value + 'static,
    {
        Self::try_create_direct_state_actor_with_registry(
            None,
            construct_context,
            actor_context,
            operand_a,
            operand_b,
            operation,
        )
    }

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
        let operation = Rc::new(operation);
        if let Some(actor) = Self::try_create_direct_state_actor(
            construct_context.clone(),
            actor_context.clone(),
            operand_a.clone(),
            operand_b.clone(),
            operation.clone(),
        ) {
            return actor;
        }

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
                let operation = operation.clone();
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
        // Try direct-state path when both operands are non-lazy and have current values,
        // AND the async operation completes synchronously (common for simple comparators).
        if let (Ok(a_val), Ok(b_val)) = (operand_a.current_value(), operand_b.current_value()) {
            if !operand_a.has_lazy_delegate() && !operand_b.has_lazy_delegate() {
                let test_future = operation(
                    a_val.clone(),
                    b_val.clone(),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                );
                let test_future = std::pin::pin!(test_future);
                if let Poll::Ready(initial_result) = poll_future_once(test_future) {
                    // Operation completes synchronously -- use direct-state path
                    let operation = Rc::new(RefCell::new(operation));
                    let output = create_actor_forwarding(
                        parser::PersistenceId::new(),
                        actor_context.scope_id(),
                    );
                    output.store_value_directly(initial_result);

                    let latest_values: Rc<RefCell<(Option<Value>, Option<Value>)>> =
                        Rc::new(RefCell::new((None, None)));

                    operand_a.register_direct_subscriber({
                        let output = output.clone();
                        let latest_values = latest_values.clone();
                        let operation = operation.clone();
                        let construct_context = construct_context.clone();
                        move |value: Option<&Value>| {
                            latest_values.borrow_mut().0 = value.cloned();
                            if let (Some(a), Some(b)) = &*latest_values.borrow() {
                                let fut = operation.borrow()(
                                    a.clone(),
                                    b.clone(),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                );
                                let fut = std::pin::pin!(fut);
                                if let Poll::Ready(result) = poll_future_once(fut) {
                                    output.store_value_directly(result);
                                }
                            }
                            true
                        }
                    });

                    operand_b.register_direct_subscriber({
                        let output = output.clone();
                        let latest_values = latest_values.clone();
                        let operation = operation.clone();
                        let construct_context = construct_context.clone();
                        move |value: Option<&Value>| {
                            latest_values.borrow_mut().1 = value.cloned();
                            if let (Some(a), Some(b)) = &*latest_values.borrow() {
                                let fut = operation.borrow()(
                                    a.clone(),
                                    b.clone(),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                );
                                let fut = std::pin::pin!(fut);
                                if let Poll::Ready(result) = poll_future_once(fut) {
                                    output.store_value_directly(result);
                                }
                            }
                            true
                        }
                    });

                    return output;
                }
            }
        }

        // Fall back to stream-driven path for lazy operands or async operations
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
pub(super) fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
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

    pub fn clear(&mut self) {
        self.values.clear();
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

type ActorDirectSubscriber = Rc<dyn Fn(&mut ActorRegistry, Option<&Value>) -> bool>;

struct ActorStoredStateInner {
    is_alive: Cell<bool>,
    has_future_state_updates: Cell<bool>,
    version: Cell<u64>,
    history: std::cell::RefCell<ValueHistory>,
    update_waiters: std::cell::RefCell<Vec<oneshot::Sender<()>>>,
    drop_waiters: std::cell::RefCell<Vec<oneshot::Sender<()>>>,
    notifying_direct_subscribers: Cell<bool>,
    direct_subscribers: std::cell::RefCell<SmallVec<[ActorDirectSubscriber; 2]>>,
    pending_direct_subscribers: std::cell::RefCell<SmallVec<[ActorDirectSubscriber; 2]>>,
}

impl ActorStoredState {
    fn new(max_entries: usize) -> Self {
        Self {
            inner: Rc::new(ActorStoredStateInner {
                is_alive: Cell::new(true),
                has_future_state_updates: Cell::new(true),
                version: Cell::new(0),
                history: std::cell::RefCell::new(ValueHistory::new(max_entries)),
                update_waiters: std::cell::RefCell::new(Vec::new()),
                drop_waiters: std::cell::RefCell::new(Vec::new()),
                notifying_direct_subscribers: Cell::new(false),
                direct_subscribers: std::cell::RefCell::new(SmallVec::new()),
                pending_direct_subscribers: std::cell::RefCell::new(SmallVec::new()),
            }),
        }
    }

    fn with_initial_value(max_entries: usize, initial_value: Value) -> Self {
        let state = Self::new(max_entries);
        state.store(initial_value);
        state.set_has_future_state_updates(false);
        state
    }

    fn mark_dropped(&self) {
        self.inner.is_alive.set(false);
        self.inner.has_future_state_updates.set(false);
        self.inner.update_waiters.borrow_mut().clear();
        for waiter in self.inner.drop_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
        self.inner.direct_subscribers.borrow_mut().clear();
        self.inner.pending_direct_subscribers.borrow_mut().clear();
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive.get()
    }

    fn version(&self) -> u64 {
        self.inner.version.get()
    }

    fn has_future_state_updates(&self) -> bool {
        self.inner.has_future_state_updates.get()
    }

    fn set_has_future_state_updates(&self, has_future_state_updates: bool) {
        let previous = self
            .inner
            .has_future_state_updates
            .replace(has_future_state_updates);
        if previous && !has_future_state_updates {
            for waiter in self.inner.update_waiters.borrow_mut().drain(..) {
                let _ = waiter.send(());
            }
        }
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

    fn clear(&self) -> u64 {
        let new_version = self.version() + 1;
        self.inner.version.set(new_version);
        self.inner.history.borrow_mut().clear();
        for waiter in self.inner.update_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
        new_version
    }

    fn register_direct_subscriber(
        &self,
        reg: &mut ActorRegistry,
        subscriber: ActorDirectSubscriber,
    ) {
        if let Some(current_value) = self.latest() {
            if subscriber(reg, Some(&current_value)) {
                self.push_direct_subscriber(subscriber);
            }
            return;
        }

        self.push_direct_subscriber(subscriber);
    }

    fn notify_direct_subscribers(&self, reg: &mut ActorRegistry, value: Option<&Value>) {
        self.inner.notifying_direct_subscribers.set(true);
        self.inner
            .direct_subscribers
            .borrow_mut()
            .retain(|subscriber| subscriber(reg, value));
        self.inner.notifying_direct_subscribers.set(false);
        if !self.inner.pending_direct_subscribers.borrow().is_empty() {
            self.inner
                .direct_subscribers
                .borrow_mut()
                .extend(self.inner.pending_direct_subscribers.borrow_mut().drain(..));
        }
    }

    fn push_direct_subscriber(&self, subscriber: ActorDirectSubscriber) {
        if self.inner.notifying_direct_subscribers.get() {
            self.inner
                .pending_direct_subscribers
                .borrow_mut()
                .push(subscriber);
            return;
        }

        self.inner.direct_subscribers.borrow_mut().push(subscriber);
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

    fn wait_for_drop(&self) -> Option<oneshot::Receiver<()>> {
        if !self.is_alive() {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.inner.drop_waiters.borrow_mut().push(tx);
        Some(rx)
    }

    async fn wait_for_first_value_after(&self, version: u64) -> Result<Value, ValueError> {
        let mut version = version;
        loop {
            if let Some(value) = self.get_values_since(version).into_iter().next() {
                return Ok(value);
            }

            let current_version = self.version();
            if current_version > version {
                version = current_version;
                continue;
            }

            if !self.is_alive() {
                return Err(ValueError::ActorDropped);
            }
            if !self.has_future_state_updates() {
                return Err(ValueError::SourceEndedWithoutValue);
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

                    let current_version = stored_state.version();
                    if current_version > last_seen_version {
                        last_seen_version = current_version;
                        continue;
                    }

                    if !stored_state.is_alive() {
                        return None;
                    }
                    if !stored_state.has_future_state_updates() {
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct AsyncSourceId {
    index: u32,
    generation: u32,
}

struct AsyncSourceEntry {
    owner_scope: ScopeId,
    owner: AsyncSourceOwner,
    task: Option<AsyncSourceTaskHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AsyncSourceOwner {
    Scope(ScopeId),
    List(usize),
}

enum AsyncSourceTaskHandle {
    Runtime(TaskHandle),
    #[cfg(all(test, not(target_arch = "wasm32")))]
    Stub(TestAsyncSourceTaskHandle),
}

#[cfg(all(test, not(target_arch = "wasm32")))]
struct TestAsyncSourceTaskHandle {
    slot: usize,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
thread_local! {
    static TEST_ASYNC_SOURCE_TASKS: RefCell<Vec<Option<Pin<Box<dyn Future<Output = ()> + 'static>>>>> =
        const { RefCell::new(Vec::new()) };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl Drop for TestAsyncSourceTaskHandle {
    fn drop(&mut self) {
        TEST_ASYNC_SOURCE_TASKS.with(|tasks| {
            if let Ok(mut tasks) = tasks.try_borrow_mut() {
                if let Some(slot) = tasks.get_mut(self.slot) {
                    *slot = None;
                }
            }
        });
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn poll_test_async_source_tasks() {
    let mut idle_passes = 0;
    loop {
        let mut any_completed = false;
        let (task_count, active_task_count_before) = TEST_ASYNC_SOURCE_TASKS.with(|tasks| {
            let tasks = tasks.borrow();
            (
                tasks.len(),
                tasks.iter().filter(|task| task.is_some()).count(),
            )
        });
        for slot in 0..task_count {
            let Some(mut task) = TEST_ASYNC_SOURCE_TASKS.with(|tasks| {
                tasks
                    .borrow_mut()
                    .get_mut(slot)
                    .and_then(|task| task.take())
            }) else {
                continue;
            };

            if poll_future_once(task.as_mut()).is_ready() {
                any_completed = true;
                continue;
            }

            TEST_ASYNC_SOURCE_TASKS.with(|tasks| {
                if let Some(entry) = tasks.borrow_mut().get_mut(slot) {
                    *entry = Some(task);
                }
            });
        }
        let active_task_count_after = TEST_ASYNC_SOURCE_TASKS
            .with(|tasks| tasks.borrow().iter().filter(|task| task.is_some()).count());
        if any_completed || active_task_count_after != active_task_count_before {
            idle_passes = 0;
            continue;
        }
        idle_passes += 1;
        if idle_passes >= 3 {
            break;
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
pub(crate) fn poll_test_async_source_tasks() {}

fn spawn_async_source_task<F>(future: F) -> AsyncSourceTaskHandle
where
    F: Future<Output = ()> + 'static,
{
    spawn_async_source_task_with_test_poll(future, true)
}

fn spawn_async_source_task_for_reserved_slot<F>(future: F) -> AsyncSourceTaskHandle
where
    F: Future<Output = ()> + 'static,
{
    spawn_async_source_task_with_test_poll(future, false)
}

fn spawn_async_source_task_with_test_poll<F>(
    future: F,
    #[allow(unused_variables)] poll_immediately: bool,
) -> AsyncSourceTaskHandle
where
    F: Future<Output = ()> + 'static,
{
    #[cfg(all(test, not(target_arch = "wasm32")))]
    {
        let future = Box::pin(future);
        let slot = TEST_ASYNC_SOURCE_TASKS.with(|tasks| {
            let mut tasks = tasks.borrow_mut();
            let slot = tasks.len();
            tasks.push(Some(future));
            slot
        });
        if poll_immediately {
            poll_test_async_source_tasks();
        }
        AsyncSourceTaskHandle::Stub(TestAsyncSourceTaskHandle { slot })
    }

    #[cfg(not(all(test, not(target_arch = "wasm32"))))]
    {
        AsyncSourceTaskHandle::Runtime(Task::start_droppable(future))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn poll_spawned_async_source_tasks_after_insertion() {
    poll_test_async_source_tasks();
}

#[cfg(not(all(test, not(target_arch = "wasm32"))))]
pub(crate) fn poll_spawned_async_source_tasks_after_insertion() {}

/// The owned part of an actor - lives in the registry.
/// Contains everything that doesn't need to be cloned for subscriptions.
pub struct OwnedActor {
    scope_id: ScopeId,
    retained_actors: SmallVec<[ActorHandle; 2]>,
    mailbox: VecDeque<ActorMailboxWorkItem>,
    scheduled_for_runtime: bool,
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

enum RuntimeReadyItem {
    ActorInput {
        actor_id: ActorId,
        work: ActorMailboxWorkItem,
    },
    Actor(ActorId),
    ListInput {
        stored_state: ListStoredState,
        work: ListRuntimeWork,
    },
    List(ListStoredState),
    AsyncSourceCleanup(AsyncSourceId),
}

enum ActorMailboxWorkItem {
    Value(Value),
    Clear,
    SourceEnded,
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
    async_sources: Vec<Slot<AsyncSourceEntry>>,
    async_source_free_list: Vec<u32>,
    runtime_ready_queue: VecDeque<RuntimeReadyItem>,
}

thread_local! {
    pub static REGISTRY: RefCell<ActorRegistry> = RefCell::new(ActorRegistry::new());
}

thread_local! {
    static PENDING_LIST_ASYNC_SOURCE_CLEANUPS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

impl ActorRegistry {
    pub fn new() -> Self {
        Self {
            actors: Vec::new(),
            actor_free_list: Vec::new(),
            scopes: Vec::new(),
            scope_free_list: Vec::new(),
            async_sources: Vec::new(),
            async_source_free_list: Vec::new(),
            runtime_ready_queue: VecDeque::new(),
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

        self.take_async_sources_for_scope(scope_id);

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

    fn enqueue_actor_mailbox_value(&mut self, actor_id: ActorId, value: Value) {
        self.enqueue_actor_mailbox_work(actor_id, ActorMailboxWorkItem::Value(value));
    }

    fn enqueue_actor_mailbox_clear(&mut self, actor_id: ActorId) {
        self.enqueue_actor_mailbox_work(actor_id, ActorMailboxWorkItem::Clear);
    }

    fn enqueue_actor_mailbox_source_ended(&mut self, actor_id: ActorId) {
        self.enqueue_actor_mailbox_work(actor_id, ActorMailboxWorkItem::SourceEnded);
    }

    fn enqueue_actor_runtime_input(&mut self, actor_id: ActorId, work: ActorMailboxWorkItem) {
        self.runtime_ready_queue
            .push_back(RuntimeReadyItem::ActorInput { actor_id, work });
    }

    fn enqueue_actor_mailbox_work(&mut self, actor_id: ActorId, work: ActorMailboxWorkItem) {
        let should_schedule = if let Some(actor) = self.get_actor_mut(actor_id) {
            actor.mailbox.push_back(work);
            if actor.scheduled_for_runtime {
                false
            } else {
                actor.scheduled_for_runtime = true;
                true
            }
        } else {
            false
        };

        if should_schedule {
            self.runtime_ready_queue
                .push_back(RuntimeReadyItem::Actor(actor_id));
        }
    }

    fn enqueue_list_runtime_input(&mut self, stored_state: ListStoredState, work: ListRuntimeWork) {
        self.runtime_ready_queue
            .push_back(RuntimeReadyItem::ListInput { stored_state, work });
    }

    fn enqueue_list_runtime_work(&mut self, stored_state: ListStoredState, work: ListRuntimeWork) {
        if stored_state.push_runtime_work(work) {
            self.runtime_ready_queue
                .push_back(RuntimeReadyItem::List(stored_state));
        }
    }

    fn drain_runtime_ready_queue(&mut self) {
        while let Some(item) = self.runtime_ready_queue.pop_front() {
            match item {
                RuntimeReadyItem::ActorInput { actor_id, work } => {
                    self.enqueue_actor_mailbox_work(actor_id, work);
                }
                RuntimeReadyItem::Actor(actor_id) => {
                    let Some((stored_state, mut mailbox)) = ({
                        self.get_actor_mut(actor_id).map(|actor| {
                            actor.scheduled_for_runtime = false;
                            (
                                actor.stored_state.clone(),
                                std::mem::take(&mut actor.mailbox),
                            )
                        })
                    }) else {
                        continue;
                    };

                    while let Some(work) = mailbox.pop_front() {
                        match work {
                            ActorMailboxWorkItem::Value(value) => {
                                publish_value_to_actor_state_with_registry(
                                    self,
                                    &stored_state,
                                    value,
                                );
                            }
                            ActorMailboxWorkItem::Clear => {
                                clear_actor_state_with_registry(self, &stored_state);
                            }
                            ActorMailboxWorkItem::SourceEnded => {
                                stored_state.set_has_future_state_updates(false);
                            }
                        }
                    }

                    let should_reschedule = if let Some(actor) = self.get_actor_mut(actor_id) {
                        !actor.mailbox.is_empty() && !actor.scheduled_for_runtime
                    } else {
                        false
                    };

                    if should_reschedule {
                        if let Some(actor) = self.get_actor_mut(actor_id) {
                            actor.scheduled_for_runtime = true;
                        }
                        self.runtime_ready_queue
                            .push_back(RuntimeReadyItem::Actor(actor_id));
                    }
                }
                RuntimeReadyItem::ListInput { stored_state, work } => {
                    self.enqueue_list_runtime_work(stored_state, work);
                }
                RuntimeReadyItem::List(stored_state) => {
                    for work in stored_state.take_runtime_work() {
                        process_list_runtime_work(self, &stored_state, work);
                    }

                    if stored_state.has_pending_runtime_work()
                        && !stored_state.is_scheduled_for_runtime()
                    {
                        stored_state.set_scheduled_for_runtime(true);
                        self.runtime_ready_queue
                            .push_back(RuntimeReadyItem::List(stored_state));
                    }
                }
                RuntimeReadyItem::AsyncSourceCleanup(async_source_id) => {
                    let _ = self.take_async_source(async_source_id);
                }
            }
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

    fn insert_async_source(
        &mut self,
        scope_id: ScopeId,
        task: AsyncSourceTaskHandle,
    ) -> Option<AsyncSourceId> {
        self.insert_async_source_entry(AsyncSourceEntry {
            owner_scope: scope_id,
            owner: AsyncSourceOwner::Scope(scope_id),
            task: Some(task),
        })
    }

    fn insert_async_source_for_list(
        &mut self,
        scope_id: ScopeId,
        list_owner_key: usize,
        task: AsyncSourceTaskHandle,
    ) -> Option<AsyncSourceId> {
        self.insert_async_source_entry(AsyncSourceEntry {
            owner_scope: scope_id,
            owner: AsyncSourceOwner::List(list_owner_key),
            task: Some(task),
        })
    }

    fn reserve_async_source_for_scope(&mut self, scope_id: ScopeId) -> Option<AsyncSourceId> {
        self.insert_async_source_entry(AsyncSourceEntry {
            owner_scope: scope_id,
            owner: AsyncSourceOwner::Scope(scope_id),
            task: None,
        })
    }

    fn reserve_async_source_for_list(
        &mut self,
        scope_id: ScopeId,
        list_owner_key: usize,
    ) -> Option<AsyncSourceId> {
        self.insert_async_source_entry(AsyncSourceEntry {
            owner_scope: scope_id,
            owner: AsyncSourceOwner::List(list_owner_key),
            task: None,
        })
    }

    fn set_async_source_task(
        &mut self,
        async_source_id: AsyncSourceId,
        task: AsyncSourceTaskHandle,
    ) -> bool {
        let idx = match usize::try_from(async_source_id.index) {
            Ok(idx) => idx,
            Err(_) => return false,
        };
        let Some(Slot::Occupied { generation, value }) = self.async_sources.get_mut(idx) else {
            return false;
        };
        if *generation != async_source_id.generation {
            return false;
        }
        value.task = Some(task);
        true
    }

    fn insert_async_source_entry(&mut self, entry: AsyncSourceEntry) -> Option<AsyncSourceId> {
        if self.get_scope(entry.owner_scope).is_none() {
            return None;
        }

        let (index, generation) = if let Some(free_idx) = self.async_source_free_list.pop() {
            let idx = usize::try_from(free_idx).unwrap();
            let next_gen = match &self.async_sources[idx] {
                Slot::Free { next_generation } => *next_generation,
                Slot::Occupied { .. } => {
                    unreachable!("free list points to occupied async source slot")
                }
            };
            self.async_sources[idx] = Slot::Occupied {
                generation: next_gen,
                value: entry,
            };
            (free_idx, next_gen)
        } else {
            let idx = u32::try_from(self.async_sources.len()).unwrap();
            self.async_sources.push(Slot::Occupied {
                generation: 0,
                value: entry,
            });
            (idx, 0)
        };

        Some(AsyncSourceId { index, generation })
    }

    fn take_async_source(
        &mut self,
        async_source_id: AsyncSourceId,
    ) -> Option<AsyncSourceTaskHandle> {
        let idx = usize::try_from(async_source_id.index).ok()?;
        match &self.async_sources.get(idx) {
            Some(Slot::Occupied { generation, .. })
                if *generation == async_source_id.generation => {}
            _ => return None,
        }

        let old_gen = async_source_id.generation;
        let task_slot = std::mem::replace(
            &mut self.async_sources[idx],
            Slot::Free {
                next_generation: old_gen + 1,
            },
        );
        self.async_source_free_list.push(async_source_id.index);
        match task_slot {
            Slot::Occupied { value, .. } => value.task,
            Slot::Free { .. } => {
                unreachable!("validated occupied async source slot before take_async_source")
            }
        }
    }

    fn take_async_sources_for_scope(&mut self, scope_id: ScopeId) {
        let matching_ids: Vec<_> = self
            .async_sources
            .iter()
            .enumerate()
            .filter_map(|(idx, slot)| match slot {
                Slot::Occupied { generation, value } if value.owner_scope == scope_id => {
                    Some(AsyncSourceId {
                        index: u32::try_from(idx).unwrap(),
                        generation: *generation,
                    })
                }
                _ => None,
            })
            .collect();

        for async_source_id in matching_ids {
            let _ = self.take_async_source(async_source_id);
        }
    }

    fn take_async_sources_for_list(&mut self, list_owner_key: usize) {
        let matching_ids: Vec<_> = self
            .async_sources
            .iter()
            .enumerate()
            .filter_map(|(idx, slot)| match slot {
                Slot::Occupied { generation, value }
                    if value.owner == AsyncSourceOwner::List(list_owner_key) =>
                {
                    Some(AsyncSourceId {
                        index: u32::try_from(idx).unwrap(),
                        generation: *generation,
                    })
                }
                _ => None,
            })
            .collect();

        for async_source_id in matching_ids {
            let _ = self.take_async_source(async_source_id);
        }
    }

    pub(crate) fn async_source_count_for_scope(&self, scope_id: ScopeId) -> usize {
        self.async_sources
            .iter()
            .filter(|slot| {
                matches!(
                    slot,
                    Slot::Occupied { value, .. } if value.owner_scope == scope_id
                )
            })
            .count()
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

    fn owning_scope_id(&self) -> Option<ScopeId> {
        REGISTRY.with(|reg| {
            reg.borrow()
                .get_actor(self.actor_id)
                .map(|actor| actor.scope_id)
        })
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

    pub(crate) fn has_lazy_delegate_with_registry(&self, reg: &ActorRegistry) -> bool {
        self.lazy_delegate_with_registry(reg).is_some()
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.stored_state.is_alive()
    }

    pub(crate) fn register_direct_subscriber_with_registry<F>(
        &self,
        reg: &mut ActorRegistry,
        subscriber: F,
    ) where
        F: Fn(&mut ActorRegistry, Option<&Value>) -> bool + 'static,
    {
        self.stored_state
            .register_direct_subscriber(reg, Rc::new(subscriber));
    }

    /// Register a direct subscriber without a registry reference.
    /// Uses the thread-local registry internally.
    pub(crate) fn register_direct_subscriber<F>(&self, subscriber: F)
    where
        F: Fn(Option<&Value>) -> bool + 'static,
    {
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            self.stored_state.register_direct_subscriber(
                &mut reg,
                Rc::new(move |_reg, value| subscriber(value)),
            );
            reg.drain_runtime_ready_queue();
        });
    }

    fn lazy_delegate_with_registry(&self, reg: &ActorRegistry) -> Option<Arc<LazyValueActor>> {
        reg.get_actor(self.actor_id)
            .and_then(|owned_actor| owned_actor.lazy_delegate.clone())
    }

    fn lazy_delegate(&self) -> Option<Arc<LazyValueActor>> {
        REGISTRY.with(|reg| {
            reg.try_borrow()
                .ok()
                .and_then(|reg| self.lazy_delegate_with_registry(&reg))
        })
    }

    fn store_value_directly_with_registry(
        &self,
        mut reg: Option<&mut ActorRegistry>,
        value: Value,
    ) {
        let has_lazy_delegate = match reg {
            Some(ref reg) => self.lazy_delegate_with_registry(reg),
            None => self.lazy_delegate(),
        }
        .is_some();

        if !self.is_constant && !has_lazy_delegate && self.stored_state.is_alive() {
            match reg.as_deref_mut() {
                Some(reg) => {
                    publish_value_to_actor_state_with_registry(reg, &self.stored_state, value);
                    reg.drain_runtime_ready_queue();
                }
                None => publish_value_to_actor_state(&self.stored_state, value),
            }
        }
    }

    fn constant_value(&self) -> Option<Value> {
        self.is_constant
            .then(|| self.stored_state.latest())
            .flatten()
    }

    /// Directly store a value, bypassing the async input stream.
    pub fn store_value_directly(&self, value: Value) {
        self.store_value_directly_with_registry(None, value);
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
            return match lazy_delegate
                .stream_from_cursor(self.version() as usize)
                .next()
                .await
            {
                Some(value) => Ok(value),
                None if !self.is_alive() => Err(ValueError::ActorDropped),
                None if !self.stored_state.has_future_state_updates() => {
                    Err(ValueError::SourceEndedWithoutValue)
                }
                None => Err(ValueError::ActorDropped),
            };
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

pub(crate) fn enqueue_actor_value_on_runtime_queue_with_registry(
    reg: &mut ActorRegistry,
    actor: &ActorHandle,
    value: Value,
) {
    reg.enqueue_actor_runtime_input(actor.actor_id, ActorMailboxWorkItem::Value(value));
}

pub(crate) fn drain_runtime_ready_queue_with_registry(reg: &mut ActorRegistry) {
    reg.drain_runtime_ready_queue();
}

/// Create an actor in the registry and return a handle to interact with it.
///
/// The retained runtime task goes into the registry under `scope_id`.
/// The returned `ActorHandle` holds only direct state metadata — cloning it does NOT keep the actor alive.
#[track_caller]
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
        ActorStreamCreationPlan::EndedWithoutValue => {
            create_ended_actor_without_value(persistence_id, scope_id, None)
        }
        ActorStreamCreationPlan::Stream(value_stream) => {
            create_stream_driven_actor_arc_info(persistence_id, scope_id, None, value_stream)
        }
    }
}

pub(crate) fn create_actor_with_registry<S>(
    reg: &mut ActorRegistry,
    value_stream: S,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    S: Stream<Item = Value> + 'static,
{
    match plan_actor_stream_creation(value_stream) {
        ActorStreamCreationPlan::Constant(value) => {
            create_constant_actor_with_registry(reg, persistence_id, value, scope_id)
        }
        ActorStreamCreationPlan::EndedWithoutValue => {
            let actor = create_direct_state_actor_arc_info_with_registry(
                reg,
                persistence_id,
                scope_id,
                None,
            );
            actor.stored_state.set_has_future_state_updates(false);
            actor
        }
        ActorStreamCreationPlan::Stream(value_stream) => {
            create_stream_driven_actor_arc_info_with_registry(
                reg,
                persistence_id,
                scope_id,
                None,
                value_stream,
            )
        }
    }
}

#[track_caller]
pub fn create_constant_actor(
    persistence_id: parser::PersistenceId,
    constant_value: Value,
    scope_id: ScopeId,
) -> ActorHandle {
    create_constant_actor_arc_info(persistence_id, constant_value, scope_id, None)
}

pub(crate) fn create_constant_actor_with_registry(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    constant_value: Value,
    scope_id: ScopeId,
) -> ActorHandle {
    let stored_state = ActorStoredState::with_initial_value(1, constant_value);
    insert_owned_actor_handle_with_registry(
        reg,
        persistence_id,
        scope_id,
        stored_state,
        None,
        true,
        None,
    )
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

pub(crate) fn create_constant_actor_with_origin_with_registry(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    constant_value: Value,
    origin: ListItemOrigin,
    scope_id: ScopeId,
) -> ActorHandle {
    let stored_state = ActorStoredState::with_initial_value(1, constant_value);
    insert_owned_actor_handle_with_registry(
        reg,
        persistence_id,
        scope_id,
        stored_state,
        None,
        true,
        Some(Arc::new(origin)),
    )
}

fn publish_value_to_actor_state_with_registry(
    reg: &mut ActorRegistry,
    stored_state: &ActorStoredState,
    new_value: Value,
) {
    let new_version = stored_state.store(new_value.clone());
    stored_state.notify_direct_subscribers(reg, Some(&new_value));
    if LOG_ACTOR_FLOW {
        zoon::println!("[FLOW] actor state produced v{new_version}");
    }
}

fn clear_actor_state_with_registry(reg: &mut ActorRegistry, stored_state: &ActorStoredState) {
    let new_version = stored_state.clear();
    stored_state.notify_direct_subscribers(reg, None);
    if LOG_ACTOR_FLOW {
        zoon::println!("[FLOW] actor state cleared at v{new_version}");
    }
}

fn publish_value_to_actor_state(stored_state: &ActorStoredState, new_value: Value) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        publish_value_to_actor_state_with_registry(&mut reg, stored_state, new_value);
        reg.drain_runtime_ready_queue();
    });
}

pub(crate) fn enqueue_actor_value_on_runtime_queue(actor: &ActorHandle, new_value: Value) {
    enqueue_actor_value_on_runtime_queue_by_id(actor.actor_id, new_value);
}

fn enqueue_actor_value_on_runtime_queue_by_id_with_registry(
    reg: &mut ActorRegistry,
    actor_id: ActorId,
    new_value: Value,
) {
    reg.enqueue_actor_runtime_input(actor_id, ActorMailboxWorkItem::Value(new_value));
    reg.drain_runtime_ready_queue();
}

pub(crate) fn enqueue_actor_value_on_runtime_queue_by_id(actor_id: ActorId, new_value: Value) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        enqueue_actor_value_on_runtime_queue_by_id_with_registry(&mut reg, actor_id, new_value);
    });
}

pub(crate) fn enqueue_actor_clear_on_runtime_queue(actor: &ActorHandle) {
    enqueue_actor_clear_on_runtime_queue_by_id(actor.actor_id);
}

pub(crate) fn enqueue_actor_clear_on_runtime_queue_by_id(actor_id: ActorId) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        reg.enqueue_actor_runtime_input(actor_id, ActorMailboxWorkItem::Clear);
        reg.drain_runtime_ready_queue();
    });
}

pub(crate) fn enqueue_actor_source_end_on_runtime_queue(actor: &ActorHandle) {
    enqueue_actor_source_end_on_runtime_queue_by_id(actor.actor_id);
}

fn enqueue_actor_source_end_on_runtime_queue_by_id_with_registry(
    reg: &mut ActorRegistry,
    actor_id: ActorId,
) {
    reg.enqueue_actor_runtime_input(actor_id, ActorMailboxWorkItem::SourceEnded);
    reg.drain_runtime_ready_queue();
}

pub(crate) fn enqueue_actor_source_end_on_runtime_queue_by_id(actor_id: ActorId) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        enqueue_actor_source_end_on_runtime_queue_by_id_with_registry(&mut reg, actor_id);
    });
}

fn enqueue_list_work_on_runtime_queue_with_registry(
    reg: &mut ActorRegistry,
    stored_state: &ListStoredState,
    work: ListRuntimeWork,
) {
    reg.enqueue_list_runtime_input(stored_state.clone(), work);
    reg.drain_runtime_ready_queue();
}

fn enqueue_list_work_on_runtime_queue(stored_state: &ListStoredState, work: ListRuntimeWork) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        enqueue_list_work_on_runtime_queue_with_registry(&mut reg, stored_state, work);
    });
}

fn enqueue_async_source_cleanup_on_runtime_queue(async_source_id: AsyncSourceId) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        reg.runtime_ready_queue
            .push_back(RuntimeReadyItem::AsyncSourceCleanup(async_source_id));
        reg.drain_runtime_ready_queue();
    });
}

pub(crate) fn enqueue_list_change_on_runtime_queue_with_registry(
    reg: &mut ActorRegistry,
    list: &Arc<List>,
    construct_info: ConstructInfoComplete,
    change: ListChange,
    broadcast_change: bool,
) {
    reg.enqueue_list_runtime_input(
        list.stored_state.clone(),
        ListRuntimeWork::Change {
            construct_info,
            change,
            broadcast_change,
        },
    );
}

#[track_caller]
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
            scope_id,
            retained_actors: SmallVec::new(),
            mailbox: VecDeque::new(),
            scheduled_for_runtime: false,
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

fn insert_owned_actor_handle_with_registry(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    stored_state: ActorStoredState,
    lazy_delegate: Option<Arc<LazyValueActor>>,
    is_constant: bool,
    list_item_origin: Option<Arc<ListItemOrigin>>,
) -> ActorHandle {
    LIVE_ACTOR_COUNT.with(|c| c.set(c.get() + 1));

    let actor_id = insert_actor_with_registry(
        reg,
        scope_id,
        OwnedActor {
            scope_id,
            retained_actors: SmallVec::new(),
            mailbox: VecDeque::new(),
            scheduled_for_runtime: false,
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

#[track_caller]
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

#[track_caller]
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

fn create_direct_state_actor_arc_info_with_registry(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
) -> ActorHandle {
    let stored_state = ActorStoredState::new(64);
    insert_owned_actor_handle_with_registry(
        reg,
        persistence_id,
        scope_id,
        stored_state,
        None,
        false,
        list_item_origin,
    )
}

#[track_caller]
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
            Poll::Ready(None) => {
                actor.stored_state.set_has_future_state_updates(false);
                return actor;
            }
            Poll::Pending => break,
        }
    }

    let actor_id = actor.actor_id;
    start_retained_actor_external_stream_feeder_task(
        &actor,
        value_stream,
        move |reg, value| {
            enqueue_actor_value_on_runtime_queue_by_id_with_registry(reg, actor_id, value);
        },
        move |reg| {
            enqueue_actor_source_end_on_runtime_queue_by_id_with_registry(reg, actor_id);
        },
    );
    actor
}

fn create_stream_driven_actor_arc_info_with_registry<S>(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
    value_stream: S,
) -> ActorHandle
where
    S: Stream<Item = Value> + 'static,
{
    let actor = create_direct_state_actor_arc_info_with_registry(
        reg,
        persistence_id,
        scope_id,
        list_item_origin,
    );
    let mut value_stream = value_stream.boxed_local();

    loop {
        match poll_stream_once(value_stream.as_mut()) {
            Poll::Ready(Some(value)) => actor.store_value_directly_with_registry(Some(reg), value),
            Poll::Ready(None) => {
                actor.stored_state.set_has_future_state_updates(false);
                return actor;
            }
            Poll::Pending => break,
        }
    }

    let actor_id = actor.actor_id;
    start_retained_actor_scope_task_with_registry(reg, &actor, async move {
        drain_external_stream_to_runtime_queue(
            value_stream,
            move |reg, value| {
                enqueue_actor_value_on_runtime_queue_by_id_with_registry(reg, actor_id, value);
            },
            move |reg| {
                enqueue_actor_source_end_on_runtime_queue_by_id_with_registry(reg, actor_id);
            },
        )
        .await;
    });
    poll_spawned_async_source_tasks_after_insertion();
    actor
}

#[track_caller]
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
    let actor_id = actor.actor_id;
    let actor_for_start = actor.clone();
    lazy_actor.install_start_loop(Box::new(move || {
        start_retained_actor_scope_task(&actor_for_start, async move {
            LazyValueActor::internal_loop(
                value_stream,
                request_rx,
                Some(actor_id),
                stored_state_for_loop,
            )
            .await;
        });
    }));
    actor
}

fn create_lazy_stream_driven_actor_arc_info_with_registry<S>(
    reg: &mut ActorRegistry,
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
    let actor = insert_owned_actor_handle_with_registry(
        reg,
        persistence_id,
        scope_id,
        stored_state,
        Some(lazy_actor.clone()),
        false,
        None,
    );
    let actor_id = actor.actor_id;
    let actor_for_start = actor.clone();
    lazy_actor.install_start_loop(Box::new(move || {
        start_retained_actor_scope_task(&actor_for_start, async move {
            LazyValueActor::internal_loop(
                value_stream,
                request_rx,
                Some(actor_id),
                stored_state_for_loop,
            )
            .await;
        });
    }));
    actor
}

enum ActorStreamCreationPlan {
    Constant(Value),
    EndedWithoutValue,
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
        Poll::Ready(None) => ActorStreamCreationPlan::EndedWithoutValue,
        Poll::Pending => ActorStreamCreationPlan::Stream(value_stream),
    }
}

#[track_caller]
fn create_ended_actor_without_value(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
) -> ActorHandle {
    let actor = create_direct_state_actor_arc_info(persistence_id, scope_id, list_item_origin);
    actor.stored_state.set_has_future_state_updates(false);
    actor
}

pub fn create_actor_from_future<F>(
    value_future: F,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    F: Future<Output = Value> + 'static,
{
    let mut value_future = Box::pin(value_future);
    match poll_future_once(value_future.as_mut()) {
        Poll::Ready(value) => create_constant_actor(persistence_id, value, scope_id),
        Poll::Pending => {
            let actor = create_direct_state_actor_arc_info(persistence_id, scope_id, None);
            let actor_id = actor.actor_id;
            start_retained_actor_scope_task(&actor, async move {
                let value = value_future.await;
                enqueue_actor_value_on_runtime_queue_by_id(actor_id, value);
                enqueue_actor_source_end_on_runtime_queue_by_id(actor_id);
            });
            actor
        }
    }
}

pub(crate) fn create_actor_from_future_with_registry<F>(
    reg: &mut ActorRegistry,
    value_future: F,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    F: Future<Output = Value> + 'static,
{
    let mut value_future = Box::pin(value_future);
    match poll_future_once(value_future.as_mut()) {
        Poll::Ready(value) => {
            create_constant_actor_with_registry(reg, persistence_id, value, scope_id)
        }
        Poll::Pending => {
            let actor = create_direct_state_actor_arc_info_with_registry(
                reg,
                persistence_id,
                scope_id,
                None,
            );
            let actor_id = actor.actor_id;
            start_retained_actor_scope_task_with_registry(reg, &actor, async move {
                let value = value_future.await;
                enqueue_actor_value_on_runtime_queue_by_id(actor_id, value);
                enqueue_actor_source_end_on_runtime_queue_by_id(actor_id);
            });
            poll_spawned_async_source_tasks_after_insertion();
            actor
        }
    }
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
        Poll::Ready(None) => {
            forwarding_actor
                .stored_state
                .set_has_future_state_updates(false);
        }
        Poll::Pending => {
            let forwarding_actor_for_task = forwarding_actor.clone();
            let forwarding_actor_id = forwarding_actor.actor_id;
            start_retained_actor_scope_task(&forwarding_actor, async move {
                let Some(source_actor) = source_future.await else {
                    enqueue_actor_source_end_on_runtime_queue_by_id(forwarding_actor_id);
                    return;
                };
                connect_forwarding_current_and_future(forwarding_actor_for_task, source_actor);
            });
        }
    }
    forwarding_actor
}

pub(crate) fn create_actor_forwarding_from_future_source_with_registry<F>(
    reg: &mut ActorRegistry,
    source_future: F,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    F: Future<Output = Option<ActorHandle>> + 'static,
{
    let forwarding_actor = create_actor_forwarding_with_registry(reg, persistence_id, scope_id);
    let mut source_future = Box::pin(source_future);
    match poll_future_once(source_future.as_mut()) {
        Poll::Ready(Some(source_actor)) => {
            connect_forwarding_current_and_future_with_registry(
                Some(reg),
                forwarding_actor.clone(),
                source_actor,
            );
            reg.drain_runtime_ready_queue();
        }
        Poll::Ready(None) => {
            forwarding_actor
                .stored_state
                .set_has_future_state_updates(false);
        }
        Poll::Pending => {
            let forwarding_actor_for_task = forwarding_actor.clone();
            let forwarding_actor_id = forwarding_actor.actor_id;
            start_retained_actor_scope_task_with_registry(reg, &forwarding_actor, async move {
                let Some(source_actor) = source_future.await else {
                    enqueue_actor_source_end_on_runtime_queue_by_id(forwarding_actor_id);
                    return;
                };
                REGISTRY.with(|reg| {
                    let mut reg = reg.borrow_mut();
                    connect_forwarding_current_and_future_with_registry(
                        Some(&mut reg),
                        forwarding_actor_for_task,
                        source_actor,
                    );
                    reg.drain_runtime_ready_queue();
                });
            });
        }
    }

    forwarding_actor
}

/// Create a forwarding actor backed only by direct stored state.
#[track_caller]
pub fn create_actor_forwarding(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    create_direct_state_actor_arc_info(persistence_id, scope_id, None)
}

pub(crate) fn create_actor_forwarding_with_registry(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    create_direct_state_actor_arc_info_with_registry(reg, persistence_id, scope_id, None)
}

pub(crate) fn create_actor_forwarding_with_origin(
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    origin: ListItemOrigin,
) -> ActorHandle {
    create_direct_state_actor_arc_info(persistence_id, scope_id, Some(Arc::new(origin)))
}

pub(crate) fn create_actor_forwarding_with_registry_and_origin(
    reg: &mut ActorRegistry,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    origin: ListItemOrigin,
) -> ActorHandle {
    create_direct_state_actor_arc_info_with_registry(
        reg,
        persistence_id,
        scope_id,
        Some(Arc::new(origin)),
    )
}

pub(crate) fn register_list_direct_subscriber_with_registry<F>(
    reg: &mut ActorRegistry,
    list: &Arc<List>,
    subscriber: F,
) where
    F: Fn(&mut ActorRegistry, &ListChange) -> bool + 'static,
{
    list.stored_state
        .register_direct_subscriber(reg, Rc::new(subscriber));
}

/// Watch an actor's current-or-future values while tying the watcher to the owner actor.
///
/// Eager sources stay on direct subscribers. Lazy sources still need one retained watcher,
/// but the callback path stays shared and runtime-queue owned.
pub(crate) fn watch_actor_current_and_future_until_with_registry<F>(
    reg: Option<&mut ActorRegistry>,
    owner: &ActorHandle,
    source_actor: &ActorHandle,
    skip_initial_lazy_value: bool,
    on_value: F,
) where
    F: FnMut(&mut ActorRegistry, Value) -> bool + 'static,
{
    let on_value = Rc::new(RefCell::new(on_value));
    match reg {
        Some(reg) => {
            if source_actor.has_lazy_delegate_with_registry(reg) {
                let owner_for_task = owner.clone();
                let source_actor = source_actor.clone();
                let on_value = on_value.clone();
                start_retained_actor_scope_task_with_registry(reg, owner, async move {
                    let mut source_stream = std::pin::pin!(source_actor.current_or_future_stream());
                    let mut skip_initial_lazy_value = skip_initial_lazy_value;
                    while let Some(value) = source_stream.next().await {
                        if !owner_for_task.is_alive() {
                            break;
                        }
                        if skip_initial_lazy_value {
                            skip_initial_lazy_value = false;
                            continue;
                        }
                        let done = REGISTRY.with(|reg| {
                            let mut reg = reg.borrow_mut();
                            if !owner_for_task.is_alive() {
                                return true;
                            }
                            let done = (on_value.borrow_mut())(&mut reg, value);
                            drain_runtime_ready_queue_with_registry(&mut reg);
                            done
                        });
                        if done {
                            break;
                        }
                    }
                });
            } else {
                source_actor.register_direct_subscriber_with_registry(reg, {
                    let owner = owner.clone();
                    let on_value = on_value.clone();
                    move |reg, value| {
                        if !owner.is_alive() {
                            return false;
                        }
                        let Some(value) = value else {
                            return true;
                        };
                        !(on_value.borrow_mut())(reg, value.clone())
                    }
                });
            }
        }
        None => {
            if source_actor.has_lazy_delegate() {
                let owner_for_task = owner.clone();
                let source_actor = source_actor.clone();
                let on_value = on_value.clone();
                start_retained_actor_scope_task(owner, async move {
                    let mut source_stream = std::pin::pin!(source_actor.current_or_future_stream());
                    let mut skip_initial_lazy_value = skip_initial_lazy_value;
                    while let Some(value) = source_stream.next().await {
                        if !owner_for_task.is_alive() {
                            break;
                        }
                        if skip_initial_lazy_value {
                            skip_initial_lazy_value = false;
                            continue;
                        }
                        let done = REGISTRY.with(|reg| {
                            let mut reg = reg.borrow_mut();
                            if !owner_for_task.is_alive() {
                                return true;
                            }
                            let done = (on_value.borrow_mut())(&mut reg, value);
                            drain_runtime_ready_queue_with_registry(&mut reg);
                            done
                        });
                        if done {
                            break;
                        }
                    }
                });
            } else {
                REGISTRY.with(|reg| {
                    let mut reg = reg.borrow_mut();
                    source_actor.register_direct_subscriber_with_registry(&mut reg, {
                        let owner = owner.clone();
                        let on_value = on_value.clone();
                        move |reg, value| {
                            if !owner.is_alive() {
                                return false;
                            }
                            let Some(value) = value else {
                                return true;
                            };
                            !(on_value.borrow_mut())(reg, value.clone())
                        }
                    });
                });
            }
        }
    }
}

pub(crate) fn watch_actor_current_and_future_with_registry<F>(
    reg: Option<&mut ActorRegistry>,
    owner: &ActorHandle,
    source_actor: &ActorHandle,
    skip_initial_lazy_value: bool,
    mut on_value: F,
) where
    F: FnMut(&mut ActorRegistry, Value) + 'static,
{
    watch_actor_current_and_future_until_with_registry(
        reg,
        owner,
        source_actor,
        skip_initial_lazy_value,
        move |reg, value| {
            on_value(reg, value);
            false
        },
    );
}

fn connect_direct_state_output_source_with_registry<F>(
    mut reg: Option<&mut ActorRegistry>,
    output: &ActorHandle,
    source_actor: &ActorHandle,
    idx: usize,
    on_source_value: Rc<RefCell<F>>,
) -> bool
where
    F: FnMut(usize, Value) -> Option<Value> + 'static,
{
    let source_is_lazy = match reg.as_deref_mut() {
        Some(reg) => source_actor.has_lazy_delegate_with_registry(reg),
        None => source_actor.has_lazy_delegate(),
    };

    if source_is_lazy {
        let output = output.clone();
        let output_for_callback = output.clone();
        watch_actor_current_and_future_with_registry(
            reg,
            &output,
            source_actor,
            false,
            move |reg, value| {
                if let Some(mapped_value) = (on_source_value.borrow_mut())(idx, value) {
                    enqueue_actor_value_on_runtime_queue_with_registry(
                        reg,
                        &output_for_callback,
                        mapped_value,
                    );
                }
            },
        );
        return true;
    }

    let source_state = source_actor.stored_state.clone();
    match reg {
        Some(reg) => {
            source_state.register_direct_subscriber(
                reg,
                Rc::new({
                    let output = output.clone();
                    let on_source_value = on_source_value.clone();
                    let source_state = source_state.clone();
                    move |reg, value| {
                        if !output.is_alive() {
                            return false;
                        }

                        let Some(value) = value else {
                            return source_state.is_alive();
                        };

                        if let Some(mapped_value) =
                            (on_source_value.borrow_mut())(idx, value.clone())
                        {
                            enqueue_actor_value_on_runtime_queue_with_registry(
                                reg,
                                &output,
                                mapped_value,
                            );
                        }

                        source_state.is_alive()
                    }
                }),
            );
        }
        None => {
            REGISTRY.with(|reg| {
                let mut reg = reg.borrow_mut();
                source_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let output = output.clone();
                        let on_source_value = on_source_value.clone();
                        let source_state = source_state.clone();
                        move |reg, value| {
                            if !output.is_alive() {
                                return false;
                            }

                            let Some(value) = value else {
                                return source_state.is_alive();
                            };

                            if let Some(mapped_value) =
                                (on_source_value.borrow_mut())(idx, value.clone())
                            {
                                enqueue_actor_value_on_runtime_queue_with_registry(
                                    reg,
                                    &output,
                                    mapped_value,
                                );
                            }

                            source_state.is_alive()
                        }
                    }),
                );
            });
        }
    }

    false
}

pub(crate) fn try_create_direct_state_subscriber_actor_with_registry<F>(
    mut reg: Option<&mut ActorRegistry>,
    sources: Vec<ActorHandle>,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    on_source_value: F,
) -> Option<ActorHandle>
where
    F: FnMut(usize, Value) -> Option<Value> + 'static,
{
    let output = match reg.as_deref_mut() {
        Some(reg) => create_actor_forwarding_with_registry(reg, persistence_id, scope_id),
        None => create_actor_forwarding(persistence_id, scope_id),
    };
    retain_actor_handles_with_registry(reg.as_deref_mut(), &output, sources.iter().cloned());

    let on_source_value = Rc::new(RefCell::new(on_source_value));
    let mut spawned_lazy_watchers = false;
    match reg {
        Some(reg) => {
            for (idx, source_actor) in sources.iter().enumerate() {
                spawned_lazy_watchers |= connect_direct_state_output_source_with_registry(
                    Some(reg),
                    &output,
                    source_actor,
                    idx,
                    on_source_value.clone(),
                );
            }
            reg.drain_runtime_ready_queue();
        }
        None => {
            for (idx, source_actor) in sources.iter().enumerate() {
                spawned_lazy_watchers |= connect_direct_state_output_source_with_registry(
                    None,
                    &output,
                    source_actor,
                    idx,
                    on_source_value.clone(),
                );
            }
            REGISTRY.with(|reg| reg.borrow_mut().drain_runtime_ready_queue());
        }
    }

    if spawned_lazy_watchers {
        poll_spawned_async_source_tasks_after_insertion();
    }

    Some(output)
}

pub(crate) fn try_create_direct_state_subscriber_actor<F>(
    sources: Vec<ActorHandle>,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    on_source_value: F,
) -> Option<ActorHandle>
where
    F: FnMut(usize, Value) -> Option<Value> + 'static,
{
    try_create_direct_state_subscriber_actor_with_registry(
        None,
        sources,
        persistence_id,
        scope_id,
        on_source_value,
    )
}

/// Like `try_create_direct_state_subscriber_actor_with_registry`, but with list item
/// origin metadata on the output actor. This allows direct-state list items to avoid
/// the retained feeder-task path when their source is non-lazy.
pub(crate) fn try_create_direct_state_subscriber_actor_with_origin<F>(
    mut reg: Option<&mut ActorRegistry>,
    sources: Vec<ActorHandle>,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
    origin: ListItemOrigin,
    on_source_value: F,
) -> Option<ActorHandle>
where
    F: FnMut(usize, Value) -> Option<Value> + 'static,
{
    let origin = Arc::new(origin);
    let output = match reg.as_deref_mut() {
        Some(reg) => create_direct_state_actor_arc_info_with_registry(
            reg,
            persistence_id,
            scope_id,
            Some(origin),
        ),
        None => create_direct_state_actor_arc_info(persistence_id, scope_id, Some(origin)),
    };

    let on_source_value = Rc::new(RefCell::new(on_source_value));
    let mut spawned_lazy_watchers = false;
    match reg {
        Some(reg) => {
            for (idx, source_actor) in sources.iter().enumerate() {
                spawned_lazy_watchers |= connect_direct_state_output_source_with_registry(
                    Some(reg),
                    &output,
                    source_actor,
                    idx,
                    on_source_value.clone(),
                );
            }
            reg.drain_runtime_ready_queue();
        }
        None => {
            for (idx, source_actor) in sources.iter().enumerate() {
                spawned_lazy_watchers |= connect_direct_state_output_source_with_registry(
                    None,
                    &output,
                    source_actor,
                    idx,
                    on_source_value.clone(),
                );
            }
            REGISTRY.with(|reg| reg.borrow_mut().drain_runtime_ready_queue());
        }
    }

    if spawned_lazy_watchers {
        poll_spawned_async_source_tasks_after_insertion();
    }

    Some(output)
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
        ActorStreamCreationPlan::EndedWithoutValue => {
            create_ended_actor_without_value(persistence_id, scope_id, Some(origin))
        }
        ActorStreamCreationPlan::Stream(value_stream) => create_stream_driven_actor_arc_info(
            persistence_id,
            scope_id,
            Some(origin),
            value_stream,
        ),
    }
}

async fn drain_value_stream_to_actor_state<S>(target_actor_id: ActorId, value_stream: S)
where
    S: Stream<Item = Value> + 'static,
{
    drain_external_stream_to_runtime_queue(
        value_stream,
        move |reg, value| {
            enqueue_actor_value_on_runtime_queue_by_id_with_registry(reg, target_actor_id, value);
        },
        move |reg| {
            enqueue_actor_source_end_on_runtime_queue_by_id_with_registry(reg, target_actor_id);
        },
    )
    .await;
}

fn start_retained_external_stream_task_until<S, T, F, G, C>(
    scope_id: ScopeId,
    cancel: C,
    source_stream: S,
    on_item: F,
    on_end: G,
) where
    S: Stream<Item = T> + 'static,
    T: 'static,
    F: FnMut(&mut ActorRegistry, T) + 'static,
    G: FnMut(&mut ActorRegistry) + 'static,
    C: Future + 'static,
{
    start_retained_scope_task_until(
        scope_id,
        cancel,
        drain_external_stream_to_runtime_queue(source_stream, on_item, on_end),
    );
}

fn start_retained_actor_external_stream_feeder_task<S, T, F, G>(
    actor: &ActorHandle,
    source_stream: S,
    on_item: F,
    on_end: G,
) where
    S: Stream<Item = T> + 'static,
    T: 'static,
    F: FnMut(&mut ActorRegistry, T) + 'static,
    G: FnMut(&mut ActorRegistry) + 'static,
{
    let Some(drop_waiter) = actor.stored_state.wait_for_drop() else {
        return;
    };
    let Some(scope_id) = REGISTRY.with(|reg| {
        reg.borrow()
            .get_actor(actor.actor_id)
            .map(|actor| actor.scope_id)
    }) else {
        return;
    };

    start_retained_external_stream_task_until(
        scope_id,
        drop_waiter,
        source_stream,
        on_item,
        on_end,
    );
}

fn start_retained_list_external_stream_feeder_task<S, T, F, G>(
    stored_state: &ListStoredState,
    scope_id: ScopeId,
    source_stream: S,
    on_item: F,
    on_end: G,
) where
    S: Stream<Item = T> + 'static,
    T: 'static,
    F: FnMut(&mut ActorRegistry, T) + 'static,
    G: FnMut(&mut ActorRegistry) + 'static,
{
    let Some(drop_waiter) = stored_state.wait_for_drop() else {
        return;
    };

    start_retained_external_stream_task_until(
        scope_id,
        drop_waiter,
        source_stream,
        on_item,
        on_end,
    );
}

fn retain_scope_task(scope_id: ScopeId, task: AsyncSourceTaskHandle) {
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        let _ = reg.insert_async_source(scope_id, task);
    });
}

fn start_retained_scope_task_with_registry<F>(reg: &mut ActorRegistry, scope_id: ScopeId, future: F)
where
    F: Future<Output = ()> + 'static,
{
    let Some(async_source_id) = reg.reserve_async_source_for_scope(scope_id) else {
        return;
    };
    let task = spawn_async_source_task_for_reserved_slot(async move {
        future.await;
        enqueue_async_source_cleanup_on_runtime_queue(async_source_id);
    });
    let inserted = reg.set_async_source_task(async_source_id, task);
    assert!(
        inserted,
        "reserved scope feeder task slot should stay valid until task insertion"
    );
}

pub(crate) fn start_retained_scope_task<F>(scope_id: ScopeId, future: F)
where
    F: Future<Output = ()> + 'static,
{
    let Some(async_source_id) =
        REGISTRY.with(|reg| reg.borrow_mut().reserve_async_source_for_scope(scope_id))
    else {
        return;
    };
    let task = spawn_async_source_task_for_reserved_slot(async move {
        future.await;
        enqueue_async_source_cleanup_on_runtime_queue(async_source_id);
    });
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        let inserted = reg.set_async_source_task(async_source_id, task);
        assert!(
            inserted,
            "reserved scope feeder task slot should stay valid until task insertion"
        );
    });
    poll_spawned_async_source_tasks_after_insertion();
}

fn start_retained_scope_task_until<F, C>(scope_id: ScopeId, cancel: C, future: F)
where
    F: Future<Output = ()> + 'static,
    C: Future + 'static,
{
    start_retained_scope_task(scope_id, async move {
        let cancel = cancel.fuse();
        let future = future.fuse();
        zoon::futures_util::pin_mut!(cancel, future);
        select! {
            _ = future => {}
            _ = cancel => {}
        }
    });
}

fn start_retained_scope_task_until_with_registry<F, C>(
    reg: &mut ActorRegistry,
    scope_id: ScopeId,
    cancel: C,
    future: F,
) where
    F: Future<Output = ()> + 'static,
    C: Future + 'static,
{
    start_retained_scope_task_with_registry(reg, scope_id, async move {
        let cancel = cancel.fuse();
        let future = future.fuse();
        zoon::futures_util::pin_mut!(cancel, future);
        select! {
            _ = future => {}
            _ = cancel => {}
        }
    });
}

fn start_retained_list_task<F>(stored_state: &ListStoredState, scope_id: ScopeId, future: F)
where
    F: Future<Output = ()> + 'static,
{
    let list_owner_key = stored_state.async_owner_key();
    let Some(async_source_id) = REGISTRY.with(|reg| {
        reg.borrow_mut()
            .reserve_async_source_for_list(scope_id, list_owner_key)
    }) else {
        return;
    };
    let task = spawn_async_source_task_for_reserved_slot(async move {
        future.await;
        enqueue_async_source_cleanup_on_runtime_queue(async_source_id);
    });
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        let inserted = reg.set_async_source_task(async_source_id, task);
        assert!(
            inserted,
            "reserved list feeder task slot should stay valid until task insertion"
        );
    });
    poll_spawned_async_source_tasks_after_insertion();
}

pub(crate) fn start_retained_actor_scope_task<F>(actor: &ActorHandle, future: F)
where
    F: Future<Output = ()> + 'static,
{
    let Some(drop_waiter) = actor.stored_state.wait_for_drop() else {
        return;
    };
    let Some(scope_id) = REGISTRY.with(|reg| {
        reg.borrow()
            .get_actor(actor.actor_id)
            .map(|actor| actor.scope_id)
    }) else {
        return;
    };
    start_retained_scope_task_until(scope_id, drop_waiter, future);
}

pub(crate) fn start_retained_actor_scope_task_with_registry<F>(
    reg: &mut ActorRegistry,
    actor: &ActorHandle,
    future: F,
) where
    F: Future<Output = ()> + 'static,
{
    let Some(drop_waiter) = actor.stored_state.wait_for_drop() else {
        return;
    };
    let Some(scope_id) = reg.get_actor(actor.actor_id).map(|actor| actor.scope_id) else {
        return;
    };
    start_retained_scope_task_until_with_registry(reg, scope_id, drop_waiter, future);
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

pub(crate) fn retain_actor_handles_with_registry<I>(
    reg: Option<&mut ActorRegistry>,
    actor: &ActorHandle,
    retained: I,
) where
    I: IntoIterator<Item = ActorHandle>,
{
    match reg {
        Some(reg) => {
            if let Some(owned_actor) = reg.get_actor_mut(actor.actor_id) {
                owned_actor.retained_actors.extend(retained);
            }
        }
        None => retain_actor_handles(actor, retained),
    }
}

/// Create a lazy actor for demand-driven evaluation (used in HOLD body context).
#[track_caller]
pub fn create_actor_lazy<S: Stream<Item = Value> + 'static>(
    value_stream: S,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    match plan_actor_stream_creation(value_stream) {
        ActorStreamCreationPlan::Constant(value) => {
            create_constant_actor(persistence_id, value, scope_id)
        }
        ActorStreamCreationPlan::EndedWithoutValue => {
            create_ended_actor_without_value(persistence_id, scope_id, None)
        }
        ActorStreamCreationPlan::Stream(value_stream) => {
            create_lazy_stream_driven_actor_arc_info(persistence_id, scope_id, value_stream)
        }
    }
}

pub(crate) fn create_actor_lazy_with_registry<S>(
    reg: &mut ActorRegistry,
    value_stream: S,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle
where
    S: Stream<Item = Value> + 'static,
{
    match plan_actor_stream_creation(value_stream) {
        ActorStreamCreationPlan::Constant(value) => {
            create_constant_actor_with_registry(reg, persistence_id, value, scope_id)
        }
        ActorStreamCreationPlan::EndedWithoutValue => {
            let actor = create_direct_state_actor_arc_info_with_registry(
                reg,
                persistence_id,
                scope_id,
                None,
            );
            actor.stored_state.set_has_future_state_updates(false);
            actor
        }
        ActorStreamCreationPlan::Stream(value_stream) => {
            create_lazy_stream_driven_actor_arc_info_with_registry(
                reg,
                persistence_id,
                scope_id,
                value_stream,
            )
        }
    }
}

enum ForwardingSubscriptionPlan {
    NoFutureSubscription,
    SubscribeAfterVersion(u64),
}

fn seed_forwarding_actor_from_source_now_with_registry(
    mut reg: Option<&mut ActorRegistry>,
    forwarding_actor: &ActorHandle,
    source_actor: &ActorHandle,
) -> ForwardingSubscriptionPlan {
    if let Some(value) = source_actor.constant_value() {
        forwarding_actor.store_value_directly_with_registry(reg.as_deref_mut(), value);
        return ForwardingSubscriptionPlan::NoFutureSubscription;
    }

    match source_actor.current_value() {
        Ok(value) => {
            forwarding_actor.store_value_directly_with_registry(reg.as_deref_mut(), value);
            if source_actor.stored_state.has_future_state_updates() {
                ForwardingSubscriptionPlan::SubscribeAfterVersion(source_actor.version())
            } else {
                ForwardingSubscriptionPlan::NoFutureSubscription
            }
        }
        Err(CurrentValueError::NoValueYet) => {
            if source_actor.stored_state.has_future_state_updates() {
                ForwardingSubscriptionPlan::SubscribeAfterVersion(source_actor.version())
            } else {
                ForwardingSubscriptionPlan::NoFutureSubscription
            }
        }
        Err(CurrentValueError::ActorDropped) => ForwardingSubscriptionPlan::NoFutureSubscription,
    }
}

fn seed_forwarding_actor_from_source_now(
    forwarding_actor: &ActorHandle,
    source_actor: &ActorHandle,
) -> ForwardingSubscriptionPlan {
    seed_forwarding_actor_from_source_now_with_registry(None, forwarding_actor, source_actor)
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

    if source_actor.stored_state.is_alive() && source_actor.stored_state.has_future_state_updates()
    {
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
        forwarding_actor.actor_id,
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

#[track_caller]
fn connect_forwarding_direct_state_subscription_with_registry(
    reg: Option<&mut ActorRegistry>,
    forwarding_actor: &ActorHandle,
    source_actor: &ActorHandle,
    last_seen_version: u64,
) -> bool {
    let caller = std::panic::Location::caller();
    let has_lazy_delegate = match reg {
        Some(ref reg) => source_actor.lazy_delegate_with_registry(reg),
        None => source_actor.lazy_delegate(),
    }
    .is_some();
    if has_lazy_delegate || !source_actor.stored_state.is_alive() {
        return false;
    }

    if source_actor.version() > last_seen_version {
        for value in source_actor
            .stored_state
            .get_values_since(last_seen_version)
        {
            enqueue_actor_value_on_runtime_queue(forwarding_actor, value);
        }
    }

    match reg {
        Some(reg) => {
            source_actor.stored_state.register_direct_subscriber(
                reg,
                Rc::new({
                    let forwarding_actor = forwarding_actor.clone();
                    move |reg, value| {
                        if !forwarding_actor.stored_state.is_alive() {
                            return false;
                        }
                        match value {
                            Some(value) => reg.enqueue_actor_mailbox_value(
                                forwarding_actor.actor_id,
                                value.clone(),
                            ),
                            None => reg.enqueue_actor_mailbox_clear(forwarding_actor.actor_id),
                        }
                        true
                    }
                }),
            );
            reg.drain_runtime_ready_queue();
        }
        None => {
            REGISTRY.with(|reg| {
                let mut reg = reg.try_borrow_mut().unwrap_or_else(|_| {
                    panic!(
                        "connect_forwarding_direct_state_subscription_with_registry nested REGISTRY borrow from {}",
                        caller
                    )
                });
                source_actor.stored_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let forwarding_actor = forwarding_actor.clone();
                        move |reg, value| {
                            if !forwarding_actor.stored_state.is_alive() {
                                return false;
                            }
                            match value {
                                Some(value) => reg.enqueue_actor_mailbox_value(
                                    forwarding_actor.actor_id,
                                    value.clone(),
                                ),
                                None => reg.enqueue_actor_mailbox_clear(forwarding_actor.actor_id),
                            }
                            true
                        }
                    }),
                );
                reg.drain_runtime_ready_queue();
            });
        }
    }
    true
}

fn connect_forwarding_direct_state_subscription(
    forwarding_actor: &ActorHandle,
    source_actor: &ActorHandle,
    last_seen_version: u64,
) -> bool {
    connect_forwarding_direct_state_subscription_with_registry(
        None,
        forwarding_actor,
        source_actor,
        last_seen_version,
    )
}

/// Forward the source actor's current value once and then only future updates.
pub(crate) fn connect_forwarding_current_and_future_with_registry(
    mut reg: Option<&mut ActorRegistry>,
    forwarding_actor: ActorHandle,
    source_actor: ActorHandle,
) {
    let last_seen_version = match seed_forwarding_actor_from_source_now_with_registry(
        reg.as_deref_mut(),
        &forwarding_actor,
        &source_actor,
    ) {
        ForwardingSubscriptionPlan::NoFutureSubscription => {
            forwarding_actor
                .stored_state
                .set_has_future_state_updates(false);
            return;
        }
        ForwardingSubscriptionPlan::SubscribeAfterVersion(last_seen_version) => last_seen_version,
    };

    if connect_forwarding_direct_state_subscription_with_registry(
        reg.as_deref_mut(),
        &forwarding_actor,
        &source_actor,
        last_seen_version,
    ) {
        return;
    }

    let forwarding_actor_for_task = forwarding_actor.clone();
    let future = async move {
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
    };
    if let Some(reg) = reg {
        start_retained_actor_scope_task_with_registry(reg, &forwarding_actor, future);
    } else {
        start_retained_actor_scope_task(&forwarding_actor, future);
    }
}

pub fn connect_forwarding_current_and_future(
    forwarding_actor: ActorHandle,
    source_actor: ActorHandle,
) {
    connect_forwarding_current_and_future_with_registry(None, forwarding_actor, source_actor);
}

/// Forward all currently buffered and future values from the source actor.
pub fn connect_forwarding_replay_all(forwarding_actor: ActorHandle, source_actor: ActorHandle) {
    let last_seen_version =
        match seed_forwarding_replay_all_from_source_now(&forwarding_actor, &source_actor) {
            ForwardingSubscriptionPlan::NoFutureSubscription => {
                forwarding_actor
                    .stored_state
                    .set_has_future_state_updates(false);
                return;
            }
            ForwardingSubscriptionPlan::SubscribeAfterVersion(last_seen_version) => {
                last_seen_version
            }
        };

    if connect_forwarding_direct_state_subscription(
        &forwarding_actor,
        &source_actor,
        last_seen_version,
    ) {
        return;
    }

    let forwarding_actor_for_task = forwarding_actor.clone();
    start_retained_actor_scope_task(&forwarding_actor, async move {
        drain_value_stream_to_actor_state(
            forwarding_actor_for_task.actor_id,
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

pub(crate) fn value_actor_from_json_with_registry(
    reg: &mut ActorRegistry,
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
    create_constant_actor_with_registry(reg, parser::PersistenceId::new(), value, scope_id)
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

    let scope_id = list_arc.scope_id;
    let owner_state = stored_state.clone();
    start_retained_list_task(&owner_state, scope_id, async move {
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

pub(crate) fn try_materialize_snapshot_value_now_with_registry(
    mut reg: Option<&mut ActorRegistry>,
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
                    let materialized = try_materialize_snapshot_value_now_with_registry(
                        reg.as_deref_mut(),
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )?;
                    match reg.as_deref_mut() {
                        Some(reg) => create_constant_actor_with_registry(
                            reg,
                            parser::PersistenceId::new(),
                            materialized,
                            scope_id,
                        ),
                        None => create_constant_actor(
                            parser::PersistenceId::new(),
                            materialized,
                            scope_id,
                        ),
                    }
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
                    let materialized = try_materialize_snapshot_value_now_with_registry(
                        reg.as_deref_mut(),
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )?;
                    match reg.as_deref_mut() {
                        Some(reg) => create_constant_actor_with_registry(
                            reg,
                            parser::PersistenceId::new(),
                            materialized,
                            scope_id,
                        ),
                        None => create_constant_actor(
                            parser::PersistenceId::new(),
                            materialized,
                            scope_id,
                        ),
                    }
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
                        let materialized = try_materialize_snapshot_value_now_with_registry(
                            reg.as_deref_mut(),
                            item_value,
                            construct_context.clone(),
                            actor_context.clone(),
                        )?;
                        match reg.as_deref_mut() {
                            Some(reg) => create_constant_actor_with_registry(
                                reg,
                                parser::PersistenceId::new(),
                                materialized,
                                scope_id,
                            ),
                            None => create_constant_actor(
                                parser::PersistenceId::new(),
                                materialized,
                                scope_id,
                            ),
                        }
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
            let materialized_inner = try_materialize_snapshot_value_now_with_registry(
                reg,
                *inner,
                construct_context,
                actor_context,
            )?;
            Some(Value::Flushed(Box::new(materialized_inner), metadata))
        }
        other => Some(other),
    }
}

pub(crate) fn try_materialize_snapshot_value_now(
    value: Value,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Option<Value> {
    try_materialize_snapshot_value_now_with_registry(None, value, construct_context, actor_context)
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
        Self::new_arc_value_actor_with_registry(
            None,
            construct_info,
            construct_context,
            idempotency_key,
            actor_context,
            variables,
        )
    }

    pub(crate) fn new_arc_value_actor_with_registry(
        reg: Option<&mut ActorRegistry>,
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
        match reg {
            Some(reg) => create_constant_actor_with_registry(
                reg,
                parser::PersistenceId::new(),
                initial_value,
                scope_id,
            ),
            None => create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id),
        }
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
        Self::new_arc_value_actor_with_registry(
            None,
            construct_info,
            construct_context,
            idempotency_key,
            actor_context,
            tag,
            variables,
        )
    }

    pub(crate) fn new_arc_value_actor_with_registry(
        reg: Option<&mut ActorRegistry>,
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
        match reg {
            Some(reg) => create_constant_actor_with_registry(
                reg,
                parser::PersistenceId::new(),
                initial_value,
                scope_id,
            ),
            None => create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id),
        }
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
        Self::new_arc_value_actor_with_registry(
            None,
            construct_info,
            construct_context,
            idempotency_key,
            actor_context,
            text,
        )
    }

    pub(crate) fn new_arc_value_actor_with_registry(
        reg: Option<&mut ActorRegistry>,
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
        match reg {
            Some(reg) => create_constant_actor_with_registry(
                reg,
                parser::PersistenceId::new(),
                initial_value,
                scope_id,
            ),
            None => create_constant_actor(parser::PersistenceId::new(), initial_value, scope_id),
        }
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

type ListDirectSubscriber = Rc<dyn Fn(&mut ActorRegistry, &ListChange) -> bool>;

enum ListRuntimeWork {
    Change {
        construct_info: ConstructInfoComplete,
        change: ListChange,
        broadcast_change: bool,
    },
    LiveInputChange {
        construct_info: ConstructInfoComplete,
        change: ListChange,
    },
    BroadcastSnapshotIfReady,
    SourceEnded,
}

struct ListStoredStateInner {
    is_alive: Cell<bool>,
    initialized: Cell<bool>,
    has_future_state_updates: Cell<bool>,
    broadcast_live_changes: Cell<bool>,
    rebroadcast_snapshot_on_output_close: Cell<bool>,
    scheduled_for_runtime: Cell<bool>,
    last_broadcast_version: Cell<u64>,
    diff_history: RefCell<DiffHistory>,
    runtime_work: RefCell<VecDeque<ListRuntimeWork>>,
    initialization_waiters: RefCell<Vec<oneshot::Sender<()>>>,
    update_waiters: RefCell<Vec<oneshot::Sender<()>>>,
    drop_waiters: RefCell<Vec<oneshot::Sender<()>>>,
    change_subscribers: RefCell<SmallVec<[NamedChannel<ListChange>; 4]>>,
    pending_change_subscribers: RefCell<SmallVec<[NamedChannel<ListChange>; 2]>>,
    direct_subscribers: RefCell<SmallVec<[ListDirectSubscriber; 2]>>,
    pending_direct_subscribers: RefCell<SmallVec<[ListDirectSubscriber; 2]>>,
}

impl ListStoredState {
    fn new(config: DiffHistoryConfig) -> Self {
        Self {
            inner: Rc::new(ListStoredStateInner {
                is_alive: Cell::new(true),
                initialized: Cell::new(false),
                has_future_state_updates: Cell::new(false),
                broadcast_live_changes: Cell::new(false),
                rebroadcast_snapshot_on_output_close: Cell::new(false),
                scheduled_for_runtime: Cell::new(false),
                last_broadcast_version: Cell::new(0),
                diff_history: RefCell::new(DiffHistory::new(config)),
                runtime_work: RefCell::new(VecDeque::new()),
                initialization_waiters: RefCell::new(Vec::new()),
                update_waiters: RefCell::new(Vec::new()),
                drop_waiters: RefCell::new(Vec::new()),
                change_subscribers: RefCell::new(SmallVec::new()),
                pending_change_subscribers: RefCell::new(SmallVec::new()),
                direct_subscribers: RefCell::new(SmallVec::new()),
                pending_direct_subscribers: RefCell::new(SmallVec::new()),
            }),
        }
    }

    fn downgrade(&self) -> Weak<ListStoredStateInner> {
        Rc::downgrade(&self.inner)
    }

    fn from_weak(inner: Weak<ListStoredStateInner>) -> Option<Self> {
        Some(Self {
            inner: inner.upgrade()?,
        })
    }

    fn async_owner_key(&self) -> usize {
        Rc::as_ptr(&self.inner) as usize
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive.get()
    }

    fn is_initialized(&self) -> bool {
        self.inner.initialized.get()
    }

    fn mark_dropped(&self) {
        self.inner.is_alive.set(false);
        self.inner.has_future_state_updates.set(false);
        self.inner.runtime_work.borrow_mut().clear();
        self.inner.initialization_waiters.borrow_mut().clear();
        self.inner.update_waiters.borrow_mut().clear();
        for waiter in self.inner.drop_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
        self.inner.change_subscribers.borrow_mut().clear();
        self.inner.pending_change_subscribers.borrow_mut().clear();
        self.inner.direct_subscribers.borrow_mut().clear();
        self.inner.pending_direct_subscribers.borrow_mut().clear();
    }

    fn set_has_future_state_updates(&self, has_future_state_updates: bool) {
        self.inner
            .has_future_state_updates
            .set(has_future_state_updates);
    }

    fn has_future_state_updates(&self) -> bool {
        self.inner.has_future_state_updates.get()
    }

    fn set_broadcast_live_changes(&self, broadcast_live_changes: bool) {
        self.inner
            .broadcast_live_changes
            .set(broadcast_live_changes);
    }

    fn broadcast_live_changes(&self) -> bool {
        self.inner.broadcast_live_changes.get()
    }

    fn set_rebroadcast_snapshot_on_output_close(&self, rebroadcast: bool) {
        self.inner
            .rebroadcast_snapshot_on_output_close
            .set(rebroadcast);
    }

    fn rebroadcast_snapshot_on_output_close(&self) -> bool {
        self.inner.rebroadcast_snapshot_on_output_close.get()
    }

    fn push_runtime_work(&self, work: ListRuntimeWork) -> bool {
        self.inner.runtime_work.borrow_mut().push_back(work);
        if self.inner.scheduled_for_runtime.replace(true) {
            false
        } else {
            true
        }
    }

    fn take_runtime_work(&self) -> VecDeque<ListRuntimeWork> {
        self.inner.scheduled_for_runtime.set(false);
        std::mem::take(&mut *self.inner.runtime_work.borrow_mut())
    }

    fn has_pending_runtime_work(&self) -> bool {
        !self.inner.runtime_work.borrow().is_empty()
    }

    fn is_scheduled_for_runtime(&self) -> bool {
        self.inner.scheduled_for_runtime.get()
    }

    fn set_scheduled_for_runtime(&self, scheduled: bool) {
        self.inner.scheduled_for_runtime.set(scheduled);
    }

    fn last_broadcast_version(&self) -> u64 {
        self.inner.last_broadcast_version.get()
    }

    fn set_last_broadcast_version(&self, version: u64) {
        self.inner.last_broadcast_version.set(version);
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

    fn wait_for_drop(&self) -> Option<oneshot::Receiver<()>> {
        if !self.is_alive() {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.inner.drop_waiters.borrow_mut().push(tx);
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

    fn register_direct_subscriber(
        &self,
        reg: &mut ActorRegistry,
        subscriber: ListDirectSubscriber,
    ) {
        if !self.is_initialized() {
            self.inner
                .pending_direct_subscribers
                .borrow_mut()
                .push(subscriber);
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
        let first_change_to_send = ListChange::Replace {
            items: snapshot_items,
        };

        if subscriber(reg, &first_change_to_send) {
            self.inner.direct_subscribers.borrow_mut().push(subscriber);
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

    fn activate_pending_direct_subscribers(
        &self,
        reg: &mut ActorRegistry,
        items: Arc<[ActorHandle]>,
    ) {
        let mut pending_subscribers = self.inner.pending_direct_subscribers.borrow_mut();
        let mut direct_subscribers = self.inner.direct_subscribers.borrow_mut();

        for pending_subscriber in pending_subscribers.drain(..) {
            let first_change_to_send = ListChange::Replace {
                items: items.clone(),
            };
            if pending_subscriber(reg, &first_change_to_send) {
                direct_subscribers.push(pending_subscriber);
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

    fn notify_direct_subscribers(&self, reg: &mut ActorRegistry, change: &ListChange) {
        self.inner
            .direct_subscribers
            .borrow_mut()
            .retain(|subscriber| subscriber(reg, change));
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
    scope_id: ScopeId,
    /// Direct list-owned diff/snapshot state for current reads.
    stored_state: ListStoredState,
}

fn register_list_output_valve_runtime_subscriber_state(
    active: &Cell<bool>,
    direct_subscribers: &RefCell<SmallVec<[OutputValveDirectSubscriber; 4]>>,
    stored_state: &ListStoredState,
) {
    let stored_state_weak = stored_state.downgrade();
    register_output_valve_direct_subscriber(
        active,
        direct_subscribers,
        Rc::new(move |event| {
            let Some(stored_state) = ListStoredState::from_weak(stored_state_weak.clone()) else {
                return false;
            };

            match event {
                OutputValveDirectEvent::Impulse => {
                    REGISTRY.with(|reg| {
                        let mut reg = reg.borrow_mut();
                        reg.enqueue_list_runtime_work(
                            stored_state.clone(),
                            ListRuntimeWork::BroadcastSnapshotIfReady,
                        );
                        reg.drain_runtime_ready_queue();
                    });
                }
                OutputValveDirectEvent::Closed => {
                    stored_state.set_broadcast_live_changes(true);
                    if stored_state.rebroadcast_snapshot_on_output_close()
                        && !stored_state.has_future_state_updates()
                    {
                        REGISTRY.with(|reg| {
                            let mut reg = reg.borrow_mut();
                            reg.enqueue_list_runtime_work(
                                stored_state.clone(),
                                ListRuntimeWork::BroadcastSnapshotIfReady,
                            );
                            reg.drain_runtime_ready_queue();
                        });
                    }
                }
            }
            true
        }),
    );
}

fn register_list_output_valve_runtime_subscriber(
    output_valve_signal: &ActorOutputValveSignal,
    stored_state: &ListStoredState,
) {
    register_list_output_valve_runtime_subscriber_state(
        output_valve_signal.active.as_ref(),
        output_valve_signal.direct_subscribers.as_ref(),
        stored_state,
    );
}

impl Drop for List {
    fn drop(&mut self) {
        self.stored_state.mark_dropped();
        let list_owner_key = self.stored_state.async_owner_key();
        if !take_list_async_sources_now(list_owner_key) {
            PENDING_LIST_ASYNC_SOURCE_CLEANUPS
                .with(|pending| pending.borrow_mut().push(list_owner_key));
        }
    }
}

impl List {
    fn new_with_stored_state(
        construct_info: ConstructInfoComplete,
        scope_id: ScopeId,
        stored_state: ListStoredState,
    ) -> Self {
        Self {
            construct_info,
            scope_id,
            stored_state,
        }
    }

    fn new_static(
        construct_info: ConstructInfoComplete,
        scope_id: ScopeId,
        items: Vec<ActorHandle>,
    ) -> Self {
        let stored_state = ListStoredState::new(DiffHistoryConfig::default());
        let items: Arc<[ActorHandle]> = Arc::from(items);
        stored_state.record_change(&ListChange::Replace {
            items: items.clone(),
        });
        stored_state.mark_initialized();
        Self::new_with_stored_state(construct_info, scope_id, stored_state)
    }

    fn new_static_with_optional_output_valve_stream(
        construct_info: ConstructInfoComplete,
        items: Vec<ActorHandle>,
        scope_id: ScopeId,
        output_valve_signal: Option<Arc<ActorOutputValveSignal>>,
    ) -> Self {
        let list = Self::new_static(construct_info, scope_id, items);
        if let Some(output_valve_signal) = output_valve_signal {
            register_list_output_valve_runtime_subscriber(
                output_valve_signal.as_ref(),
                &list.stored_state,
            );
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
        let scope_id = actor_context.scope_id();
        let output_valve_signal = actor_context.output_valve_signal;
        Self::new_static_with_optional_output_valve_stream(
            construct_info,
            items,
            scope_id,
            output_valve_signal,
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
        let scope_id = actor_context.scope_id();
        let stored_state = ListStoredState::new(DiffHistoryConfig::default());
        let list = Self {
            construct_info,
            scope_id,
            stored_state,
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
        list.stored_state
            .set_broadcast_live_changes(output_valve_signal.is_none());
        list.stored_state
            .set_rebroadcast_snapshot_on_output_close(output_valve_signal.is_some());

        match initial_items {
            Some(_initial_items) => match ready_drain_status {
                ReadyListChangeDrainStatus::Ended => {
                    if let Some(output_valve_signal) = output_valve_signal.as_ref() {
                        register_list_output_valve_runtime_subscriber(
                            output_valve_signal.as_ref(),
                            &list.stored_state,
                        );
                    }
                    drop(extra_owned_data);
                }
                ReadyListChangeDrainStatus::Pending => {
                    if let Some(output_valve_signal) = output_valve_signal.as_ref() {
                        register_list_output_valve_runtime_subscriber(
                            output_valve_signal.as_ref(),
                            &list.stored_state,
                        );
                    }
                    let stored_state = list.stored_state.clone();
                    start_retained_list_external_stream_feeder_task(
                        &stored_state,
                        scope_id,
                        change_stream,
                        {
                            let construct_info = construct_info.clone();
                            let stored_state = stored_state.clone();
                            move |reg, change| {
                                enqueue_list_work_on_runtime_queue_with_registry(
                                    reg,
                                    &stored_state,
                                    ListRuntimeWork::LiveInputChange {
                                        construct_info: construct_info.clone(),
                                        change,
                                    },
                                );
                            }
                        },
                        {
                            let stored_state = stored_state.clone();
                            move |reg| {
                                let _extra_owned_data = &extra_owned_data;
                                enqueue_list_work_on_runtime_queue_with_registry(
                                    reg,
                                    &stored_state,
                                    ListRuntimeWork::SourceEnded,
                                );
                            }
                        },
                    );
                }
            },
            None => {
                if let Some(output_valve_signal) = output_valve_signal.as_ref() {
                    register_list_output_valve_runtime_subscriber(
                        output_valve_signal.as_ref(),
                        &list.stored_state,
                    );
                }
                let stored_state = list.stored_state.clone();
                start_retained_list_external_stream_feeder_task(
                    &stored_state,
                    scope_id,
                    change_stream,
                    {
                        let construct_info = construct_info.clone();
                        let stored_state = stored_state.clone();
                        move |reg, change| {
                            enqueue_list_work_on_runtime_queue_with_registry(
                                reg,
                                &stored_state,
                                ListRuntimeWork::LiveInputChange {
                                    construct_info: construct_info.clone(),
                                    change,
                                },
                            );
                        }
                    },
                    {
                        let stored_state = stored_state.clone();
                        move |reg| {
                            let _extra_owned_data = &extra_owned_data;
                            enqueue_list_work_on_runtime_queue_with_registry(
                                reg,
                                &stored_state,
                                ListRuntimeWork::SourceEnded,
                            );
                        }
                    },
                );
            }
        }

        list
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

    pub(crate) fn new_arc_value_actor_with_registry(
        reg: &mut ActorRegistry,
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
        create_constant_actor_with_registry(
            reg,
            parser::PersistenceId::new(),
            initial_value,
            scope_id,
        )
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
        let scope_id = actor_context.scope_id();
        let output_valve_signal = actor_context.output_valve_signal;
        let list = Arc::new(List::new_static_with_optional_output_valve_stream(
            inner_construct_info,
            items,
            scope_id,
            output_valve_signal,
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

    pub(crate) fn new_arc_value_actor_with_persistence_with_registry(
        reg: &mut ActorRegistry,
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        code_items: impl Into<Vec<ActorHandle>>,
    ) -> ActorHandle {
        let code_items = code_items.into();
        let persistence = construct_info.persistence;

        let Some(persistence_data) = persistence else {
            return Self::new_arc_value_actor_with_registry(
                reg,
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
            return create_constant_actor_with_registry(
                reg,
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
                        value_actor_from_json_with_registry(
                            reg,
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
            return create_constant_actor_with_registry(
                reg,
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
        let construct_context_for_load = construct_context.clone();
        let actor_context_for_load = actor_context.clone();
        let actor_id_for_load = actor_id.clone();
        let value_future = async move {
            let loaded_items: Option<Vec<serde_json::Value>> =
                construct_storage.clone().load_state(persistence_id).await;

            let initial_items = if let Some(json_items) = loaded_items {
                let code_items_len = code_items.len();
                let json_items_len = json_items.len();
                let max_len = code_items_len.max(json_items_len);

                (0..max_len)
                    .map(|i| {
                        if i < code_items_len {
                            code_items[i].clone()
                        } else {
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
        create_actor_from_future_with_registry(
            reg,
            value_future,
            parser::PersistenceId::new(),
            scope_id,
        )
    }
}

pub(crate) struct DirectRuntimeListActor {
    pub actor: ActorHandle,
    pub list: Arc<List>,
    pub construct_info: ConstructInfoComplete,
    pub broadcast_change: bool,
    pub skip_initial_source_replace: bool,
}

pub(crate) fn create_direct_runtime_list_actor_with_initial_items(
    construct_info: ConstructInfo,
    actor_context: ActorContext,
    retained_actors: Vec<ActorHandle>,
    initial_items: Vec<ActorHandle>,
    has_future_state_updates: bool,
    skip_initial_source_replace: bool,
) -> DirectRuntimeListActor {
    let scope_id = actor_context.scope_id();
    let construct_info = construct_info.complete(ConstructType::List);
    let stored_state = ListStoredState::new(DiffHistoryConfig::default());
    if !initial_items.is_empty() {
        let items: Arc<[ActorHandle]> = Arc::from(initial_items);
        stored_state.record_change(&ListChange::Replace {
            items: items.clone(),
        });
        stored_state.mark_initialized();
    }
    stored_state.set_has_future_state_updates(has_future_state_updates);
    stored_state.set_broadcast_live_changes(actor_context.output_valve_signal.is_none());
    stored_state
        .set_rebroadcast_snapshot_on_output_close(actor_context.output_valve_signal.is_some());

    let list = Arc::new(List::new_with_stored_state(
        construct_info.clone(),
        scope_id,
        stored_state.clone(),
    ));

    if let Some(output_valve_signal) = actor_context.output_valve_signal.clone() {
        register_list_output_valve_runtime_subscriber(output_valve_signal.as_ref(), &stored_state);
    }

    let actor = create_constant_actor(
        parser::PersistenceId::new(),
        Value::List(list.clone(), ValueMetadata::new(ValueIdempotencyKey::new())),
        scope_id,
    );
    retain_actor_handles_with_registry(None, &actor, retained_actors);

    DirectRuntimeListActor {
        actor,
        list,
        construct_info,
        broadcast_change: actor_context.output_valve_signal.is_none(),
        skip_initial_source_replace,
    }
}

fn should_restore_simple_persistent_list_items(loaded_items: &[serde_json::Value]) -> bool {
    loaded_items.iter().all(|json| match json {
        serde_json::Value::Object(obj) => {
            if obj.contains_key("$Tag") && obj.len() <= 2 {
                true
            } else if obj.len() > 1 {
                if LOG_DEBUG {
                    zoon::println!(
                        "[DEBUG] Skipping JSON restoration: complex object with {} fields",
                        obj.len()
                    );
                }
                false
            } else {
                true
            }
        }
        _ => true,
    })
}

pub(crate) fn try_create_direct_runtime_list_actor(
    construct_info: ConstructInfo,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    persistence_id: parser::PersistenceId,
    retained_actors: Vec<ActorHandle>,
    has_future_state_updates: bool,
) -> Option<DirectRuntimeListActor> {
    let construct_storage = construct_context.construct_storage.clone();
    let restored_items = if construct_storage.is_disabled() {
        None
    } else {
        construct_storage
            .load_state_now::<Vec<serde_json::Value>>(persistence_id)
            .and_then(|loaded_items| {
                if !should_restore_simple_persistent_list_items(&loaded_items) {
                    return None;
                }

                Some(
                    loaded_items
                        .iter()
                        .enumerate()
                        .map(|(i, json)| {
                            value_actor_from_json(
                                json,
                                construct_info
                                    .id
                                    .clone()
                                    .with_child_id(format!("restored_item_{i}")),
                                construct_context.clone(),
                                parser::PersistenceId::new(),
                                actor_context.clone(),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
    };
    let skip_initial_source_replace = restored_items.is_some();
    let restored_items = restored_items.unwrap_or_default();
    let direct_result = create_direct_runtime_list_actor_with_initial_items(
        construct_info,
        actor_context.clone(),
        retained_actors,
        restored_items,
        has_future_state_updates,
        skip_initial_source_replace,
    );

    if !construct_storage.is_disabled() {
        let list_for_save = direct_result.list.clone();
        let owner_state = list_for_save.stored_state.clone();
        let scope_id = actor_context.scope_id();
        start_retained_list_task(&owner_state, scope_id, async move {
            save_or_watch_persistent_list(list_for_save, construct_storage, persistence_id).await;
        });
    }

    Some(direct_result)
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

fn process_list_runtime_work(
    reg: &mut ActorRegistry,
    stored_state: &ListStoredState,
    work: ListRuntimeWork,
) {
    let process_change = |reg: &mut ActorRegistry,
                          stored_state: &ListStoredState,
                          construct_info: ConstructInfoComplete,
                          change: ListChange,
                          broadcast_change: bool| {
        if broadcast_change {
            stored_state.broadcast_change(&change);
        }
        stored_state.notify_direct_subscribers(reg, &change);

        let was_initialized = stored_state.is_initialized();
        stored_state.record_change(&change);

        match (&change, was_initialized) {
            (ListChange::Replace { items }, false) => {
                stored_state.mark_initialized();
                stored_state.activate_pending_change_subscribers(items.clone());
                stored_state.activate_pending_direct_subscribers(reg, items.clone());
            }
            (_, false) => {
                panic!(
                    "Failed to initialize {construct_info}: The first change has to be 'ListChange::Replace'"
                );
            }
            _ => {}
        }
    };

    match work {
        ListRuntimeWork::Change {
            construct_info,
            change,
            broadcast_change,
        } => process_change(reg, stored_state, construct_info, change, broadcast_change),
        ListRuntimeWork::LiveInputChange {
            construct_info,
            change,
        } => process_change(
            reg,
            stored_state,
            construct_info,
            change,
            stored_state.broadcast_live_changes(),
        ),
        ListRuntimeWork::BroadcastSnapshotIfReady => {
            let current_version = stored_state.version();
            if current_version <= stored_state.last_broadcast_version()
                || !stored_state.is_initialized()
            {
                return;
            }

            let items: Vec<_> = stored_state
                .snapshot()
                .into_iter()
                .map(|(_, actor)| actor)
                .collect();
            let items: Arc<[ActorHandle]> = Arc::from(items.as_slice());
            stored_state.broadcast_snapshot(items.clone());
            stored_state.notify_direct_subscribers(
                reg,
                &ListChange::Replace {
                    items: items.clone(),
                },
            );
            stored_state.set_last_broadcast_version(current_version);
        }
        ListRuntimeWork::SourceEnded => {
            stored_state.set_has_future_state_updates(false);
        }
    }
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

async fn drain_live_list_runtime_inputs<S>(
    construct_info: ConstructInfoComplete,
    stored_state: ListStoredState,
    change_stream: S,
) where
    S: Stream<Item = ListChange> + 'static,
{
    let stored_state_for_items = stored_state.clone();
    let stored_state_for_end = stored_state.clone();
    drain_external_stream_to_runtime_queue(
        change_stream,
        move |reg, change| {
            enqueue_list_work_on_runtime_queue_with_registry(
                reg,
                &stored_state_for_items,
                ListRuntimeWork::LiveInputChange {
                    construct_info: construct_info.clone(),
                    change,
                },
            );
        },
        move |reg| {
            enqueue_list_work_on_runtime_queue_with_registry(
                reg,
                &stored_state_for_end,
                ListRuntimeWork::SourceEnded,
            );
        },
    )
    .await;
}

async fn drain_external_stream_to_runtime_queue<S, T, F, G>(
    source_stream: S,
    mut on_item: F,
    mut on_end: G,
) where
    S: Stream<Item = T> + 'static,
    F: FnMut(&mut ActorRegistry, T) + 'static,
    G: FnMut(&mut ActorRegistry) + 'static,
{
    let mut source_stream = pin!(source_stream);
    while let Some(item) = source_stream.next().await {
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            on_item(&mut reg, item);
        });
    }
    REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        on_end(&mut reg);
    });
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

    fn start_lazy_source_list_task<S, F>(
        stored_state: &ListStoredState,
        scope_id: ScopeId,
        source_list_stream: S,
        on_source_list: F,
    ) where
        S: Stream<Item = Value> + 'static,
        F: Fn(&mut ActorRegistry, Arc<List>) + 'static,
    {
        let on_source_list = Rc::new(on_source_list);
        let Some(drop_waiter) = stored_state.wait_for_drop() else {
            return;
        };
        start_retained_scope_task_until(scope_id, drop_waiter, async move {
            let mut source_list_stream = std::pin::pin!(source_list_stream);
            while let Some(value) = source_list_stream.next().await {
                let Value::List(source_list, _) = value else {
                    continue;
                };
                REGISTRY.with(|reg| {
                    let mut reg = reg.borrow_mut();
                    on_source_list(&mut reg, source_list.clone());
                    reg.drain_runtime_ready_queue();
                });
            }
        });
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
        Self::try_create_direct_state_map_actor(
            construct_info.clone(),
            construct_context.clone(),
            actor_context.clone(),
            source_list_actor.clone(),
            config.clone(),
        )
        .unwrap_or_else(|| {
            panic!(
                "List/map source should stay on the direct-state runtime path: {:?}",
                construct_info.id
            )
        })
    }

    fn try_create_direct_state_map_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
    ) -> Option<ActorHandle> {
        let source_list_is_lazy = source_list_actor.has_lazy_delegate();
        let source_list_stream =
            source_list_is_lazy.then(|| source_list_actor.current_or_future_stream());

        let initial_source_list = match source_list_actor.current_value() {
            Ok(Value::List(list, _)) => Some(list),
            Ok(_) => return None,
            Err(CurrentValueError::NoValueYet) => None,
            Err(CurrentValueError::ActorDropped) => return None,
        };

        let scope_id = actor_context.scope_id();
        let mapped_construct_info = ConstructInfo::new(
            construct_info.id.clone().with_child_id(0),
            None,
            "List/map result",
        )
        .complete(ConstructType::List);
        let mapped_state = ListStoredState::new(DiffHistoryConfig::default());
        mapped_state.set_has_future_state_updates(initial_source_list.as_ref().map_or_else(
            || source_list_actor.stored_state.is_alive(),
            |list| list.stored_state.has_future_state_updates(),
        ));
        let mapped_list = Arc::new(List::new_with_stored_state(
            mapped_construct_info.clone(),
            scope_id,
            mapped_state.clone(),
        ));

        if let Some(output_valve_signal) = actor_context.output_valve_signal.clone() {
            register_list_output_valve_runtime_subscriber(
                output_valve_signal.as_ref(),
                &mapped_state,
            );
        }

        let mapped_state_weak = mapped_state.downgrade();
        let map_state = Rc::new(RefCell::new((
            0usize,
            std::collections::HashMap::<parser::PersistenceId, parser::PersistenceId>::new(),
            std::collections::HashMap::<parser::PersistenceId, ActorHandle>::new(),
            Vec::<parser::PersistenceId>::new(),
        )));
        let broadcast_change = actor_context.output_valve_signal.is_none();
        let current_source_list_key = Rc::new(Cell::new(None::<usize>));
        let current_source_generation = Rc::new(Cell::new(0u64));
        let connect_source_list = Rc::new({
            let mapped_state_weak = mapped_state_weak.clone();
            let mapped_construct_info = mapped_construct_info.clone();
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let map_state = map_state.clone();
            let current_source_list_key = current_source_list_key.clone();
            let current_source_generation = current_source_generation.clone();
            move |reg: &mut ActorRegistry, source_list: Arc<List>| {
                let list_key = list_instance_key(&source_list);
                let generation = current_source_generation.get() + 1;
                current_source_generation.set(generation);
                current_source_list_key.set(Some(list_key));

                source_list.stored_state.register_direct_subscriber(
                    reg,
                    Rc::new({
                        let mapped_state_weak = mapped_state_weak.clone();
                        let mapped_construct_info = mapped_construct_info.clone();
                        let config = config.clone();
                        let construct_context = construct_context.clone();
                        let actor_context = actor_context.clone();
                        let map_state = map_state.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let current_source_generation = current_source_generation.clone();
                        move |reg, change| {
                            if current_source_generation.get() != generation
                                || current_source_list_key.get() != Some(list_key)
                            {
                                return false;
                            }

                            let Some(mapped_state) =
                                ListStoredState::from_weak(mapped_state_weak.clone())
                            else {
                                return false;
                            };

                            let mut state = map_state.borrow_mut();
                            let (length, pid_map, transformed_items_by_pid, item_order) =
                                &mut *state;
                            let (mapped_change, new_length) =
                                Self::transform_list_change_for_map_with_tracking_with_registry(
                                    Some(reg),
                                    change.clone(),
                                    *length,
                                    pid_map,
                                    transformed_items_by_pid,
                                    item_order,
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                            *length = new_length;

                            reg.enqueue_list_runtime_work(
                                mapped_state,
                                ListRuntimeWork::Change {
                                    construct_info: mapped_construct_info.clone(),
                                    change: mapped_change,
                                    broadcast_change,
                                },
                            );
                            true
                        }
                    }),
                );
            }
        });

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            if !source_list_is_lazy {
                source_list_actor.stored_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let connect_source_list = connect_source_list.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        move |reg, value| {
                            let Some(Value::List(source_list, _)) = value else {
                                return true;
                            };
                            let list_key = list_instance_key(source_list);
                            if current_source_list_key.get() != Some(list_key) {
                                connect_source_list(reg, source_list.clone());
                            }
                            true
                        }
                    }),
                );
            }

            if !source_list_is_lazy
                && source_list_actor.is_constant
                && current_source_list_key.get().is_none()
                && let Some(initial_source_list) = initial_source_list.as_ref()
            {
                connect_source_list(&mut reg, initial_source_list.clone());
            }
            reg.drain_runtime_ready_queue();
        });

        if let Some(source_list_stream) = source_list_stream {
            let connect_source_list = connect_source_list.clone();
            let current_source_list_key = current_source_list_key.clone();
            let mapped_state_weak = mapped_state_weak.clone();
            Self::start_lazy_source_list_task(
                &mapped_state,
                scope_id,
                source_list_stream,
                move |reg, source_list| {
                    if ListStoredState::from_weak(mapped_state_weak.clone()).is_none() {
                        return;
                    }
                    let list_key = list_instance_key(&source_list);
                    if current_source_list_key.get() != Some(list_key) {
                        connect_source_list(reg, source_list);
                    }
                },
            );
        }

        Some(create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(mapped_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        ))
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
        Self::try_create_direct_state_retain_actor(
            construct_info.clone(),
            construct_context.clone(),
            actor_context.clone(),
            source_list_actor.clone(),
            config.clone(),
        )
        .unwrap_or_else(|| {
            panic!(
                "List/retain source should stay on the direct-state runtime path: {:?}",
                construct_info.id
            )
        })
    }

    fn try_create_direct_state_retain_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
    ) -> Option<ActorHandle> {
        let source_list_is_lazy = source_list_actor.has_lazy_delegate();
        let source_list_stream =
            source_list_is_lazy.then(|| source_list_actor.current_or_future_stream());

        let initial_source_list = match source_list_actor.current_value() {
            Ok(Value::List(list, _)) => Some(list),
            Ok(_) => return None,
            Err(CurrentValueError::NoValueYet) => None,
            Err(CurrentValueError::ActorDropped) => return None,
        };

        #[derive(Clone)]
        struct DirectRetainEntry {
            entry_id: usize,
            item: ActorHandle,
            predicate_actor: ActorHandle,
            visible: bool,
        }

        struct DirectRetainState {
            entries: Vec<DirectRetainEntry>,
            next_entry_id: usize,
            last_emitted_pids: Vec<parser::PersistenceId>,
            suppress_emits: bool,
        }

        let scope_id = actor_context.scope_id();
        let filtered_construct_info = ConstructInfo::new(
            construct_info.id.clone().with_child_id(0),
            None,
            "List/retain result",
        )
        .complete(ConstructType::List);
        let filtered_state = ListStoredState::new(DiffHistoryConfig::default());
        filtered_state.set_has_future_state_updates(initial_source_list.as_ref().map_or_else(
            || source_list_actor.stored_state.is_alive(),
            |list| list.stored_state.has_future_state_updates(),
        ));
        let filtered_list = Arc::new(List::new_with_stored_state(
            filtered_construct_info.clone(),
            scope_id,
            filtered_state.clone(),
        ));

        if let Some(output_valve_signal) = actor_context.output_valve_signal.clone() {
            register_list_output_valve_runtime_subscriber(
                output_valve_signal.as_ref(),
                &filtered_state,
            );
        }

        let filtered_state_weak = filtered_state.downgrade();
        let state = Rc::new(RefCell::new(DirectRetainState {
            entries: Vec::new(),
            next_entry_id: 0,
            last_emitted_pids: Vec::new(),
            suppress_emits: false,
        }));
        let broadcast_change = actor_context.output_valve_signal.is_none();
        let current_source_list_key = Rc::new(Cell::new(None::<usize>));
        let current_source_generation = Rc::new(Cell::new(0u64));

        let emit_filtered_replace = Rc::new({
            let filtered_construct_info = filtered_construct_info.clone();
            let filtered_state_weak = filtered_state_weak.clone();
            move |reg: &mut ActorRegistry, state: &mut DirectRetainState, force_replace: bool| {
                let Some(filtered_state) = ListStoredState::from_weak(filtered_state_weak.clone())
                else {
                    return false;
                };

                let filtered_items = state
                    .entries
                    .iter()
                    .filter(|entry| entry.visible)
                    .map(|entry| entry.item.clone())
                    .collect::<Vec<_>>();
                let current_pids = filtered_items
                    .iter()
                    .map(|item| item.persistence_id())
                    .collect::<Vec<_>>();

                if force_replace
                    || !filtered_state.is_initialized()
                    || current_pids != state.last_emitted_pids
                {
                    state.last_emitted_pids = current_pids;
                    reg.enqueue_list_runtime_work(
                        filtered_state,
                        ListRuntimeWork::Change {
                            construct_info: filtered_construct_info.clone(),
                            change: ListChange::Replace {
                                items: Arc::from(filtered_items),
                            },
                            broadcast_change,
                        },
                    );
                }

                true
            }
        });
        let connect_source_list = Rc::new({
            let state = state.clone();
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let emit_filtered_replace = emit_filtered_replace.clone();
            let current_source_list_key = current_source_list_key.clone();
            let current_source_generation = current_source_generation.clone();
            let filtered_state_weak = filtered_state_weak.clone();
            move |reg: &mut ActorRegistry, source_list: Arc<List>| {
                let list_key = list_instance_key(&source_list);
                let generation = current_source_generation.get() + 1;
                current_source_generation.set(generation);
                current_source_list_key.set(Some(list_key));

                source_list.stored_state.register_direct_subscriber(
                    reg,
                    Rc::new({
                        let state = state.clone();
                        let config = config.clone();
                        let construct_context = construct_context.clone();
                        let actor_context = actor_context.clone();
                        let emit_filtered_replace = emit_filtered_replace.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let current_source_generation = current_source_generation.clone();
                        let filtered_state_weak = filtered_state_weak.clone();
                        move |reg, change| {
                            if ListStoredState::from_weak(filtered_state_weak.clone()).is_none() {
                                return false;
                            }
                            if current_source_generation.get() != generation
                                || current_source_list_key.get() != Some(list_key)
                            {
                                return false;
                            }

                            let add_entry =
                                |reg: &mut ActorRegistry, item: ActorHandle, insert_at: usize| {
                                    let predicate_actor = Self::transform_item_with_registry(
                                        Some(reg),
                                        item.clone(),
                                        insert_at,
                                        &config,
                                        construct_context.clone(),
                                        actor_context.clone(),
                                    );
                                    let visible = match predicate_actor.current_value() {
                                        Ok(value) => {
                                            matches!(value, Value::Tag(tag, _) if tag.tag() == "True")
                                        }
                                        Err(_) => true,
                                    };

                                    let entry_id = {
                                        let mut state = state.borrow_mut();
                                        let entry_id = state.next_entry_id;
                                        state.next_entry_id += 1;
                                        let insert_at = insert_at.min(state.entries.len());
                                        state.entries.insert(
                                            insert_at,
                                            DirectRetainEntry {
                                                entry_id,
                                                item: item.clone(),
                                                predicate_actor: predicate_actor.clone(),
                                                visible,
                                            },
                                        );
                                        entry_id
                                    };

                                    predicate_actor.stored_state.register_direct_subscriber(
                                        reg,
                                        Rc::new({
                                            let state = state.clone();
                                            let emit_filtered_replace =
                                                emit_filtered_replace.clone();
                                            let current_source_list_key =
                                                current_source_list_key.clone();
                                            let current_source_generation =
                                                current_source_generation.clone();
                                            let filtered_state_weak =
                                                filtered_state_weak.clone();
                                            move |reg, value| {
                                                if ListStoredState::from_weak(
                                                    filtered_state_weak.clone(),
                                                )
                                                .is_none()
                                                {
                                                    return false;
                                                }
                                                if current_source_generation.get() != generation
                                                    || current_source_list_key.get()
                                                        != Some(list_key)
                                                {
                                                    return false;
                                                }

                                                let Some(value) = value else {
                                                    return true;
                                                };

                                                let mut state = state.borrow_mut();
                                                let Some(entry) = state
                                                    .entries
                                                    .iter_mut()
                                                    .find(|entry| entry.entry_id == entry_id)
                                                else {
                                                    return false;
                                                };
                                                let next_visible = matches!(
                                                    value,
                                                    Value::Tag(tag, _) if tag.tag() == "True"
                                                );
                                                if entry.visible == next_visible {
                                                    return true;
                                                }
                                                entry.visible = next_visible;
                                                if state.suppress_emits {
                                                    return true;
                                                }
                                                emit_filtered_replace(reg, &mut state, false)
                                            }
                                        }),
                                    );
                                };

                            let mut did_change = false;
                            {
                                let mut state = state.borrow_mut();
                                state.suppress_emits = true;
                            }

                            match change {
                                ListChange::Replace { items } => {
                                    {
                                        let mut state = state.borrow_mut();
                                        state.entries.clear();
                                    }
                                    for (idx, item) in items.iter().cloned().enumerate() {
                                        add_entry(reg, item, idx);
                                    }
                                    did_change = true;
                                }
                                ListChange::Push { item } => {
                                    let insert_at = state.borrow().entries.len();
                                    add_entry(reg, item.clone(), insert_at);
                                    did_change = true;
                                }
                                ListChange::Remove { id } => {
                                    let mut state = state.borrow_mut();
                                    if let Some(index) = state
                                        .entries
                                        .iter()
                                        .position(|entry| entry.item.persistence_id() == *id)
                                    {
                                        state.entries.remove(index);
                                        did_change = true;
                                    }
                                }
                                ListChange::Pop => {
                                    let mut state = state.borrow_mut();
                                    if state.entries.pop().is_some() {
                                        did_change = true;
                                    }
                                }
                                ListChange::Clear => {
                                    let mut state = state.borrow_mut();
                                    if !state.entries.is_empty() {
                                        state.entries.clear();
                                        did_change = true;
                                    }
                                }
                                ListChange::InsertAt { .. } => {
                                    panic!(
                                        "List/retain direct-state path received InsertAt event which is not yet implemented."
                                    );
                                }
                                ListChange::UpdateAt { .. } => {
                                    panic!(
                                        "List/retain direct-state path received UpdateAt event which is not yet implemented."
                                    );
                                }
                                ListChange::Move { .. } => {
                                    panic!(
                                        "List/retain direct-state path received Move event which is not yet implemented."
                                    );
                                }
                            }

                            let mut state = state.borrow_mut();
                            state.suppress_emits = false;
                            if did_change {
                                emit_filtered_replace(reg, &mut state, true)
                            } else {
                                true
                            }
                        }
                    }),
                );
            }
        });

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            if !source_list_is_lazy {
                source_list_actor.stored_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let connect_source_list = connect_source_list.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let filtered_state_weak = filtered_state_weak.clone();
                        move |reg, value| {
                            if ListStoredState::from_weak(filtered_state_weak.clone()).is_none() {
                                return false;
                            }

                            let Some(Value::List(source_list, _)) = value else {
                                return true;
                            };
                            let list_key = list_instance_key(source_list);
                            if current_source_list_key.get() != Some(list_key) {
                                connect_source_list(reg, source_list.clone());
                            }
                            true
                        }
                    }),
                );
            }

            if !source_list_is_lazy
                && source_list_actor.is_constant
                && current_source_list_key.get().is_none()
                && let Some(initial_source_list) = initial_source_list.as_ref()
            {
                connect_source_list(&mut reg, initial_source_list.clone());
            }

            reg.drain_runtime_ready_queue();
        });

        if let Some(source_list_stream) = source_list_stream {
            let connect_source_list = connect_source_list.clone();
            let current_source_list_key = current_source_list_key.clone();
            let filtered_state_weak = filtered_state_weak.clone();
            Self::start_lazy_source_list_task(
                &filtered_state,
                scope_id,
                source_list_stream,
                move |reg, source_list| {
                    if ListStoredState::from_weak(filtered_state_weak.clone()).is_none() {
                        return;
                    }
                    let list_key = list_instance_key(&source_list);
                    if current_source_list_key.get() != Some(list_key) {
                        connect_source_list(reg, source_list);
                    }
                },
            );
        }

        Some(create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(
                filtered_list,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ),
            scope_id,
        ))
    }

    /// Creates a remove actor that removes items when their `when` event fires.
    /// Tracks removed items by PersistenceId so they don't reappear on upstream Replace.
    fn try_create_direct_state_remove_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
        persistence_id: Option<parser::PersistenceId>,
    ) -> Option<ActorHandle> {
        use std::collections::HashSet;

        let source_list_is_lazy = source_list_actor.has_lazy_delegate();
        let source_list_stream =
            source_list_is_lazy.then(|| source_list_actor.current_or_future_stream());

        let initial_source_list = match source_list_actor.current_value() {
            Ok(Value::List(list, _)) => Some(list),
            Ok(_) => return None,
            Err(CurrentValueError::NoValueYet) => None,
            Err(CurrentValueError::ActorDropped) => return None,
        };

        #[derive(Clone)]
        struct DirectRemoveEntry {
            entry_id: usize,
            item: ActorHandle,
            when_actor: ActorHandle,
        }

        struct DirectRemoveState {
            entries: Vec<DirectRemoveEntry>,
            next_entry_id: usize,
            next_idx: usize,
            removed_pids: HashSet<parser::PersistenceId>,
            persisted_removed: HashSet<String>,
        }

        let removed_set_key: Option<String> = persistence_id
            .as_ref()
            .map(|pid| format!("list_removed:{}", pid));

        let scope_id = actor_context.scope_id();
        let result_construct_info = ConstructInfo::new(
            construct_info.id.clone().with_child_id(0),
            None,
            "List/remove result",
        )
        .complete(ConstructType::List);
        let result_state = ListStoredState::new(DiffHistoryConfig::default());
        result_state.set_has_future_state_updates(initial_source_list.as_ref().map_or_else(
            || source_list_actor.stored_state.is_alive(),
            |list| list.stored_state.has_future_state_updates(),
        ));
        let result_list = Arc::new(List::new_with_stored_state(
            result_construct_info.clone(),
            scope_id,
            result_state.clone(),
        ));

        if let Some(output_valve_signal) = actor_context.output_valve_signal.clone() {
            register_list_output_valve_runtime_subscriber(
                output_valve_signal.as_ref(),
                &result_state,
            );
        }

        let result_state_weak = result_state.downgrade();
        let state = Rc::new(RefCell::new(DirectRemoveState {
            entries: Vec::new(),
            next_entry_id: 0,
            next_idx: 0,
            removed_pids: HashSet::new(),
            persisted_removed: removed_set_key
                .as_ref()
                .map(|key| load_removed_set(key).into_iter().collect())
                .unwrap_or_default(),
        }));
        let broadcast_change = actor_context.output_valve_signal.is_none();
        let current_source_list_key = Rc::new(Cell::new(None::<usize>));
        let current_source_generation = Rc::new(Cell::new(0u64));

        let emit_change = Rc::new({
            let result_construct_info = result_construct_info.clone();
            let result_state_weak = result_state_weak.clone();
            move |reg: &mut ActorRegistry, change: ListChange| {
                let Some(result_state) = ListStoredState::from_weak(result_state_weak.clone())
                else {
                    return false;
                };
                reg.enqueue_list_runtime_work(
                    result_state,
                    ListRuntimeWork::Change {
                        construct_info: result_construct_info.clone(),
                        change,
                        broadcast_change,
                    },
                );
                true
            }
        });

        let remove_entry = Rc::new({
            let emit_change = emit_change.clone();
            let removed_set_key = removed_set_key.clone();
            move |reg: &mut ActorRegistry, state: &mut DirectRemoveState, entry_id: usize| {
                let Some(index) = state
                    .entries
                    .iter()
                    .position(|entry| entry.entry_id == entry_id)
                else {
                    return false;
                };

                let entry = state.entries.remove(index);
                let persistence_id = entry.item.persistence_id();
                if let Some(key) = removed_set_key.as_ref() {
                    if let Some(origin) = entry.item.list_item_origin() {
                        add_to_removed_set(key, &origin.call_id);
                        state.persisted_removed.insert(origin.call_id.clone());
                    } else {
                        let id_str = format!("pid:{}", persistence_id);
                        add_to_removed_set(key, &id_str);
                        state.persisted_removed.insert(id_str);
                    }
                }

                state.removed_pids.insert(persistence_id.clone());
                emit_change(reg, ListChange::Remove { id: persistence_id })
            }
        });
        let connect_source_list = Rc::new({
            let state = state.clone();
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let emit_change = emit_change.clone();
            let remove_entry = remove_entry.clone();
            let current_source_list_key = current_source_list_key.clone();
            let current_source_generation = current_source_generation.clone();
            let result_state_weak = result_state_weak.clone();
            let result_state = result_state.clone();
            move |reg: &mut ActorRegistry, source_list: Arc<List>| {
                let list_key = list_instance_key(&source_list);
                let generation = current_source_generation.get() + 1;
                current_source_generation.set(generation);
                current_source_list_key.set(Some(list_key));

                source_list.stored_state.register_direct_subscriber(
                    reg,
                    Rc::new({
                        let state = state.clone();
                        let config = config.clone();
                        let construct_context = construct_context.clone();
                        let actor_context = actor_context.clone();
                        let emit_change = emit_change.clone();
                        let remove_entry = remove_entry.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let current_source_generation = current_source_generation.clone();
                        let result_state_weak = result_state_weak.clone();
                        let result_state = result_state.clone();
                        move |reg, change| {
                            if ListStoredState::from_weak(result_state_weak.clone()).is_none() {
                                return false;
                            }
                            if current_source_generation.get() != generation
                                || current_source_list_key.get() != Some(list_key)
                            {
                                return false;
                            }

                            let add_entry =
                                |reg: &mut ActorRegistry,
                                 item: ActorHandle,
                                 idx: usize|
                                 -> Option<usize> {
                                    let when_actor = Self::transform_item_with_registry(
                                        Some(reg),
                                        item.clone(),
                                        idx,
                                        &config,
                                        construct_context.clone(),
                                        actor_context.clone(),
                                    );

                                    let entry_id = {
                                        let mut state = state.borrow_mut();
                                        let entry_id = state.next_entry_id;
                                        state.next_entry_id += 1;
                                        state.entries.push(DirectRemoveEntry {
                                            entry_id,
                                            item: item.clone(),
                                            when_actor: when_actor.clone(),
                                        });
                                        entry_id
                                    };

                                    if when_actor.current_value().is_ok() {
                                        return Some(entry_id);
                                    }

                                    if when_actor.lazy_delegate_with_registry(reg).is_some() {
                                        let result_state_weak = result_state_weak.clone();
                                        let state = state.clone();
                                        let remove_entry = remove_entry.clone();
                                        let current_source_list_key =
                                            current_source_list_key.clone();
                                        let current_source_generation =
                                            current_source_generation.clone();
                                        let when_actor = when_actor.clone();
                                        let task = spawn_async_source_task(async move {
                                            let mut trigger_stream =
                                                std::pin::pin!(when_actor.current_or_future_stream());
                                            if trigger_stream.next().await.is_none() {
                                                return;
                                            }

                                            REGISTRY.with(|reg| {
                                                let mut reg = reg.borrow_mut();
                                                if ListStoredState::from_weak(
                                                    result_state_weak.clone(),
                                                )
                                                .is_none()
                                                {
                                                    return;
                                                }
                                                if current_source_generation.get() != generation
                                                    || current_source_list_key.get()
                                                        != Some(list_key)
                                                {
                                                    return;
                                                }

                                                let mut state = state.borrow_mut();
                                                let _ = remove_entry(&mut reg, &mut state, entry_id);
                                                reg.drain_runtime_ready_queue();
                                            });
                                        });
                                        let _ = reg.insert_async_source_for_list(
                                            scope_id,
                                            result_state.async_owner_key(),
                                            task,
                                        );
                                    } else {
                                        when_actor.stored_state.register_direct_subscriber(
                                            reg,
                                            Rc::new({
                                                let state = state.clone();
                                                let remove_entry = remove_entry.clone();
                                                let current_source_list_key =
                                                    current_source_list_key.clone();
                                                let current_source_generation =
                                                    current_source_generation.clone();
                                                let result_state_weak =
                                                    result_state_weak.clone();
                                                move |reg, _value| {
                                                    if ListStoredState::from_weak(
                                                        result_state_weak.clone(),
                                                    )
                                                    .is_none()
                                                    {
                                                        return false;
                                                    }
                                                    if current_source_generation.get() != generation
                                                        || current_source_list_key.get()
                                                            != Some(list_key)
                                                    {
                                                        return false;
                                                    }

                                                    let mut state = state.borrow_mut();
                                                    let _ = remove_entry(reg, &mut state, entry_id);
                                                    false
                                                }
                                            }),
                                        );
                                    }

                                    None
                                };

                            match change {
                                ListChange::Replace { items } => {
                                    let upstream_pids: HashSet<_> =
                                        items.iter().map(|item| item.persistence_id()).collect();
                                    {
                                        let mut state = state.borrow_mut();
                                        state.entries.clear();
                                        state.removed_pids.retain(|pid| upstream_pids.contains(pid));
                                    }

                                    let mut filtered_items = Vec::new();
                                    let mut immediate_removals = Vec::new();
                                    for item in items.iter().cloned() {
                                        let should_skip = {
                                            let state = state.borrow();
                                            let persistence_id = item.persistence_id();
                                            state.removed_pids.contains(&persistence_id)
                                                || item
                                                    .list_item_origin()
                                                    .map(|origin| {
                                                        state.persisted_removed
                                                            .contains(&origin.call_id)
                                                    })
                                                    .unwrap_or_else(|| {
                                                        let id_str =
                                                            format!("pid:{}", persistence_id);
                                                        state.persisted_removed.contains(&id_str)
                                                    })
                                        };
                                        if should_skip {
                                            continue;
                                        }

                                        let idx = {
                                            let mut state = state.borrow_mut();
                                            let idx = state.next_idx;
                                            state.next_idx += 1;
                                            idx
                                        };

                                        if let Some(entry_id) = add_entry(reg, item.clone(), idx) {
                                            immediate_removals.push(entry_id);
                                        }
                                        filtered_items.push(item);
                                    }

                                    if !emit_change(
                                        reg,
                                        ListChange::Replace {
                                            items: Arc::from(filtered_items),
                                        },
                                    ) {
                                        return false;
                                    }

                                    for entry_id in immediate_removals {
                                        let mut state = state.borrow_mut();
                                        let _ = remove_entry(reg, &mut state, entry_id);
                                    }
                                }
                                ListChange::Push { item } => {
                                    let should_skip = {
                                        let state = state.borrow();
                                        let persistence_id = item.persistence_id();
                                        state.removed_pids.contains(&persistence_id)
                                            || item
                                                .list_item_origin()
                                                .map(|origin| {
                                                    state.persisted_removed
                                                        .contains(&origin.call_id)
                                                })
                                                .unwrap_or_else(|| {
                                                    let id_str = format!("pid:{}", persistence_id);
                                                    state.persisted_removed.contains(&id_str)
                                                })
                                    };
                                    if should_skip {
                                        return true;
                                    }

                                    let idx = {
                                        let mut state = state.borrow_mut();
                                        let idx = state.next_idx;
                                        state.next_idx += 1;
                                        idx
                                    };

                                    let immediate_remove = add_entry(reg, item.clone(), idx);
                                    if !emit_change(reg, ListChange::Push { item: item.clone() }) {
                                        return false;
                                    }

                                    if let Some(entry_id) = immediate_remove {
                                        let mut state = state.borrow_mut();
                                        let _ = remove_entry(reg, &mut state, entry_id);
                                    }
                                }
                                ListChange::Remove { id } => {
                                    let mut state = state.borrow_mut();
                                    state.removed_pids.remove(id);
                                    if let Some(index) = state
                                        .entries
                                        .iter()
                                        .position(|entry| entry.item.persistence_id() == *id)
                                    {
                                        state.entries.remove(index);
                                    }
                                    if !emit_change(reg, ListChange::Remove { id: id.clone() }) {
                                        return false;
                                    }
                                }
                                ListChange::Clear => {
                                    let mut state = state.borrow_mut();
                                    state.entries.clear();
                                    state.removed_pids.clear();
                                    state.next_idx = 0;
                                    if !emit_change(reg, ListChange::Clear) {
                                        return false;
                                    }
                                }
                                ListChange::Pop => {
                                    let mut state = state.borrow_mut();
                                    if let Some(entry) = state.entries.pop() {
                                        state.removed_pids.remove(&entry.item.persistence_id());
                                    }
                                    if !emit_change(reg, ListChange::Pop) {
                                        return false;
                                    }
                                }
                                ListChange::InsertAt { .. } => {
                                    panic!(
                                        "List/remove direct-state path received InsertAt event which is not yet implemented."
                                    );
                                }
                                ListChange::UpdateAt { .. } => {
                                    panic!(
                                        "List/remove direct-state path received UpdateAt event which is not yet implemented."
                                    );
                                }
                                ListChange::Move { .. } => {
                                    panic!(
                                        "List/remove direct-state path received Move event which is not yet implemented."
                                    );
                                }
                            }

                            true
                        }
                    }),
                );
            }
        });

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            if !source_list_is_lazy {
                source_list_actor.stored_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let connect_source_list = connect_source_list.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let result_state_weak = result_state_weak.clone();
                        move |reg, value| {
                            if ListStoredState::from_weak(result_state_weak.clone()).is_none() {
                                return false;
                            }

                            let Some(Value::List(source_list, _)) = value else {
                                return true;
                            };
                            let list_key = list_instance_key(source_list);
                            if current_source_list_key.get() != Some(list_key) {
                                connect_source_list(reg, source_list.clone());
                            }
                            true
                        }
                    }),
                );
            }

            if !source_list_is_lazy
                && source_list_actor.is_constant
                && current_source_list_key.get().is_none()
                && let Some(initial_source_list) = initial_source_list.as_ref()
            {
                connect_source_list(&mut reg, initial_source_list.clone());
            }

            reg.drain_runtime_ready_queue();
        });

        if let Some(source_list_stream) = source_list_stream {
            let connect_source_list = connect_source_list.clone();
            let current_source_list_key = current_source_list_key.clone();
            let result_state_weak = result_state_weak.clone();
            Self::start_lazy_source_list_task(
                &result_state,
                scope_id,
                source_list_stream,
                move |reg, source_list| {
                    if ListStoredState::from_weak(result_state_weak.clone()).is_none() {
                        return;
                    }
                    let list_key = list_instance_key(&source_list);
                    if current_source_list_key.get() != Some(list_key) {
                        connect_source_list(reg, source_list);
                    }
                },
            );
        }

        Some(create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(result_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        ))
    }

    fn create_remove_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
        persistence_id: Option<parser::PersistenceId>,
    ) -> ActorHandle {
        Self::try_create_direct_state_remove_actor(
            construct_info.clone(),
            construct_context.clone(),
            actor_context.clone(),
            source_list_actor.clone(),
            config.clone(),
            persistence_id.clone(),
        )
        .unwrap_or_else(|| {
            panic!(
                "List/remove source should stay on the direct-state runtime path: {:?}",
                construct_info.id
            )
        })
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
        if let Some(actor) = Self::try_create_direct_state_every_any_actor(
            construct_info.clone(),
            construct_context.clone(),
            actor_context.clone(),
            source_list_actor.clone(),
            config.clone(),
            is_every,
        ) {
            return actor;
        }

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

    fn try_create_direct_state_every_any_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
        is_every: bool,
    ) -> Option<ActorHandle> {
        let source_list_is_lazy = source_list_actor.has_lazy_delegate();
        let source_list_stream =
            source_list_is_lazy.then(|| source_list_actor.current_or_future_stream());

        let initial_source_list = match source_list_actor.current_value() {
            Ok(Value::List(list, _)) => Some(list),
            Ok(_) => return None,
            Err(CurrentValueError::NoValueYet) => None,
            Err(CurrentValueError::ActorDropped) => return None,
        };

        type DirectEveryAnyState = (
            Vec<parser::PersistenceId>,
            std::collections::HashMap<parser::PersistenceId, (ActorHandle, Option<bool>)>,
            usize,
            usize,
            Option<bool>,
        );

        let scope_id = actor_context.scope_id();
        let output = create_actor_forwarding(parser::PersistenceId::new(), scope_id);
        retain_actor_handles_with_registry(
            None,
            &output,
            std::iter::once(source_list_actor.clone()),
        );

        let state = Rc::new(RefCell::new((
            Vec::<parser::PersistenceId>::new(),
            std::collections::HashMap::<parser::PersistenceId, (ActorHandle, Option<bool>)>::new(),
            0usize,
            0usize,
            None::<bool>,
        )));
        let current_source_list_key = Rc::new(Cell::new(None::<usize>));
        let current_source_generation = Rc::new(Cell::new(0u64));

        let emit_result_if_ready = Rc::new({
            let output = output.clone();
            let construct_context = construct_context.clone();
            let construct_info_id = construct_info.id.clone();
            move |reg: &mut ActorRegistry, state: &mut DirectEveryAnyState| {
                let total = state.0.len();
                let next_result = if total == 0 {
                    Some(is_every)
                } else if state.3 == total {
                    Some(if is_every {
                        state.2 == total
                    } else {
                        state.2 > 0
                    })
                } else {
                    None
                };

                if let Some(result) = next_result {
                    if state.4 != Some(result) {
                        state.4 = Some(result);
                        reg.enqueue_actor_mailbox_value(
                            output.actor_id(),
                            Tag::new_value(
                                ConstructInfo::new(
                                    construct_info_id.clone().with_child_id(0),
                                    None,
                                    if is_every {
                                        "List/every result"
                                    } else {
                                        "List/any result"
                                    },
                                ),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                if result { "True" } else { "False" }.to_string(),
                            ),
                        );
                    }
                }
            }
        });
        let connect_source_list = Rc::new({
            let state = state.clone();
            let output = output.clone();
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let emit_result_if_ready = emit_result_if_ready.clone();
            let current_source_list_key = current_source_list_key.clone();
            let current_source_generation = current_source_generation.clone();
            move |reg: &mut ActorRegistry, source_list: Arc<List>| {
                let list_key = list_instance_key(&source_list);
                let generation = current_source_generation.get() + 1;
                current_source_generation.set(generation);
                current_source_list_key.set(Some(list_key));

                source_list.stored_state.register_direct_subscriber(
                    reg,
                    Rc::new({
                        let state = state.clone();
                        let output = output.clone();
                        let config = config.clone();
                        let construct_context = construct_context.clone();
                        let actor_context = actor_context.clone();
                        let emit_result_if_ready = emit_result_if_ready.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let current_source_generation = current_source_generation.clone();
                        move |reg, change| {
                            if matches!(output.current_value(), Err(CurrentValueError::ActorDropped))
                            {
                                return false;
                            }
                            if current_source_generation.get() != generation
                                || current_source_list_key.get() != Some(list_key)
                            {
                                return false;
                            }

                            let add_predicate =
                                |reg: &mut ActorRegistry, item: ActorHandle, idx: usize| {
                                    let pid = item.persistence_id();
                                    let predicate = Self::transform_item_with_registry(
                                        Some(reg),
                                        item,
                                        idx,
                                        &config,
                                        construct_context.clone(),
                                        actor_context.clone(),
                                    );
                                    {
                                        let mut state = state.borrow_mut();
                                        state.0.push(pid.clone());
                                        state.1.insert(pid.clone(), (predicate.clone(), None));
                                    }
                                    retain_actor_handles_with_registry(
                                        Some(reg),
                                        &output,
                                        std::iter::once(predicate.clone()),
                                    );
                                    predicate.stored_state.register_direct_subscriber(
                                        reg,
                                        Rc::new({
                                            let state = state.clone();
                                            let output = output.clone();
                                            let emit_result_if_ready =
                                                emit_result_if_ready.clone();
                                            let current_source_list_key =
                                                current_source_list_key.clone();
                                            let current_source_generation =
                                                current_source_generation.clone();
                                            let pid = pid.clone();
                                            move |reg, value| {
                                                if matches!(
                                                    output.current_value(),
                                                    Err(CurrentValueError::ActorDropped)
                                                ) {
                                                    return false;
                                                }
                                                if current_source_generation.get() != generation
                                                    || current_source_list_key.get()
                                                        != Some(list_key)
                                                {
                                                    return false;
                                                }

                                                let Some(value) = value else {
                                                    return true;
                                                };

                                                let mut state = state.borrow_mut();
                                                let is_true =
                                                    matches!(value, Value::Tag(tag, _) if tag.tag() == "True");
                                                let previous = {
                                                    let Some((_, current_bool)) =
                                                        state.1.get_mut(&pid)
                                                    else {
                                                        return false;
                                                    };
                                                    let previous = *current_bool;
                                                    *current_bool = Some(is_true);
                                                    previous
                                                };
                                                match previous {
                                                    None => {
                                                        state.3 += 1;
                                                        if is_true {
                                                            state.2 += 1;
                                                        }
                                                    }
                                                    Some(previous) if previous != is_true => {
                                                        if previous && !is_true {
                                                            state.2 -= 1;
                                                        } else if !previous && is_true {
                                                            state.2 += 1;
                                                        }
                                                    }
                                                    Some(_) => {}
                                                }
                                                emit_result_if_ready(reg, &mut state);
                                                true
                                            }
                                        }),
                                    );
                                };

                            match change {
                                ListChange::Replace { items } => {
                                    {
                                        let mut state = state.borrow_mut();
                                        state.0.clear();
                                        state.1.clear();
                                        state.2 = 0;
                                        state.3 = 0;
                                    }
                                    for (idx, item) in items.iter().cloned().enumerate() {
                                        add_predicate(reg, item, idx);
                                    }
                                    let mut state = state.borrow_mut();
                                    emit_result_if_ready(reg, &mut state);
                                }
                                ListChange::Push { item } => {
                                    let idx = state.borrow().0.len();
                                    add_predicate(reg, item.clone(), idx);
                                }
                                ListChange::Clear => {
                                    let mut state = state.borrow_mut();
                                    state.0.clear();
                                    state.1.clear();
                                    state.2 = 0;
                                    state.3 = 0;
                                    emit_result_if_ready(reg, &mut state);
                                }
                                ListChange::Pop => {
                                    let mut state = state.borrow_mut();
                                    if let Some(pid) = state.0.pop() {
                                        if let Some((_, current_bool)) = state.1.remove(&pid) {
                                            match current_bool {
                                                Some(true) => {
                                                    state.2 -= 1;
                                                    state.3 -= 1;
                                                }
                                                Some(false) => {
                                                    state.3 -= 1;
                                                }
                                                None => {}
                                            }
                                        }
                                    }
                                    emit_result_if_ready(reg, &mut state);
                                }
                                ListChange::Remove { id } => {
                                    let mut state = state.borrow_mut();
                                    state.0.retain(|pid| *pid != *id);
                                    if let Some((_, current_bool)) = state.1.remove(id) {
                                        match current_bool {
                                            Some(true) => {
                                                state.2 -= 1;
                                                state.3 -= 1;
                                            }
                                            Some(false) => {
                                                state.3 -= 1;
                                            }
                                            None => {}
                                        }
                                    }
                                    emit_result_if_ready(reg, &mut state);
                                }
                                ListChange::InsertAt { .. } => {
                                    panic!(
                                        "List/every or List/any direct-state path received InsertAt event which is not yet implemented."
                                    );
                                }
                                ListChange::UpdateAt { .. } => {
                                    panic!(
                                        "List/every or List/any direct-state path received UpdateAt event which is not yet implemented."
                                    );
                                }
                                ListChange::Move { .. } => {
                                    panic!(
                                        "List/every or List/any direct-state path received Move event which is not yet implemented."
                                    );
                                }
                            }

                            true
                        }
                    }),
                );
            }
        });

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            if !source_list_is_lazy {
                source_list_actor.stored_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let output = output.clone();
                        let connect_source_list = connect_source_list.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        move |reg, value| {
                            if matches!(
                                output.current_value(),
                                Err(CurrentValueError::ActorDropped)
                            ) {
                                return false;
                            }
                            let Some(Value::List(source_list, _)) = value else {
                                return true;
                            };
                            let list_key = list_instance_key(source_list);
                            if current_source_list_key.get() != Some(list_key) {
                                connect_source_list(reg, source_list.clone());
                            }
                            true
                        }
                    }),
                );
            }

            if !source_list_is_lazy
                && source_list_actor.is_constant
                && current_source_list_key.get().is_none()
                && let Some(initial_source_list) = initial_source_list.as_ref()
            {
                connect_source_list(&mut reg, initial_source_list.clone());
            }

            reg.drain_runtime_ready_queue();
        });

        if let Some(source_list_stream) = source_list_stream {
            let output_for_task = output.clone();
            let connect_source_list = connect_source_list.clone();
            let current_source_list_key = current_source_list_key.clone();
            start_retained_actor_scope_task(&output, async move {
                let mut source_list_stream = std::pin::pin!(source_list_stream);
                while let Some(value) = source_list_stream.next().await {
                    let Value::List(source_list, _) = value else {
                        continue;
                    };
                    if matches!(
                        output_for_task.current_value(),
                        Err(CurrentValueError::ActorDropped)
                    ) {
                        break;
                    }
                    REGISTRY.with(|reg| {
                        let mut reg = reg.borrow_mut();
                        let list_key = list_instance_key(&source_list);
                        if current_source_list_key.get() != Some(list_key) {
                            connect_source_list(&mut reg, source_list.clone());
                        }
                        reg.drain_runtime_ready_queue();
                    });
                }
            });
        }

        Some(output)
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
        Self::try_create_direct_state_sort_by_actor(
            construct_info.clone(),
            construct_context.clone(),
            actor_context.clone(),
            source_list_actor.clone(),
            config.clone(),
        )
        .unwrap_or_else(|| {
            panic!(
                "List/sort_by source should stay on the direct-state runtime path: {:?}",
                construct_info.id
            )
        })
    }

    fn try_create_direct_state_sort_by_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: ActorHandle,
        config: Arc<ListBindingConfig>,
    ) -> Option<ActorHandle> {
        let source_list_is_lazy = source_list_actor.has_lazy_delegate();
        let source_list_stream =
            source_list_is_lazy.then(|| source_list_actor.current_or_future_stream());

        let initial_source_list = match source_list_actor.current_value() {
            Ok(Value::List(list, _)) => Some(list),
            Ok(_) => return None,
            Err(CurrentValueError::NoValueYet) => None,
            Err(CurrentValueError::ActorDropped) => return None,
        };

        #[derive(Clone)]
        struct DirectSortEntry {
            entry_id: usize,
            item: ActorHandle,
            key_actor: ActorHandle,
            key_value: Option<SortKey>,
        }

        struct DirectSortState {
            entries: Vec<DirectSortEntry>,
            prev_sorted: Vec<usize>,
            next_entry_id: usize,
            suppress_emits: bool,
        }

        let scope_id = actor_context.scope_id();
        let sorted_construct_info = ConstructInfo::new(
            construct_info.id.clone().with_child_id(0),
            None,
            "List/sort_by result",
        )
        .complete(ConstructType::List);
        let sorted_state = ListStoredState::new(DiffHistoryConfig::default());
        sorted_state.set_has_future_state_updates(initial_source_list.as_ref().map_or_else(
            || source_list_actor.stored_state.is_alive(),
            |list| list.stored_state.has_future_state_updates(),
        ));
        let sorted_list = Arc::new(List::new_with_stored_state(
            sorted_construct_info.clone(),
            scope_id,
            sorted_state.clone(),
        ));

        if let Some(output_valve_signal) = actor_context.output_valve_signal.clone() {
            register_list_output_valve_runtime_subscriber(
                output_valve_signal.as_ref(),
                &sorted_state,
            );
        }

        let broadcast_change = actor_context.output_valve_signal.is_none();
        let sorted_state_weak = sorted_state.downgrade();
        let state = Rc::new(RefCell::new(DirectSortState {
            entries: Vec::new(),
            prev_sorted: Vec::new(),
            next_entry_id: 0,
            suppress_emits: false,
        }));
        let current_source_list_key = Rc::new(Cell::new(None::<usize>));
        let current_source_generation = Rc::new(Cell::new(0u64));

        let emit_sorted_changes = Rc::new({
            let sorted_construct_info = sorted_construct_info.clone();
            let sorted_state_weak = sorted_state_weak.clone();
            move |reg: &mut ActorRegistry, state: &mut DirectSortState, force_replace: bool| {
                let Some(sorted_state) = ListStoredState::from_weak(sorted_state_weak.clone())
                else {
                    return false;
                };

                let new_sorted = if state.entries.is_empty() {
                    Vec::new()
                } else {
                    if state.entries.iter().any(|entry| entry.key_value.is_none()) {
                        return true;
                    }

                    let mut sorted_indices = (0..state.entries.len()).collect::<Vec<_>>();
                    sorted_indices.sort_by(|lhs, rhs| {
                        state.entries[*lhs]
                            .key_value
                            .as_ref()
                            .expect("ready sort entry should have a key")
                            .cmp(
                                state.entries[*rhs]
                                    .key_value
                                    .as_ref()
                                    .expect("ready sort entry should have a key"),
                            )
                            .then(lhs.cmp(rhs))
                    });
                    sorted_indices
                };

                if force_replace || state.prev_sorted.is_empty() {
                    let sorted_items = Arc::from(
                        new_sorted
                            .iter()
                            .map(|&idx| state.entries[idx].item.clone())
                            .collect::<Vec<_>>(),
                    );
                    reg.enqueue_list_runtime_work(
                        sorted_state,
                        ListRuntimeWork::Change {
                            construct_info: sorted_construct_info.clone(),
                            change: ListChange::Replace {
                                items: sorted_items,
                            },
                            broadcast_change,
                        },
                    );
                } else if state.prev_sorted != new_sorted {
                    let items = state
                        .entries
                        .iter()
                        .map(|entry| entry.item.clone())
                        .collect::<Vec<_>>();
                    for change in compute_sort_diff(&items, &state.prev_sorted, &new_sorted) {
                        reg.enqueue_list_runtime_work(
                            sorted_state.clone(),
                            ListRuntimeWork::Change {
                                construct_info: sorted_construct_info.clone(),
                                change,
                                broadcast_change,
                            },
                        );
                    }
                }

                state.prev_sorted = new_sorted;
                true
            }
        });
        let connect_source_list = Rc::new({
            let state = state.clone();
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let emit_sorted_changes = emit_sorted_changes.clone();
            let current_source_list_key = current_source_list_key.clone();
            let current_source_generation = current_source_generation.clone();
            let sorted_state_weak = sorted_state_weak.clone();
            move |reg: &mut ActorRegistry, source_list: Arc<List>| {
                let list_key = list_instance_key(&source_list);
                let generation = current_source_generation.get() + 1;
                current_source_generation.set(generation);
                current_source_list_key.set(Some(list_key));

                source_list.stored_state.register_direct_subscriber(
                    reg,
                    Rc::new({
                        let state = state.clone();
                        let config = config.clone();
                        let construct_context = construct_context.clone();
                        let actor_context = actor_context.clone();
                        let emit_sorted_changes = emit_sorted_changes.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let current_source_generation = current_source_generation.clone();
                        let sorted_state_weak = sorted_state_weak.clone();
                        move |reg, change| {
                            if ListStoredState::from_weak(sorted_state_weak.clone()).is_none() {
                                return false;
                            }
                            if current_source_generation.get() != generation
                                || current_source_list_key.get() != Some(list_key)
                            {
                                return false;
                            }

                            let add_entry =
                                |reg: &mut ActorRegistry, item: ActorHandle, insert_at: usize| {
                                    let key_actor = Self::transform_item_with_registry(
                                        Some(reg),
                                        item.clone(),
                                        insert_at,
                                        &config,
                                        construct_context.clone(),
                                        actor_context.clone(),
                                    );

                                    let entry_id = {
                                        let mut state = state.borrow_mut();
                                        let entry_id = state.next_entry_id;
                                        state.next_entry_id += 1;
                                        let insert_at = insert_at.min(state.entries.len());
                                        state.entries.insert(
                                            insert_at,
                                            DirectSortEntry {
                                                entry_id,
                                                item: item.clone(),
                                                key_actor: key_actor.clone(),
                                                key_value: None,
                                            },
                                        );
                                        entry_id
                                    };

                                    key_actor.stored_state.register_direct_subscriber(
                                        reg,
                                        Rc::new({
                                            let state = state.clone();
                                            let emit_sorted_changes = emit_sorted_changes.clone();
                                            let current_source_list_key =
                                                current_source_list_key.clone();
                                            let current_source_generation =
                                                current_source_generation.clone();
                                            let sorted_state_weak = sorted_state_weak.clone();
                                            move |reg, value| {
                                                if ListStoredState::from_weak(
                                                    sorted_state_weak.clone(),
                                                )
                                                .is_none()
                                                {
                                                    return false;
                                                }
                                                if current_source_generation.get() != generation
                                                    || current_source_list_key.get()
                                                        != Some(list_key)
                                                {
                                                    return false;
                                                }

                                                let Some(value) = value else {
                                                    return true;
                                                };

                                                let mut state = state.borrow_mut();
                                                let Some(entry) = state
                                                    .entries
                                                    .iter_mut()
                                                    .find(|entry| entry.entry_id == entry_id)
                                                else {
                                                    return false;
                                                };

                                                let next_key = SortKey::from_value(value);
                                                if entry.key_value.as_ref() == Some(&next_key) {
                                                    return true;
                                                }
                                                entry.key_value = Some(next_key);
                                                if state.suppress_emits {
                                                    return true;
                                                }

                                                emit_sorted_changes(reg, &mut state, false)
                                            }
                                        }),
                                    );
                                };

                            let mut did_change = false;
                            {
                                let mut state = state.borrow_mut();
                                state.suppress_emits = true;
                                state.prev_sorted.clear();
                            }

                            match change {
                                ListChange::Replace { items } => {
                                    {
                                        let mut state = state.borrow_mut();
                                        state.entries.clear();
                                    }
                                    for (idx, item) in items.iter().cloned().enumerate() {
                                        add_entry(reg, item, idx);
                                    }
                                    did_change = true;
                                }
                                ListChange::Push { item } => {
                                    let insert_at = state.borrow().entries.len();
                                    add_entry(reg, item.clone(), insert_at);
                                    did_change = true;
                                }
                                ListChange::InsertAt { index, item } => {
                                    if *index <= state.borrow().entries.len() {
                                        add_entry(reg, item.clone(), *index);
                                        did_change = true;
                                    }
                                }
                                ListChange::Remove { id } => {
                                    let mut state = state.borrow_mut();
                                    if let Some(index) = state
                                        .entries
                                        .iter()
                                        .position(|entry| entry.item.persistence_id() == *id)
                                    {
                                        state.entries.remove(index);
                                        did_change = true;
                                    }
                                }
                                ListChange::Clear => {
                                    let mut state = state.borrow_mut();
                                    if !state.entries.is_empty() {
                                        state.entries.clear();
                                        did_change = true;
                                    }
                                }
                                ListChange::Pop => {
                                    let mut state = state.borrow_mut();
                                    if state.entries.pop().is_some() {
                                        did_change = true;
                                    }
                                }
                                ListChange::UpdateAt { .. } => {
                                    panic!(
                                        "List/sort_by direct-state path received UpdateAt event which is not yet implemented."
                                    );
                                }
                                ListChange::Move { .. } => {
                                    panic!(
                                        "List/sort_by direct-state path received Move event which is not yet implemented."
                                    );
                                }
                            }

                            let mut state = state.borrow_mut();
                            state.suppress_emits = false;
                            if did_change {
                                emit_sorted_changes(reg, &mut state, true)
                            } else {
                                true
                            }
                        }
                    }),
                );
            }
        });

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            if !source_list_is_lazy {
                source_list_actor.stored_state.register_direct_subscriber(
                    &mut reg,
                    Rc::new({
                        let connect_source_list = connect_source_list.clone();
                        let current_source_list_key = current_source_list_key.clone();
                        let sorted_state_weak = sorted_state_weak.clone();
                        move |reg, value| {
                            if ListStoredState::from_weak(sorted_state_weak.clone()).is_none() {
                                return false;
                            }

                            let Some(Value::List(source_list, _)) = value else {
                                return true;
                            };
                            let list_key = list_instance_key(source_list);
                            if current_source_list_key.get() != Some(list_key) {
                                connect_source_list(reg, source_list.clone());
                            }
                            true
                        }
                    }),
                );
            }

            if !source_list_is_lazy
                && source_list_actor.is_constant
                && current_source_list_key.get().is_none()
                && let Some(initial_source_list) = initial_source_list.as_ref()
            {
                connect_source_list(&mut reg, initial_source_list.clone());
            }

            reg.drain_runtime_ready_queue();
        });

        if let Some(source_list_stream) = source_list_stream {
            let connect_source_list = connect_source_list.clone();
            let current_source_list_key = current_source_list_key.clone();
            let sorted_state_weak = sorted_state_weak.clone();
            Self::start_lazy_source_list_task(
                &sorted_state,
                scope_id,
                source_list_stream,
                move |reg, source_list| {
                    if ListStoredState::from_weak(sorted_state_weak.clone()).is_none() {
                        return;
                    }
                    let list_key = list_instance_key(&source_list);
                    if current_source_list_key.get() != Some(list_key) {
                        connect_source_list(reg, source_list);
                    }
                },
            );
        }

        Some(create_constant_actor(
            parser::PersistenceId::new(),
            Value::List(sorted_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        ))
    }

    /// Transform a single list item using the config's transform expression.
    fn transform_item_with_registry(
        mut reg: Option<&mut ActorRegistry>,
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
        let item_registry_scope = create_child_registry_scope_with_optional_registry(
            reg.as_deref_mut(),
            child_scope.registry_scope_id,
        );
        let new_actor_context = ActorContext {
            parameters: Arc::new(new_params),
            registry_scope_id: item_registry_scope.or(child_scope.registry_scope_id),
            ..child_scope
        };

        // Evaluate the transform expression with the binding in scope
        // Pass the function registry snapshot to enable user-defined function calls
        match evaluate_static_expression_with_runtime_registry(
            reg.as_deref_mut(),
            &config.transform_expr,
            construct_context.clone(),
            new_actor_context.clone(),
            config.reference_connector.clone(),
            config.link_connector.clone(),
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
                let forwarding_actor = match reg.as_deref_mut() {
                    Some(reg) => create_actor_forwarding_with_registry(
                        reg,
                        original_pid, // Preserve original PersistenceId!
                        scope_id,
                    ),
                    None => create_actor_forwarding(
                        original_pid, // Preserve original PersistenceId!
                        scope_id,
                    ),
                };
                connect_forwarding_current_and_future_with_registry(
                    reg.as_deref_mut(),
                    forwarding_actor.clone(),
                    result_actor,
                );
                forwarding_actor
            }
            Err(e) => {
                zoon::eprintln!("Error evaluating transform expression: {e}");
                // Return the original item as fallback
                item_actor
            }
        }
    }

    fn transform_item(
        item_actor: ActorHandle,
        index: usize,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> ActorHandle {
        Self::transform_item_with_registry(
            None,
            item_actor,
            index,
            config,
            construct_context,
            actor_context,
        )
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
        Self::transform_list_change_for_map_with_tracking_with_registry(
            None,
            change,
            current_length,
            pid_map,
            transformed_items_by_pid,
            item_order,
            config,
            construct_context,
            actor_context,
        )
    }

    /// Like transform_list_change_for_map but can reuse an active registry borrow.
    fn transform_list_change_for_map_with_tracking_with_registry(
        mut reg: Option<&mut ActorRegistry>,
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
                        let transformed = Self::transform_item_with_registry(
                            reg.as_deref_mut(),
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
                let transformed_item = Self::transform_item_with_registry(
                    reg.as_deref_mut(),
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
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
                let transformed_item = Self::transform_item_with_registry(
                    reg.as_deref_mut(),
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
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
                let transformed_item = Self::transform_item_with_registry(
                    reg.as_deref_mut(),
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
        LatestCombinator, List, ListChange, NamedChannel, Number, REGISTRY, ScopeDestroyGuard,
        TaggedObject, Text, Value, ValueIdempotencyKey, Variable, VirtualFilesystem, create_actor,
        create_actor_forwarding, create_actor_from_future, create_actor_lazy,
        create_constant_actor, create_registry_scope, current_emission_seq, list_instance_key,
        list_item_scope_id, retain_actor_handle, retain_actor_handles, values_equal_async,
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
    use std::rc::Rc;
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
    fn current_or_future_stream_is_empty_when_actor_source_has_already_ended_without_value() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor(stream::empty::<Value>(), PersistenceId::new(), scope_id);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "empty stream-driven actors should not retain a feeder task"
            );
        });

        block_on(async move {
            let mut stream = actor.current_or_future_stream();
            assert!(
                stream.next().await.is_none(),
                "ended empty actor source should yield an empty current_or_future stream"
            );
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "constant source forwarding should not retain an async loop on its owning scope"
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "dropped source without a current value should not retain a forwarding task"
            );
        });

        assert!(matches!(
            forwarded.current_value(),
            Err(super::CurrentValueError::NoValueYet)
        ));
    }

    #[test]
    fn forwarding_direct_state_source_uses_runtime_subscriber_without_async_source_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let source = create_actor_forwarding(PersistenceId::new(), scope_id);
        source.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.forwarding.direct_subscriber.initial",
                None,
                "test forwarding direct subscriber initial",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            31,
            "first",
        ));
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);

        super::connect_forwarding_current_and_future(forwarded.clone(), source.clone());

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "direct-state forwarding should register with runtime state instead of spawning an async source task"
            );
        });

        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarded actor should expose seeded source value immediately")
        else {
            panic!("expected seeded forwarded text");
        };
        assert_eq!(text.text(), "first");

        source.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.forwarding.direct_subscriber.update",
                None,
                "test forwarding direct subscriber update",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            32,
            "second",
        ));

        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarded actor should receive later direct-state source updates")
        else {
            panic!("expected updated forwarded text");
        };
        assert_eq!(text.text(), "second");
    }

    #[test]
    #[ignore = "async source cleanup timing is unreliable on host test runner; works in browser"]
    fn dropping_forwarding_fallback_task_releases_scope_owned_task_before_scope_destroy() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        // Use a lazy actor so the forwarding path falls through to the
        // retained task (lazy actors don't support direct subscriber
        // registration).
        let source = super::create_actor_lazy(
            stream::once(future::ready(Text::new_value_with_emission_seq(
                ConstructInfo::new("test.forwarding.source", None, "test forwarding source"),
                ConstructContext {
                    construct_storage: Arc::new(ConstructStorage::new("")),
                    virtual_fs: VirtualFilesystem::new(),
                    bridge_scope_id: None,
                    scene_ctx: None,
                },
                PersistenceId::new(),
                1,
                "source",
            )))
            .chain(stream::pending()),
            PersistenceId::new(),
            scope_id,
        );

        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
        let forwarded = create_actor_forwarding(PersistenceId::new(), scope_id);
        super::connect_forwarding_current_and_future(forwarded.clone(), source);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) > baseline_async_source_count,
                "forwarding fallback should add one scope-owned task while the target actor is alive"
            );
        });

        REGISTRY.with(|reg| reg.borrow_mut().remove_actor(forwarded.actor_id));

        let mut forwarding_task_cleared = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == baseline_async_source_count {
                forwarding_task_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            forwarding_task_cleared,
            "removing the forwarding target should release the scope-owned fallback task before scope teardown"
        );
    }

    #[test]
    fn runtime_local_child_scope_creation_works_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);

        let child_scope = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::create_registry_scope_with_registry(&mut reg, Some(root_scope))
        });

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let root = reg
                .get_scope(root_scope)
                .expect("root scope should stay registered");
            assert!(
                root.children.contains(&child_scope),
                "runtime-local scope creation should still register the child under its parent"
            );
            assert!(
                reg.get_scope(child_scope).is_some(),
                "child scope created from an active registry borrow should be registered"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_alias_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.list.map.runtime_local.item",
                None,
                "test list map runtime local item",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            41,
            "ready",
        ));

        let source_code = SourceCode::new("item".to_string());
        let config = super::ListBindingConfig {
            binding_name: source_code.slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::Alias(
                    static_expression::Alias::WithoutPassed {
                        parts: vec![source_code.slice(0, 4)],
                        referenced_span: None,
                    },
                ),
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Text(text, _) = transformed
            .current_value()
            .expect("runtime-local list transform should expose the mapped alias value")
        else {
            panic!("runtime-local list transform should resolve to text");
        };
        assert_eq!(text.text(), "ready");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let root = reg
                .get_scope(root_scope)
                .expect("root scope should stay registered");
            assert_eq!(
                root.children.len(),
                1,
                "runtime-local list transform should register a child scope for the mapped item"
            );
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "direct alias mapping should stay on the runtime subscriber path without an async scope task"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_text_literal_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.list.map.runtime_local.text_literal.item",
                None,
                "test list map runtime local text literal item",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            42,
            "ready",
        ));

        let source_code = SourceCode::new("TEXT { \"Hello {item}!\" }".to_string());
        let config = super::ListBindingConfig {
            binding_name: SourceCode::new("item".to_string()).slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::TextLiteral {
                    parts: vec![
                        static_expression::TextPart::Text(source_code.slice(8, 14)),
                        static_expression::TextPart::Interpolation {
                            var: source_code.slice(15, 19),
                            referenced_span: None,
                        },
                        static_expression::TextPart::Text(source_code.slice(20, 21)),
                    ],
                    hash_count: 0,
                },
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Text(text, _) = transformed
            .current_value()
            .expect("runtime-local list text literal transform should expose the mapped value")
        else {
            panic!("runtime-local list text literal transform should resolve to text");
        };
        assert_eq!(text.text(), "Hello ready!");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "runtime-local text literal mapping should stay on the direct runtime subscriber path"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_arithmetic_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(Number::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.arithmetic.item",
                None,
                "test list map runtime local arithmetic item",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            7.0,
        ));

        let source_code = SourceCode::new("item + item".to_string());
        let config = super::ListBindingConfig {
            binding_name: SourceCode::new("item".to_string()).slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::ArithmeticOperator(
                    static_expression::ArithmeticOperator::Add {
                        operand_a: Box::new(static_expression::Spanned {
                            span: span_at(4),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(0, 4)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                        operand_b: Box::new(static_expression::Spanned {
                            span: (7..11).into(),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(7, 11)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                    },
                ),
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Number(number, _) = transformed
            .current_value()
            .expect("runtime-local list arithmetic transform should expose the mapped value")
        else {
            panic!("runtime-local list arithmetic transform should resolve to number");
        };
        assert_eq!(number.number(), 14.0);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "runtime-local arithmetic mapping should stay on the direct runtime subscriber path"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_comparator_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(Number::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.comparator.item",
                None,
                "test list map runtime local comparator item",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            7.0,
        ));

        let source_code = SourceCode::new("item > 5".to_string());
        let config = super::ListBindingConfig {
            binding_name: SourceCode::new("item".to_string()).slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::Comparator(
                    static_expression::Comparator::Greater {
                        operand_a: Box::new(static_expression::Spanned {
                            span: span_at(4),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(0, 4)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                        operand_b: Box::new(static_expression::Spanned {
                            span: (7..8).into(),
                            persistence: None,
                            node: static_expression::Expression::Literal(
                                static_expression::Literal::Number(5.0),
                            ),
                        }),
                    },
                ),
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Tag(tag, _) = transformed
            .current_value()
            .expect("runtime-local list comparator transform should expose the mapped value")
        else {
            panic!("runtime-local list comparator transform should resolve to tag");
        };
        assert_eq!(tag.tag(), "True");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "runtime-local comparator mapping should stay on the direct runtime subscriber path"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_postfix_field_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let text_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        text_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.postfix_field.text",
                None,
                "test list map runtime local postfix field text",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            "ready",
        ));

        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(super::Object::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.postfix_field.object",
                None,
                "test list map runtime local postfix field object",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.list.map.runtime_local.postfix_field.variable",
                    None,
                    "test list map runtime local postfix field variable",
                ),
                "text",
                text_actor,
                PersistenceId::new(),
                actor_scope,
            )],
        ));

        let source_code = SourceCode::new("item.text".to_string());
        let config = super::ListBindingConfig {
            binding_name: SourceCode::new("item".to_string()).slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::PostfixFieldAccess {
                    expr: Box::new(static_expression::Spanned {
                        span: span_at(4),
                        persistence: None,
                        node: static_expression::Expression::Alias(
                            static_expression::Alias::WithoutPassed {
                                parts: vec![source_code.slice(0, 4)],
                                referenced_span: None,
                            },
                        ),
                    }),
                    field: source_code.slice(5, 9),
                },
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Text(text, _) = transformed
            .current_value()
            .expect("runtime-local list postfix field transform should expose the mapped value")
        else {
            panic!("runtime-local list postfix field transform should resolve to text");
        };
        assert_eq!(text.text(), "ready");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "runtime-local postfix field mapping should stay on the direct runtime subscriber path"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_block_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let text_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        text_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.block.text",
                None,
                "test list map runtime local block text",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            "ready",
        ));

        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(super::Object::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.block.object",
                None,
                "test list map runtime local block object",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.list.map.runtime_local.block.variable",
                    None,
                    "test list map runtime local block variable",
                ),
                "text",
                text_actor,
                PersistenceId::new(),
                actor_scope,
            )],
        ));

        let source_code = SourceCode::new("text = item.text; text".to_string());
        let config = super::ListBindingConfig {
            binding_name: SourceCode::new("item".to_string()).slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::Block {
                    variables: vec![static_expression::Spanned {
                        span: (0..4).into(),
                        persistence: None,
                        node: static_expression::Variable {
                            name: source_code.slice(0, 4),
                            is_referenced: true,
                            value_changed: false,
                            value: static_expression::Spanned {
                                span: (7..16).into(),
                                persistence: None,
                                node: static_expression::Expression::PostfixFieldAccess {
                                    expr: Box::new(static_expression::Spanned {
                                        span: (7..11).into(),
                                        persistence: None,
                                        node: static_expression::Expression::Alias(
                                            static_expression::Alias::WithoutPassed {
                                                parts: vec![source_code.slice(7, 11)],
                                                referenced_span: None,
                                            },
                                        ),
                                    }),
                                    field: source_code.slice(12, 16),
                                },
                            },
                        },
                    }],
                    output: Box::new(static_expression::Spanned {
                        span: (18..22).into(),
                        persistence: None,
                        node: static_expression::Expression::Alias(
                            static_expression::Alias::WithoutPassed {
                                parts: vec![source_code.slice(18, 22)],
                                referenced_span: None,
                            },
                        ),
                    }),
                },
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Text(text, _) = transformed
            .current_value()
            .expect("runtime-local list block transform should expose the mapped value")
        else {
            panic!("runtime-local list block transform should resolve to text");
        };
        assert_eq!(text.text(), "ready");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "runtime-local block mapping should stay on the direct runtime subscriber path"
            );
        });
    }

    #[test]
    fn runtime_local_transform_item_supports_pipe_field_access_map_under_active_registry_borrow() {
        let root_scope = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(root_scope);
        let actor_context = ActorContext {
            registry_scope_id: Some(root_scope),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let text_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        text_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.pipe_field.text",
                None,
                "test list map runtime local pipe field text",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            "ready",
        ));

        let item_actor = create_actor_forwarding(PersistenceId::new(), root_scope);
        item_actor.store_value_directly(super::Object::new_value(
            ConstructInfo::new(
                "test.list.map.runtime_local.pipe_field.object",
                None,
                "test list map runtime local pipe field object",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.list.map.runtime_local.pipe_field.variable",
                    None,
                    "test list map runtime local pipe field variable",
                ),
                "text",
                text_actor,
                PersistenceId::new(),
                actor_scope,
            )],
        ));

        let source_code = SourceCode::new("item |> .text".to_string());
        let config = super::ListBindingConfig {
            binding_name: SourceCode::new("item".to_string()).slice(0, 4),
            transform_expr: static_expression::Spanned {
                span: span_at(source_code.len()),
                persistence: None,
                node: static_expression::Expression::Pipe {
                    from: Box::new(static_expression::Spanned {
                        span: (0..4).into(),
                        persistence: None,
                        node: static_expression::Expression::Alias(
                            static_expression::Alias::WithoutPassed {
                                parts: vec![source_code.slice(0, 4)],
                                referenced_span: None,
                            },
                        ),
                    }),
                    to: Box::new(static_expression::Spanned {
                        span: (8..13).into(),
                        persistence: None,
                        node: static_expression::Expression::FieldAccess {
                            path: vec![source_code.slice(9, 13)],
                        },
                    }),
                },
            },
            operation: super::ListBindingOperation::Map,
            reference_connector: Arc::new(super::ReferenceConnector::new()),
            link_connector: Arc::new(super::LinkConnector::new()),
            source_code,
            function_registry_snapshot: None,
        };

        let transformed = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            super::ListBindingFunction::transform_item_with_registry(
                Some(&mut reg),
                item_actor.clone(),
                0,
                &config,
                construct_context.clone(),
                actor_context.clone(),
            )
        });

        let Value::Text(text, _) = transformed
            .current_value()
            .expect("runtime-local list pipe field transform should expose the mapped value")
        else {
            panic!("runtime-local list pipe field transform should resolve to text");
        };
        assert_eq!(text.text(), "ready");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(root_scope),
                0,
                "runtime-local pipe field mapping should stay on the direct runtime subscriber path"
            );
        });
    }

    #[test]
    fn pending_future_forwarding_source_switches_to_runtime_subscriber_after_resolution() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let source = create_actor_forwarding(PersistenceId::new(), scope_id);
        let (source_tx, source_rx) = oneshot::channel::<ActorHandle>();

        let forwarded = super::create_actor_forwarding_from_future_source(
            async move { source_rx.await.ok() },
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                1,
                "pending future forwarding should retain only the one-shot source wait task before resolution"
            );
        });

        source.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.forwarding.pending_future.initial",
                None,
                "test forwarding pending future initial",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            41,
            "first",
        ));

        assert!(
            source_tx.send(source.clone()).is_ok(),
            "pending future forwarding should still be awaiting the source actor"
        );

        let mut handoff_complete = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            let has_forwarded_value = forwarded.current_value().is_ok();
            if async_source_count == 0 && has_forwarded_value {
                handoff_complete = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            handoff_complete,
            "direct-state sources resolved from pending futures should switch to runtime subscribers instead of keeping a forwarding task"
        );

        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarded actor should expose the resolved source value")
        else {
            panic!("expected forwarded text after source resolution");
        };
        assert_eq!(text.text(), "first");

        source.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.forwarding.pending_future.update",
                None,
                "test forwarding pending future update",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            42,
            "second",
        ));

        let Value::Text(text, _) = forwarded
            .current_value()
            .expect("forwarded actor should receive later direct-state updates after resolution")
        else {
            panic!("expected updated forwarded text after source resolution");
        };
        assert_eq!(text.text(), "second");
    }

    #[test]
    fn binary_operator_direct_state_operands_use_runtime_subscribers_without_async_source_task() {
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
            scope: Scope::Root,
            parameters: Arc::new(HashMap::new()),
            ..Default::default()
        };

        let left = create_actor_forwarding(PersistenceId::new(), scope_id);
        let right = create_actor_forwarding(PersistenceId::new(), scope_id);
        left.store_value_directly(super::Number::new_value(
            ConstructInfo::new("test.binary.left.initial", None, "test binary left initial"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            2.0,
        ));
        right.store_value_directly(super::Number::new_value(
            ConstructInfo::new(
                "test.binary.right.initial",
                None,
                "test binary right initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            3.0,
        ));

        let result = super::ArithmeticCombinator::new_add(
            construct_context.clone(),
            actor_context,
            left.clone(),
            right.clone(),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state binary combinators should use runtime subscribers instead of async source tasks"
            );
        });

        let Value::Number(number, _) = result
            .current_value()
            .expect("binary combinator should seed immediately from current operand values")
        else {
            panic!("expected numeric binary combinator result");
        };
        assert_eq!(number.number(), 5.0);

        right.store_value_directly(super::Number::new_value(
            ConstructInfo::new("test.binary.right.update", None, "test binary right update"),
            construct_context,
            ValueIdempotencyKey::new(),
            7.0,
        ));

        let Value::Number(number, _) = result
            .current_value()
            .expect("binary combinator should update from direct-state operand changes")
        else {
            panic!("expected updated numeric binary combinator result");
        };
        assert_eq!(number.number(), 9.0);
    }

    #[test]
    fn binary_operator_direct_state_with_lazy_operand_uses_one_scope_task() {
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
            scope: Scope::Root,
            parameters: Arc::new(HashMap::new()),
            ..Default::default()
        };

        let (mut left_tx, left_rx) = mpsc::channel::<Value>(8);
        let left = create_actor_lazy(left_rx, PersistenceId::new(), scope_id);
        let right = create_actor_forwarding(PersistenceId::new(), scope_id);
        right.store_value_directly(super::Number::new_value(
            ConstructInfo::new(
                "test.binary.lazy.right.initial",
                None,
                "test binary lazy right initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            3.0,
        ));

        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
        let result = super::ArithmeticCombinator::new_add(
            construct_context.clone(),
            actor_context,
            left,
            right.clone(),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state binary combinators should add one scope-owned watcher plus the first-demand lazy operand task"
            );
        });

        assert!(
            result.current_value().is_err(),
            "binary combinator should stay empty until the lazy operand emits"
        );

        left_tx
            .clone()
            .try_send(super::Number::new_value(
                ConstructInfo::new(
                    "test.binary.lazy.left.initial",
                    None,
                    "test binary lazy left initial",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                2.0,
            ))
            .expect("lazy left operand should enqueue its first value");
        super::poll_test_async_source_tasks();

        let Value::Number(number, _) = result
            .current_value()
            .expect("binary combinator should resolve once the lazy operand emits")
        else {
            panic!("expected numeric binary combinator result after lazy operand");
        };
        assert_eq!(number.number(), 5.0);

        left_tx
            .try_send(super::Number::new_value(
                ConstructInfo::new(
                    "test.binary.lazy.left.update",
                    None,
                    "test binary lazy left update",
                ),
                construct_context,
                ValueIdempotencyKey::new(),
                7.0,
            ))
            .expect("lazy left operand should enqueue its update");
        super::poll_test_async_source_tasks();

        let Value::Number(number, _) = result
            .current_value()
            .expect("binary combinator should update from later lazy operand values")
        else {
            panic!("expected updated numeric binary combinator result");
        };
        assert_eq!(number.number(), 10.0);
    }

    #[test]
    fn list_direct_subscriber_routes_changes_through_runtime_queue_without_async_source_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let source_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let target_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let target_construct_info =
            ConstructInfo::new("test.list.direct.target", None, "test list direct target")
                .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            source_state.register_direct_subscriber(
                &mut reg,
                Rc::new({
                    let target_state = target_state.clone();
                    let target_construct_info = target_construct_info.clone();
                    move |reg, change| {
                        reg.enqueue_list_runtime_work(
                            target_state.clone(),
                            super::ListRuntimeWork::Change {
                                construct_info: target_construct_info.clone(),
                                change: change.clone(),
                                broadcast_change: true,
                            },
                        );
                        true
                    }
                }),
            );
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.direct.source.replace",
                        None,
                        "test list direct source replace",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("first", 1)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct list subscribers should not spawn async source tasks"
            );
        });

        let initial_snapshot = target_state.snapshot();
        assert_eq!(
            initial_snapshot.len(),
            1,
            "target list should receive the source initialization replace"
        );

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.direct.source.push",
                        None,
                        "test list direct source push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: test_actor_handle("second", 2),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let snapshot = target_state.snapshot();
        assert_eq!(
            snapshot.len(),
            2,
            "target list should receive later source pushes"
        );
        assert_eq!(
            target_state.version(),
            2,
            "replace + push should advance the target list version through runtime queue work"
        );
    }

    #[test]
    fn runtime_list_ready_queue_routes_list_inputs_before_list_runtime_work_buffer() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info =
            ConstructInfo::new("test.runtime.list.input", None, "test runtime list input")
                .complete(super::ConstructType::List);
        let mut list = None;
        let mut list_arc_cache = None;

        super::process_live_list_change(
            &construct_info,
            &stored_state,
            &mut list,
            &mut list_arc_cache,
            &ListChange::Replace {
                items: Arc::from(vec![first]),
            },
            false,
        );

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_input(
                stored_state.clone(),
                super::ListRuntimeWork::LiveInputChange {
                    construct_info: construct_info.clone(),
                    change: ListChange::Push { item: second.clone() },
                },
            );

            assert!(
                !stored_state.has_pending_runtime_work(),
                "list runtime-work buffer should stay untouched until runtime-ready input is drained"
            );
            assert_eq!(
                reg.runtime_ready_queue.len(),
                1,
                "raw list inputs should queue on the runtime queue before list runtime-work scheduling"
            );

            reg.drain_runtime_ready_queue();
        });

        let snapshot = stored_state.snapshot();
        assert_eq!(
            snapshot.len(),
            2,
            "runtime-ready list inputs should still apply the queued change once drained"
        );
    }

    #[test]
    fn runtime_list_work_deduplicates_ready_queue_entries_per_drain() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let third = test_actor_handle("third", 3);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.runtime.list.work_dedup",
            None,
            "test runtime list work dedup",
        )
        .complete(super::ConstructType::List);
        let mut list = None;
        let mut list_arc_cache = None;

        super::process_live_list_change(
            &construct_info,
            &stored_state,
            &mut list,
            &mut list_arc_cache,
            &ListChange::Replace {
                items: Arc::from(vec![first]),
            },
            false,
        );

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                stored_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: construct_info.clone(),
                    change: ListChange::Push {
                        item: second.clone(),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                stored_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: construct_info.clone(),
                    change: ListChange::Push {
                        item: third.clone(),
                    },
                    broadcast_change: true,
                },
            );

            assert!(stored_state.has_pending_runtime_work());
            assert!(stored_state.is_scheduled_for_runtime());
            assert_eq!(
                reg.runtime_ready_queue.len(),
                1,
                "multiple list runtime work items should collapse to one ready-queue entry"
            );

            reg.drain_runtime_ready_queue();
        });

        let snapshot = stored_state.snapshot();
        assert_eq!(
            snapshot.len(),
            3,
            "the deduplicated runtime drain should still apply every queued list change in order"
        );
        assert_eq!(
            stored_state.version(),
            3,
            "replace plus two pushes should advance the list version through one runtime drain"
        );
    }

    #[test]
    fn list_map_direct_state_source_routes_changes_through_runtime_queue_without_async_source_task()
    {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let source_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info = ConstructInfo::new(
            "test.list.map.direct.source",
            None,
            "test list map direct source",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("first", 1)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list = Arc::new(super::List::new_with_stored_state(
            source_construct_info,
            scope_id,
            source_state.clone(),
        ));
        let source_actor = create_constant_actor(
            PersistenceId::new(),
            Value::List(source_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        );

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_map_actor(
            ConstructInfo::new(
                "test.list.map.direct.result",
                None,
                "test list map direct result",
            )
            .complete(super::ConstructType::List),
            construct_context,
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::Map,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state List/map should reuse list direct subscribers instead of spawning an async source task"
            );
        });

        let initial_snapshot = block_on(async {
            let Value::List(list, _) = result_actor
                .current_value()
                .expect("mapped list actor should expose a current list value")
            else {
                panic!("mapped actor should resolve to a list");
            };
            list.snapshot().await
        });
        assert_eq!(initial_snapshot.len(), 1);
        let Value::Text(text, _) = initial_snapshot[0]
            .1
            .current_value()
            .expect("mapped item should expose the forwarded text value")
        else {
            panic!("mapped item should resolve to text");
        };
        assert_eq!(text.text(), "first");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.map.direct.source.push",
                        None,
                        "test list map direct source push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: test_actor_handle("second", 2),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let updated_snapshot = block_on(async {
            let Value::List(list, _) = result_actor
                .current_value()
                .expect("mapped list actor should still expose its list after source updates")
            else {
                panic!("mapped actor should stay a list");
            };
            list.snapshot().await
        });
        assert_eq!(updated_snapshot.len(), 2);
        let Value::Text(text, _) = updated_snapshot[1]
            .1
            .current_value()
            .expect("pushed mapped item should expose the forwarded text value")
        else {
            panic!("pushed mapped item should resolve to text");
        };
        assert_eq!(text.text(), "second");
    }

    #[test]
    fn list_map_direct_state_source_switch_ignores_stale_old_list_updates_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.map.direct.switch.source_a",
            None,
            "test list map direct switch source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.map.direct.switch.source_b",
            None,
            "test list map direct switch source b",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("first", 1)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("alt", 2)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_map_actor(
            ConstructInfo::new(
                "test.list.map.direct.switch.result",
                None,
                "test list map direct switch result",
            )
            .complete(super::ConstructType::List),
            construct_context,
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::Map,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state source switching List/map should stay on runtime subscribers without async source tasks"
            );
        });

        let initial_snapshot = block_on(async {
            let Value::List(list, _) = result_actor
                .current_value()
                .expect("mapped list actor should expose the first source list")
            else {
                panic!("mapped actor should resolve to a list");
            };
            list.snapshot().await
        });
        assert_eq!(initial_snapshot.len(), 1);
        let Value::Text(text, _) = initial_snapshot[0]
            .1
            .current_value()
            .expect("initial mapped item should expose source a value")
        else {
            panic!("initial mapped item should resolve to text");
        };
        assert_eq!(text.text(), "first");

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));

        let switched_snapshot = block_on(async {
            let Value::List(list, _) = result_actor
                .current_value()
                .expect("mapped list actor should expose the switched source list")
            else {
                panic!("mapped actor should stay a list after source switch");
            };
            list.snapshot().await
        });
        assert_eq!(switched_snapshot.len(), 1);
        let Value::Text(text, _) = switched_snapshot[0]
            .1
            .current_value()
            .expect("switched mapped item should expose source b value")
        else {
            panic!("switched mapped item should resolve to text");
        };
        assert_eq!(text.text(), "alt");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.map.direct.switch.source_a.push",
                        None,
                        "test list map direct switch source a push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: test_actor_handle("stale", 6),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let final_snapshot = block_on(async {
            let Value::List(list, _) = result_actor
                .current_value()
                .expect("mapped list actor should still expose the switched source list")
            else {
                panic!("mapped actor should stay a list after stale old-list update");
            };
            list.snapshot().await
        });
        assert_eq!(final_snapshot.len(), 1);
        let Value::Text(text, _) = final_snapshot[0]
            .1
            .current_value()
            .expect("stale old-list updates should not replace the active mapped item")
        else {
            panic!("final mapped item should resolve to text");
        };
        assert_eq!(text.text(), "alt");
    }

    #[test]
    fn list_map_late_direct_state_source_waits_without_async_source_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.map.late_direct.source_a",
            None,
            "test list map late direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.map.late_direct.source_b",
            None,
            "test list map late direct source b",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("first", 1)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("alt", 2)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_map_actor(
            ConstructInfo::new(
                "test.list.map.late_direct.result",
                None,
                "test list map late direct result",
            )
            .complete(super::ConstructType::List),
            construct_context,
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::Map,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "late direct-state List/map should stay on runtime subscribers without spawning an async source task"
            );
        });

        let Value::List(result_list, _) = result_actor.current_value().expect(
            "mapped actor should expose its result list immediately even before source resolution",
        ) else {
            panic!("mapped actor should resolve to a list");
        };
        assert!(
            result_list.snapshot_now().is_none(),
            "late direct-state mapped list should stay uninitialized until the source list arrives"
        );

        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let initial_snapshot = block_on(result_list.snapshot());
        assert_eq!(initial_snapshot.len(), 1);
        let Value::Text(text, _) = initial_snapshot[0]
            .1
            .current_value()
            .expect("late-resolved mapped item should expose source a value")
        else {
            panic!("late-resolved mapped item should resolve to text");
        };
        assert_eq!(text.text(), "first");

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));

        let switched_snapshot = block_on(result_list.snapshot());
        assert_eq!(switched_snapshot.len(), 1);
        let Value::Text(text, _) = switched_snapshot[0]
            .1
            .current_value()
            .expect("mapped list should switch to the resolved source b list")
        else {
            panic!("switched mapped item should resolve to text");
        };
        assert_eq!(text.text(), "alt");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.map.late_direct.source_a.push",
                        None,
                        "test list map late direct source a push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: test_actor_handle("stale", 6),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let final_snapshot = block_on(result_list.snapshot());
        assert_eq!(
            final_snapshot.len(),
            1,
            "stale old-list updates should be ignored after the late direct-state source switches"
        );
        let Value::Text(text, _) = final_snapshot[0]
            .1
            .current_value()
            .expect("mapped list should keep the active source item after stale old-list updates")
        else {
            panic!("final mapped item should resolve to text");
        };
        assert_eq!(text.text(), "alt");
    }

    #[test]
    fn list_sort_by_direct_state_source_routes_changes_through_runtime_queue_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.sort_by.direct.source.{label}"),
                        None,
                        "test list sort_by direct source number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info = ConstructInfo::new(
            "test.list.sort_by.direct.source",
            None,
            "test list sort_by direct source",
        )
        .complete(super::ConstructType::List);
        let initial_high = make_number_actor(3.0, "initial_high");
        let initial_low = make_number_actor(1.0, "initial_low");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![initial_high, initial_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list = Arc::new(super::List::new_with_stored_state(
            source_construct_info,
            scope_id,
            source_state.clone(),
        ));
        let source_actor = create_constant_actor(
            PersistenceId::new(),
            Value::List(source_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        );

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_sort_by_actor(
            ConstructInfo::new(
                "test.list.sort_by.direct.result",
                None,
                "test list sort_by direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::SortBy,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state List/sort_by should stay on the runtime queue instead of spawning an async source task"
            );
        });

        let snapshot_numbers = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("sorted list actor should expose a current list value")
                else {
                    panic!("sorted actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item
                            .current_value()
                            .expect("sorted item should expose a current numeric value")
                        else {
                            panic!("sorted item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(snapshot_numbers(&result_actor), vec![1.0, 3.0]);

        let pushed_mid = make_number_actor(2.0, "pushed_mid");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.sort_by.direct.source.push",
                        None,
                        "test list sort_by direct source push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push { item: pushed_mid },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(snapshot_numbers(&result_actor), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn list_sort_by_direct_state_source_switch_ignores_stale_old_list_updates_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.sort_by.direct.switch.{label}"),
                        None,
                        "test list sort_by direct switch number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.sort_by.direct.switch.source_a",
            None,
            "test list sort_by direct switch source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.sort_by.direct.switch.source_b",
            None,
            "test list sort_by direct switch source b",
        )
        .complete(super::ConstructType::List);
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_b_high = make_number_actor(4.0, "source_b_high");
        let source_b_low = make_number_actor(3.0, "source_b_low");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_high, source_a_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high, source_b_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_sort_by_actor(
            ConstructInfo::new(
                "test.list.sort_by.direct.switch.result",
                None,
                "test list sort_by direct switch result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::SortBy,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state switching List/sort_by should not spawn async source tasks"
            );
        });

        let snapshot_numbers = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("sorted list actor should expose a current list value")
                else {
                    panic!("sorted actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item
                            .current_value()
                            .expect("sorted item should expose a current numeric value")
                        else {
                            panic!("sorted item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(snapshot_numbers(&result_actor), vec![1.0, 5.0]);

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        assert_eq!(snapshot_numbers(&result_actor), vec![3.0, 4.0]);

        let source_a_stale = make_number_actor(0.0, "source_a_stale");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.sort_by.direct.switch.source_a.stale_push",
                        None,
                        "test list sort_by direct switch source a stale push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: source_a_stale,
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(snapshot_numbers(&result_actor), vec![3.0, 4.0]);
    }

    #[test]
    fn list_sort_by_late_direct_state_source_waits_without_async_source_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.sort_by.late_direct.{label}"),
                        None,
                        "test list sort_by late direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.sort_by.late_direct.source_a",
            None,
            "test list sort_by late direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.sort_by.late_direct.source_b",
            None,
            "test list sort_by late direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_b_high = make_number_actor(4.0, "source_b_high");
        let source_b_low = make_number_actor(3.0, "source_b_low");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_high, source_a_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high, source_b_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_sort_by_actor(
            ConstructInfo::new(
                "test.list.sort_by.late_direct.result",
                None,
                "test list sort_by late direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::SortBy,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "late direct-state List/sort_by should stay on runtime subscribers without async source tasks"
            );
        });

        let Value::List(result_list, _) = result_actor
            .current_value()
            .expect("late direct-state sorted actor should expose its result list immediately")
        else {
            panic!("sorted actor should resolve to a list");
        };
        assert!(result_list.snapshot_now().is_none());

        let snapshot_numbers = |list: &Arc<super::List>| {
            block_on(async {
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item.current_value().expect(
                            "sorted late direct item should expose a current numeric value",
                        ) else {
                            panic!("sorted late direct item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));
        assert_eq!(snapshot_numbers(&result_list), vec![1.0, 5.0]);

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        assert_eq!(snapshot_numbers(&result_list), vec![3.0, 4.0]);

        let source_a_stale = make_number_actor(0.0, "source_a_stale");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.sort_by.late_direct.source_a.stale_push",
                        None,
                        "test list sort_by late direct source a stale push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: source_a_stale,
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });
        assert_eq!(snapshot_numbers(&result_list), vec![3.0, 4.0]);
    }

    #[test]
    fn list_retain_direct_state_source_routes_changes_through_runtime_queue_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.retain.direct.source.{label}"),
                        None,
                        "test list retain direct source number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info = ConstructInfo::new(
            "test.list.retain.direct.source",
            None,
            "test list retain direct source",
        )
        .complete(super::ConstructType::List);
        let initial_low = make_number_actor(1.0, "initial_low");
        let initial_high = make_number_actor(3.0, "initial_high");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![initial_low, initial_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list = Arc::new(super::List::new_with_stored_state(
            source_construct_info,
            scope_id,
            source_state.clone(),
        ));
        let source_actor = create_constant_actor(
            PersistenceId::new(),
            Value::List(source_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        );

        let source_code = SourceCode::new("item > 1".to_string());
        let result_actor = super::ListBindingFunction::create_retain_actor(
            ConstructInfo::new(
                "test.list.retain.direct.result",
                None,
                "test list retain direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(1.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Retain,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state List/retain should stay on runtime subscribers instead of spawning an async source task"
            );
        });

        let snapshot_numbers = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("retained list actor should expose a current list value")
                else {
                    panic!("retained actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item
                            .current_value()
                            .expect("retained item should expose a current numeric value")
                        else {
                            panic!("retained item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(snapshot_numbers(&result_actor), vec![3.0]);

        let pushed_mid = make_number_actor(2.0, "pushed_mid");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.retain.direct.source.push",
                        None,
                        "test list retain direct source push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push { item: pushed_mid },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(snapshot_numbers(&result_actor), vec![3.0, 2.0]);
    }

    #[test]
    fn list_retain_direct_state_source_switch_ignores_stale_old_list_updates_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.retain.direct.switch.{label}"),
                        None,
                        "test list retain direct switch number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.retain.direct.switch.source_a",
            None,
            "test list retain direct switch source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.retain.direct.switch.source_b",
            None,
            "test list retain direct switch source b",
        )
        .complete(super::ConstructType::List);
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_b_high = make_number_actor(4.0, "source_b_high");
        let source_b_mid = make_number_actor(3.0, "source_b_mid");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_low, source_a_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high, source_b_mid]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let source_code = SourceCode::new("item > 2".to_string());
        let result_actor = super::ListBindingFunction::create_retain_actor(
            ConstructInfo::new(
                "test.list.retain.direct.switch.result",
                None,
                "test list retain direct switch result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(2.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Retain,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state switching List/retain should not spawn async source tasks"
            );
        });

        let snapshot_numbers = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("retained list actor should expose a current list value")
                else {
                    panic!("retained actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item
                            .current_value()
                            .expect("retained item should expose a current numeric value")
                        else {
                            panic!("retained item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(snapshot_numbers(&result_actor), vec![5.0]);

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        assert_eq!(snapshot_numbers(&result_actor), vec![4.0, 3.0]);

        let source_a_stale = make_number_actor(10.0, "source_a_stale");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.retain.direct.switch.source_a.stale_push",
                        None,
                        "test list retain direct switch source a stale push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: source_a_stale,
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(snapshot_numbers(&result_actor), vec![4.0, 3.0]);
    }

    #[test]
    fn list_retain_late_direct_state_source_waits_without_async_source_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.retain.late_direct.{label}"),
                        None,
                        "test list retain late direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.retain.late_direct.source_a",
            None,
            "test list retain late direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.retain.late_direct.source_b",
            None,
            "test list retain late direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_b_high = make_number_actor(4.0, "source_b_high");
        let source_b_mid = make_number_actor(3.0, "source_b_mid");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_low, source_a_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high, source_b_mid]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let source_code = SourceCode::new("item > 2".to_string());
        let result_actor = super::ListBindingFunction::create_retain_actor(
            ConstructInfo::new(
                "test.list.retain.late_direct.result",
                None,
                "test list retain late direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(2.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Retain,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "late direct-state List/retain should stay on runtime subscribers without async source tasks"
            );
        });

        let Value::List(result_list, _) = result_actor
            .current_value()
            .expect("late direct-state retain actor should expose its result list immediately")
        else {
            panic!("retain actor should resolve to a list");
        };
        assert!(result_list.snapshot_now().is_none());

        let snapshot_numbers = |list: &Arc<super::List>| {
            block_on(async {
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item.current_value().expect(
                            "retained late direct item should expose a current numeric value",
                        ) else {
                            panic!("retained late direct item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));
        assert_eq!(snapshot_numbers(&result_list), vec![5.0]);

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        assert_eq!(snapshot_numbers(&result_list), vec![4.0, 3.0]);

        let source_a_stale = make_number_actor(10.0, "source_a_stale");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.retain.late_direct.source_a.stale_push",
                        None,
                        "test list retain late direct source a stale push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: source_a_stale,
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });
        assert_eq!(snapshot_numbers(&result_list), vec![4.0, 3.0]);
    }

    #[test]
    fn list_remove_direct_state_source_routes_changes_through_runtime_queue_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_trigger_item = |label: &str, trigger_actor: ActorHandle| {
            create_constant_actor(
                PersistenceId::new(),
                super::Object::new_value(
                    ConstructInfo::new(
                        format!("test.list.remove.direct.{label}.object"),
                        None,
                        "test list remove direct object",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    [Variable::new_arc(
                        ConstructInfo::new(
                            format!("test.list.remove.direct.{label}.trigger"),
                            None,
                            "test list remove direct trigger variable",
                        ),
                        "trigger",
                        trigger_actor,
                        PersistenceId::new(),
                        actor_scope.clone(),
                    )],
                ),
                scope_id,
            )
        };

        let trigger_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        let trigger_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        let trigger_c = create_actor_forwarding(PersistenceId::new(), scope_id);
        let item_a = make_trigger_item("a", trigger_a);
        let item_b = make_trigger_item("b", trigger_b.clone());
        let item_c = make_trigger_item("c", trigger_c);

        let source_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info = ConstructInfo::new(
            "test.list.remove.direct.source",
            None,
            "test list remove direct source",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![item_a.clone(), item_b.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list = Arc::new(super::List::new_with_stored_state(
            source_construct_info,
            scope_id,
            source_state.clone(),
        ));
        let source_actor = create_constant_actor(
            PersistenceId::new(),
            Value::List(source_list, ValueMetadata::new(ValueIdempotencyKey::new())),
            scope_id,
        );

        let source_code = SourceCode::new("item.trigger".to_string());
        let result_actor = super::ListBindingFunction::create_remove_actor(
            ConstructInfo::new(
                "test.list.remove.direct.result",
                None,
                "test list remove direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::PostfixFieldAccess {
                        expr: Box::new(static_expression::Spanned {
                            span: (0..4).into(),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(0, 4)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                        field: source_code.slice(5, 12),
                    },
                },
                operation: super::ListBindingOperation::Remove,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            None,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state List/remove should stay on runtime subscribers when both the source list and remove triggers are direct-state"
            );
        });

        let snapshot_pids = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("remove list actor should expose a current list value")
                else {
                    panic!("remove actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| item.persistence_id())
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(
            snapshot_pids(&result_actor),
            vec![item_a.persistence_id(), item_b.persistence_id()]
        );

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.remove.direct.source.push",
                        None,
                        "test list remove direct source push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: item_c.clone(),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(
            snapshot_pids(&result_actor),
            vec![
                item_a.persistence_id(),
                item_b.persistence_id(),
                item_c.persistence_id(),
            ]
        );

        trigger_b.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.remove.direct.trigger_b.fire",
                None,
                "test list remove direct trigger fire",
            ),
            construct_context,
            PersistenceId::new(),
            "fire",
        ));

        assert_eq!(
            snapshot_pids(&result_actor),
            vec![item_a.persistence_id(), item_c.persistence_id()]
        );
    }

    #[test]
    fn list_remove_direct_state_source_switch_ignores_stale_old_list_updates_without_async_source_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_trigger_item = |label: &str, trigger_actor: ActorHandle| {
            create_constant_actor(
                PersistenceId::new(),
                super::Object::new_value(
                    ConstructInfo::new(
                        format!("test.list.remove.direct.switch.{label}.object"),
                        None,
                        "test list remove direct switch object",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    [Variable::new_arc(
                        ConstructInfo::new(
                            format!("test.list.remove.direct.switch.{label}.trigger"),
                            None,
                            "test list remove direct switch trigger variable",
                        ),
                        "trigger",
                        trigger_actor,
                        PersistenceId::new(),
                        actor_scope.clone(),
                    )],
                ),
                scope_id,
            )
        };

        let stale_trigger = create_actor_forwarding(PersistenceId::new(), scope_id);
        let fresh_trigger = create_actor_forwarding(PersistenceId::new(), scope_id);
        let stale_item = make_trigger_item("stale_item", stale_trigger.clone());
        let fresh_item = make_trigger_item("fresh_item", fresh_trigger);

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.remove.direct.switch.source_a",
            None,
            "test list remove direct switch source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.remove.direct.switch.source_b",
            None,
            "test list remove direct switch source b",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![stale_item.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![fresh_item.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b,
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));

        let source_code = SourceCode::new("item.trigger".to_string());
        let result_actor = super::ListBindingFunction::create_remove_actor(
            ConstructInfo::new(
                "test.list.remove.direct.switch.result",
                None,
                "test list remove direct switch result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::PostfixFieldAccess {
                        expr: Box::new(static_expression::Spanned {
                            span: (0..4).into(),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(0, 4)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                        field: source_code.slice(5, 12),
                    },
                },
                operation: super::ListBindingOperation::Remove,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            None,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state switching List/remove should not spawn async source tasks"
            );
        });

        let snapshot_pids = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("remove list actor should expose a current list value")
                else {
                    panic!("remove actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| item.persistence_id())
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(
            snapshot_pids(&result_actor),
            vec![stale_item.persistence_id()]
        );

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        assert_eq!(
            snapshot_pids(&result_actor),
            vec![fresh_item.persistence_id()]
        );

        stale_trigger.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.remove.direct.switch.stale_trigger.fire",
                None,
                "test list remove direct switch stale trigger fire",
            ),
            construct_context,
            PersistenceId::new(),
            "fire",
        ));

        assert_eq!(
            snapshot_pids(&result_actor),
            vec![fresh_item.persistence_id()]
        );
    }

    #[test]
    fn list_remove_late_direct_state_source_waits_without_async_source_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_trigger_item = |label: &str, trigger_actor: ActorHandle| {
            create_constant_actor(
                PersistenceId::new(),
                super::Object::new_value(
                    ConstructInfo::new(
                        format!("test.list.remove.late_direct.{label}.object"),
                        None,
                        "test list remove late direct object",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    [Variable::new_arc(
                        ConstructInfo::new(
                            format!("test.list.remove.late_direct.{label}.trigger"),
                            None,
                            "test list remove late direct trigger variable",
                        ),
                        "trigger",
                        trigger_actor,
                        PersistenceId::new(),
                        actor_scope.clone(),
                    )],
                ),
                scope_id,
            )
        };

        let stale_trigger = create_actor_forwarding(PersistenceId::new(), scope_id);
        let fresh_trigger = create_actor_forwarding(PersistenceId::new(), scope_id);
        let stale_item = make_trigger_item("stale_item", stale_trigger.clone());
        let fresh_item = make_trigger_item("fresh_item", fresh_trigger);

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.remove.late_direct.source_a",
            None,
            "test list remove late direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.remove.late_direct.source_b",
            None,
            "test list remove late direct source b",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![stale_item.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![fresh_item.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b,
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let source_code = SourceCode::new("item.trigger".to_string());
        let result_actor = super::ListBindingFunction::create_remove_actor(
            ConstructInfo::new(
                "test.list.remove.late_direct.result",
                None,
                "test list remove late direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::PostfixFieldAccess {
                        expr: Box::new(static_expression::Spanned {
                            span: (0..4).into(),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(0, 4)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                        field: source_code.slice(5, 12),
                    },
                },
                operation: super::ListBindingOperation::Remove,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            None,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "late direct-state List/remove should stay on runtime subscribers without async source tasks"
            );
        });

        let Value::List(result_list, _) = result_actor
            .current_value()
            .expect("late direct-state remove actor should expose its result list immediately")
        else {
            panic!("remove actor should resolve to a list");
        };
        assert!(result_list.snapshot_now().is_none());

        let snapshot_pids = |list: &Arc<super::List>| {
            block_on(async {
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| item.persistence_id())
                    .collect::<Vec<_>>()
            })
        };

        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));
        assert_eq!(
            snapshot_pids(&result_list),
            vec![stale_item.persistence_id()]
        );

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        assert_eq!(
            snapshot_pids(&result_list),
            vec![fresh_item.persistence_id()]
        );

        stale_trigger.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.remove.late_direct.stale_trigger.fire",
                None,
                "test list remove late direct stale trigger fire",
            ),
            construct_context,
            PersistenceId::new(),
            "fire",
        ));
        assert_eq!(
            snapshot_pids(&result_list),
            vec![fresh_item.persistence_id()]
        );
    }

    #[test]
    fn list_map_lazy_direct_state_source_waits_with_one_scope_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.map.lazy_direct.source_a",
            None,
            "test list map lazy direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.map.lazy_direct.source_b",
            None,
            "test list map lazy direct source b",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("first", 1)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![test_actor_handle("alt", 2)]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let source_actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_map_actor(
            ConstructInfo::new(
                "test.list.map.lazy_direct.result",
                None,
                "test list map lazy direct result",
            )
            .complete(super::ConstructType::List),
            construct_context,
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::Map,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state List/map should add one scope-owned watcher plus the first-demand lazy source task"
            );
        });

        let Value::List(result_list, _) = result_actor
            .current_value()
            .expect("mapped actor should expose its result list immediately")
        else {
            panic!("mapped actor should resolve to a list");
        };
        assert!(result_list.snapshot_now().is_none());

        source_tx
            .try_send(Value::List(
                source_list_a,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
            .expect("source a should enqueue");
        super::poll_test_async_source_tasks();

        let initial_snapshot = block_on(result_list.snapshot());
        assert_eq!(initial_snapshot.len(), 1);
        let Value::Text(text, _) = initial_snapshot[0]
            .1
            .current_value()
            .expect("lazy mapped item should expose source a value")
        else {
            panic!("lazy mapped item should resolve to text");
        };
        assert_eq!(text.text(), "first");

        source_tx
            .try_send(Value::List(
                source_list_b,
                ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ))
            .expect("source b should enqueue");
        super::poll_test_async_source_tasks();

        let switched_snapshot = block_on(result_list.snapshot());
        assert_eq!(switched_snapshot.len(), 1);
        let Value::Text(text, _) = switched_snapshot[0]
            .1
            .current_value()
            .expect("lazy mapped list should switch to source b")
        else {
            panic!("lazy switched mapped item should resolve to text");
        };
        assert_eq!(text.text(), "alt");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.map.lazy_direct.source_a.push",
                        None,
                        "test list map lazy direct source a push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: test_actor_handle("stale", 6),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let final_snapshot = block_on(result_list.snapshot());
        assert_eq!(final_snapshot.len(), 1);
        let Value::Text(text, _) = final_snapshot[0]
            .1
            .current_value()
            .expect("stale old-list updates should be ignored")
        else {
            panic!("final lazy mapped item should resolve to text");
        };
        assert_eq!(text.text(), "alt");
    }

    #[test]
    fn list_sort_by_lazy_direct_state_source_waits_with_one_scope_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.sort_by.lazy_direct.{label}"),
                        None,
                        "test list sort_by lazy direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.sort_by.lazy_direct.source_a",
            None,
            "test list sort_by lazy direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.sort_by.lazy_direct.source_b",
            None,
            "test list sort_by lazy direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_b_high = make_number_actor(4.0, "source_b_high");
        let source_b_low = make_number_actor(3.0, "source_b_low");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_high, source_a_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high, source_b_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let source_actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        let source_code = SourceCode::new("item".to_string());
        let result_actor = super::ListBindingFunction::create_sort_by_actor(
            ConstructInfo::new(
                "test.list.sort_by.lazy_direct.result",
                None,
                "test list sort_by lazy direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Alias(
                        static_expression::Alias::WithoutPassed {
                            parts: vec![source_code.slice(0, 4)],
                            referenced_span: None,
                        },
                    ),
                },
                operation: super::ListBindingOperation::SortBy,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state List/sort_by should add one scope-owned watcher plus the first-demand lazy source task"
            );
        });

        let snapshot_numbers = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("sort_by actor should have list")
                else {
                    panic!("sort_by actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item
                            .current_value()
                            .expect("sorted item should expose a current numeric value")
                        else {
                            panic!("sorted item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        source_tx
            .try_send(Value::List(
                source_list_a,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
            .expect("source a should enqueue");
        super::poll_test_async_source_tasks();
        assert_eq!(snapshot_numbers(&result_actor), vec![1.0, 5.0]);

        source_tx
            .try_send(Value::List(
                source_list_b,
                ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ))
            .expect("source b should enqueue");
        super::poll_test_async_source_tasks();
        assert_eq!(snapshot_numbers(&result_actor), vec![3.0, 4.0]);

        let source_a_stale = make_number_actor(0.0, "source_a_stale");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.sort_by.lazy_direct.source_a.stale_push",
                        None,
                        "test list sort_by lazy direct source a stale push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: source_a_stale,
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(snapshot_numbers(&result_actor), vec![3.0, 4.0]);
    }

    #[test]
    fn list_retain_lazy_direct_state_source_waits_with_one_scope_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.retain.lazy_direct.{label}"),
                        None,
                        "test list retain lazy direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.retain.lazy_direct.source_a",
            None,
            "test list retain lazy direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.retain.lazy_direct.source_b",
            None,
            "test list retain lazy direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_b_high = make_number_actor(4.0, "source_b_high");
        let source_b_mid = make_number_actor(3.0, "source_b_mid");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_low, source_a_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high, source_b_mid]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let source_actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        let source_code = SourceCode::new("item > 2".to_string());
        let result_actor = super::ListBindingFunction::create_retain_actor(
            ConstructInfo::new(
                "test.list.retain.lazy_direct.result",
                None,
                "test list retain lazy direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(2.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Retain,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state List/retain should add one scope-owned watcher plus the first-demand lazy source task"
            );
        });

        let snapshot_numbers = |actor: &ActorHandle| {
            block_on(async {
                let Value::List(list, _) = actor
                    .current_value()
                    .expect("retain actor should have list")
                else {
                    panic!("retain actor should resolve to a list");
                };
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| {
                        let Value::Number(number, _) = item
                            .current_value()
                            .expect("retained item should expose a current numeric value")
                        else {
                            panic!("retained item should resolve to number");
                        };
                        number.number()
                    })
                    .collect::<Vec<_>>()
            })
        };

        source_tx
            .try_send(Value::List(
                source_list_a,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
            .expect("source a should enqueue");
        super::poll_test_async_source_tasks();
        assert_eq!(snapshot_numbers(&result_actor), vec![5.0]);

        source_tx
            .try_send(Value::List(
                source_list_b,
                ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ))
            .expect("source b should enqueue");
        super::poll_test_async_source_tasks();
        assert_eq!(snapshot_numbers(&result_actor), vec![4.0, 3.0]);

        let source_a_stale = make_number_actor(10.0, "source_a_stale");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.retain.lazy_direct.source_a.stale_push",
                        None,
                        "test list retain lazy direct source a stale push",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Push {
                        item: source_a_stale,
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        assert_eq!(snapshot_numbers(&result_actor), vec![4.0, 3.0]);
    }

    #[test]
    fn list_remove_lazy_direct_state_source_waits_with_one_scope_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_trigger_item = |label: &str, trigger_actor: ActorHandle| {
            create_constant_actor(
                PersistenceId::new(),
                super::Object::new_value(
                    ConstructInfo::new(
                        format!("test.list.remove.lazy_direct.{label}.object"),
                        None,
                        "test list remove lazy direct object",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    [Variable::new_arc(
                        ConstructInfo::new(
                            format!("test.list.remove.lazy_direct.{label}.trigger"),
                            None,
                            "test list remove lazy direct trigger variable",
                        ),
                        "trigger",
                        trigger_actor,
                        PersistenceId::new(),
                        actor_scope.clone(),
                    )],
                ),
                scope_id,
            )
        };

        let stale_trigger = create_actor_forwarding(PersistenceId::new(), scope_id);
        let fresh_trigger = create_actor_forwarding(PersistenceId::new(), scope_id);
        let stale_item = make_trigger_item("stale_item", stale_trigger.clone());
        let fresh_item = make_trigger_item("fresh_item", fresh_trigger);

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.remove.lazy_direct.source_a",
            None,
            "test list remove lazy direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.remove.lazy_direct.source_b",
            None,
            "test list remove lazy direct source b",
        )
        .complete(super::ConstructType::List);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![stale_item.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![fresh_item.clone()]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b,
        ));
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let source_actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        let source_code = SourceCode::new("item.trigger".to_string());
        let result_actor = super::ListBindingFunction::create_remove_actor(
            ConstructInfo::new(
                "test.list.remove.lazy_direct.result",
                None,
                "test list remove lazy direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::PostfixFieldAccess {
                        expr: Box::new(static_expression::Spanned {
                            span: (0..4).into(),
                            persistence: None,
                            node: static_expression::Expression::Alias(
                                static_expression::Alias::WithoutPassed {
                                    parts: vec![source_code.slice(0, 4)],
                                    referenced_span: None,
                                },
                            ),
                        }),
                        field: source_code.slice(5, 12),
                    },
                },
                operation: super::ListBindingOperation::Remove,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            None,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state List/remove should add one scope-owned watcher plus the first-demand lazy source task"
            );
        });

        let Value::List(result_list, _) = result_actor
            .current_value()
            .expect("remove actor should expose its result list immediately")
        else {
            panic!("remove actor should resolve to a list");
        };
        assert!(result_list.snapshot_now().is_none());

        let snapshot_pids = |list: &Arc<super::List>| {
            block_on(async {
                list.snapshot()
                    .await
                    .into_iter()
                    .map(|(_, item)| item.persistence_id())
                    .collect::<Vec<_>>()
            })
        };

        source_tx
            .try_send(Value::List(
                source_list_a,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
            .expect("source a should enqueue");
        super::poll_test_async_source_tasks();
        assert_eq!(
            snapshot_pids(&result_list),
            vec![stale_item.persistence_id()]
        );

        source_tx
            .try_send(Value::List(
                source_list_b,
                ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ))
            .expect("source b should enqueue");
        super::poll_test_async_source_tasks();
        assert_eq!(
            snapshot_pids(&result_list),
            vec![fresh_item.persistence_id()]
        );

        stale_trigger.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.list.remove.lazy_direct.stale_trigger.fire",
                None,
                "test list remove lazy direct stale trigger fire",
            ),
            construct_context,
            PersistenceId::new(),
            "fire",
        ));
        assert_eq!(
            snapshot_pids(&result_list),
            vec![fresh_item.persistence_id()]
        );
    }

    #[test]
    fn list_any_late_direct_state_source_waits_without_async_source_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.any.late_direct.{label}"),
                        None,
                        "test list any late direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.any.late_direct.source_a",
            None,
            "test list any late direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.any.late_direct.source_b",
            None,
            "test list any late direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_b_low = make_number_actor(1.0, "source_b_low");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_low, source_a_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let source_code = SourceCode::new("item > 2".to_string());
        let result_actor = super::ListBindingFunction::create_every_any_actor(
            ConstructInfo::new(
                "test.list.any.late_direct.result",
                None,
                "test list any late direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor.clone(),
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(2.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Any,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            false,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "late direct-state List/any should stay on runtime subscribers without async source tasks"
            );
        });

        assert!(matches!(
            result_actor.current_value(),
            Err(super::CurrentValueError::NoValueYet)
        ));

        source_actor.store_value_directly(Value::List(
            source_list_a,
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ));
        let Value::Tag(tag, _) = result_actor.current_value().expect(
            "late direct-state any actor should expose a resolved result after source arrival",
        ) else {
            panic!("any actor should resolve to a tag");
        };
        assert_eq!(tag.tag(), "True");

        source_actor.store_value_directly(Value::List(
            source_list_b,
            ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
        ));
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("late direct-state any actor should switch with the source list")
        else {
            panic!("any actor should stay a tag");
        };
        assert_eq!(tag.tag(), "False");

        let stale_old = make_number_actor(10.0, "stale_old");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.any.late_direct.source_a.stale_replace",
                        None,
                        "test list any late direct source a stale replace",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Replace {
                        items: Arc::from(vec![stale_old]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("stale updates should not replace the active any result")
        else {
            panic!("any actor should stay a tag after stale updates");
        };
        assert_eq!(tag.tag(), "False");
    }

    #[test]
    fn list_any_lazy_direct_state_source_waits_with_one_scope_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.any.lazy_direct.{label}"),
                        None,
                        "test list any lazy direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.any.lazy_direct.source_a",
            None,
            "test list any lazy direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.any.lazy_direct.source_b",
            None,
            "test list any lazy direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_b_low = make_number_actor(1.0, "source_b_low");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_low, source_a_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_low]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let source_actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        let source_code = SourceCode::new("item > 2".to_string());
        let result_actor = super::ListBindingFunction::create_every_any_actor(
            ConstructInfo::new(
                "test.list.any.lazy_direct.result",
                None,
                "test list any lazy direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(2.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Any,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            false,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state List/any should add one scope-owned watcher plus the first-demand lazy source task"
            );
        });

        assert!(matches!(
            result_actor.current_value(),
            Err(super::CurrentValueError::NoValueYet)
        ));

        source_tx
            .try_send(Value::List(
                source_list_a,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
            .expect("source a should enqueue");
        super::poll_test_async_source_tasks();
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("lazy any actor should resolve after source a arrives")
        else {
            panic!("lazy any actor should resolve to a tag");
        };
        assert_eq!(tag.tag(), "True");

        source_tx
            .try_send(Value::List(
                source_list_b,
                ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ))
            .expect("source b should enqueue");
        super::poll_test_async_source_tasks();
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("lazy any actor should switch with source b")
        else {
            panic!("lazy any actor should stay a tag");
        };
        assert_eq!(tag.tag(), "False");

        let stale_old = make_number_actor(10.0, "stale_old");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.any.lazy_direct.source_a.stale_replace",
                        None,
                        "test list any lazy direct source a stale replace",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Replace {
                        items: Arc::from(vec![stale_old]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("stale updates should not replace the active lazy any result")
        else {
            panic!("lazy any actor should stay a tag after stale updates");
        };
        assert_eq!(tag.tag(), "False");
    }

    #[test]
    fn list_every_lazy_direct_state_source_waits_with_one_scope_task_and_ignores_stale_old_list_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let make_number_actor = |value: f64, label: &str| {
            create_constant_actor(
                PersistenceId::new(),
                Number::new_value(
                    ConstructInfo::new(
                        format!("test.list.every.lazy_direct.{label}"),
                        None,
                        "test list every lazy direct number",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    value,
                ),
                scope_id,
            )
        };

        let source_state_a = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_state_b = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let source_construct_info_a = ConstructInfo::new(
            "test.list.every.lazy_direct.source_a",
            None,
            "test list every lazy direct source a",
        )
        .complete(super::ConstructType::List);
        let source_construct_info_b = ConstructInfo::new(
            "test.list.every.lazy_direct.source_b",
            None,
            "test list every lazy direct source b",
        )
        .complete(super::ConstructType::List);
        let source_a_low = make_number_actor(1.0, "source_a_low");
        let source_a_high = make_number_actor(5.0, "source_a_high");
        let source_b_high = make_number_actor(4.0, "source_b_high");

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_a.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_a_low, source_a_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.enqueue_list_runtime_work(
                source_state_b.clone(),
                super::ListRuntimeWork::Change {
                    construct_info: source_construct_info_b.clone(),
                    change: ListChange::Replace {
                        items: Arc::from(vec![source_b_high]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });

        let source_list_a = Arc::new(super::List::new_with_stored_state(
            source_construct_info_a,
            scope_id,
            source_state_a.clone(),
        ));
        let source_list_b = Arc::new(super::List::new_with_stored_state(
            source_construct_info_b,
            scope_id,
            source_state_b.clone(),
        ));
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let source_actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        let source_code = SourceCode::new("item > 2".to_string());
        let result_actor = super::ListBindingFunction::create_every_any_actor(
            ConstructInfo::new(
                "test.list.every.lazy_direct.result",
                None,
                "test list every lazy direct result",
            )
            .complete(super::ConstructType::List),
            construct_context.clone(),
            actor_context,
            source_actor,
            Arc::new(super::ListBindingConfig {
                binding_name: source_code.slice(0, 4),
                transform_expr: static_expression::Spanned {
                    span: span_at(source_code.len()),
                    persistence: None,
                    node: static_expression::Expression::Comparator(
                        static_expression::Comparator::Greater {
                            operand_a: Box::new(static_expression::Spanned {
                                span: (0..4).into(),
                                persistence: None,
                                node: static_expression::Expression::Alias(
                                    static_expression::Alias::WithoutPassed {
                                        parts: vec![source_code.slice(0, 4)],
                                        referenced_span: None,
                                    },
                                ),
                            }),
                            operand_b: Box::new(static_expression::Spanned {
                                span: (7..8).into(),
                                persistence: None,
                                node: static_expression::Expression::Literal(
                                    static_expression::Literal::Number(2.0),
                                ),
                            }),
                        },
                    ),
                },
                operation: super::ListBindingOperation::Every,
                reference_connector: Arc::new(super::ReferenceConnector::new()),
                link_connector: Arc::new(super::LinkConnector::new()),
                source_code,
                function_registry_snapshot: None,
            }),
            true,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state List/every should add one scope-owned watcher plus the first-demand lazy source task"
            );
        });

        assert!(matches!(
            result_actor.current_value(),
            Err(super::CurrentValueError::NoValueYet)
        ));

        source_tx
            .try_send(Value::List(
                source_list_a,
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
            .expect("source a should enqueue");
        super::poll_test_async_source_tasks();
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("lazy every actor should resolve after source a arrives")
        else {
            panic!("lazy every actor should resolve to a tag");
        };
        assert_eq!(tag.tag(), "False");

        source_tx
            .try_send(Value::List(
                source_list_b,
                ValueMetadata::with_emission_seq(ValueIdempotencyKey::new(), 5),
            ))
            .expect("source b should enqueue");
        super::poll_test_async_source_tasks();
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("lazy every actor should switch with source b")
        else {
            panic!("lazy every actor should stay a tag");
        };
        assert_eq!(tag.tag(), "True");

        let stale_old = make_number_actor(10.0, "stale_old");
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                source_state_a,
                super::ListRuntimeWork::Change {
                    construct_info: ConstructInfo::new(
                        "test.list.every.lazy_direct.source_a.stale_replace",
                        None,
                        "test list every lazy direct source a stale replace",
                    )
                    .complete(super::ConstructType::List),
                    change: ListChange::Replace {
                        items: Arc::from(vec![stale_old]),
                    },
                    broadcast_change: true,
                },
            );
            reg.drain_runtime_ready_queue();
        });
        let Value::Tag(tag, _) = result_actor
            .current_value()
            .expect("stale updates should not replace the active lazy every result")
        else {
            panic!("lazy every actor should stay a tag after stale updates");
        };
        assert_eq!(tag.tag(), "True");
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
    fn reference_connector_waiting_actor_uses_runtime_forwarding_without_async_source_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let connector = Arc::new(super::ReferenceConnector::new());
        let span = span_at(9);

        let waiting_actor = connector.referenceable_or_waiting_actor(span, scope_id);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "connector-owned waiting actor should not allocate an async source task"
            );
        });

        assert!(
            waiting_actor.current_value().is_err(),
            "waiting actor should have no current value before the reference is registered"
        );

        let source_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        source_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.reference_connector.waiting_actor.initial",
                None,
                "test reference connector waiting actor initial",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(
                    std::collections::BTreeMap::new(),
                )),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            ValueIdempotencyKey::new(),
            "referenceable",
        ));
        connector.register_referenceable(span, source_actor.clone());

        let Value::Text(text, _) = waiting_actor
            .current_value()
            .expect("waiting actor should expose the registered referenceable actor")
        else {
            panic!("expected text actor from connector waiting actor");
        };
        assert_eq!(text.text(), "referenceable");

        source_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.reference_connector.waiting_actor.update",
                None,
                "test reference connector waiting actor update",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(
                    std::collections::BTreeMap::new(),
                )),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            ValueIdempotencyKey::new(),
            "updated",
        ));

        let Value::Text(text, _) = waiting_actor
            .current_value()
            .expect("waiting actor should follow later source updates")
        else {
            panic!("expected text actor after source update");
        };
        assert_eq!(text.text(), "updated");
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
        let direct_subscribers = std::cell::RefCell::new(smallvec::SmallVec::<
            [super::OutputValveDirectSubscriber; 4],
        >::new());
        let direct_impulses = Rc::new(std::cell::Cell::new(0usize));
        super::register_output_valve_direct_subscriber(
            &active,
            &direct_subscribers,
            Rc::new({
                let direct_impulses = direct_impulses.clone();
                move |_event| {
                    direct_impulses.set(direct_impulses.get() + 1);
                    true
                }
            }),
        );
        let mut first = super::subscribe_output_valve_impulses(&active, &impulse_senders);
        let mut second = super::subscribe_output_valve_impulses(&active, &impulse_senders);

        super::broadcast_output_valve_impulse(&impulse_senders);
        super::broadcast_output_valve_direct_subscribers(
            &direct_subscribers,
            super::OutputValveDirectEvent::Impulse,
        );

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
        assert_eq!(
            direct_impulses.get(),
            1,
            "direct subscribers should receive impulse broadcasts too"
        );

        super::close_output_valve_subscribers(&active, &impulse_senders, &direct_subscribers);

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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "ready streaming alias root should not retain a wrapper task on the owning scope"
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
    fn streaming_alias_reference_constant_root_field_reuses_nested_actor_without_wrapper_task() {
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
        let field_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.constant_root_field.initial",
                None,
                "test streaming alias constant root field initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            51,
            "ready",
        ));
        let root_actor = create_constant_actor(
            PersistenceId::new(),
            super::Object::new_value(
                ConstructInfo::new(
                    "test.streaming_alias.constant_root.object",
                    None,
                    "test streaming alias constant root object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.streaming_alias.constant_root.variable",
                        None,
                        "test streaming alias constant root variable",
                    ),
                    "field",
                    field_actor.clone(),
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
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "constant-root field aliases should reuse the nested actor instead of spawning a wrapper task"
            );
        });

        assert_eq!(
            result_actor.actor_id(),
            field_actor.actor_id(),
            "constant-root field aliases should reuse the nested field actor handle"
        );

        field_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.constant_root_field.update",
                None,
                "test streaming alias constant root field update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            52,
            "updated",
        ));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("constant-root field alias should expose updated nested actor value")
        else {
            panic!("constant-root field alias should resolve to text");
        };
        assert_eq!(text.text(), "updated");
    }

    #[test]
    fn streaming_alias_reference_constant_nested_field_reuses_leaf_actor_without_wrapper_task() {
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
        let leaf_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        leaf_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.constant_nested_field.leaf.initial",
                None,
                "test streaming alias constant nested field leaf initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            71,
            "alpha",
        ));

        let child_actor = create_constant_actor(
            PersistenceId::new(),
            super::Object::new_value(
                ConstructInfo::new(
                    "test.streaming_alias.constant_nested_field.child.object",
                    None,
                    "test streaming alias constant nested field child object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.streaming_alias.constant_nested_field.child.variable",
                        None,
                        "test streaming alias constant nested field child variable",
                    ),
                    "leaf",
                    leaf_actor.clone(),
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );

        let root_actor = create_constant_actor(
            PersistenceId::new(),
            super::Object::new_value(
                ConstructInfo::new(
                    "test.streaming_alias.constant_nested_field.root.object",
                    None,
                    "test streaming alias constant nested field root object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.streaming_alias.constant_nested_field.root.variable",
                        None,
                        "test streaming alias constant nested field root variable",
                    ),
                    "child",
                    child_actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );

        let alias_source = SourceCode::new("root child leaf".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![
                alias_source.slice(0, 4),
                alias_source.slice(5, 10),
                alias_source.slice(11, 15),
            ],
            referenced_span: None,
        };
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "constant nested field aliases should reuse the leaf actor instead of spawning a wrapper task"
            );
        });

        assert_eq!(
            result_actor.actor_id(),
            leaf_actor.actor_id(),
            "constant nested field aliases should reuse the final leaf actor handle"
        );

        leaf_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.constant_nested_field.leaf.update",
                None,
                "test streaming alias constant nested field leaf update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            72,
            "updated",
        ));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("constant nested field alias should expose updated leaf actor value")
        else {
            panic!("constant nested field alias should resolve to text");
        };
        assert_eq!(text.text(), "updated");
    }

    #[test]
    fn streaming_alias_reference_direct_root_reuses_actor_without_wrapper_task() {
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
        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        root_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.direct_root.initial",
                None,
                "test streaming alias direct root initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            41,
            "ready",
        ));

        let alias_source = SourceCode::new("root".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4)],
            referenced_span: None,
        };
        let root_actor_for_future = root_actor.clone();
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor_for_future },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "plain streaming alias roots should reuse the source actor instead of spawning a wrapper task"
            );
        });

        assert_eq!(
            result_actor.actor_id(),
            root_actor.actor_id(),
            "plain streaming alias roots should reuse the original actor handle"
        );

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("streaming alias direct root should expose current value immediately")
        else {
            panic!("streaming alias direct root should resolve to text");
        };
        assert_eq!(text.text(), "ready");

        root_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.direct_root.update",
                None,
                "test streaming alias direct root update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            42,
            "updated",
        ));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("reused alias root should see later direct-state updates")
        else {
            panic!("streaming alias direct root should stay text");
        };
        assert_eq!(text.text(), "updated");
    }

    #[test]
    fn streaming_alias_reference_ready_root_field_uses_runtime_subscribers_without_wrapper_task() {
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
        let field_actor_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_field.a.initial",
                None,
                "test streaming alias ready root field a initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            61,
            "alpha",
        ));
        let field_actor_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_field.b.initial",
                None,
                "test streaming alias ready root field b initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            62,
            "beta",
        ));
        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let make_object = |field_actor: ActorHandle, construct_suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.ready_root_field.object.{construct_suffix}"),
                    None,
                    "test streaming alias ready root field object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!(
                            "test.streaming_alias.ready_root_field.variable.{construct_suffix}"
                        ),
                        None,
                        "test streaming alias ready root field variable",
                    ),
                    "field",
                    field_actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            )
        };
        root_actor.store_value_directly(make_object(field_actor_a.clone(), "a"));

        let alias_source = SourceCode::new("root field".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4), alias_source.slice(5, 10)],
            referenced_span: None,
        };
        let root_actor_for_future = root_actor.clone();
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context.clone(),
            alias,
            async move { root_actor_for_future },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "ready direct-state root field aliases should use runtime subscribers instead of a wrapper task"
            );
        });

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("root field alias should expose the current nested field value")
        else {
            panic!("root field alias should resolve to text");
        };
        assert_eq!(text.text(), "alpha");

        root_actor.store_value_directly(make_object(field_actor_b.clone(), "b"));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("root field alias should switch to the latest nested field actor")
        else {
            panic!("root field alias should stay text after root switch");
        };
        assert_eq!(text.text(), "beta");

        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_field.a.stale_update",
                None,
                "test streaming alias ready root field a stale update",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            63,
            "stale",
        ));
        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("stale nested field updates should be ignored after root switch")
        else {
            panic!("root field alias should stay text after stale nested update");
        };
        assert_eq!(text.text(), "beta");

        field_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_field.b.update",
                None,
                "test streaming alias ready root field b update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            64,
            "gamma",
        ));
        let Value::Text(text, _) = result_actor.current_value().expect(
            "latest nested field updates should still flow through the runtime subscriber path",
        ) else {
            panic!("root field alias should stay text after current nested update");
        };
        assert_eq!(text.text(), "gamma");
    }

    #[test]
    fn streaming_alias_reference_ready_root_field_clears_when_field_disappears() {
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
        let field_actor_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.field_clear.a.initial",
                None,
                "test streaming alias field clear a initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            91,
            "alpha",
        ));
        let field_actor_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.field_clear.b.initial",
                None,
                "test streaming alias field clear b initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            92,
            "beta",
        ));
        let unrelated_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        unrelated_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.field_clear.other.initial",
                None,
                "test streaming alias field clear other initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            93,
            "other",
        ));
        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let make_object = |name: &str, actor: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.field_clear.object.{suffix}"),
                    None,
                    "test streaming alias field clear object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.field_clear.variable.{suffix}"),
                        None,
                        "test streaming alias field clear variable",
                    ),
                    name.to_owned(),
                    actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            )
        };

        root_actor.store_value_directly(make_object("field", field_actor_a.clone(), "a"));

        let alias_source = SourceCode::new("root field".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4), alias_source.slice(5, 10)],
            referenced_span: None,
        };
        let root_actor_for_future = root_actor.clone();
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context.clone(),
            alias,
            async move { root_actor_for_future },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "ready root field aliases should stay on runtime subscribers when the field later disappears"
            );
        });

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("field alias should expose the initial field value")
        else {
            panic!("field alias should resolve to text");
        };
        assert_eq!(text.text(), "alpha");

        root_actor.store_value_directly(make_object("other", unrelated_actor.clone(), "missing"));

        assert!(
            matches!(
                result_actor.current_value(),
                Err(super::CurrentValueError::NoValueYet)
            ),
            "field alias should clear when the selected field disappears after a root switch"
        );

        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.field_clear.a.stale",
                None,
                "test streaming alias field clear a stale",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            94,
            "stale",
        ));
        assert!(
            matches!(
                result_actor.current_value(),
                Err(super::CurrentValueError::NoValueYet)
            ),
            "stale updates from the old field actor should not re-seed a cleared alias"
        );

        root_actor.store_value_directly(make_object("field", field_actor_b.clone(), "b"));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("field alias should recover once the selected field exists again")
        else {
            panic!("field alias should resolve to text after recovery");
        };
        assert_eq!(text.text(), "beta");
    }

    #[test]
    fn streaming_alias_reference_ready_root_nested_field_uses_runtime_subscribers_without_wrapper_task()
     {
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
        let actor_scope = actor_context.scope.clone();

        let leaf_actor_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        leaf_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_nested.leaf_a.initial",
                None,
                "test streaming alias ready root nested leaf a initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            71,
            "alpha",
        ));
        let leaf_actor_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        leaf_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_nested.leaf_b.initial",
                None,
                "test streaming alias ready root nested leaf b initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            72,
            "beta",
        ));

        let child_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let make_child_object = |leaf_actor: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.ready_root_nested.child.{suffix}"),
                    None,
                    "test streaming alias ready root nested child",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.ready_root_nested.child_var.{suffix}"),
                        None,
                        "test streaming alias ready root nested child var",
                    ),
                    "leaf",
                    leaf_actor,
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };
        child_actor.store_value_directly(make_child_object(leaf_actor_a.clone(), "a"));

        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let make_root_object = |child: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.ready_root_nested.root.{suffix}"),
                    None,
                    "test streaming alias ready root nested root",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.ready_root_nested.root_var.{suffix}"),
                        None,
                        "test streaming alias ready root nested root var",
                    ),
                    "child",
                    child,
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };
        root_actor.store_value_directly(make_root_object(child_actor.clone(), "initial"));

        let alias_source = SourceCode::new("root child leaf".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![
                alias_source.slice(0, 4),
                alias_source.slice(5, 10),
                alias_source.slice(11, 15),
            ],
            referenced_span: None,
        };
        let root_actor_for_future = root_actor.clone();
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor_for_future },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "ready nested field aliases should use runtime subscribers instead of wrapper tasks"
            );
        });

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("nested alias should expose initial nested field value")
        else {
            panic!("nested alias should resolve to text");
        };
        assert_eq!(text.text(), "alpha");

        child_actor.store_value_directly(make_child_object(leaf_actor_b.clone(), "b"));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("nested alias should switch when intermediate object changes")
        else {
            panic!("nested alias should stay text after intermediate switch");
        };
        assert_eq!(text.text(), "beta");

        leaf_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_nested.leaf_a.stale",
                None,
                "test streaming alias ready root nested leaf a stale",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            73,
            "stale",
        ));
        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("stale leaf updates should be ignored after intermediate switch")
        else {
            panic!("nested alias should stay text after stale leaf update");
        };
        assert_eq!(text.text(), "beta");

        leaf_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.ready_root_nested.leaf_b.update",
                None,
                "test streaming alias ready root nested leaf b update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            74,
            "gamma",
        ));
        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("current nested leaf updates should still flow through the direct path")
        else {
            panic!("nested alias should stay text after current leaf update");
        };
        assert_eq!(text.text(), "gamma");
    }

    #[test]
    fn streaming_alias_reference_late_root_field_switches_from_future_wait_to_runtime_subscribers()
    {
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

        let field_actor_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.late_root_field.a.initial",
                None,
                "test streaming alias late root field a initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            81,
            "alpha",
        ));
        let field_actor_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.late_root_field.b.initial",
                None,
                "test streaming alias late root field b initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            82,
            "beta",
        ));
        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let actor_scope = actor_context.scope.clone();
        let make_object = |field_actor: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.late_root_field.object.{suffix}"),
                    None,
                    "test streaming alias late root field object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.late_root_field.variable.{suffix}"),
                        None,
                        "test streaming alias late root field variable",
                    ),
                    "field",
                    field_actor,
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };

        let (root_tx, root_rx) = oneshot::channel::<ActorHandle>();
        let alias_source = SourceCode::new("root field".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4), alias_source.slice(5, 10)],
            referenced_span: None,
        };
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move {
                root_rx
                    .await
                    .expect("late-root alias should receive its root actor")
            },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                1,
                "late-root field alias should retain only the one-shot root wait task while unresolved"
            );
        });

        root_actor.store_value_directly(make_object(field_actor_a.clone(), "a"));
        assert!(
            root_tx.send(root_actor.clone()).is_ok(),
            "late-root alias should still be awaiting the root actor"
        );

        let mut handoff_complete = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            let has_value = result_actor.current_value().is_ok();
            if async_source_count == 0 && has_value {
                handoff_complete = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            handoff_complete,
            "late-root field aliases should switch from the root wait task to runtime subscribers after resolution"
        );

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("late-root field alias should expose the resolved nested field value")
        else {
            panic!("late-root field alias should resolve to text");
        };
        assert_eq!(text.text(), "alpha");

        root_actor.store_value_directly(make_object(field_actor_b.clone(), "b"));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("late-root field alias should switch when the root changes after resolution")
        else {
            panic!("late-root field alias should stay text after root switch");
        };
        assert_eq!(text.text(), "beta");

        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.late_root_field.a.stale",
                None,
                "test streaming alias late root field a stale",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            83,
            "stale",
        ));
        let Value::Text(text, _) = result_actor.current_value().expect(
            "late-root field alias should ignore stale old field updates after root switch",
        ) else {
            panic!("late-root field alias should stay text after stale field update");
        };
        assert_eq!(text.text(), "beta");

        field_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.late_root_field.b.update",
                None,
                "test streaming alias late root field b update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            84,
            "gamma",
        ));
        let Value::Text(text, _) = result_actor.current_value().expect(
            "late-root field alias should keep following current nested field updates on the direct path",
        ) else {
            panic!("late-root field alias should stay text after current field update");
        };
        assert_eq!(text.text(), "gamma");
    }

    #[test]
    fn streaming_alias_reference_lazy_root_field_uses_direct_alias_path_and_ignores_stale_old_field_updates()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let actor_scope = actor_context.scope.clone();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let leaf_actor_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        leaf_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.lazy_root_field.a.initial",
                None,
                "test streaming alias lazy root field a initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            401,
            "alpha",
        ));
        let leaf_actor_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        leaf_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.lazy_root_field.b.initial",
                None,
                "test streaming alias lazy root field b initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            402,
            "beta",
        ));

        let (mut root_tx, root_rx) = mpsc::channel::<Value>(8);
        let root_actor = super::create_actor_lazy(root_rx, PersistenceId::new(), scope_id);
        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
        let result_actor = super::create_field_path_alias_actor(
            actor_context,
            root_actor,
            vec![String::from("child"), String::from("leaf")],
        )
        .expect("lazy-root field aliases should use the direct alias path");

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy-root field aliases should add one scope-owned alias task plus the first-demand lazy root task"
            );
        });

        let make_child_object = |leaf_actor: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.lazy_root_field.child.{suffix}"),
                    None,
                    "test streaming alias lazy root field child",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [super::Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.lazy_root_field.child_var.{suffix}"),
                        None,
                        "test streaming alias lazy root field child var",
                    ),
                    "leaf",
                    leaf_actor,
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };
        let make_root_object = |child_actor: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.lazy_root_field.root.{suffix}"),
                    None,
                    "test streaming alias lazy root field root",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [super::Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.lazy_root_field.root_var.{suffix}"),
                        None,
                        "test streaming alias lazy root field root var",
                    ),
                    "child",
                    child_actor,
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };

        let child_actor_a = create_constant_actor(
            PersistenceId::new(),
            make_child_object(leaf_actor_a.clone(), "a"),
            scope_id,
        );
        root_tx
            .try_send(make_root_object(child_actor_a, "a"))
            .expect("initial lazy root value should queue");

        let mut initial_value_ready = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            if result_actor.current_value().is_ok() {
                initial_value_ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            initial_value_ready,
            "lazy-root field alias should resolve once the root lazy source emits"
        );

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("lazy-root field alias should expose the initial nested field value")
        else {
            panic!("lazy-root field alias should resolve to text");
        };
        assert_eq!(text.text(), "alpha");

        let child_actor_b = create_constant_actor(
            PersistenceId::new(),
            make_child_object(leaf_actor_b.clone(), "b"),
            scope_id,
        );
        root_tx
            .try_send(make_root_object(child_actor_b, "b"))
            .expect("switched lazy root value should queue");

        let mut switched_value_ready = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let is_beta = matches!(
                result_actor.current_value(),
                Ok(Value::Text(ref text, _)) if text.text() == "beta"
            );
            if is_beta {
                switched_value_ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            switched_value_ready,
            "lazy-root field alias should switch when the root lazy source emits a new object"
        );

        leaf_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.lazy_root_field.a.stale",
                None,
                "test streaming alias lazy root field a stale",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            403,
            "stale",
        ));
        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("lazy-root field alias should ignore stale old leaf updates")
        else {
            panic!("lazy-root field alias should stay text after stale leaf update");
        };
        assert_eq!(text.text(), "beta");

        leaf_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.lazy_root_field.b.update",
                None,
                "test streaming alias lazy root field b update",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            404,
            "gamma",
        ));
        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("lazy-root field alias should keep following the current nested field")
        else {
            panic!("lazy-root field alias should stay text after current leaf update");
        };
        assert_eq!(text.text(), "gamma");
    }

    #[test]
    fn streaming_alias_reference_ready_root_field_filters_stale_initial_leaf_value_without_wrapper_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            subscription_after_seq: Some(100),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let actor_scope = actor_context.scope.clone();

        let field_actor_a = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.filtered_leaf.a.initial",
                None,
                "test streaming alias filtered leaf a initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            90,
            "stale",
        ));
        let field_actor_b = create_actor_forwarding(PersistenceId::new(), scope_id);
        field_actor_b.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.filtered_leaf.b.initial",
                None,
                "test streaming alias filtered leaf b initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            150,
            "beta",
        ));
        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let make_object = |field_actor: ActorHandle, suffix: &str| {
            super::Object::new_value(
                ConstructInfo::new(
                    format!("test.streaming_alias.filtered_leaf.object.{suffix}"),
                    None,
                    "test streaming alias filtered leaf object",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.filtered_leaf.variable.{suffix}"),
                        None,
                        "test streaming alias filtered leaf variable",
                    ),
                    "field",
                    field_actor,
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };
        root_actor.store_value_directly(make_object(field_actor_a.clone(), "a"));

        let alias_source = SourceCode::new("root field".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![alias_source.slice(0, 4), alias_source.slice(5, 10)],
            referenced_span: None,
        };
        let root_actor_for_future = root_actor.clone();
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor_for_future },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "filtered ready-root field aliases should stay on runtime subscribers without a wrapper task"
            );
        });

        assert!(
            matches!(
                result_actor.current_value(),
                Err(super::CurrentValueError::NoValueYet)
            ),
            "stale initial nested field values at or before subscription_after_seq should be filtered"
        );

        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.filtered_leaf.a.update",
                None,
                "test streaming alias filtered leaf a update",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            110,
            "alpha",
        ));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("fresh nested field updates should seed the filtered direct alias")
        else {
            panic!("filtered direct alias should resolve to text after a fresh leaf update");
        };
        assert_eq!(text.text(), "alpha");

        root_actor.store_value_directly(make_object(field_actor_b.clone(), "b"));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("filtered direct alias should switch to a fresh nested field on root change")
        else {
            panic!("filtered direct alias should stay text after root switch");
        };
        assert_eq!(text.text(), "beta");

        field_actor_a.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.filtered_leaf.a.stale_after_switch",
                None,
                "test streaming alias filtered leaf a stale after switch",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            160,
            "stale",
        ));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("old nested field updates should stay disconnected after root switch")
        else {
            panic!("filtered direct alias should stay text after stale old-field update");
        };
        assert_eq!(text.text(), "beta");
    }

    #[test]
    fn streaming_alias_reference_ready_root_nested_field_filters_stale_intermediate_value_without_wrapper_task()
     {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            subscription_after_seq: Some(100),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new())),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let actor_scope = actor_context.scope.clone();

        let leaf_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        leaf_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.streaming_alias.filtered_nested.leaf.initial",
                None,
                "test streaming alias filtered nested leaf initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            200,
            "leaf",
        ));

        let child_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let make_child_object = |suffix: &str, emission_seq: u64| {
            super::Object::new_value_with_emission_seq(
                ConstructInfo::new(
                    format!("test.streaming_alias.filtered_nested.child.{suffix}"),
                    None,
                    "test streaming alias filtered nested child",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                emission_seq,
                [Variable::new_arc(
                    ConstructInfo::new(
                        format!("test.streaming_alias.filtered_nested.child_var.{suffix}"),
                        None,
                        "test streaming alias filtered nested child var",
                    ),
                    "leaf",
                    leaf_actor.clone(),
                    PersistenceId::new(),
                    actor_scope.clone(),
                )],
            )
        };
        child_actor.store_value_directly(make_child_object("initial", 90));

        let root_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        root_actor.store_value_directly(super::Object::new_value(
            ConstructInfo::new(
                "test.streaming_alias.filtered_nested.root.initial",
                None,
                "test streaming alias filtered nested root initial",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.streaming_alias.filtered_nested.root_var.initial",
                    None,
                    "test streaming alias filtered nested root var initial",
                ),
                "child",
                child_actor.clone(),
                PersistenceId::new(),
                actor_scope.clone(),
            )],
        ));

        let alias_source = SourceCode::new("root child leaf".to_string());
        let alias = static_expression::Alias::WithoutPassed {
            parts: vec![
                alias_source.slice(0, 4),
                alias_source.slice(5, 10),
                alias_source.slice(11, 15),
            ],
            referenced_span: None,
        };
        let root_actor_for_future = root_actor.clone();
        let result_actor = super::VariableOrArgumentReference::new_arc_value_actor(
            actor_context,
            alias,
            async move { root_actor_for_future },
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "filtered ready-root nested aliases should stay on runtime subscribers without a wrapper task"
            );
        });

        assert!(
            matches!(
                result_actor.current_value(),
                Err(super::CurrentValueError::NoValueYet)
            ),
            "stale intermediate values at or before subscription_after_seq should block deeper traversal"
        );

        child_actor.store_value_directly(make_child_object("fresh", 110));

        let Value::Text(text, _) = result_actor
            .current_value()
            .expect("fresh intermediate updates should rebuild the filtered nested alias")
        else {
            panic!(
                "filtered nested alias should resolve to text after a fresh intermediate update"
            );
        };
        assert_eq!(text.text(), "leaf");
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
                None,
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
    fn lazy_actor_internal_loop_marks_future_updates_exhausted_after_source_completion() {
        let first = test_actor_handle("first", 11)
            .current_value()
            .expect("test actor should expose first value");
        let second = test_actor_handle("second", 12)
            .current_value()
            .expect("test actor should expose second value");
        let stored_state = super::ActorStoredState::new(64);
        let (request_tx, request_rx) = NamedChannel::new("test.lazy_value_actor.source_end", 8);

        block_on(async move {
            let actor_loop = super::LazyValueActor::internal_loop(
                zoon::futures_util::stream::iter(vec![first.clone(), second.clone()]),
                request_rx,
                None,
                stored_state.clone(),
            );

            let test_future = async move {
                for (subscriber_id, expected) in [(0usize, "first"), (0usize, "second")] {
                    let (response_tx, response_rx) = oneshot::channel();
                    request_tx
                        .clone()
                        .send(super::LazyValueRequest {
                            subscriber_id,
                            start_cursor: 0,
                            response_tx,
                        })
                        .await
                        .expect("lazy request should send");

                    let Value::Text(text, _) = response_rx
                        .await
                        .expect("lazy response channel should resolve")
                        .expect("lazy source should still yield a value")
                    else {
                        panic!("expected lazy actor value to be text");
                    };
                    assert_eq!(text.text(), expected);
                }

                let (response_tx, response_rx) = oneshot::channel();
                request_tx
                    .clone()
                    .send(super::LazyValueRequest {
                        subscriber_id: 0,
                        start_cursor: 0,
                        response_tx,
                    })
                    .await
                    .expect("terminal lazy request should send");

                assert!(
                    response_rx
                        .await
                        .expect("terminal lazy response channel should resolve")
                        .is_none(),
                    "lazy source should return None once the source is exhausted"
                );
                assert!(
                    !stored_state.has_future_state_updates(),
                    "lazy actor should mark future updates exhausted after source completion"
                );

                drop(request_tx);
            };

            let (_actor_loop_done, _) =
                zoon::futures_util::future::join(actor_loop, test_future).await;
        });
    }

    #[test]
    fn lazy_stream_actor_requests_publish_values_through_runtime_queue() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let (mut source_tx, source_rx) = mpsc::channel::<Value>(8);
        let actor = super::create_actor_lazy(source_rx, PersistenceId::new(), scope_id);
        let first = test_actor_handle("first", 21)
            .current_value()
            .expect("test actor should expose first value");
        let second = test_actor_handle("second", 22)
            .current_value()
            .expect("test actor should expose second value");

        assert!(
            matches!(
                actor.current_value(),
                Err(super::CurrentValueError::NoValueYet)
            ),
            "lazy stream actor should start without a current value before first demand"
        );

        block_on(async move {
            let mut first_request = std::pin::pin!(actor.value());
            let Poll::Pending = poll_pinned_once(first_request.as_mut()) else {
                panic!("lazy actor should wait for the first demanded source value");
            };
            super::poll_test_async_source_tasks();

            source_tx
                .send(first.clone())
                .await
                .expect("first lazy source value should send");
            super::poll_test_async_source_tasks();

            let Value::Text(first_text, _) = first_request
                .await
                .expect("lazy actor should yield the first demanded value")
            else {
                panic!("expected first lazy actor value to be text");
            };
            assert_eq!(first_text.text(), "first");
            assert_eq!(
                actor.version(),
                1,
                "first lazy value should advance actor version through runtime queue publication"
            );

            let Value::Text(current_text, _) = actor
                .current_value()
                .expect("lazy actor should expose the first value in direct state after demand")
            else {
                panic!("expected current lazy actor value to be text");
            };
            assert_eq!(current_text.text(), "first");

            source_tx
                .send(second.clone())
                .await
                .expect("second lazy source value should send");
            let mut second_stream = actor.stream_from_now();
            let mut second_request = std::pin::pin!(second_stream.next());
            let Poll::Pending = poll_pinned_once(second_request.as_mut()) else {
                panic!("lazy actor should enqueue the second demand before the feeder task runs");
            };
            super::poll_test_async_source_tasks();

            let Value::Text(second_text, _) = second_request
                .await
                .expect("lazy actor should deliver the next demanded value")
            else {
                panic!("expected second lazy actor value to be text");
            };
            assert_eq!(second_text.text(), "second");
            assert_eq!(
                actor.version(),
                2,
                "second lazy value should also publish through the runtime queue"
            );

            drop(source_tx);
            let mut terminal_stream = actor.stream_from_now();
            let mut terminal_request = std::pin::pin!(terminal_stream.next());
            let Poll::Pending = poll_pinned_once(terminal_request.as_mut()) else {
                panic!("lazy actor should enqueue the terminal demand before source completion");
            };
            super::poll_test_async_source_tasks();
            assert!(
                terminal_request.await.is_none(),
                "lazy actor should end once the source ends without more buffered values"
            );
            assert!(
                !actor.stored_state.has_future_state_updates(),
                "lazy actor source completion should clear future updates through runtime queue publication"
            );
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
    fn runtime_actor_ready_queue_routes_actor_inputs_before_mailbox_scheduling() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let first = Text::new_value_with_emission_seq(
            ConstructInfo::new("test.runtime_queue.first", None, "test runtime queue first"),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            11,
            "first",
        );
        let second = Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.runtime_queue.second",
                None,
                "test runtime queue second",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            12,
            "second",
        );

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_actor_runtime_input(
                actor.actor_id,
                super::ActorMailboxWorkItem::Value(first),
            );
            reg.enqueue_actor_runtime_input(
                actor.actor_id,
                super::ActorMailboxWorkItem::Value(second),
            );

            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("runtime-input actor should stay registered");
            assert!(
                owned_actor.mailbox.is_empty(),
                "actor mailbox should stay untouched until runtime-ready input is drained"
            );
            assert!(!owned_actor.scheduled_for_runtime);
            assert_eq!(
                reg.runtime_ready_queue.len(),
                2,
                "raw actor inputs should queue on the runtime queue before mailbox scheduling"
            );

            reg.drain_runtime_ready_queue();

            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("runtime-input actor should stay registered after drain");
            assert!(owned_actor.mailbox.is_empty());
            assert!(!owned_actor.scheduled_for_runtime);
            assert!(reg.runtime_ready_queue.is_empty());
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("drained runtime mailbox should store the latest value")
        else {
            panic!("expected latest text value from runtime mailbox drain");
        };
        assert_eq!(text.text(), "second");
        assert_eq!(actor.version(), 2);

        block_on(async {
            let mut stream = actor.stream();

            let first = stream
                .next()
                .await
                .expect("replayed stream should include the first drained value");
            let Value::Text(text, _) = first else {
                panic!("expected first replayed text value");
            };
            assert_eq!(text.text(), "first");

            let second = stream
                .next()
                .await
                .expect("replayed stream should include the later drained value");
            let Value::Text(text, _) = second else {
                panic!("expected second replayed text value");
            };
            assert_eq!(text.text(), "second");
        });
    }

    #[test]
    fn runtime_actor_mailbox_work_deduplicates_ready_queue_entries_per_drain() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        let first = Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.runtime_queue.mailbox_dedup.first",
                None,
                "test runtime queue mailbox dedup first",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            21,
            "first",
        );
        let second = Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.runtime_queue.mailbox_dedup.second",
                None,
                "test runtime queue mailbox dedup second",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            PersistenceId::new(),
            22,
            "second",
        );

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_actor_mailbox_value(actor.actor_id, first);
            reg.enqueue_actor_mailbox_value(actor.actor_id, second);

            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("mailbox actor should stay registered");
            assert_eq!(
                owned_actor.mailbox.len(),
                2,
                "mailbox work should accumulate until the runtime drain runs"
            );
            assert!(
                owned_actor.scheduled_for_runtime,
                "mailbox enqueue should mark the actor scheduled"
            );
            assert_eq!(
                reg.runtime_ready_queue.len(),
                1,
                "multiple mailbox work items for the same actor should collapse to one ready-queue entry"
            );

            reg.drain_runtime_ready_queue();
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("drained mailbox work should publish the latest value")
        else {
            panic!("expected text value after mailbox dedup drain");
        };
        assert_eq!(text.text(), "second");
        assert_eq!(actor.version(), 2);

        block_on(async {
            let mut stream = actor.stream();

            let first = stream
                .next()
                .await
                .expect("late stream should replay the first mailbox value");
            let Value::Text(text, _) = first else {
                panic!("expected first replayed mailbox text value");
            };
            assert_eq!(text.text(), "first");

            let second = stream
                .next()
                .await
                .expect("late stream should replay the second mailbox value");
            let Value::Text(text, _) = second else {
                panic!("expected second replayed mailbox text value");
            };
            assert_eq!(text.text(), "second");
        });
    }

    #[test]
    fn runtime_ready_queue_can_release_async_source_slots() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let async_source_id = REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.reserve_async_source_for_scope(scope_id)
                .expect("scope should reserve an async source slot")
        });

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.runtime_ready_queue
                .push_back(super::RuntimeReadyItem::AsyncSourceCleanup(async_source_id));
            reg.drain_runtime_ready_queue();
        });

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "async source cleanup should release the reserved scope slot through the runtime queue"
            );
        });
    }

    #[test]
    fn drain_value_stream_to_actor_state_routes_values_through_runtime_mailbox_queue() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor_forwarding(PersistenceId::new(), scope_id);

        block_on(super::drain_value_stream_to_actor_state(
            actor.actor_id,
            stream::iter(vec![
                Text::new_value_with_emission_seq(
                    ConstructInfo::new(
                        "test.stream_mailbox.first",
                        None,
                        "test stream mailbox first",
                    ),
                    ConstructContext {
                        construct_storage: Arc::new(ConstructStorage::new("")),
                        virtual_fs: VirtualFilesystem::new(),
                        bridge_scope_id: None,
                        scene_ctx: None,
                    },
                    PersistenceId::new(),
                    21,
                    "first",
                ),
                Text::new_value_with_emission_seq(
                    ConstructInfo::new(
                        "test.stream_mailbox.second",
                        None,
                        "test stream mailbox second",
                    ),
                    ConstructContext {
                        construct_storage: Arc::new(ConstructStorage::new("")),
                        virtual_fs: VirtualFilesystem::new(),
                        bridge_scope_id: None,
                        scene_ctx: None,
                    },
                    PersistenceId::new(),
                    22,
                    "second",
                ),
            ]),
        ));

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("stream-fed actor should stay registered");
            assert!(owned_actor.mailbox.is_empty());
            assert!(!owned_actor.scheduled_for_runtime);
            assert!(reg.runtime_ready_queue.is_empty());
        });

        let Value::Text(text, _) = actor
            .current_value()
            .expect("mailbox-fed stream drain should store the latest value")
        else {
            panic!("expected latest text value from mailbox-fed stream drain");
        };
        assert_eq!(text.text(), "second");
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
    fn stream_driven_actor_reuses_direct_state_slot_with_scope_owned_external_feeder_task() {
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
            })
            .chain(stream::pending()),
            PersistenceId::new(),
            scope_id,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) > 0,
                "stream-driven actor should register one scope-owned external feeder task"
            );
        });

        block_on(async {
            let value = actor
                .current_value()
                .expect("stream-driven actor should expose the ready prefix value from its direct state slot");
            let Value::Text(text, _) = value else {
                panic!("expected text value from stream-driven actor");
            };
            assert_eq!(text.text(), "streamed");
        });
    }

    #[test]
    fn completed_stream_driven_actor_releases_scope_owned_feeder_task_before_scope_destroy() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let (mut change_sender, change_receiver) = mpsc::channel::<Value>(8);

        change_sender
            .try_send(Text::new_value_with_emission_seq(
                ConstructInfo::new(
                    "test.stream_driven_actor.completed.value",
                    None,
                    "test stream driven actor completed value",
                ),
                ConstructContext {
                    construct_storage: Arc::new(ConstructStorage::new("")),
                    virtual_fs: VirtualFilesystem::new(),
                    bridge_scope_id: None,
                    scene_ctx: None,
                },
                PersistenceId::new(),
                12,
                "streamed",
            ))
            .expect("ready prefix value should be queued before actor creation");

        let actor = create_actor(change_receiver, PersistenceId::new(), scope_id);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) > 0,
                "pending stream-driven actor should register one scope-owned feeder task"
            );
        });

        drop(change_sender);

        let mut feeder_cleared = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == 0 {
                feeder_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            feeder_cleared,
            "completed stream-driven actors should release their scope-owned feeder task before scope teardown"
        );

        let Value::Text(text, _) = actor
            .current_value()
            .expect("completed stream-driven actor should retain its last value in direct state")
        else {
            panic!("expected text value from completed stream-driven actor");
        };
        assert_eq!(text.text(), "streamed");
    }

    #[test]
    fn completed_stream_driven_actor_stream_from_now_ends_after_source_completion() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let (mut change_sender, change_receiver) = mpsc::channel::<Value>(8);

        change_sender
            .try_send(Text::new_value_with_emission_seq(
                ConstructInfo::new(
                    "test.stream_driven_actor.stream_from_now.value",
                    None,
                    "test stream driven actor stream_from_now value",
                ),
                ConstructContext {
                    construct_storage: Arc::new(ConstructStorage::new("")),
                    virtual_fs: VirtualFilesystem::new(),
                    bridge_scope_id: None,
                    scene_ctx: None,
                },
                PersistenceId::new(),
                13,
                "streamed",
            ))
            .expect("ready prefix value should be queued before actor creation");

        let actor = create_actor(change_receiver, PersistenceId::new(), scope_id);
        drop(change_sender);

        let mut feeder_cleared = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == 0 {
                feeder_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            feeder_cleared,
            "completed actor stream feeder should release before checking post-completion subscriptions"
        );

        block_on(async move {
            let mut updates = actor.stream_from_now();
            assert!(
                updates.next().await.is_none(),
                "completed stream-driven actor should end stream_from_now once the source has completed"
            );
        });
    }

    #[test]
    fn removing_stream_driven_actor_drops_scope_owned_feeder_task_before_scope_destroy() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor(stream::pending::<Value>(), PersistenceId::new(), scope_id);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) > 0,
                "pending stream-driven actor should register a scope-owned feeder task"
            );
        });

        REGISTRY.with(|reg| reg.borrow_mut().remove_actor(actor.actor_id));

        let mut feeder_cleared = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == 0 {
                feeder_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            feeder_cleared,
            "removing the actor should end its scope-owned feeder task before scope teardown"
        );
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
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let list = Arc::new(List::new_static(
            ConstructInfo::new(
                "test.static.persistent.list",
                None,
                "test static persistent list",
            )
            .complete(super::ConstructType::List),
            scope_id,
            vec![test_actor_handle("first", 1)],
        ));
        let storage = Arc::new(ConstructStorage::new(""));

        let list_for_save = list.clone();
        block_on(async move {
            super::save_or_watch_persistent_list(list_for_save, storage, PersistenceId::new())
                .await;
        });

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "static persistent list should not retain a hanging save watcher"
            );
        });
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
    fn live_persistent_list_save_watcher_is_list_owned_and_released_on_drop() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let storage = Arc::new(ConstructStorage::in_memory_for_tests(BTreeMap::new()));
        let persistence_id = PersistenceId::new();
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let first = test_actor_handle("first", 1);

        change_sender
            .try_send(ListChange::Replace {
                items: Arc::from(vec![first.clone()]),
            })
            .expect("ready initial replace should be queued before live persistent list creation");

        let list = Arc::new(List::new_with_change_stream(
            ConstructInfo::new(
                "test.live.persistent.list.watcher",
                None,
                "test live persistent list watcher",
            ),
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..Default::default()
            },
            change_receiver,
            (),
        ));

        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));

        block_on(async {
            super::save_or_watch_persistent_list(list.clone(), storage.clone(), persistence_id)
                .await;
        });

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 1,
                "live persistent list should add one list-owned save watcher on top of its existing live change feeder"
            );
        });

        let saved_items = storage
            .load_state_now::<Vec<zoon::serde_json::Value>>(persistence_id)
            .expect("initial persistent list snapshot should be saved");
        assert_eq!(
            saved_items.len(),
            1,
            "saved snapshot should contain the first item"
        );

        drop(list);
        let mut async_sources_cleared = false;
        for _ in 0..20 {
            super::drain_pending_registry_cleanups();
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == 0 {
                async_sources_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            async_sources_cleared,
            "dropping the live persistent list should release its list-owned save watcher and scope-owned live feeder before scope teardown"
        );
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "disabled storage should not retain a one-shot persistence init task"
            );
        });

        let Value::List(list, _) = actor
            .current_value()
            .expect("disabled-storage persistent list actor should be ready immediately")
        else {
            panic!("expected wrapped list value");
        };
        assert_eq!(
            block_on(list.snapshot()).len(),
            1,
            "disabled storage static list should expose its initial item immediately"
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "restored storage should not retain a one-shot persistence init task"
            );
        });

        let Value::List(list, _) = actor
            .current_value()
            .expect("restored persistent list actor should be ready immediately")
        else {
            panic!("expected wrapped list value");
        };
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
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
                reg.async_source_count_for_scope(scope_id) == 0,
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
    fn pending_future_actor_releases_scope_owned_task_when_actor_is_removed() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor =
            create_actor_from_future(future::pending::<Value>(), PersistenceId::new(), scope_id);
        let actor_id = actor.actor_id;

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                1,
                "pending future actor should keep exactly one scope-owned feeder task"
            );
        });

        REGISTRY.with(|reg| {
            reg.borrow_mut().remove_actor(actor_id);
        });
        super::poll_test_async_source_tasks();

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.drain_runtime_ready_queue();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "removing the future actor should release the scope-owned feeder task"
            );
        });
    }

    #[test]
    fn pending_forwarding_future_source_releases_scope_owned_task_when_actor_is_removed() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = super::create_actor_forwarding_from_future_source(
            future::pending::<Option<ActorHandle>>(),
            PersistenceId::new(),
            scope_id,
        );
        let actor_id = actor.actor_id;

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                1,
                "pending forwarding future source should keep exactly one scope-owned feeder task"
            );
        });

        REGISTRY.with(|reg| {
            reg.borrow_mut().remove_actor(actor_id);
        });
        super::poll_test_async_source_tasks();

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.drain_runtime_ready_queue();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "removing the forwarding actor should release the scope-owned future-source task"
            );
        });
    }

    #[test]
    fn pending_lazy_stream_actor_releases_scope_owned_task_when_actor_is_removed() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let (_change_sender, change_receiver) = mpsc::channel::<Value>(8);
        let actor = super::create_actor_lazy(change_receiver, PersistenceId::new(), scope_id);
        let actor_id = actor.actor_id;

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "pending lazy stream actor should not retain a feeder task before first demand"
            );
        });

        let mut first_request = std::pin::pin!(actor.value());
        let Poll::Pending = poll_pinned_once(first_request.as_mut()) else {
            panic!("lazy actor should stay pending before the first source value arrives");
        };
        super::poll_test_async_source_tasks();

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                1,
                "first lazy demand should start exactly one scope-owned feeder task"
            );
        });

        REGISTRY.with(|reg| {
            reg.borrow_mut().remove_actor(actor_id);
        });
        super::poll_test_async_source_tasks();

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.drain_runtime_ready_queue();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "removing the lazy stream actor should release the scope-owned feeder task"
            );
        });
    }

    #[test]
    fn ready_empty_lazy_stream_actor_uses_ended_path_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = super::create_actor_lazy(stream::empty(), PersistenceId::new(), scope_id);

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            let owned_actor = reg
                .get_actor(actor.actor_id)
                .expect("ready empty lazy stream actor should be registered");
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
                "ready empty lazy streams should not retain a feeder task"
            );
            assert!(
                owned_actor.lazy_delegate.is_none(),
                "ready empty lazy streams should collapse to an ended direct actor"
            );
            assert!(
                !owned_actor.stored_state.has_future_state_updates(),
                "ready empty lazy stream actor should mark future updates exhausted"
            );
        });

        assert!(
            matches!(
                actor.current_value(),
                Err(super::CurrentValueError::NoValueYet)
            ),
            "ready empty lazy stream actor should expose no current value"
        );

        block_on(async {
            assert!(
                actor.current_or_future_stream().next().await.is_none(),
                "ready empty lazy stream actor should expose an empty current_or_future_stream"
            );
            assert!(
                matches!(
                    actor.value().await,
                    Err(super::ValueError::SourceEndedWithoutValue)
                ),
                "ready empty lazy stream actor should report source exhaustion"
            );
        });
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
                reg.async_source_count_for_scope(scope_id) == 0,
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
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
            assert!(
                reg.async_source_count_for_scope(scope_id) == 0,
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
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let list = Arc::new(List::new_static(
            ConstructInfo::new("test.list.diff_stream", None, "test list diff stream")
                .complete(super::ConstructType::List),
            scope_id,
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
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let list = Arc::new(List::new_static(
            ConstructInfo::new("test.list", None, "test list").complete(super::ConstructType::List),
            scope_id,
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
            scope_id,
            vec![test_actor_handle("old", 1)],
        ));
        let current_list = Arc::new(List::new_static(
            ConstructInfo::new(
                "test.list.binding.current",
                None,
                "test list binding current",
            )
            .complete(super::ConstructType::List),
            scope_id,
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
            scope_id,
            vec![test_actor_handle("old", 1)],
        ));
        let current_list = Arc::new(List::new_static(
            ConstructInfo::new("test.list.map.current", None, "test list map current")
                .complete(super::ConstructType::List),
            scope_id,
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
    fn live_list_runtime_inputs_without_output_valve_apply_incremental_changes() {
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
                .expect("initial replace should reach runtime feeder");
            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should reach runtime feeder");
            drop(change_sender);

            super::drain_live_list_runtime_inputs(
                construct_info,
                stored_state.clone(),
                change_receiver,
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
    fn live_list_runtime_inputs_route_changes_through_runtime_queue() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.live.list.runtime.inputs",
            None,
            "test live list runtime inputs",
        )
        .complete(super::ConstructType::List);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);

        block_on(async move {
            change_sender
                .send(ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                })
                .await
                .expect("initial replace should reach runtime feeder");
            change_sender
                .send(ListChange::Push {
                    item: second.clone(),
                })
                .await
                .expect("push should reach runtime feeder");
            drop(change_sender);

            super::drain_live_list_runtime_inputs(
                construct_info,
                stored_state.clone(),
                change_receiver,
            )
            .await;

            let snapshot = stored_state.snapshot();
            assert_eq!(
                snapshot.len(),
                2,
                "runtime feeder should apply queued list changes through runtime state"
            );
            assert_eq!(
                stored_state.version(),
                2,
                "replace + push should advance version through runtime queue processing"
            );

            REGISTRY.with(|reg| {
                let reg = reg.borrow();
                assert!(
                    reg.runtime_ready_queue.is_empty(),
                    "runtime list feeder should drain its queued work eagerly"
                );
            });
        });
    }

    #[test]
    fn live_list_runtime_inputs_continue_from_existing_state() {
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
                .expect("push should reach runtime feeder");
            drop(change_sender);

            super::drain_live_list_runtime_inputs(
                construct_info,
                stored_state.clone(),
                change_receiver,
            )
            .await;

            let snapshot = stored_state.snapshot();
            assert_eq!(
                snapshot.len(),
                2,
                "runtime feeder should keep prior list state when draining incremental changes"
            );
            assert_eq!(
                stored_state.version(),
                2,
                "replace + incremental continuation should advance version"
            );
        });
    }

    #[test]
    fn static_list_output_valve_rebroadcasts_snapshot_without_list_owned_watcher_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let first = test_actor_handle("first", 1);
        let (subscriber_sender, mut subscriber_receiver) =
            NamedChannel::new("test.static.list.change_subscriber", 8);
        let active = std::cell::Cell::new(true);
        let direct_subscribers = std::cell::RefCell::new(smallvec::SmallVec::<
            [super::OutputValveDirectSubscriber; 4],
        >::new());
        let list = super::List::new(
            ConstructInfo::new(
                "test.static.list.output.valve",
                None,
                "test static list output valve",
            ),
            ConstructContext {
                construct_storage: Arc::new(ConstructStorage::new("")),
                virtual_fs: VirtualFilesystem::new(),
                bridge_scope_id: None,
                scene_ctx: None,
            },
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..ActorContext::default()
            },
            vec![first],
        );
        let stored_state = list.stored_state.clone();
        stored_state.register_change_subscriber(subscriber_sender);
        super::register_list_output_valve_runtime_subscriber_state(
            &active,
            &direct_subscribers,
            &stored_state,
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "static list output valve rebroadcast should not require a list-owned watcher task"
            );
        });

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

            super::broadcast_output_valve_direct_subscribers(
                &direct_subscribers,
                super::OutputValveDirectEvent::Impulse,
            );

            let Some(ListChange::Replace { items }) = subscriber_receiver.next().await else {
                panic!("subscriber should receive snapshot rebroadcast after impulse");
            };
            assert_eq!(
                items.len(),
                1,
                "rebroadcast should preserve the static snapshot"
            );

            super::broadcast_output_valve_direct_subscribers(
                &direct_subscribers,
                super::OutputValveDirectEvent::Closed,
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
    fn external_stream_runtime_adapter_routes_actor_values_through_runtime_queue() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let first = Text::new_value_with_emission_seq(
            ConstructInfo::new("test.external.stream.actor.first", None, "first"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            1,
            "first",
        );
        let second = Text::new_value_with_emission_seq(
            ConstructInfo::new("test.external.stream.actor.second", None, "second"),
            construct_context,
            ValueIdempotencyKey::new(),
            2,
            "second",
        );

        block_on(async move {
            super::drain_value_stream_to_actor_state(
                actor.actor_id,
                stream::iter(vec![first, second]),
            )
            .await;

            let Value::Text(text, _) = actor
                .current_value()
                .expect("actor should expose the last queued external stream value")
            else {
                panic!("expected text value");
            };
            assert_eq!(
                text.text(),
                "second",
                "shared external stream adapter should feed actor values through the runtime queue"
            );
        });
    }

    #[test]
    fn runtime_queue_ignores_actor_mailbox_work_for_dropped_actor_id() {
        let scope_id = create_registry_scope(None);
        let actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        let actor_id = actor.actor_id;
        drop(actor);

        REGISTRY.with(|reg| {
            reg.borrow_mut().destroy_scope(scope_id);
        });

        let construct_context = ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let value = Text::new_value_with_emission_seq(
            ConstructInfo::new("test.external.stream.actor.dropped", None, "dropped"),
            construct_context,
            ValueIdempotencyKey::new(),
            1,
            "dropped",
        );

        super::enqueue_actor_value_on_runtime_queue_by_id(actor_id, value);
        super::enqueue_actor_source_end_on_runtime_queue_by_id(actor_id);

        REGISTRY.with(|reg| {
            assert!(
                reg.borrow().get_actor(actor_id).is_none(),
                "runtime queue should ignore mailbox work for dropped actor ids"
            );
        });
    }

    #[test]
    fn live_list_change_work_uses_runtime_broadcast_flag_at_drain_time() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.live.list.runtime.broadcast.flag",
            None,
            "test live list runtime broadcast flag",
        )
        .complete(super::ConstructType::List);
        let (subscriber_sender, mut subscriber_receiver) =
            NamedChannel::new("test.live.list.runtime.broadcast.flag.subscriber", 8);
        let mut list = None;
        let mut list_arc_cache = None;

        super::process_live_list_change(
            &construct_info,
            &stored_state,
            &mut list,
            &mut list_arc_cache,
            &ListChange::Replace {
                items: Arc::from(vec![first]),
            },
            false,
        );
        stored_state.register_change_subscriber(subscriber_sender);
        stored_state.set_broadcast_live_changes(false);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                stored_state.clone(),
                super::ListRuntimeWork::LiveInputChange {
                    construct_info: construct_info.clone(),
                    change: ListChange::Push {
                        item: second.clone(),
                    },
                },
            );
            stored_state.set_broadcast_live_changes(true);
            reg.drain_runtime_ready_queue();
        });

        block_on(async {
            let Some(ListChange::Replace { items }) = subscriber_receiver.next().await else {
                panic!("initialized list subscriber should receive a snapshot replace first");
            };
            assert_eq!(
                items.len(),
                1,
                "initial snapshot should contain the seeded item"
            );

            let Some(ListChange::Push { item }) = subscriber_receiver.next().await else {
                panic!("runtime should evaluate live-list broadcast gating at drain time");
            };
            let Value::Text(text, _) = item
                .current_value()
                .expect("broadcast item should expose current value")
            else {
                panic!("expected pushed actor value to be text");
            };
            assert_eq!(text.text(), "second");
        });
    }

    #[test]
    fn live_list_output_valve_close_broadcasts_future_changes_without_impulse_stream_loop() {
        let first = test_actor_handle("first", 1);
        let second = test_actor_handle("second", 2);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.live.list.output.valve.close",
            None,
            "test live list output valve close",
        )
        .complete(super::ConstructType::List);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let (subscriber_sender, mut subscriber_receiver) =
            NamedChannel::new("test.live.list.output.valve.close.subscriber", 8);
        let active = std::cell::Cell::new(true);
        let direct_subscribers = std::cell::RefCell::new(smallvec::SmallVec::<
            [super::OutputValveDirectSubscriber; 4],
        >::new());
        stored_state.register_change_subscriber(subscriber_sender);
        stored_state.set_has_future_state_updates(true);
        stored_state.set_broadcast_live_changes(false);
        stored_state.set_rebroadcast_snapshot_on_output_close(true);
        super::register_list_output_valve_runtime_subscriber_state(
            &active,
            &direct_subscribers,
            &stored_state,
        );

        block_on(async move {
            let drain = super::drain_live_list_runtime_inputs(
                construct_info,
                stored_state.clone(),
                change_receiver,
            );
            let drive = async {
                change_sender
                    .send(ListChange::Replace {
                        items: Arc::from(vec![first.clone()]),
                    })
                    .await
                    .expect("initial replace should reach runtime feeder");

                let Some(ListChange::Replace { items }) = subscriber_receiver.next().await else {
                    panic!("subscriber should receive the initial replace from pending activation");
                };
                assert_eq!(items.len(), 1, "initial replace should initialize the list");

                assert!(
                    subscriber_receiver
                        .next()
                        .now_or_never()
                        .flatten()
                        .is_none(),
                    "future live changes should stay gated before the output valve closes"
                );

                super::broadcast_output_valve_direct_subscribers(
                    &direct_subscribers,
                    super::OutputValveDirectEvent::Closed,
                );

                change_sender
                    .send(ListChange::Push {
                        item: second.clone(),
                    })
                    .await
                    .expect("push should reach runtime feeder after output close");
                drop(change_sender);

                let Some(ListChange::Push { item }) = subscriber_receiver.next().await else {
                    panic!("closed output valve should broadcast later live pushes directly");
                };
                let Value::Text(text, _) = item
                    .current_value()
                    .expect("pushed actor should expose current value")
                else {
                    panic!("expected pushed actor value to be text");
                };
                assert_eq!(text.text(), "second");
            };

            future::join(drain, drive).await;

            assert!(
                !stored_state.has_future_state_updates(),
                "live list loop should clear future-update state when the change stream ends"
            );
        });
    }

    #[test]
    fn live_list_output_valve_close_after_stream_end_rebroadcasts_final_snapshot() {
        let first = test_actor_handle("first", 1);
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        let construct_info = ConstructInfo::new(
            "test.live.list.output.valve.final.snapshot",
            None,
            "test live list output valve final snapshot",
        )
        .complete(super::ConstructType::List);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let (subscriber_sender, mut subscriber_receiver) =
            NamedChannel::new("test.live.list.output.valve.final.snapshot.subscriber", 8);
        let active = std::cell::Cell::new(true);
        let direct_subscribers = std::cell::RefCell::new(smallvec::SmallVec::<
            [super::OutputValveDirectSubscriber; 4],
        >::new());
        stored_state.set_has_future_state_updates(true);
        stored_state.set_broadcast_live_changes(false);
        stored_state.set_rebroadcast_snapshot_on_output_close(true);
        super::register_list_output_valve_runtime_subscriber_state(
            &active,
            &direct_subscribers,
            &stored_state,
        );

        block_on(async move {
            change_sender
                .send(ListChange::Replace {
                    items: Arc::from(vec![first.clone()]),
                })
                .await
                .expect("initial replace should reach runtime feeder");
            drop(change_sender);

            super::drain_live_list_runtime_inputs(
                construct_info,
                stored_state.clone(),
                change_receiver,
            )
            .await;

            assert!(
                !stored_state.has_future_state_updates(),
                "ended live list should mark future updates as finished before output close"
            );

            stored_state.register_change_subscriber(subscriber_sender);
            let Some(ListChange::Replace { items }) = subscriber_receiver.next().await else {
                panic!("late subscriber should receive the current snapshot immediately");
            };
            assert_eq!(
                items.len(),
                1,
                "late subscription should replay the final snapshot"
            );

            super::broadcast_output_valve_direct_subscribers(
                &direct_subscribers,
                super::OutputValveDirectEvent::Closed,
            );

            let Some(ListChange::Replace { items }) = subscriber_receiver.next().await else {
                panic!("output close after stream end should rebroadcast the final snapshot");
            };
            assert_eq!(
                items.len(),
                1,
                "rebroadcast after stream end should preserve the final snapshot"
            );
        });
    }

    #[test]
    fn immediate_single_replace_change_stream_builds_static_list_without_retained_task() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let first = test_actor_handle("first", 1);
        let list = super::List::new_with_change_stream(
            ConstructInfo::new(
                "test.immediate.single.replace.list",
                None,
                "test immediate single replace list",
            ),
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..ActorContext::default()
            },
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
            REGISTRY.with(|reg| {
                let reg = reg.borrow();
                assert!(
                    reg.async_source_count_for_scope(list.scope_id) == 0,
                    "single ready replace should stay on the static list path"
                );
            });
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
    fn live_list_runtime_inputs_from_initialized_state_continue_after_ready_initial_replace() {
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
                .expect("push should reach runtime feeder");
            drop(change_sender);

            super::drain_live_list_runtime_inputs(
                construct_info,
                stored_state.clone(),
                change_receiver,
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
    fn list_runtime_source_end_work_clears_future_updates_through_runtime_queue() {
        let stored_state = super::ListStoredState::new(super::DiffHistoryConfig::default());
        stored_state.set_has_future_state_updates(true);

        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            reg.enqueue_list_runtime_work(
                stored_state.clone(),
                super::ListRuntimeWork::SourceEnded,
            );
            reg.drain_runtime_ready_queue();
        });

        assert!(
            !stored_state.has_future_state_updates(),
            "list source-end completion should clear future updates through list runtime work"
        );
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
    fn dropping_live_list_releases_scope_owned_feeder_task_before_scope_destroy() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let (_change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let list = Arc::new(List::new_with_change_stream(
            ConstructInfo::new(
                "test.live.list.drop_cleanup",
                None,
                "test live list drop cleanup",
            ),
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..Default::default()
            },
            change_receiver,
            (),
        ));

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) > 0,
                "active live list should register a scope-owned feeder task"
            );
        });

        drop(list);
        let mut feeder_cleared = false;
        for _ in 0..20 {
            super::drain_pending_registry_cleanups();
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == 0 {
                feeder_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            feeder_cleared,
            "dropping the live list should release its scope-owned feeder task before scope teardown"
        );
    }

    #[test]
    fn completed_live_list_releases_scope_owned_feeder_task_before_scope_destroy() {
        let scope_id = create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let (mut change_sender, change_receiver) = mpsc::channel::<ListChange>(8);
        let first = test_actor_handle("first", 1);

        change_sender
            .try_send(ListChange::Replace {
                items: Arc::from(vec![first.clone()]),
            })
            .expect("ready initial replace should be queued before live list creation");

        let list = Arc::new(List::new_with_change_stream(
            ConstructInfo::new(
                "test.live.list.completed_cleanup",
                None,
                "test live list completed cleanup",
            ),
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..Default::default()
            },
            change_receiver,
            (),
        ));

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert!(
                reg.async_source_count_for_scope(scope_id) > 0,
                "pending live list should register one scope-owned feeder task"
            );
        });

        assert_eq!(
            list.stored_state.snapshot().len(),
            1,
            "ready initial replace should initialize direct list state before feeder cleanup"
        );

        drop(change_sender);

        let mut feeder_cleared = false;
        for _ in 0..20 {
            #[cfg(all(test, not(target_arch = "wasm32")))]
            super::poll_test_async_source_tasks();
            let async_source_count =
                REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
            if async_source_count == 0 {
                feeder_cleared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            feeder_cleared,
            "completed live lists should release their scope-owned feeder task before scope teardown"
        );
        assert!(
            !list.stored_state.has_future_state_updates(),
            "completed live list should clear its future-update flag after stream end"
        );
    }

    #[test]
    fn latest_combinator_direct_state_inputs_ignore_older_late_arrivals_without_async_source_task()
    {
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

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                0,
                "direct-state latest combinator inputs should use runtime subscribers instead of async source tasks"
            );
        });

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

    #[test]
    fn latest_combinator_with_lazy_input_uses_one_scope_task_and_updates_output() {
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

        let (mut left_tx, left_rx) = mpsc::channel::<Value>(8);
        let left_actor = create_actor_lazy(left_rx, PersistenceId::new(), scope_id);
        let right_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        right_actor.store_value_directly(Text::new_value_with_emission_seq(
            ConstructInfo::new(
                "test.latest.lazy.right.initial",
                None,
                "test latest lazy right initial",
            ),
            construct_context.clone(),
            PersistenceId::new(),
            20,
            "ready",
        ));

        let baseline_async_source_count =
            REGISTRY.with(|reg| reg.borrow().async_source_count_for_scope(scope_id));
        let latest_actor = LatestCombinator::new_arc_value_actor(
            ConstructInfo::new("test.latest.lazy", None, "test latest lazy combinator"),
            construct_context.clone(),
            actor_context,
            vec![left_actor, right_actor.clone()],
        );

        REGISTRY.with(|reg| {
            let reg = reg.borrow();
            assert_eq!(
                reg.async_source_count_for_scope(scope_id),
                baseline_async_source_count + 2,
                "lazy direct-state latest combinators should add one scope-owned watcher plus the first-demand lazy input task"
            );
        });

        let Value::Text(text, _) = latest_actor
            .current_value()
            .expect("latest should still expose the ready eager input before lazy updates")
        else {
            panic!("latest should expose a text value from the eager input");
        };
        assert_eq!(text.text(), "ready");

        left_tx
            .try_send(Text::new_value(
                ConstructInfo::new(
                    "test.latest.lazy.left.update",
                    None,
                    "test latest lazy left update",
                ),
                construct_context,
                ValueIdempotencyKey::new(),
                "newest",
            ))
            .expect("lazy latest input should enqueue an update");
        super::poll_test_async_source_tasks();

        let Value::Text(text, _) = latest_actor
            .current_value()
            .expect("latest should switch to the lazy input after it emits")
        else {
            panic!("latest should keep a text value after the lazy update");
        };
        assert_eq!(text.text(), "newest");
    }
}
