use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};


use crate::parser;
use crate::parser::SourceCode;
use crate::parser::static_expression;
use super::evaluator::{evaluate_static_expression_with_registry, FunctionRegistry};

use ulid::Ulid;

use zoon::IntoCowStr;
use zoon::future;
use zoon::futures_channel::{mpsc, oneshot};
use zoon::futures_util::future::{FutureExt, Shared};
use zoon::futures_util::select;
use zoon::futures_util::stream::{self, LocalBoxStream, Stream, StreamExt};
use zoon::{Deserialize, DeserializeOwned, Serialize, serde, serde_json};
use zoon::{Task, TaskHandle};
use zoon::{WebStorage, local_storage};
use zoon::futures_util::SinkExt;

use std::cell::Cell;

// --- Performance Metrics ---
//
// Compile-time instrumentation counters for profiling the actors engine.
// Enabled via `--features actors-metrics`. Compiles to no-ops when disabled.

/// Increment a metric counter. No-op when `actors-metrics` feature is disabled.
#[cfg(feature = "actors-metrics")]
macro_rules! inc_metric {
    ($counter:ident) => {
        $crate::platform::browser::engine_actors::engine::metrics::$counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    };
    ($counter:ident, $amount:expr) => {
        $crate::platform::browser::engine_actors::engine::metrics::$counter
            .fetch_add($amount, std::sync::atomic::Ordering::Relaxed);
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
        zoon::println!("[actors-metrics] Bridge: change_constructed={}, keydown_constructed={}, hover_emitted={}, hover_deduped={}, change_deduped={}",
            CHANGE_EVENTS_CONSTRUCTED.load(Ordering::Relaxed),
            KEYDOWN_EVENTS_CONSTRUCTED.load(Ordering::Relaxed),
            HOVER_EVENTS_EMITTED.load(Ordering::Relaxed),
            HOVER_EVENTS_DEDUPED.load(Ordering::Relaxed),
            CHANGE_EVENTS_DEDUPED.load(Ordering::Relaxed),
        );
        zoon::println!("[actors-metrics] List: replace_sent={}, replace_items={}, fanout_sends={}, retain_rebuilds={}, retain_items={}, remove_spawned={}, remove_completed={}",
            REPLACE_PAYLOADS_SENT.load(Ordering::Relaxed),
            REPLACE_PAYLOAD_TOTAL_ITEMS.load(Ordering::Relaxed),
            REPLACE_FANOUT_SENDS.load(Ordering::Relaxed),
            RETAIN_PREDICATE_REBUILDS.load(Ordering::Relaxed),
            RETAIN_PREDICATE_ITEMS.load(Ordering::Relaxed),
            REMOVE_TASKS_SPAWNED.load(Ordering::Relaxed),
            REMOVE_TASKS_COMPLETED.load(Ordering::Relaxed),
        );
        zoon::println!("[actors-metrics] Actors: created={}, dropped={}, channel_drops={}",
            ACTORS_CREATED.load(Ordering::Relaxed),
            ACTORS_DROPPED.load(Ordering::Relaxed),
            CHANNEL_DROPS.load(Ordering::Relaxed),
        );
        zoon::println!("[actors-metrics] Evaluator: slots_allocated={}, registry_clones={}, registry_clone_entries={}",
            SLOTS_ALLOCATED.load(Ordering::Relaxed),
            REGISTRY_CLONES.load(Ordering::Relaxed),
            REGISTRY_CLONE_ENTRIES.load(Ordering::Relaxed),
        );
        zoon::println!("[actors-metrics] Persistence: writes={}",
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

// --- Lamport Clock ---
//
// Provides happened-before ordering for values without global state.
// Used to filter "stale" events that existed before a subscription was created.
//
// Algorithm:
// 1. Each execution context maintains a local counter
// 2. On value creation: increment counter, stamp value with current time
// 3. On value receive: local = max(local, received) + 1
//
// This ensures that if event A causes event B, then timestamp(A) < timestamp(B).

thread_local! {
    /// Thread-local Lamport clock for the current execution context.
    /// In browser WASM, there's only one thread so this acts as a global clock.
    /// If Boon moves to WebWorkers, each worker gets its own clock that syncs
    /// on message passing - exactly how Lamport clocks are designed to work.
    static LOCAL_CLOCK: Cell<u64> = const { Cell::new(0) };
}

/// Advance local clock and return new timestamp (for value creation).
pub fn lamport_tick() -> u64 {
    LOCAL_CLOCK.with(|c| {
        let new_time = c.get() + 1;
        c.set(new_time);
        new_time
    })
}

/// Update local clock on receiving a value (Lamport receive rule).
/// local = max(local, received) + 1
pub fn lamport_receive(received_time: u64) -> u64 {
    LOCAL_CLOCK.with(|c| {
        let new_time = c.get().max(received_time) + 1;
        c.set(new_time);
        new_time
    })
}

/// Get current local clock value (for recording subscription time).
pub fn lamport_now() -> u64 {
    LOCAL_CLOCK.with(|c| c.get())
}

// --- TimestampedEvent ---
//
// Wrapper type that ENFORCES Lamport timestamp on all DOM events.
// Using this type instead of raw data prevents future bugs where
// developers forget to capture timestamps.

/// DOM event data with captured Lamport timestamp.
/// The timestamp is captured at the moment the DOM callback fires,
/// BEFORE the event is sent through channels. This ensures correct
/// happened-before ordering even when `select!` processes events
/// out of order.
#[derive(Debug, Clone)]
pub struct TimestampedEvent<T> {
    pub data: T,
    pub lamport_time: u64,
}

impl<T> TimestampedEvent<T> {
    /// Create a timestamped event - captures Lamport clock at this moment.
    /// Call this in DOM callbacks, BEFORE sending to channels.
    pub fn now(data: T) -> Self {
        Self {
            data,
            lamport_time: lamport_tick(),
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
            zoon::println!("[DEBUG] Added {} to removed set {}, now {} items",
                call_id, removed_set_key, removed.len());
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
        if self.inner.clone().try_send(value).is_err() {
            inc_metric!(CHANNEL_DROPS);
            if LOG_ACTOR_FLOW {
                zoon::eprintln!(
                    "[FLOW] send_or_drop FAILED on '{}' (full or disconnected, capacity: {})",
                    self.name, self.capacity
                );
            }
            #[cfg(feature = "debug-channels")]
            zoon::eprintln!(
                "[BACKPRESSURE DROP] '{}' dropped event (full, capacity: {})",
                self.name, self.capacity
            );
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
                    self.name, self.capacity
                );
            }
            // Don't log disconnected - it's expected during WHILE arm switches
        }
        result
    }

    /// Check if the channel is closed (receiver dropped).
    #[allow(dead_code)]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

/// Debug flag to log when ValueActors, Variables, and other constructs are dropped,
/// and when their internal loops end. This is useful for debugging premature drop issues
/// where subscriptions fail with "receiver is gone" errors.
///
/// When enabled, prints messages like:
/// - "Dropped: {construct_info}" - when a construct is deallocated
/// - "Loop ended {construct_info}" - when a ValueActor's internal loop exits
///
/// Common drop-related issues in the engine:
/// - Subscriber dropped before all events are processed
/// - ValueActor dropped while subscriptions are still active
/// - Extra owned data not properly keeping actors alive
const LOG_DROPS_AND_LOOP_ENDS: bool = false;
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

// --- TypedStream ---

/// Marker for streams that never terminate (safe for ValueActor).
/// Streams with this marker can be safely used as input to ValueActor
/// because they will never cause the actor's internal loop to exit.
pub struct Infinite;

/// Marker for streams that will terminate (unsafe for ValueActor without conversion).
/// Using a Finite stream directly with ValueActor will cause "receiver is gone" errors
/// when the stream terminates and the actor's loop exits.
pub struct Finite;

/// A stream wrapper with compile-time lifecycle information.
///
/// This type enforces at compile time that only infinite streams are used
/// with ValueActor, preventing "receiver is gone" errors.
///
/// # Type Parameters
/// - `S`: The underlying stream type
/// - `Lifecycle`: Either `Infinite` or `Finite`, indicating whether the stream terminates
///
/// # Example
/// ```ignore
/// // This compiles - constant() returns TypedStream<_, Infinite>
/// ValueActor::new(constant(value), ...);
///
/// // This would NOT compile - stream::once() is Finite
/// ValueActor::new(TypedStream::finite(stream::once(...)), ...);
///
/// // To fix, convert to infinite:
/// ValueActor::new(TypedStream::finite(stream::once(...)).keep_alive(), ...);
/// ```
#[pin_project::pin_project]
pub struct TypedStream<S, Lifecycle> {
    #[pin]
    pub(crate) inner: S,
    _marker: PhantomData<Lifecycle>,
}

impl<S> TypedStream<S, Infinite> {
    /// Create an infinite stream wrapper.
    /// Use this for streams that you know will never terminate.
    pub fn infinite(stream: S) -> Self {
        Self {
            inner: stream,
            _marker: PhantomData,
        }
    }
}

impl<S> TypedStream<S, Finite> {
    /// Create a finite stream wrapper.
    /// Use this for streams that will terminate (like stream::once()).
    pub fn finite(stream: S) -> Self {
        Self {
            inner: stream,
            _marker: PhantomData,
        }
    }

    /// Convert a finite stream to an infinite one by chaining with pending().
    /// This is the proper way to use a finite stream with ValueActor.
    pub fn keep_alive(self) -> TypedStream<stream::Chain<S, stream::Once<future::Pending<S::Item>>>, Infinite>
    where
        S: Stream,
    {
        TypedStream::infinite(self.inner.chain(stream::once(future::pending())))
    }
}

impl<S: Stream, L> Stream for TypedStream<S, L> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().inner.poll_next(cx)
    }
}

// --- constant ---

/// Creates an infinite stream that emits a single value and then stays pending forever.
///
/// This is the preferred way to create a ValueActor for a constant value.
/// Unlike `stream::once()`, this stream never terminates, so the ValueActor's
/// internal loop will never exit and subscribers will never receive
/// "receiver is gone" errors.
///
/// # Example
/// ```ignore
/// // Safe: constant() returns an infinite stream
/// ValueActor::new(constant(value), ...);
///
/// // Unsafe: stream::once() terminates after emitting
/// ValueActor::new(TypedStream::infinite(stream::once(...)), ...); // BUG! Will cause errors
/// ```
pub fn constant<T>(item: T) -> TypedStream<impl Stream<Item = T>, Infinite> {
    TypedStream::infinite(stream::once(future::ready(item)).chain(stream::once(future::pending())))
}

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

// --- ActorLoop ---

/// Encapsulates the async loop that makes an Actor an Actor.
///
/// This abstraction keeps `Task::start_droppable` in ONE place (here),
/// rather than scattered throughout the codebase. All infrastructure
/// actors (StorageActor, RegistryActor, BroadcastActor) should use this.
///
/// # Why This Exists
/// - **Hardware portability**: Actors map to FSMs, Tasks are runtime-specific
/// - **Conceptual clarity**: If something uses ActorLoop, it IS an Actor
/// - **Single point of change**: If Task spawning changes, only change here
pub struct ActorLoop {
    handle: TaskHandle,
}

impl ActorLoop {
    /// Create a new actor loop from an async closure.
    /// The future should contain the actor's main loop (typically with select!).
    pub fn new(future: impl Future<Output = ()> + 'static) -> Self {
        Self {
            handle: Task::start_droppable(future),
        }
    }
}

// --- switch_map ---

/// Applies a function to each value from the outer stream, creating an inner stream.
/// When a new outer value arrives, the previous inner stream is cancelled (dropped)
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

    // State as tuple: (outer_stream, inner_stream_opt, map_fn, pending_outer_value)
    // pending_outer_value: When outer emits while inner is active, we store the outer value
    // and drain the inner stream before switching. This prevents losing in-flight events.
    let initial: (FusedOuter<S::Item>, Option<FusedInner<U::Item>>, F, Option<S::Item>) = (
        outer.boxed_local().fuse(),
        None,
        f,
        None, // pending_outer_value
    );

    stream::unfold(initial, |state| async move {
        use zoon::futures_util::future::Either;
        use std::task::Poll;

        // Destructure state - we need to rebuild it for the return
        let (mut outer_stream, mut inner_opt, map_fn, mut pending_outer) = state;

        loop {
            // If we have a pending outer value and the inner stream is done/empty, switch now
            if let Some(pending_value) = pending_outer.take() {
                inner_opt = Some(map_fn(pending_value).boxed_local().fuse());
                continue;
            }

            match &mut inner_opt {
                Some(inner) if !inner.is_terminated() => {
                    // Both streams active - race between them
                    let outer_fut = outer_stream.next();
                    let inner_fut = inner.next();

                    match future::select(pin!(outer_fut), pin!(inner_fut)).await {
                        Either::Left((outer_opt, _inner_fut_incomplete)) => {
                            match outer_opt {
                                Some(new_outer_value) => {
                                    // Outer emitted - but don't switch immediately!
                                    // First, try to drain any ready items from inner stream.
                                    // Poll the inner stream once to check for ready items
                                    // We use poll_fn to do a non-blocking check
                                    let inner_item = std::future::poll_fn(|cx| {
                                        match Pin::new(&mut *inner).poll_next(cx) {
                                            Poll::Ready(item) => Poll::Ready(item),
                                            Poll::Pending => Poll::Ready(None), // No ready item
                                        }
                                    }).await;

                                    if let Some(item) = inner_item {
                                        // Inner had a ready item - emit it and store outer for later
                                        pending_outer = Some(new_outer_value);
                                        return Some((item, (outer_stream, inner_opt, map_fn, pending_outer)));
                                    } else {
                                        // No ready items in inner - safe to switch now
                                        drop(inner_opt.take());
                                        inner_opt = Some(map_fn(new_outer_value).boxed_local().fuse());
                                    }
                                }
                                None => {
                                    // Outer ended - drain inner then finish
                                    while let Some(item) = inner.next().await {
                                        return Some((item, (outer_stream, inner_opt, map_fn, None)));
                                    }
                                    return None;
                                }
                            }
                        }
                        Either::Right((inner_opt_val, _)) => {
                            match inner_opt_val {
                                Some(item) => {
                                    return Some((item, (outer_stream, inner_opt, map_fn, pending_outer)));
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
    use zoon::futures_util::stream::FusedStream;
    use std::task::Poll;

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

/// Request sent to BackpressureCoordinator to acquire a permit.
struct BackpressureRequest {
    /// Channel to send grant acknowledgment back to requester.
    grant_tx: oneshot::Sender<()>,
}

/// Inner state of BackpressureCoordinator, held in Arc for cloning.
struct BackpressureCoordinatorInner {
    /// Channel for acquire requests from THEN.
    request_tx: mpsc::Sender<BackpressureRequest>,
    /// Channel for release signals from HOLD callback.
    completion_tx: mpsc::Sender<()>,
    /// Actor loop managing permit coordination.
    _actor_loop: ActorLoop,
}

/// Message-based backpressure coordination for HOLD/THEN synchronization.
///
/// Replaces the shared-state BackpressurePermit with pure channel-based coordination.
/// This makes the engine cluster/distributed-ready - no shared memory required.
///
/// # How it works:
/// 1. HOLD creates coordinator
/// 2. THEN calls `acquire().await` which sends a request and waits for grant
/// 3. Coordinator grants the request (sends on grant channel)
/// 4. THEN evaluates body and emits result
/// 5. HOLD callback calls `release()` which sends completion signal
/// 6. Coordinator receives completion, ready for next request
///
/// # Actor Model
/// The coordinator is an actor with an internal loop that processes requests
/// sequentially. This ensures only one THEN body runs at a time without
/// any shared mutable state.
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
        // Bounded(1) channels - only one pending request/completion at a time
        let (request_tx, mut request_rx) = mpsc::channel::<BackpressureRequest>(1);
        let (completion_tx, mut completion_rx) = mpsc::channel::<()>(1);

        let actor_loop = ActorLoop::new(async move {
            // Process requests sequentially - this IS the permit
            while let Some(request) = request_rx.next().await {
                // Grant permit to requester
                if request.grant_tx.send(()).is_err() {
                    // Requester dropped - they gave up waiting, continue to next
                    continue;
                }

                // Wait for completion signal (release) before processing next request
                // This ensures state is updated before next body evaluation
                if completion_rx.next().await.is_none() {
                    // Completion channel closed - shutdown
                    break;
                }
            }

            if LOG_DROPS_AND_LOOP_ENDS {
                zoon::println!("Loop ended: BackpressureCoordinator");
            }
        });

        Self {
            inner: Arc::new(BackpressureCoordinatorInner {
                request_tx,
                completion_tx,
                _actor_loop: actor_loop,
            }),
        }
    }

    /// Acquire a permit asynchronously.
    ///
    /// Blocks until:
    /// 1. Previous permit holder has released (if any)
    /// 2. This request is granted
    ///
    /// Called by THEN before each body evaluation.
    pub async fn acquire(&self) {
        let (grant_tx, grant_rx) = oneshot::channel();
        let request = BackpressureRequest { grant_tx };

        // Send request (blocks on bounded channel if previous request pending)
        if self.inner.request_tx.clone().send(request).await.is_err() {
            // Coordinator was dropped - this can happen during shutdown
            return;
        }

        // Wait for grant (if coordinator dropped, we just proceed - shutdown case)
        if grant_rx.await.is_err() {
            zoon::println!("[BACKPRESSURE] Grant channel closed during acquire");
        }
    }

    /// Release the permit, allowing the next acquire to proceed.
    ///
    /// This is synchronous (non-blocking) so it can be called from callbacks.
    /// Called by HOLD after updating state.
    pub fn release(&self) {
        match self.inner.completion_tx.clone().try_send(()) {
            Ok(()) => {}
            Err(e) if e.is_full() => {
                // This shouldn't happen - indicates double release or logic bug
                zoon::eprintln!(
                    "[BACKPRESSURE BUG] release() failed - completion channel full (double release?)"
                );
            }
            Err(_) => {
                // Channel disconnected - coordinator was dropped, OK during shutdown
            }
        }
    }
}

// Type alias for backwards compatibility during migration
pub type BackpressurePermit = BackpressureCoordinator;

// --- LazyValueActor ---

/// Request from subscriber to LazyValueActor for the next value.
struct LazyValueRequest {
    subscriber_id: usize,
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
    construct_info: Arc<ConstructInfoComplete>,
    /// Channel for subscribers to request the next value.
    /// Bounded(16) - demand-driven requests from subscribers.
    request_tx: NamedChannel<LazyValueRequest>,
    /// Counter for unique subscriber IDs
    subscriber_counter: std::sync::atomic::AtomicUsize,
    /// The actor's internal loop
    actor_loop: ActorLoop,
}

impl LazyValueActor {
    /// Create a new LazyValueActor from a stream.
    ///
    /// The stream will only be polled when subscribers request values.
    /// This is the key difference from regular ValueActor.
    pub fn new<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfoComplete,
        source_stream: S,
    ) -> Self {
        let construct_info = Arc::new(construct_info);
        let (request_tx, request_rx) = NamedChannel::new("lazy_value_actor.requests", 16);

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            Self::internal_loop(construct_info, source_stream, request_rx)
        });

        Self {
            construct_info,
            request_tx,
            subscriber_counter: std::sync::atomic::AtomicUsize::new(0),
            actor_loop,
        }
    }

    /// Create a new Arc<LazyValueActor>.
    pub fn new_arc<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        source_stream: S,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info.complete(ConstructType::LazyValueActor),
            source_stream,
        ))
    }

    /// The internal loop that handles demand-driven value delivery.
    ///
    /// This loop owns all state (buffer, cursors) - no locks needed.
    /// Values are only pulled from the source when a subscriber requests one.
    async fn internal_loop<S: Stream<Item = Value> + 'static>(
        construct_info: Arc<ConstructInfoComplete>,
        source_stream: S,
        mut request_rx: mpsc::Receiver<LazyValueRequest>,
    ) {
        let mut source = Box::pin(source_stream);
        let mut buffer: Vec<Value> = Vec::new();
        let mut cursors: HashMap<usize, usize> = HashMap::new();
        let mut source_exhausted = false;
        const CLEANUP_THRESHOLD: usize = 100;

        while let Some(request) = request_rx.next().await {
            let cursor = cursors.entry(request.subscriber_id).or_insert(0);

            let value = if *cursor < buffer.len() {
                // Return buffered value (replay for this subscriber)
                Some(buffer[*cursor].clone())
            } else if source_exhausted {
                // Source is exhausted, no more values
                None
            } else {
                // Poll source for next value (demand-driven pull!)
                match source.next().await {
                    Some(value) => {
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
                let min_cursor = *cursors.values().min().unwrap_or(&0);
                if min_cursor >= CLEANUP_THRESHOLD {
                    // Remove consumed values from buffer
                    buffer.drain(0..min_cursor);
                    // Adjust all cursors
                    for c in cursors.values_mut() {
                        *c -= min_cursor;
                    }
                }
            }

            // Send response to subscriber (subscriber may have dropped)
            if request.response_tx.send(value).is_err() {
                zoon::println!("[LAZY_ACTOR] Subscriber dropped before receiving value");
            }
        }

        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("LazyValueActor loop ended: {}", construct_info);
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
        let subscriber_id = self.subscriber_counter.fetch_add(1, Ordering::SeqCst);
        LazySubscription {
            actor: self,
            subscriber_id,
            pending_response: None,
        }
    }

    /// Get construct info for debugging.
    pub fn construct_info(&self) -> &ConstructInfoComplete {
        &self.construct_info
    }
}

impl Drop for LazyValueActor {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped LazyValueActor: {}", self.construct_info);
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
    /// Stores the pending response receiver when a request is in-flight.
    /// This prevents creating duplicate requests on repeated polls.
    pending_response: Option<oneshot::Receiver<Option<Value>>>,
}

impl Stream for LazySubscription {
    type Item = Value;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // We need to send a request and poll the response.
        // We store the pending receiver to avoid creating duplicate requests on repeated polls.
        let this = self.get_mut();

        // If we have a pending response, poll it first
        if let Some(ref mut response_rx) = this.pending_response {
            match Pin::new(response_rx).poll(cx) {
                Poll::Ready(Ok(value)) => {
                    this.pending_response = None; // Clear the pending response
                    return Poll::Ready(value);
                }
                Poll::Ready(Err(_)) => {
                    this.pending_response = None;
                    return Poll::Ready(None); // Channel closed
                }
                Poll::Pending => {
                    // Keep the pending response for the next poll
                    return Poll::Pending;
                }
            }
        }

        // No pending response, create a new request
        let (response_tx, response_rx) = oneshot::channel();
        let request = LazyValueRequest {
            subscriber_id: this.subscriber_id,
            response_tx,
        };

        // Try to send the request (non-blocking for poll context)
        if this.actor.request_tx.try_send(request).is_err() {
            // Actor dropped or channel full
            return Poll::Ready(None);
        }

        // Store the pending response and poll it
        this.pending_response = Some(response_rx);

        // Poll the newly created response receiver
        if let Some(ref mut response_rx) = this.pending_response {
            match Pin::new(response_rx).poll(cx) {
                Poll::Ready(Ok(value)) => {
                    this.pending_response = None;
                    Poll::Ready(value)
                }
                Poll::Ready(Err(_)) => {
                    this.pending_response = None;
                    Poll::Ready(None) // Channel closed
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            // Should never happen since we just set it
            Poll::Pending
        }
    }
}

// --- Subscription Notification ---
// Version notifications use bounded channels in the actor loop (no RefCell).
// See ActorMessage::Subscribe and ValueActor.notify_sender_sender

// --- ActorMessage ---

/// Messages that can be sent to an actor.
/// All actor communication happens via these typed messages.
pub enum ActorMessage {
    // === Value Updates ===
    /// New value from the input stream (used during migration forwarding)
    StreamValue(Value),

    // === Migration Protocol (Phase 3) ===
    /// Request to migrate state to a new actor
    MigrateTo {
        target: ActorHandle,
        /// Optional transform to apply to values during migration
        transform: Option<Box<dyn Fn(Value) -> Value + Send>>,
    },
    /// Batch of migrated data (for streaming large datasets)
    MigrationBatch {
        batch_id: u64,
        items: Vec<Value>,
        is_final: bool,
    },
    /// Acknowledgment of batch receipt (backpressure)
    BatchAck {
        batch_id: u64,
    },
    /// Migration complete, ready to receive new subscribers
    MigrationComplete,
    /// Redirect all subscribers to a new actor
    RedirectSubscribers {
        target: ActorHandle,
    },

    // === Lifecycle ===
    /// Graceful shutdown request
    Shutdown,
}

// --- MigrationState ---

/// State machine for actor migration.
/// Tracks the progress of streaming state to a new actor.
pub enum MigrationState {
    /// Normal operation - no migration in progress
    Normal,

    /// Sending state to new actor
    Migrating {
        target: ActorHandle,
        /// Optional transform to apply to values
        transform: Option<Box<dyn Fn(Value) -> Value + Send>>,
        /// IDs of batches that haven't been acknowledged yet
        pending_batches: HashSet<u64>,
        /// Values received during migration that need forwarding
        buffered_writes: Vec<Value>,
    },

    /// Receiving state from old actor
    Receiving {
        source: ActorHandle,
        /// Batches received, keyed by batch_id for ordering
        received_batches: BTreeMap<u64, Vec<Value>>,
    },

    /// Shutting down after migration complete
    ShuttingDown,
}

impl Default for MigrationState {
    fn default() -> Self {
        Self::Normal
    }
}

// --- Subscription Registration (Push-Based Model) ---

/// Fire-and-forget subscription setup.
/// The caller creates the channel and sends the sender to the actor.
/// No reply needed - the data channel IS the acknowledgment.
pub struct SubscriptionSetup {
    /// The sender half of the subscription channel (caller keeps receiver)
    pub sender: mpsc::Sender<Value>,
    /// Starting version - 0 means all history, current_version means future only
    pub starting_version: u64,
}

/// Request for getting the current stored value from a ValueActor.
pub struct StoredValueQuery {
    pub reply: oneshot::Sender<Option<Value>>,
}

// --- VirtualFilesystem ---

/// Request types for VirtualFilesystem actor
enum FsRequest {
    ReadText {
        path: String,
        reply: oneshot::Sender<Option<String>>,
    },
    WriteText {
        path: String,
        content: String,
    },
    Exists {
        path: String,
        reply: oneshot::Sender<bool>,
    },
    Delete {
        path: String,
        reply: oneshot::Sender<bool>,
    },
    ListDirectory {
        path: String,
        reply: oneshot::Sender<Vec<String>>,
    },
}

/// Actor-based virtual filesystem for module loading.
///
/// Uses actor model with channels instead of RefCell for thread safety
/// and portability to WebWorkers, HVM, and hardware.
///
/// - Writes are fire-and-forget (sync send, async processing)
/// - Reads are async (request/response via oneshot channel)
#[derive(Clone)]
pub struct VirtualFilesystem {
    /// Bounded(32) - filesystem operations.
    request_sender: NamedChannel<FsRequest>,
    // Note: ActorLoop is NOT cloned - only the sender.
    // The actor loop is stored separately and kept alive by the context owner.
    _actor_loop: Arc<ActorLoop>,
}

impl VirtualFilesystem {
    pub fn new() -> Self {
        Self::with_files(HashMap::new())
    }

    /// Create a VirtualFilesystem pre-populated with files
    pub fn with_files(initial_files: HashMap<String, String>) -> Self {
        let (tx, mut rx) = NamedChannel::new("virtual_filesystem.requests", 32);

        let actor_loop = ActorLoop::new(async move {
            let mut files: HashMap<String, String> = initial_files;

            while let Some(req) = rx.next().await {
                match req {
                    FsRequest::ReadText { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let result = files.get(&normalized).cloned();
                        if reply.send(result).is_err() {
                            zoon::println!("[VFS] ReadText reply receiver dropped for {}", path);
                        }
                    }
                    FsRequest::WriteText { path, content } => {
                        let normalized = Self::normalize_path(&path);
                        files.insert(normalized, content);
                    }
                    FsRequest::Exists { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let exists = files.contains_key(&normalized);
                        if reply.send(exists).is_err() {
                            zoon::println!("[VFS] Exists reply receiver dropped for {}", path);
                        }
                    }
                    FsRequest::Delete { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let was_present = files.remove(&normalized).is_some();
                        if reply.send(was_present).is_err() {
                            zoon::println!("[VFS] Delete reply receiver dropped for {}", path);
                        }
                    }
                    FsRequest::ListDirectory { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let prefix = if normalized.is_empty() || normalized == "/" {
                            String::new()
                        } else if normalized.ends_with('/') {
                            normalized.clone()
                        } else {
                            format!("{}/", normalized)
                        };

                        let mut entries: Vec<String> = files
                            .keys()
                            .filter_map(|file_path| {
                                if prefix.is_empty() {
                                    // Root directory - get first path component
                                    file_path.split('/').next().map(|s| s.to_string())
                                } else if file_path.starts_with(&prefix) {
                                    // Get the next path component after the prefix
                                    let remainder = &file_path[prefix.len()..];
                                    remainder.split('/').next().map(|s| s.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect();

                        // Remove duplicates and sort
                        entries.sort();
                        entries.dedup();
                        if reply.send(entries).is_err() {
                            zoon::println!("[VFS] ListDirectory reply receiver dropped for {}", path);
                        }
                    }
                }
            }
        });

        Self {
            request_sender: tx,
            _actor_loop: Arc::new(actor_loop),
        }
    }

    /// Read text content from a file (async)
    pub async fn read_text(&self, path: &str) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.request_sender.send(FsRequest::ReadText {
            path: path.to_string(),
            reply: tx,
        }).await {
            zoon::eprintln!("[VFS] Failed to send ReadText request for {}: {e}", path);
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Write text content to a file (fire-and-forget)
    ///
    /// This is synchronous because it just sends a message to the actor.
    /// The actual write happens asynchronously in the actor loop.
    pub fn write_text(&self, path: &str, content: String) {
        self.request_sender.send_or_drop(FsRequest::WriteText {
            path: path.to_string(),
            content,
        });
    }

    /// List entries in a directory (async)
    pub async fn list_directory(&self, path: &str) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.request_sender.send(FsRequest::ListDirectory {
            path: path.to_string(),
            reply: tx,
        }).await {
            zoon::eprintln!("[VFS] Failed to send ListDirectory request for {}: {e}", path);
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Check if a file exists (async)
    pub async fn exists(&self, path: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.request_sender.send(FsRequest::Exists {
            path: path.to_string(),
            reply: tx,
        }).await {
            zoon::eprintln!("[VFS] Failed to send Exists request for {}: {e}", path);
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Delete a file (async)
    pub async fn delete(&self, path: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.request_sender.send(FsRequest::Delete {
            path: path.to_string(),
            reply: tx,
        }).await {
            zoon::eprintln!("[VFS] Failed to send Delete request for {}: {e}", path);
            return false;
        }
        rx.await.unwrap_or(false)
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
/// Uses ActorLoop internally to encapsulate the async task.
pub struct ConstructStorage {
    /// Bounded(32) - state save operations (fire-and-forget).
    state_inserter_sender: NamedChannel<(parser::PersistenceId, serde_json::Value)>,
    /// Bounded(32) - state load queries.
    state_getter_sender: NamedChannel<(
        parser::PersistenceId,
        oneshot::Sender<Option<serde_json::Value>>,
    )>,
    actor_loop: ActorLoop,
}

// @TODO Replace LocalStorage with IndexedDB
// - https://crates.io/crates/indexed_db
// - https://developer.mozilla.org/en-US/docs/Web/API/IndexedDB_API
// - https://blog.openreplay.com/the-ultimate-guide-to-browser-side-storage/
impl ConstructStorage {
    pub fn new(states_local_storage_key: impl Into<Cow<'static, str>>) -> Self {
        let states_local_storage_key = states_local_storage_key.into();
        let (state_inserter_sender, mut state_inserter_receiver) = NamedChannel::new("construct_storage.inserter", 32);
        let (state_getter_sender, mut state_getter_receiver) = NamedChannel::new("construct_storage.getter", 32);
        Self {
            state_inserter_sender,
            state_getter_sender,
            actor_loop: ActorLoop::new(async move {
                let mut states: BTreeMap<String, serde_json::Value> = match local_storage().get::<BTreeMap<String, serde_json::Value>>(&states_local_storage_key) {
                    None => BTreeMap::new(),
                    Some(Ok(states)) => states,
                    Some(Err(error)) => panic!("Failed to deserialize states: {error:#}"),
                };
                let mut dirty = false;
                loop {
                    // C3: Coalesce writes - drain all pending inserts before flushing once
                    if dirty {
                        while let Ok(Some((persistence_id, json_value))) = state_inserter_receiver.try_next() {
                            let key = persistence_id.to_string();
                            states.insert(key, json_value);
                        }
                        inc_metric!(PERSISTENCE_WRITES);
                        if let Err(error) = local_storage().insert(&states_local_storage_key, &states) {
                            zoon::eprintln!("Failed to save states: {error:#}");
                        }
                        dirty = false;
                    }
                    select! {
                        (persistence_id, json_value) = state_inserter_receiver.select_next_some() => {
                            // @TODO remove `.to_string()` call when LocalStorage is replaced with IndexedDB (?)
                            let key = persistence_id.to_string();
                            states.insert(key, json_value);
                            dirty = true;
                        },
                        (persistence_id, state_sender) = state_getter_receiver.select_next_some() => {
                            // @TODO Cheaper cloning? Replace get with remove?
                            // Note: reads always see up-to-date in-memory state, even before flush
                            let key = persistence_id.to_string();
                            let state = states.get(&key).cloned();
                            if state_sender.send(state).is_err() {
                                zoon::eprintln!("Failed to send state from construct storage");
                            }
                        }
                    }
                }
            }),
        }
    }

    /// Save state to persistent storage (fire-and-forget).
    ///
    /// This is synchronous - the actor persists asynchronously.
    /// Uses send_or_drop() which logs in debug mode if channel is full.
    pub fn save_state<T: Serialize>(&self, persistence_id: parser::PersistenceId, state: &T) {
        let json_value = match serde_json::to_value(state) {
            Ok(json_value) => json_value,
            Err(error) => {
                zoon::eprintln!("Failed to serialize state: {error:#}");
                return;
            }
        };
        // Fire-and-forget - actor persists asynchronously
        self.state_inserter_sender.send_or_drop((persistence_id, json_value));
    }

    // @TODO is &self enough?
    pub async fn load_state<T: DeserializeOwned>(
        self: Arc<Self>,
        persistence_id: parser::PersistenceId,
    ) -> Option<T> {
        let (state_sender, state_receiver) = oneshot::channel::<Option<serde_json::Value>>();
        if self
            .state_getter_sender
            .send((persistence_id, state_sender))
            .await
            .is_err()
        {
            zoon::eprintln!("Failed to load state: channel closed")
        }
        let json_value = state_receiver
            .await
            .expect("Failed to get state from ConstructStorage")?;
        match serde_json::from_value(json_value) {
            Ok(state) => Some(state),
            Err(error) => {
                panic!("Failed to load state: {error:#}");
            }
        }
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
    ActorRef {
        persistence_id: u128,
        scope: String,
    },
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
        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
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
        Self { scope_id: Some(scope_id) }
    }

    /// Prevent destruction on drop (e.g., when transferring ownership).
    pub fn defuse(&mut self) {
        self.scope_id = None;
    }
}

impl Drop for ScopeDestroyGuard {
    fn drop(&mut self) {
        if let Some(scope_id) = self.scope_id {
            REGISTRY.with(|reg| {
                reg.borrow_mut().destroy_scope(scope_id);
            });
        }
    }
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
    pub parameters: HashMap<String, ActorHandle>,
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
    pub hold_state_update_callback: Option<Arc<dyn Fn(Value) + Send + Sync>>,
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
    pub object_locals: HashMap<parser::Span, ActorHandle>,
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
    /// Lamport timestamp at which this context was created.
    ///
    /// Used by THEN/WHEN to filter "stale" values that existed before subscription.
    /// When set, values with `lamport_time <= subscription_time` are filtered out
    /// in VariableOrArgumentReference, preventing replay of old events.
    ///
    /// - `None` = no filtering (streaming context, accept all values)
    /// - `Some(time)` = filter values with `lamport_time <= time`
    ///
    /// This implements "glitch freedom" from FRP theory: ensuring that a late
    /// subscriber doesn't receive events that happened before it subscribed.
    pub subscription_time: Option<u64>,
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
        self.registry_scope_id.expect("Bug: no registry scope - all actors should be created within a scope")
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
    pub fn with_persisting_child_scope(&self, scope_id: &str, storage_key: String) -> (Self, mpsc::Receiver<RecordedCall>) {
        let new_prefix = match &self.scope {
            parser::Scope::Root => scope_id.to_string(),
            parser::Scope::Nested(existing) => format!("{}:{}", existing, scope_id),
        };
        // Use static name for channel - the actual scope identity is in self.scope
        let (call_recorder, receiver) = NamedChannel::new("call_recorder", 64);
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
/// Uses ActorLoop internally to encapsulate the async task.
/// Impulse channels are bounded(1) - a single pending signal is sufficient.
pub struct ActorOutputValveSignal {
    impulse_sender_sender: NamedChannel<mpsc::Sender<()>>,
    actor_loop: ActorLoop,
}

impl ActorOutputValveSignal {
    pub fn new(impulse_stream: impl Stream<Item = ()> + 'static) -> Self {
        let (impulse_sender_sender, mut impulse_sender_receiver) =
            NamedChannel::new("output_valve.subscriptions", 32);
        Self {
            impulse_sender_sender,
            actor_loop: ActorLoop::new(async move {
                let mut impulse_stream = pin!(impulse_stream.fuse());
                let mut impulse_senders = Vec::<mpsc::Sender<()>>::new();
                loop {
                    select! {
                        impulse = impulse_stream.next() => {
                            if impulse.is_none() { break };
                            impulse_senders.retain_mut(|impulse_sender| {
                                // try_send for bounded(1) - drop if already signaled
                                impulse_sender.try_send(()).is_ok()
                            });
                        }
                        impulse_sender = impulse_sender_receiver.select_next_some() => {
                            impulse_senders.push(impulse_sender);
                        }
                    }
                }
            }),
        }
    }

    pub fn stream(&self) -> impl Stream<Item = ()> {
        let (impulse_sender, impulse_receiver) = mpsc::channel(1);
        if let Err(error) = self.impulse_sender_sender.try_send(impulse_sender) {
            zoon::eprintln!("Failed to subscribe to actor output valve signal: {error:#}");
        }
        impulse_receiver
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
    /// Holds the forwarding actor loop for referenced fields (fixes forward reference race).
    /// The ActorLoop must be kept alive to prevent the forwarding task from being cancelled.
    forwarding_loop: Option<ActorLoop>,
}

impl Variable {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
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
            forwarding_loop: None,
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        value_actor: ActorHandle,
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info,
            construct_context,
            name,
            value_actor,
            persistence_id,
            scope,
        ))
    }

    /// Create a new Arc<Variable> with a forwarding actor loop.
    /// The loop will be kept alive as long as the Variable exists.
    pub fn new_arc_with_forwarding_loop(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        value_actor: ActorHandle,
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
        forwarding_loop: ActorLoop,
    ) -> Arc<Self> {
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::Variable),
            persistence_id,
            scope,
            name: name.into(),
            value_actor,
            link_value_sender: None,
            forwarding_loop: Some(forwarding_loop),
        })
    }

    pub fn persistence_id(&self) -> parser::PersistenceId {
        self.persistence_id
    }

    pub fn scope(&self) -> &parser::Scope {
        &self.scope
    }

    pub fn new_link_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
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
        let actor_construct_info =
            ConstructInfo::new(actor_id.clone(), persistence, "Link variable value actor")
                .complete(ConstructType::ValueActor);
        let (link_value_sender, link_value_receiver) = NamedChannel::new("link.values", 128);
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
                zoon::println!("[LINK_ACTOR] Received from channel: {} for {}", value_desc, desc_for_closure);
            }
            value
        });

        if LOG_DEBUG {
            zoon::println!("[LINK_ACTOR] Created link_value_actor pid={} scope={:?} for {}",
                persistence_id, scope, variable_description_for_log);
        }

        let value_actor = create_actor_complete(
            actor_construct_info, actor_context, TypedStream::infinite(logged_receiver), persistence_id, scope_id,
        );
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            scope,
            name: name.into(),
            value_actor,
            link_value_sender: Some(link_value_sender),
            forwarding_loop: None,
        })
    }

    /// Create a new LINK variable with a forwarding actor for sibling field access.
    /// This is used when a LINK is referenced by another field in the same Object.
    /// The forwarding actor was pre-created and registered with ReferenceConnector,
    /// so sibling fields will find it. The forwarding_loop connects the LINK's internal
    /// value_actor to the forwarding_actor so events flow through correctly.
    ///
    /// Arguments:
    /// - `forwarding_actor`: The actor sibling fields will subscribe to (via ReferenceConnector)
    /// - `link_value_sender`: The sender for elements to send events to the LINK
    /// - `forwarding_loop`: Connects internal link_value_actor → forwarding_actor
    ///   (the link_value_actor is kept alive by forwarding_loop's subscription)
    pub fn new_link_arc_with_forwarding_loop(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
        forwarding_actor: ActorHandle,
        link_value_sender: NamedChannel<Value>,
        forwarding_loop: ActorLoop,
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
            // Keep forwarding loop alive - it connects link_value_actor to forwarding_actor
            forwarding_loop: Some(forwarding_loop),
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
                subscription.next().await.map(|value| (value, (subscription, variable)))
            }
        ).boxed_local()
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
                subscription.next().await.map(|value| (value, (subscription, variable)))
            }
        ).boxed_local()
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

impl Drop for Variable {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- VariableOrArgumentReference ---

pub struct VariableOrArgumentReference {}

impl VariableOrArgumentReference {
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        alias: static_expression::Alias,
        root_value_actor: impl Future<Output = ActorHandle> + 'static,
    ) -> ActorHandle {
        let construct_info = construct_info.complete(ConstructType::VariableOrArgumentReference);
        // Capture context flags before closures
        let use_snapshot = actor_context.is_snapshot_context;
        let subscription_time = actor_context.subscription_time;
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
        // For snapshot context (THEN/WHEN bodies), we get a single value.
        // For streaming context, we get continuous updates.
        let mut value_stream: LocalBoxStream<'static, Value> = if use_snapshot {
            // Snapshot: get current value once
            stream::once(async move {
                let actor = root_value_actor.await;
                actor.value().await
            })
            .filter_map(|v| future::ready(v.ok()))
            .boxed_local()
        } else {
            // Streaming: continuous updates
            stream::once(async move { root_value_actor.await })
                .then(move |actor| async move { actor.stream() })
                .flatten()
                .boxed_local()
        };
        // Collect parts to detect the last one
        let parts_vec: Vec<_> = alias_parts.into_iter().skip(skip_alias_parts).collect();
        let num_parts = parts_vec.len();

        // Log subscription_time for debugging
        if LOG_DEBUG {
            if let Some(sub_time) = subscription_time {
                zoon::println!("[VAR_REF] Creating path subscription with subscription_time={}", sub_time);
            }
        }

        for (idx, alias_part) in parts_vec.into_iter().enumerate() {
            let alias_part = alias_part.to_string();
            let _is_last = idx == num_parts - 1;
            let step_idx = idx;

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
            value_stream = switch_map(
                value_stream,
                move |value| {
                let alias_part = alias_part.clone();
                let alias_part_log = alias_part_for_log.clone();
                if LOG_DEBUG {
                    let value_type = match &value {
                        Value::Object(_, _) => "Object",
                        Value::TaggedObject(tagged, _) => tagged.tag(),
                        Value::Tag(tag, _) => tag.tag(),
                        _ => "Other",
                    };
                    zoon::println!("[VAR_REF] step {} outer received {} for field '{}'", step_idx, value_type, alias_part_log);
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
                            // Always use stream() - stale events are filtered by Lamport timestamps.
                            // This replaces the old hardcoded is_event_link pattern.
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), object, variable, subscription_time),
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
                                                    // Apply Lamport receive rule
                                                    lamport_receive(value.lamport_time());

                                                    // Filter stale values based on subscription_time
                                                    if let Some(time) = sub_time {
                                                        if value.happened_before(time) {
                                                            // Skip stale value, keep polling
                                                            if LOG_DEBUG {
                                                                let value_type = match &value {
                                                                    Value::Object(_, _) => "Object",
                                                                    Value::TaggedObject(tagged, _) => tagged.tag(),
                                                                    Value::Tag(tag, _) => tag.tag(),
                                                                    _ => "Other",
                                                                };
                                                                zoon::println!("[ALIAS_PATH] FILTERED stale {} (value.lamport={:?} < sub_time={})",
                                                                    value_type, value.lamport_time(), time);
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
                                                        zoon::println!("[VAR_REF] Emitting {} (lamport={:?}, sub_time={:?})",
                                                            emit_value_type, value.lamport_time(), sub_time);
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
                            // Always use stream() - stale events are filtered by Lamport timestamps.
                            // This replaces the old hardcoded is_event_link pattern.
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), tagged_object, variable, subscription_time),
                                move |(subscription_opt, actor_opt, tagged_object, variable, sub_time)| {
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
                                                    // Apply Lamport receive rule
                                                    lamport_receive(value.lamport_time());

                                                    // Filter stale values based on subscription_time
                                                    if let Some(time) = sub_time {
                                                        if value.happened_before(time) {
                                                            // Skip stale value, keep polling
                                                            continue;
                                                        }
                                                    }
                                                    // Fresh value - emit it
                                                    return Some((value, (Some(subscription), None, tagged_object, variable, sub_time)));
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
                    other => panic!(
                        "Failed to get Object or TaggedObject to create VariableOrArgumentReference: The Value has a different type {}",
                        other.construct_info()
                    ),
                }
            });
        }
        // Subscription-based streams are infinite (subscriptions never terminate first)
        let scope_id = actor_context.scope_id();
        create_actor_complete(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
            scope_id,
        )
    }
}

// --- ReferenceConnector ---

/// Actor for connecting references to actors by span.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct ReferenceConnector {
    referenceable_inserter_sender: NamedChannel<(parser::Span, ActorHandle)>,
    referenceable_getter_sender:
        NamedChannel<(parser::Span, oneshot::Sender<ActorHandle>)>,
    actor_loop: ActorLoop,
}

impl ReferenceConnector {
    pub fn new() -> Self {
        let (referenceable_inserter_sender, referenceable_inserter_receiver) =
            NamedChannel::new("reference_connector.inserter", 64);
        let (referenceable_getter_sender, referenceable_getter_receiver) =
            NamedChannel::new("reference_connector.getter", 64);
        Self {
            referenceable_inserter_sender,
            referenceable_getter_sender,
            actor_loop: ActorLoop::new(async move {
                let mut referenceables = HashMap::<parser::Span, ActorHandle>::new();
                let mut referenceable_senders =
                    HashMap::<parser::Span, Vec<oneshot::Sender<ActorHandle>>>::new();
                // Track whether channels are closed
                let mut inserter_closed = false;
                let mut getter_closed = false;
                // Fuse the receivers so they don't poll after returning None
                let mut referenceable_inserter_receiver = referenceable_inserter_receiver.fuse();
                let mut referenceable_getter_receiver = referenceable_getter_receiver.fuse();
                loop {
                    select! {
                        result = referenceable_inserter_receiver.next() => {
                            match result {
                                Some((span, actor)) => {
                                    if let Some(senders) = referenceable_senders.remove(&span) {
                                        for sender in senders {
                                            if sender.send(actor.clone()).is_err() {
                                                zoon::eprintln!("Failed to send referenceable actor from reference connector");
                                            }
                                        }
                                    }
                                    referenceables.insert(span, actor);
                                }
                                None => {
                                    inserter_closed = true;
                                    if getter_closed {
                                        // Both channels closed - exit loop to drop actors
                                        break;
                                    }
                                }
                            }
                        },
                        result = referenceable_getter_receiver.next() => {
                            match result {
                                Some((span, referenceable_sender)) => {
                                    if let Some(actor) = referenceables.get(&span) {
                                        if referenceable_sender.send(actor.clone()).is_err() {
                                            zoon::eprintln!("Failed to send referenceable actor from reference connector");
                                        }
                                    } else {
                                        referenceable_senders.entry(span).or_default().push(referenceable_sender);
                                    }
                                }
                                None => {
                                    getter_closed = true;
                                    if inserter_closed {
                                        // Both channels closed - exit loop to drop actors
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                // Explicitly drop the referenceables to ensure actors are cleaned up
                drop(referenceables);
                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("ReferenceConnector loop ended - all actors dropped");
                }
            }),
        }
    }

    pub fn register_referenceable(&self, span: parser::Span, actor: ActorHandle) {
        if let Err(error) = self
            .referenceable_inserter_sender
            .try_send((span, actor))
        {
            zoon::eprintln!("Failed to register referenceable: {error:#}")
        }
    }

    // @TODO is &self enough?
    pub async fn referenceable(self: Arc<Self>, span: parser::Span) -> ActorHandle {
        let (referenceable_sender, referenceable_receiver) = oneshot::channel();
        if let Err(error) = self
            .referenceable_getter_sender
            .try_send((span, referenceable_sender))
        {
            zoon::eprintln!("Failed to get referenceable: {error:#}")
        }
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
/// Uses ActorLoop internally to encapsulate the async task.
///
/// IMPORTANT: Uses ScopedSpan (span + scope) as the key to ensure LINK bindings
/// inside function calls (like new_todo() in List/map) get unique identities
/// per list item, not just per source position.
pub struct LinkConnector {
    link_inserter_sender: NamedChannel<(ScopedSpan, NamedChannel<Value>)>,
    link_getter_sender:
        NamedChannel<(ScopedSpan, oneshot::Sender<NamedChannel<Value>>)>,
    actor_loop: ActorLoop,
}

impl LinkConnector {
    pub fn new() -> Self {
        let (link_inserter_sender, link_inserter_receiver) =
            NamedChannel::new("link_connector.inserter", 64);
        let (link_getter_sender, link_getter_receiver) =
            NamedChannel::new("link_connector.getter", 64);
        Self {
            link_inserter_sender,
            link_getter_sender,
            actor_loop: ActorLoop::new(async move {
                let mut links = HashMap::<ScopedSpan, NamedChannel<Value>>::new();
                let mut link_senders =
                    HashMap::<ScopedSpan, Vec<oneshot::Sender<NamedChannel<Value>>>>::new();
                // Track whether channels are closed
                let mut inserter_closed = false;
                let mut getter_closed = false;
                // Fuse the receivers so they don't poll after returning None
                let mut link_inserter_receiver = link_inserter_receiver.fuse();
                let mut link_getter_receiver = link_getter_receiver.fuse();
                loop {
                    select! {
                        result = link_inserter_receiver.next() => {
                            match result {
                                Some((span, sender)) => {
                                    if let Some(senders) = link_senders.remove(&span) {
                                        for link_sender in senders {
                                            if link_sender.send(sender.clone()).is_err() {
                                                zoon::eprintln!("Failed to send link sender from link connector");
                                            }
                                        }
                                    }
                                    links.insert(span, sender);
                                }
                                None => {
                                    inserter_closed = true;
                                    if getter_closed {
                                        // Both channels closed - exit loop
                                        break;
                                    }
                                }
                            }
                        },
                        result = link_getter_receiver.next() => {
                            match result {
                                Some((span, link_sender)) => {
                                    if let Some(sender) = links.get(&span) {
                                        if link_sender.send(sender.clone()).is_err() {
                                            zoon::eprintln!("Failed to send link sender from link connector");
                                        }
                                    } else {
                                        link_senders.entry(span).or_default().push(link_sender);
                                    }
                                }
                                None => {
                                    getter_closed = true;
                                    if inserter_closed {
                                        // Both channels closed - exit loop
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                // Explicitly drop the links to ensure channels are cleaned up
                drop(links);
                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("LinkConnector loop ended - all links dropped");
                }
            }),
        }
    }

    /// Register a LINK variable's sender with its span and scope.
    /// The scope is critical for distinguishing LINK bindings at the same source position
    /// but in different contexts (e.g., different list items).
    pub fn register_link(&self, span: parser::Span, scope: parser::Scope, sender: NamedChannel<Value>) {
        let scoped_span = ScopedSpan::new(span, scope);
        if let Err(error) = self
            .link_inserter_sender
            .try_send((scoped_span, sender))
        {
            zoon::eprintln!("Failed to register link: {error:#}")
        }
    }

    /// Get a LINK variable's sender by its span and scope.
    pub async fn link_sender(self: Arc<Self>, span: parser::Span, scope: parser::Scope) -> NamedChannel<Value> {
        let scoped_span = ScopedSpan::new(span, scope);
        let (link_sender, link_receiver) = oneshot::channel();
        if let Err(error) = self
            .link_getter_sender
            .try_send((scoped_span, link_sender))
        {
            zoon::eprintln!("Failed to get link sender: {error:#}")
        }
        link_receiver
            .await
            .expect("Failed to get link sender from LinkConnector")
    }
}

// --- PassThroughConnector ---

/// Actor for connecting LINK pass-through actors across re-evaluations.
/// When `element |> LINK { alias }` re-evaluates, this ensures the same
/// pass-through ValueActor receives the new value instead of creating a new one.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct PassThroughConnector {
    /// Sender for registering new pass-throughs or forwarding values
    op_sender: NamedChannel<PassThroughOp>,
    /// Sender for getting existing pass-through actors
    getter_sender: NamedChannel<(PassThroughKey, oneshot::Sender<Option<ActorHandle>>)>,
    /// Sender for getting existing pass-through value senders
    sender_getter_sender: NamedChannel<(PassThroughKey, oneshot::Sender<Option<mpsc::Sender<Value>>>)>,
    actor_loop: ActorLoop,
}

/// Key for identifying pass-throughs: persistence_id + scope
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct PassThroughKey {
    pub persistence_id: parser::PersistenceId,
    pub scope: parser::Scope,
}

enum PassThroughOp {
    /// Register a new pass-through with its value sender
    Register {
        key: PassThroughKey,
        value_sender: mpsc::Sender<Value>,
        actor: ActorHandle,
    },
    /// Forward a value to an existing pass-through
    Forward {
        key: PassThroughKey,
        value: Value,
    },
    /// Add a forwarder to keep alive for an existing pass-through
    AddForwarder {
        key: PassThroughKey,
        forwarder: ActorHandle,
    },
}

impl PassThroughConnector {
    pub fn new() -> Self {
        let (op_sender, op_receiver) = NamedChannel::new("pass_through.ops", 32);
        let (getter_sender, getter_receiver) = NamedChannel::new("pass_through.getter", 32);
        let (sender_getter_sender, sender_getter_receiver) =
            NamedChannel::new("pass_through.sender_getter", 32);

        Self {
            op_sender,
            getter_sender,
            sender_getter_sender,
            actor_loop: ActorLoop::new(async move {
                // (sender, actor, forwarders) - forwarders kept alive for the lifetime of the pass-through
                let mut pass_throughs = HashMap::<PassThroughKey, (mpsc::Sender<Value>, ActorHandle, Vec<ActorHandle>)>::new();
                let mut op_receiver = op_receiver.fuse();
                let mut getter_receiver = getter_receiver.fuse();
                let mut sender_getter_receiver = sender_getter_receiver.fuse();
                let mut channels_closed = 0u8;

                loop {
                    select! {
                        result = op_receiver.next() => {
                            match result {
                                Some(PassThroughOp::Register { key, value_sender, actor }) => {
                                    pass_throughs.insert(key, (value_sender, actor, Vec::new()));
                                }
                                Some(PassThroughOp::Forward { key, value }) => {
                                    if let Some((sender, _, _)) = pass_throughs.get_mut(&key) {
                                        if let Err(e) = sender.send(value).await {
                                            zoon::println!("[PASS_THROUGH] Forward failed for key {:?}: {e}", key);
                                        }
                                    }
                                }
                                Some(PassThroughOp::AddForwarder { key, forwarder }) => {
                                    if let Some((_, _, forwarders)) = pass_throughs.get_mut(&key) {
                                        forwarders.push(forwarder);
                                    }
                                }
                                None => {
                                    channels_closed += 1;
                                    if channels_closed >= 3 { break; }
                                }
                            }
                        }
                        result = getter_receiver.next() => {
                            match result {
                                Some((key, response_sender)) => {
                                    let actor = pass_throughs.get(&key).map(|(_, actor, _)| actor.clone());
                                    if response_sender.send(actor).is_err() {
                                        zoon::println!("[PASS_THROUGH] Getter reply receiver dropped for key {:?}", key);
                                    }
                                }
                                None => {
                                    channels_closed += 1;
                                    if channels_closed >= 3 { break; }
                                }
                            }
                        }
                        result = sender_getter_receiver.next() => {
                            match result {
                                Some((key, response_sender)) => {
                                    let sender = pass_throughs.get(&key).map(|(sender, _, _)| sender.clone());
                                    if response_sender.send(sender).is_err() {
                                        zoon::println!("[PASS_THROUGH] Sender getter reply receiver dropped for key {:?}", key);
                                    }
                                }
                                None => {
                                    channels_closed += 1;
                                    if channels_closed >= 3 { break; }
                                }
                            }
                        }
                    }
                }

                drop(pass_throughs);
                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("PassThroughConnector loop ended");
                }
            }),
        }
    }

    /// Register a new pass-through actor
    pub fn register(&self, key: PassThroughKey, value_sender: mpsc::Sender<Value>, actor: ActorHandle) {
        if let Err(e) = self.op_sender.try_send(PassThroughOp::Register { key, value_sender, actor }) {
            zoon::eprintln!("[PASS_THROUGH] Failed to send Register: {e}");
        }
    }

    /// Forward a value to an existing pass-through
    pub fn forward(&self, key: PassThroughKey, value: Value) {
        if let Err(e) = self.op_sender.try_send(PassThroughOp::Forward { key, value }) {
            zoon::println!("[PASS_THROUGH] Failed to send Forward: {e}");
        }
    }

    /// Add a forwarder to keep alive for an existing pass-through
    pub fn add_forwarder(&self, key: PassThroughKey, forwarder: ActorHandle) {
        if let Err(e) = self.op_sender.try_send(PassThroughOp::AddForwarder { key, forwarder }) {
            zoon::eprintln!("[PASS_THROUGH] Failed to send AddForwarder: {e}");
        }
    }

    /// Get an existing pass-through actor if it exists
    pub async fn get(&self, key: PassThroughKey) -> Option<ActorHandle> {
        let (response_sender, response_receiver) = oneshot::channel();
        if let Err(e) = self.getter_sender.try_send((key, response_sender)) {
            zoon::eprintln!("[PASS_THROUGH] Failed to send getter request: {e}");
            return None;
        }
        response_receiver.await.ok().flatten()
    }

    /// Get the value sender for an existing pass-through
    pub async fn get_sender(&self, key: PassThroughKey) -> Option<mpsc::Sender<Value>> {
        let (response_sender, response_receiver) = oneshot::channel();
        if let Err(e) = self.sender_getter_sender.try_send((key, response_sender)) {
            zoon::eprintln!("[PASS_THROUGH] Failed to send sender getter request: {e}");
            return None;
        }
        response_receiver.await.ok().flatten()
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
                .map(|arg| arg.clone().stream().filter(|v| {
                    let is_flushed = v.is_flushed();
                    std::future::ready(is_flushed)
                }))
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
                construct_info,
                combined_stream,
                parser::PersistenceId::new(),
                actor_context.scope_id(),
            )
        } else {
            let scope_id = actor_context.scope_id();
            create_actor_complete(
                construct_info,
                actor_context,
                TypedStream::infinite(combined_stream),
                parser::PersistenceId::new(),
                scope_id,
            )
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
            input_idempotency_keys: BTreeMap<usize, ValueIdempotencyKey>,
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

        // Merge all input streams, then sort by Lamport timestamp to restore temporal ordering.
        // `stream::select_all` races between channels, but events that arrived at nearly
        // the same time (within the same poll) should be processed in Lamport order.
        let value_stream =
            stream::select_all(inputs.iter().enumerate().map(|(index, value_actor)| {
                // Use stream() to properly handle lazy actors in HOLD body context
                value_actor.clone().stream().map(move |value| (index, value))
            }))
            .ready_chunks(16)  // Buffer up to 16 concurrent values
            .flat_map(|mut chunk| {
                // Sort by Lamport timestamp to restore happened-before ordering
                chunk.sort_by_key(|(_, value)| value.lamport_time());
                stream::iter(chunk)
            })
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
            .scan(State::default(), move |state, (new_state, index, value)| {
                if let Some(new_state) = new_state {
                    *state = new_state;
                }
                let idempotency_key = value.idempotency_key();
                let skip_value = state.input_idempotency_keys.get(&index).is_some_and(
                    |previous_idempotency_key| *previous_idempotency_key == idempotency_key,
                );
                if !skip_value {
                    state.input_idempotency_keys.insert(index, idempotency_key);
                }
                // @TODO Refactor to get rid of the `clone` call. Use async closure?
                let state = state.clone();
                let storage = storage.clone();
                async move {
                    if skip_value {
                        Some(None)
                    } else {
                        storage.save_state(persistent_id, &state);
                        Some(Some(value))
                    }
                }
            })
            .filter_map(future::ready);

        // Subscription-based streams are infinite (subscriptions never terminate first)
        let scope_id = actor_context.scope_id();
        create_actor_complete(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
            scope_id,
        )
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
        operation: F,
    ) -> ActorHandle
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Value + 'static,
    {
        let construct_info = construct_info.complete(ConstructType::ValueActor);

        // Merge both operand streams, tracking which operand changed.
        // Use stream() to properly handle lazy actors in HOLD body context.
        // Sort by Lamport timestamp to restore happened-before ordering when both
        // operands have values ready at the same time.
        let a_stream = operand_a.stream().map(|v| (0usize, v));
        let b_stream = operand_b.stream().map(|v| (1usize, v));
        let value_stream = stream::select_all([a_stream.boxed_local(), b_stream.boxed_local()])
        .ready_chunks(4)  // Buffer concurrent values
        .flat_map(|mut chunk| {
            chunk.sort_by_key(|(_, value)| value.lamport_time());
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
        create_actor_complete(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
            scope_id,
        )
    }
}

// --- ComparatorCombinator ---

/// Helper for creating comparison combinators.
pub struct ComparatorCombinator {}

impl ComparatorCombinator {
    pub fn new_equal(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = values_equal(&a, &b);
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = !values_equal(&a, &b);
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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

/// Compare two Values for equality.
/// NOTE: Object/TaggedObject comparison currently compares by identity only (Arc pointer).
/// Deep comparison would require async access to variable values.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(n1, _), Value::Number(n2, _)) => n1.number() == n2.number(),
        (Value::Text(t1, _), Value::Text(t2, _)) => t1.text() == t2.text(),
        (Value::Tag(tag1, _), Value::Tag(tag2, _)) => tag1.tag() == tag2.tag(),
        // For objects, we can only do identity comparison without async
        (Value::TaggedObject(to1, _), Value::TaggedObject(to2, _)) => {
            Arc::ptr_eq(to1, to2)
        }
        (Value::Object(o1, _), Value::Object(o2, _)) => Arc::ptr_eq(o1, o2),
        _ => false, // Different types are not equal
    }
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: ActorHandle,
        operand_b: ActorHandle,
    ) -> ActorHandle {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
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
/// - If subscriber is too far behind (version not in buffer), falls back to snapshot
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
    /// Returns (values, oldest_available_version).
    /// If the requested version is too old (not in buffer), returns empty vec
    /// and the caller should fall back to snapshot.
    pub fn get_values_since(&self, since_version: u64) -> (Vec<Value>, Option<u64>) {
        let oldest = self.values.front().map(|(v, _)| *v);
        let values: Vec<Value> = self.values
            .iter()
            .filter(|(v, _)| *v > since_version)
            .map(|(_, val)| val.clone())
            .collect();
        (values, oldest)
    }

    /// Check if a version is available in the history.
    pub fn has_version(&self, version: u64) -> bool {
        self.values.front().map(|(v, _)| *v <= version).unwrap_or(false)
    }

    /// Get the latest value in the history.
    pub fn get_latest(&self) -> Option<Value> {
        self.values.back().map(|(_, v)| v.clone())
    }
}

// --- Scope-Based Generational Arena (Track D) ---
//
// Replaces Arc<ValueActor> with scope-based ownership.
// OwnedActor holds the heavy parts (ActorLoop, construct_info).
// ActorHandle is a lightweight clone-able reference (channel senders only).
// The registry owns all actors; scopes manage hierarchical lifetimes.

use std::cell::RefCell;

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
    actor_loop: ActorLoop,
    extra_loops: Vec<ActorLoop>,
    construct_info: Arc<ConstructInfoComplete>,
    scope_id: ScopeId,
    list_item_origin: Option<Arc<ListItemOrigin>>,
    /// Lazy delegate (kept alive by OwnedActor, referenced by ActorHandle for routing).
    lazy_delegate: Option<Arc<LazyValueActor>>,
    /// Keepalive copies of channel senders. The actor loop holds the receivers.
    /// Without these, when all ActorHandle clones are dropped (e.g., evaluation state cleanup),
    /// the channel senders are lost, causing receivers to close and the actor loop to exit.
    /// The registry owns the actor's lifetime, so these senders must live as long as the OwnedActor.
    _channel_keepalive: ChannelKeepalive,
}

/// Holds sender clones to keep actor channels alive as long as the OwnedActor exists in the registry.
/// When the OwnedActor is dropped (scope destruction), these are dropped too, closing the channels
/// and allowing the actor loop to exit naturally.
struct ChannelKeepalive {
    _subscription: NamedChannel<SubscriptionSetup>,
    _message: NamedChannel<ActorMessage>,
    _direct_store: NamedChannel<Value>,
    _query: NamedChannel<StoredValueQuery>,
}

impl Drop for OwnedActor {
    fn drop(&mut self) {
        inc_metric!(ACTORS_DROPPED);
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
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
            self.scopes[idx] = Slot::Occupied { generation: next_gen, value: scope };
            (free_idx, next_gen)
        } else {
            let idx = u32::try_from(self.scopes.len()).unwrap();
            self.scopes.push(Slot::Occupied { generation: 0, value: scope });
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

    /// Destroy a scope and all its actors and child scopes (recursive).
    pub fn destroy_scope(&mut self, scope_id: ScopeId) {
        let scope_idx = usize::try_from(scope_id.index).unwrap();
        let Some(scope) = self.get_scope(scope_id) else { return };

        // Collect children and actors before removing
        let children: Vec<ScopeId> = scope.children.clone();
        let actors: Vec<ActorId> = scope.actors.clone();
        if LOG_ACTOR_FLOW {
            zoon::println!("[FLOW] destroy_scope({:?}): {} children, {} actors: {:?}", scope_id, children.len(), actors.len(), actors);
        }

        // Free the scope slot with incremented generation
        let old_gen = scope_id.generation;
        self.scopes[scope_idx] = Slot::Free { next_generation: old_gen + 1 };
        self.scope_free_list.push(scope_id.index);

        // Destroy child scopes recursively
        for child_id in children {
            self.destroy_scope(child_id);
        }

        // Remove actors
        for actor_id in actors {
            self.remove_actor(actor_id);
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
            self.actors[idx] = Slot::Occupied { generation: next_gen, value: actor };
            (free_idx, next_gen)
        } else {
            let idx = u32::try_from(self.actors.len()).unwrap();
            self.actors.push(Slot::Occupied { generation: 0, value: actor });
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
        let actor_idx = usize::try_from(actor_id.index).unwrap();
        match &self.actors.get(actor_idx) {
            Some(Slot::Occupied { generation, value }) if *generation == actor_id.generation => {
                if LOG_ACTOR_FLOW {
                    zoon::println!("[FLOW] remove_actor({:?}): dropping {}", actor_id, value.construct_info);
                }
            }
            _ => return,
        }
        let old_gen = actor_id.generation;
        self.actors[actor_idx] = Slot::Free { next_generation: old_gen + 1 };
        self.actor_free_list.push(actor_id.index);
    }

    /// Get a reference to an owned actor.
    pub fn get_actor(&self, actor_id: ActorId) -> Option<&OwnedActor> {
        let actor_idx = usize::try_from(actor_id.index).ok()?;
        match self.actors.get(actor_idx)? {
            Slot::Occupied { generation, value } if *generation == actor_id.generation => Some(value),
            _ => None,
        }
    }

    /// Get a mutable reference to an owned actor.
    pub fn get_actor_mut(&mut self, actor_id: ActorId) -> Option<&mut OwnedActor> {
        let actor_idx = usize::try_from(actor_id.index).ok()?;
        match self.actors.get_mut(actor_idx)? {
            Slot::Occupied { generation, value } if *generation == actor_id.generation => Some(value),
            _ => None,
        }
    }

    /// Get a reference to a scope.
    fn get_scope(&self, scope_id: ScopeId) -> Option<&Scope> {
        let idx = usize::try_from(scope_id.index).ok()?;
        match self.scopes.get(idx)? {
            Slot::Occupied { generation, value } if *generation == scope_id.generation => Some(value),
            _ => None,
        }
    }

    /// Get a mutable reference to a scope.
    fn get_scope_mut(&mut self, scope_id: ScopeId) -> Option<&mut Scope> {
        let idx = usize::try_from(scope_id.index).ok()?;
        match self.scopes.get_mut(idx)? {
            Slot::Occupied { generation, value } if *generation == scope_id.generation => Some(value),
            _ => None,
        }
    }
}

// --- ActorHandle ---

/// Lightweight, clone-able handle for interacting with an actor.
///
/// Unlike `Arc<ValueActor>`, cloning an ActorHandle does NOT keep the actor alive.
/// The `ActorRegistry` owns the actor (via `OwnedActor`). The handle only holds
/// channel senders and metadata needed to send messages to the actor.
///
/// This separates "how to talk to an actor" (ActorHandle) from
/// "who owns the actor" (registry scope).
#[derive(Clone)]
pub struct ActorHandle {
    actor_id: ActorId,

    /// Channel for fire-and-forget subscription setup.
    subscription_sender: NamedChannel<SubscriptionSetup>,

    /// Channel for direct value storage (used by HOLD).
    direct_store_sender: NamedChannel<Value>,

    /// Channel for stored value queries.
    stored_value_query_sender: NamedChannel<StoredValueQuery>,

    /// Channel for actor messages (migration, shutdown).
    message_sender: NamedChannel<ActorMessage>,

    /// Signal that fires when actor has processed at least one value.
    ready_signal: Shared<oneshot::Receiver<()>>,

    /// Current version number - increments on each value change.
    current_version: Arc<AtomicU64>,

    /// Persistence ID for this actor.
    persistence_id: parser::PersistenceId,

    /// Optional lazy delegate for demand-driven evaluation.
    lazy_delegate: Option<Arc<LazyValueActor>>,

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
        self.current_version.load(Ordering::SeqCst)
    }

    pub fn has_lazy_delegate(&self) -> bool {
        self.lazy_delegate.is_some()
    }

    /// Directly store a value, bypassing the async input stream.
    pub fn store_value_directly(&self, value: Value) {
        self.direct_store_sender.send_or_drop(value);
    }

    /// Send a message to this actor.
    pub fn send_message(&self, msg: ActorMessage) -> Result<(), mpsc::TrySendError<ActorMessage>> {
        self.message_sender.try_send(msg)
    }

    /// Get the current stored value (async).
    pub async fn current_value(&self) -> Result<Value, CurrentValueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.stored_value_query_sender.send(StoredValueQuery { reply: reply_tx }).await.is_err() {
            return Err(CurrentValueError::ActorDropped);
        }
        match reply_rx.await {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(CurrentValueError::NoValueYet),
            Err(_) => Err(CurrentValueError::ActorDropped),
        }
    }

    /// Subscribe to continuous stream of all values from version 0.
    ///
    /// The registry scope owns the actor's lifetime.
    pub fn stream(&self) -> LocalBoxStream<'static, Value> {
        if let Some(ref lazy_delegate) = self.lazy_delegate {
            if LOG_ACTOR_FLOW { zoon::println!("[FLOW] stream() on {:?} → lazy delegate", self.actor_id); }
            return lazy_delegate.clone().stream().boxed_local();
        }

        let (tx, rx) = mpsc::channel(32);
        if LOG_ACTOR_FLOW { zoon::println!("[FLOW] stream() on {:?} → sending subscription (v0)", self.actor_id); }
        self.subscription_sender.send_or_drop(SubscriptionSetup {
            sender: tx,
            starting_version: 0,
        });

        rx.boxed_local()
    }

    /// Subscribe starting from current version - only future values.
    pub fn stream_from_now(&self) -> LocalBoxStream<'static, Value> {
        if let Some(ref lazy_delegate) = self.lazy_delegate {
            return lazy_delegate.clone().stream().boxed_local();
        }

        let current_version = self.version();
        let (tx, rx) = mpsc::channel(32);
        self.subscription_sender.send_or_drop(SubscriptionSetup {
            sender: tx,
            starting_version: current_version,
        });

        rx.boxed_local()
    }

    /// Get exactly ONE value - waiting if necessary.
    pub async fn value(&self) -> Result<Value, ValueError> {
        if self.version() > 0 {
            return self.current_value().await.map_err(|e| match e {
                CurrentValueError::NoValueYet => unreachable!("version > 0 implies value exists"),
                CurrentValueError::ActorDropped => ValueError::ActorDropped,
            });
        }
        let mut s = self.stream();
        s.next().await.ok_or(ValueError::ActorDropped)
    }

    /// Set list item origin (builder pattern).
    pub fn with_list_item_origin(mut self, origin: ListItemOrigin) -> Self {
        self.list_item_origin = Some(Arc::new(origin));
        self
    }

    /// Set extra loops (builder pattern).
    pub fn set_extra_loops_on_owned(&self, extra_loops: Vec<ActorLoop>) {
        REGISTRY.with(|reg| {
            let mut reg = reg.borrow_mut();
            let actor_idx = usize::try_from(self.actor_id.index).unwrap();
            if let Some(Slot::Occupied { generation, value }) = reg.actors.get_mut(actor_idx) {
                if *generation == self.actor_id.generation {
                    value.extra_loops = extra_loops;
                }
            }
        });
    }
}

/// Create an actor in the registry and return a handle to interact with it.
///
/// The `OwnedActor` (loop, construct_info) goes into the registry under `scope_id`.
/// The returned `ActorHandle` holds only channel senders — cloning it does NOT keep the actor alive.
pub fn create_actor<S: Stream<Item = Value> + 'static>(
    construct_info: ConstructInfo,
    actor_context: ActorContext,
    value_stream: TypedStream<S, Infinite>,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    let construct_info = Arc::new(construct_info.complete(ConstructType::ValueActor));
    create_actor_arc_info(construct_info, actor_context, value_stream, persistence_id, scope_id)
}

/// Create an actor from a pre-completed `ConstructInfoComplete`.
///
/// Use this in construct implementations that already have `ConstructInfoComplete`.
pub fn create_actor_complete<S: Stream<Item = Value> + 'static>(
    construct_info: ConstructInfoComplete,
    actor_context: ActorContext,
    value_stream: TypedStream<S, Infinite>,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    create_actor_arc_info(Arc::new(construct_info), actor_context, value_stream, persistence_id, scope_id)
}

fn create_actor_arc_info<S: Stream<Item = Value> + 'static>(
    construct_info: Arc<ConstructInfoComplete>,
    actor_context: ActorContext,
    value_stream: TypedStream<S, Infinite>,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    inc_metric!(ACTORS_CREATED);
    let (message_sender, message_receiver) = NamedChannel::new("value_actor.messages", 16);
    let current_version = Arc::new(AtomicU64::new(0));

    let (subscription_sender, subscription_receiver) = NamedChannel::new("value_actor.subscriptions", 32);
    let (direct_store_sender, direct_store_receiver) = NamedChannel::<Value>::new("value_actor.direct_store", 64);
    let (stored_value_query_sender, stored_value_query_receiver) = NamedChannel::new("value_actor.queries", 8);

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let ready_signal = ready_rx.shared();

    let boxed_stream: std::pin::Pin<Box<dyn Stream<Item = Value>>> =
        Box::pin(value_stream.inner);

    let actor_loop = ActorLoop::new({
        let construct_info = construct_info.clone();
        let current_version = current_version.clone();
        let output_valve_signal = actor_context.output_valve_signal;

        async move {
            if LOG_ACTOR_FLOW { zoon::println!("[FLOW] Actor loop STARTED: {construct_info}"); }
            let mut value_history = ValueHistory::new(64);
            let mut subscribers: Vec<mpsc::Sender<Value>> = Vec::new();
            let mut stream_ever_produced = false;
            let mut stream_ended = false;
            let mut ready_tx = Some(ready_tx);

            let mut output_valve_impulse_stream =
                if let Some(output_valve_signal) = &output_valve_signal {
                    output_valve_signal.stream().left_stream()
                } else {
                    stream::pending().right_stream()
                }
                .fuse();

            let mut value_stream = boxed_stream.fuse();
            let mut message_receiver = pin!(message_receiver.fuse());
            let mut subscription_receiver = pin!(subscription_receiver.fuse());
            let mut direct_store_receiver = pin!(direct_store_receiver.fuse());
            let mut stored_value_query_receiver = pin!(stored_value_query_receiver.fuse());
            let mut migration_state = MigrationState::Normal;

            loop {
                select! {
                    setup = subscription_receiver.next() => {
                        if let Some(SubscriptionSetup { mut sender, starting_version }) = setup {
                            if stream_ended && !stream_ever_produced {
                                if LOG_ACTOR_FLOW { zoon::println!("[FLOW] {construct_info}: subscription dropped (stream ended, never produced)"); }
                                drop(sender);
                            } else {
                                let (historical_values, _) = value_history.get_values_since(starting_version);
                                if LOG_ACTOR_FLOW { zoon::println!("[FLOW] {construct_info}: new subscriber (v{starting_version}), replaying {} historical values, stream_ended={stream_ended}", historical_values.len()); }
                                for value in historical_values {
                                    if sender.try_send(value.clone()).is_err() {
                                        if LOG_ACTOR_FLOW { zoon::println!("[FLOW] {construct_info}: historical value replay FAILED (channel full/closed)"); }
                                        break;
                                    }
                                }
                                subscribers.push(sender);
                            }
                        }
                    }

                    value = direct_store_receiver.next() => {
                        if let Some(value) = value {
                            if !stream_ever_produced {
                                stream_ever_produced = true;
                                if let Some(tx) = ready_tx.take() {
                                    tx.send(()).ok();
                                }
                            }
                            let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                            value_history.add(new_version, value.clone());
                            subscribers.retain_mut(|tx| {
                                match tx.try_send(value.clone()) {
                                    Ok(()) => true,
                                    Err(e) if e.is_disconnected() => false,
                                    Err(_) => true,
                                }
                            });
                        }
                    }

                    query = stored_value_query_receiver.next() => {
                        if let Some(StoredValueQuery { reply }) = query {
                            let current_value = value_history.get_latest();
                            if reply.send(current_value).is_err() {
                                zoon::println!("[VALUE_ACTOR] Stored value query reply receiver dropped");
                            }
                        }
                    }

                    msg = message_receiver.next() => {
                        let Some(msg) = msg else { break; };
                        match msg {
                            ActorMessage::StreamValue(value) => {
                                if !stream_ever_produced {
                                    stream_ever_produced = true;
                                    if let Some(tx) = ready_tx.take() {
                                        tx.send(()).ok();
                                    }
                                }
                                let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                value_history.add(new_version, value.clone());
                                subscribers.retain_mut(|tx| {
                                    match tx.try_send(value.clone()) {
                                        Ok(()) => true,
                                        Err(e) if e.is_disconnected() => false,
                                        Err(_) => true,
                                    }
                                });
                            }
                            ActorMessage::MigrateTo { target, transform } => {
                                migration_state = MigrationState::Migrating {
                                    target,
                                    transform,
                                    pending_batches: HashSet::new(),
                                    buffered_writes: Vec::new(),
                                };
                            }
                            ActorMessage::MigrationBatch { batch_id, items, is_final: _ } => {
                                if let MigrationState::Receiving { received_batches, source: _ } = &mut migration_state {
                                    received_batches.insert(batch_id, items);
                                }
                            }
                            ActorMessage::BatchAck { batch_id } => {
                                if let MigrationState::Migrating { pending_batches, .. } = &mut migration_state {
                                    pending_batches.remove(&batch_id);
                                }
                            }
                            ActorMessage::MigrationComplete => {
                                migration_state = MigrationState::Normal;
                            }
                            ActorMessage::RedirectSubscribers { target: _ } => {
                                migration_state = MigrationState::ShuttingDown;
                            }
                            ActorMessage::Shutdown => {
                                break;
                            }
                        }
                    }

                    new_value = value_stream.next() => {
                        let Some(new_value) = new_value else {
                            if LOG_ACTOR_FLOW { zoon::println!("[FLOW] {construct_info}: value_stream ENDED (stream_ever_produced={stream_ever_produced})"); }
                            stream_ended = true;
                            if !stream_ever_produced {
                                subscribers.clear();
                            }
                            continue;
                        };

                        if !stream_ever_produced {
                            stream_ever_produced = true;
                            if let Some(tx) = ready_tx.take() {
                                tx.send(()).ok();
                            }
                        }
                        let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                        if LOG_ACTOR_FLOW { zoon::println!("[FLOW] {construct_info}: value_stream produced v{new_version}, broadcasting to {} subscribers", subscribers.len()); }
                        value_history.add(new_version, new_value.clone());
                        subscribers.retain_mut(|tx| {
                            match tx.try_send(new_value.clone()) {
                                Ok(()) => true,
                                Err(e) if e.is_disconnected() => false,
                                Err(_) => true,
                            }
                        });

                        if let MigrationState::Migrating { buffered_writes, target, .. } = &mut migration_state {
                            buffered_writes.push(new_value.clone());
                            if let Err(e) = target.send_message(ActorMessage::StreamValue(new_value)) {
                                zoon::println!("[VALUE_ACTOR] Migration forward failed: {e}");
                            }
                        }
                    }

                    impulse = output_valve_impulse_stream.next() => {
                        if impulse.is_none() {
                            continue;
                        }
                    }
                }
            }

            if LOG_DROPS_AND_LOOP_ENDS {
                zoon::println!("Loop ended {construct_info}");
            }
        }
    });

    let owned_actor = OwnedActor {
        actor_loop,
        extra_loops: Vec::new(),
        construct_info: construct_info.clone(),
        scope_id,
        list_item_origin: None,
        lazy_delegate: None,
        _channel_keepalive: ChannelKeepalive {
            _subscription: subscription_sender.clone(),
            _message: message_sender.clone(),
            _direct_store: direct_store_sender.clone(),
            _query: stored_value_query_sender.clone(),
        },
    };

    let actor_id = REGISTRY.with(|reg| {
        reg.borrow_mut().insert_actor(scope_id, owned_actor)
    });

    ActorHandle {
        actor_id,
        subscription_sender,
        direct_store_sender,
        stored_value_query_sender,
        message_sender,
        ready_signal,
        current_version,
        persistence_id,
        lazy_delegate: None,
        list_item_origin: None,
    }
}

/// Create an actor with a pre-set initial value (version starts at 1, ready fires immediately).
pub fn create_actor_with_initial_value<S: Stream<Item = Value> + 'static>(
    construct_info: ConstructInfo,
    actor_context: ActorContext,
    value_stream: TypedStream<S, Infinite>,
    persistence_id: parser::PersistenceId,
    initial_value: Value,
    scope_id: ScopeId,
) -> ActorHandle {
    inc_metric!(ACTORS_CREATED);
    let value_stream = value_stream.inner;
    let construct_info = Arc::new(construct_info.complete(ConstructType::ValueActor));
    let (message_sender, message_receiver) = NamedChannel::new("value_actor.initial.messages", 16);
    let current_version = Arc::new(AtomicU64::new(1));

    let (subscription_sender, subscription_receiver) = NamedChannel::new("value_actor.initial.subscriptions", 32);
    let (direct_store_sender, direct_store_receiver) = NamedChannel::<Value>::new("value_actor.initial.direct_store", 64);
    let (stored_value_query_sender, stored_value_query_receiver) = NamedChannel::new("value_actor.initial.queries", 8);

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    ready_tx.send(()).ok();
    let ready_signal = ready_rx.shared();

    let actor_loop = ActorLoop::new({
        let construct_info = construct_info.clone();
        let current_version = current_version.clone();
        let output_valve_signal = actor_context.output_valve_signal;

        async move {
            let mut value_history = {
                let mut history = ValueHistory::new(64);
                history.add(1, initial_value);
                history
            };
            let mut subscribers: Vec<mpsc::Sender<Value>> = Vec::new();
            let mut stream_ever_produced = true;
            let mut stream_ended = false;

            let mut output_valve_impulse_stream =
                if let Some(output_valve_signal) = &output_valve_signal {
                    output_valve_signal.stream().left_stream()
                } else {
                    stream::pending().right_stream()
                }
                .fuse();
            let mut value_stream = pin!(value_stream.fuse());
            let mut message_receiver = pin!(message_receiver.fuse());
            let mut subscription_receiver = pin!(subscription_receiver.fuse());
            let mut direct_store_receiver = pin!(direct_store_receiver.fuse());
            let mut stored_value_query_receiver = pin!(stored_value_query_receiver.fuse());
            let migration_state = MigrationState::Normal;

            loop {
                select! {
                    setup = subscription_receiver.next() => {
                        if let Some(SubscriptionSetup { mut sender, starting_version }) = setup {
                            if stream_ended && !stream_ever_produced {
                                drop(sender);
                            } else {
                                for value in value_history.get_values_since(starting_version).0 {
                                    if sender.try_send(value.clone()).is_err() { break; }
                                }
                                subscribers.push(sender);
                            }
                        }
                    }

                    value = direct_store_receiver.next() => {
                        if let Some(value) = value {
                            stream_ever_produced = true;
                            let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                            value_history.add(new_version, value.clone());
                            subscribers.retain_mut(|tx| match tx.try_send(value.clone()) { Ok(()) => true, Err(e) if e.is_disconnected() => false, Err(_) => true });
                        }
                    }

                    query = stored_value_query_receiver.next() => {
                        if let Some(StoredValueQuery { reply }) = query {
                            let current_value = value_history.get_latest();
                            if reply.send(current_value).is_err() {
                                zoon::println!("[VALUE_ACTOR] Stored value query reply receiver dropped");
                            }
                        }
                    }

                    msg = message_receiver.next() => {
                        let Some(msg) = msg else { break; };
                        match msg {
                            ActorMessage::StreamValue(value) => {
                                stream_ever_produced = true;
                                let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                value_history.add(new_version, value.clone());
                                subscribers.retain_mut(|tx| match tx.try_send(value.clone()) { Ok(()) => true, Err(e) if e.is_disconnected() => false, Err(_) => true });
                            }
                            _ => {}
                        }
                    }

                    new_value = value_stream.next() => {
                        let Some(new_value) = new_value else {
                            stream_ended = true;
                            if !stream_ever_produced && let MigrationState::Normal = migration_state {
                                break;
                            }
                            continue;
                        };

                        stream_ever_produced = true;
                        let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                        value_history.add(new_version, new_value.clone());
                        subscribers.retain_mut(|tx| match tx.try_send(new_value.clone()) { Ok(()) => true, Err(e) if e.is_disconnected() => false, Err(_) => true });
                    }

                    impulse = output_valve_impulse_stream.next() => {
                        if impulse.is_none() { continue; }
                    }
                }
            }

            if LOG_DROPS_AND_LOOP_ENDS {
                zoon::println!("Loop ended {construct_info}");
            }
        }
    });

    let owned_actor = OwnedActor {
        actor_loop,
        extra_loops: Vec::new(),
        construct_info: construct_info.clone(),
        scope_id,
        list_item_origin: None,
        lazy_delegate: None,
        _channel_keepalive: ChannelKeepalive {
            _subscription: subscription_sender.clone(),
            _message: message_sender.clone(),
            _direct_store: direct_store_sender.clone(),
            _query: stored_value_query_sender.clone(),
        },
    };

    let actor_id = REGISTRY.with(|reg| {
        reg.borrow_mut().insert_actor(scope_id, owned_actor)
    });

    ActorHandle {
        actor_id,
        subscription_sender,
        direct_store_sender,
        stored_value_query_sender,
        message_sender,
        ready_signal,
        current_version,
        persistence_id,
        lazy_delegate: None,
        list_item_origin: None,
    }
}

/// Create an actor with a channel-based stream for forwarding values.
/// Returns the actor handle and a sender for pushing values.
pub fn create_actor_forwarding(
    construct_info: ConstructInfo,
    actor_context: ActorContext,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> (ActorHandle, NamedChannel<Value>) {
    let (sender, receiver) = NamedChannel::new("forwarding.values", 64);
    let handle = create_actor(
        construct_info,
        actor_context,
        TypedStream::infinite(receiver),
        persistence_id,
        scope_id,
    );
    (handle, sender)
}

/// Create an actor with list item origin metadata.
pub fn create_actor_with_origin<S: Stream<Item = Value> + 'static>(
    construct_info: ConstructInfo,
    actor_context: ActorContext,
    value_stream: TypedStream<S, Infinite>,
    persistence_id: parser::PersistenceId,
    origin: ListItemOrigin,
    scope_id: ScopeId,
) -> ActorHandle {
    let mut handle = create_actor(construct_info, actor_context, value_stream, persistence_id, scope_id);
    let origin = Arc::new(origin);
    handle.list_item_origin = Some(origin.clone());
    // Also set on OwnedActor in registry
    REGISTRY.with(|reg| {
        if let Some(owned) = reg.borrow_mut().get_actor_mut(handle.actor_id) {
            owned.list_item_origin = Some(origin);
        }
    });
    handle
}

/// Create an actor with list item origin metadata from a boxed stream.
pub fn create_actor_with_origin_boxed(
    construct_info: ConstructInfo,
    actor_context: ActorContext,
    value_stream: Pin<Box<dyn Stream<Item = Value> + 'static>>,
    persistence_id: parser::PersistenceId,
    origin: ListItemOrigin,
    scope_id: ScopeId,
) -> ActorHandle {
    create_actor_with_origin(
        construct_info,
        actor_context,
        TypedStream::infinite(value_stream),
        persistence_id,
        origin,
        scope_id,
    )
}

/// Create a lazy actor for demand-driven evaluation (used in HOLD body context).
pub fn create_actor_lazy<S: Stream<Item = Value> + 'static>(
    construct_info: ConstructInfoComplete,
    value_stream: S,
    persistence_id: parser::PersistenceId,
    scope_id: ScopeId,
) -> ActorHandle {
    inc_metric!(ACTORS_CREATED);
    let lazy_actor = Arc::new(LazyValueActor::new(
        construct_info.clone(),
        value_stream,
    ));

    let construct_info = Arc::new(construct_info);
    // Dummy channels (not used — lazy_delegate handles subscriptions)
    let (message_sender, _message_receiver) = NamedChannel::new("value_actor.lazy.messages", 16);
    let current_version = Arc::new(AtomicU64::new(0));
    let (subscription_sender, _subscription_receiver) = NamedChannel::<SubscriptionSetup>::new("value_actor.lazy.subscriptions", 32);
    let (direct_store_sender, _direct_store_receiver) = NamedChannel::new("value_actor.lazy.direct_store", 64);
    let (stored_value_query_sender, _stored_value_query_receiver) = NamedChannel::new("value_actor.lazy.queries", 8);
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    drop(ready_tx);
    let ready_signal = ready_rx.shared();

    let actor_loop = ActorLoop::new(async {});

    let owned_actor = OwnedActor {
        actor_loop,
        extra_loops: Vec::new(),
        construct_info: construct_info.clone(),
        scope_id,
        list_item_origin: None,
        lazy_delegate: Some(lazy_actor.clone()),
        _channel_keepalive: ChannelKeepalive {
            _subscription: subscription_sender.clone(),
            _message: message_sender.clone(),
            _direct_store: direct_store_sender.clone(),
            _query: stored_value_query_sender.clone(),
        },
    };

    let actor_id = REGISTRY.with(|reg| {
        reg.borrow_mut().insert_actor(scope_id, owned_actor)
    });

    ActorHandle {
        actor_id,
        subscription_sender,
        direct_store_sender,
        stored_value_query_sender,
        message_sender,
        ready_signal,
        current_version,
        persistence_id,
        lazy_delegate: Some(lazy_actor),
        list_item_origin: None,
    }
}

/// Connect a forwarding actor to its source actor.
///
/// This creates an ActorLoop that subscribes to the source actor and forwards
/// all values through the provided sender to the forwarding actor.
/// Optionally sends an initial value synchronously before starting the async forwarding.
///
/// # Arguments
/// - `forwarding_sender`: The sender from `create_actor_forwarding()` to send values to
/// - `source_actor`: The source actor to subscribe to
/// - `initial_value_future`: Future that resolves to an optional initial value to send before async forwarding
///
/// # Returns
/// An ActorLoop that must be kept alive for forwarding to continue.
///
/// # Usage Pattern
/// ```ignore
/// let (handle, sender) = create_actor_forwarding(...);
/// // Register handle immediately
/// // Later, when source_actor is available:
/// let actor_loop = connect_forwarding(sender, source_actor, initial_value);
/// // Store actor_loop to keep forwarding alive
/// ```
pub fn connect_forwarding(
    forwarding_sender: NamedChannel<Value>,
    source_actor: ActorHandle,
    initial_value_future: impl Future<Output = Option<Value>> + 'static,
) -> ActorLoop {
    ActorLoop::new(async move {
        if LOG_DEBUG { zoon::println!("[FWD2] connect_forwarding loop STARTED"); }
        // Send initial value first (awaiting if needed)
        if let Some(value) = initial_value_future.await {
            if LOG_DEBUG { zoon::println!("[FORWARDING] Sending initial value"); }
            if let Err(e) = forwarding_sender.send(value).await {
                if LOG_DEBUG { zoon::println!("[FORWARDING] Initial forwarding FAILED: {e} - EXITING!"); }
                return;
            }
            if LOG_DEBUG { zoon::println!("[FORWARDING] Initial value sent OK"); }
        } else {
            if LOG_DEBUG { zoon::println!("[FORWARDING] No initial value"); }
        }

        if LOG_DEBUG { zoon::println!("[FORWARDING] Subscribing to source_actor..."); }
        let mut subscription = source_actor.stream();
        if LOG_DEBUG { zoon::println!("[FORWARDING] Subscribed, entering forwarding loop"); }
        while let Some(value) = subscription.next().await {
            if LOG_DEBUG {
                let value_desc = match &value {
                    Value::Tag(tag, _) => format!("Tag({})", tag.tag()),
                    Value::Object(_, _) => "Object".to_string(),
                    _ => "Other".to_string(),
                };
                zoon::println!("[FORWARDING] Received value: {}", value_desc);
            }
            if forwarding_sender.send(value).await.is_err() {
                if LOG_DEBUG { zoon::println!("[FORWARDING] Forwarding FAILED - breaking loop"); }
                break;
            }
        }
        if LOG_DEBUG { zoon::println!("[FORWARDING] Forwarding loop ENDED"); }
    })
}

// --- ValueUpdate ---

/// What a subscriber receives when pulling updates.
/// Part of the push-pull architecture that prevents unbounded memory growth.
#[derive(Clone)]
pub enum ValueUpdate {
    /// No changes since last pull
    Current,
    /// Full current value (always for scalars, fallback for collections when gap too large)
    Snapshot(Value),
    /// Incremental diffs for collections (more efficient when subscriber is close to current)
    Diffs(Vec<Arc<ListDiff>>),
}

// --- ListDiffSubscription ---

/// Bounded-channel subscription for List that returns diffs.
///
/// Unlike ListSubscription which queues ListChange events,
/// this uses bounded(1) channels and pulls data on demand.
pub struct ListDiffSubscription {
    list: Arc<List>,
    last_seen_version: u64,
    notify_receiver: mpsc::Receiver<()>,
}

impl ListDiffSubscription {
    /// Wait for next update and return optimal data (diffs or snapshot).
    pub async fn next_update(&mut self) -> Option<ValueUpdate> {
        // Wait for version to change
        loop {
            let current = self.list.version();
            if current > self.last_seen_version {
                break;
            }
            // Wait for notification
            if self.notify_receiver.next().await.is_none() {
                return None;
            }
        }

        let update = self.list.get_update_since(self.last_seen_version).await;
        self.last_seen_version = self.list.version();
        Some(update)
    }

    /// Get current snapshot (async).
    pub async fn snapshot(&self) -> Vec<(ItemId, ActorHandle)> {
        self.list.snapshot().await
    }

    /// Check if there are pending updates without consuming them.
    pub fn has_pending(&self) -> bool {
        self.list.version() > self.last_seen_version
    }

    /// Get the list being subscribed to.
    pub fn list(&self) -> &Arc<List> {
        &self.list
    }

    /// Convert to a Stream using unfold (async-compatible).
    pub fn into_stream(self) -> impl Stream<Item = ValueUpdate> {
        stream::unfold(self, |mut subscription| async move {
            let update = subscription.next_update().await?;
            Some((update, subscription))
        })
    }
}

// Note: Direct Stream impl removed because get_update_since is now async.
// Use into_stream() or next_update() instead.

// --- ValueIdempotencyKey ---

pub type ValueIdempotencyKey = parser::PersistenceId;

// --- ValueMetadata ---

#[derive(Clone, Copy)]
pub struct ValueMetadata {
    pub idempotency_key: ValueIdempotencyKey,
    /// Lamport timestamp when this value was created.
    /// Used for happened-before ordering to filter stale events.
    pub lamport_time: u64,
}

impl ValueMetadata {
    /// Create new metadata with a fresh Lamport timestamp.
    pub fn new(idempotency_key: ValueIdempotencyKey) -> Self {
        Self {
            idempotency_key,
            lamport_time: lamport_tick(),
        }
    }

    /// Create metadata with a pre-captured Lamport timestamp from DOM callback.
    /// Used when the timestamp was captured earlier (e.g., in a DOM event handler)
    /// and needs to be preserved through async processing.
    pub fn with_lamport_time(idempotency_key: ValueIdempotencyKey, lamport_time: u64) -> Self {
        // Update local clock to maintain Lamport semantics:
        // our clock must be at least as high as any timestamp we've seen
        lamport_receive(lamport_time);
        Self {
            idempotency_key,
            lamport_time,
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

    /// Get the Lamport timestamp of when this value was created.
    pub fn lamport_time(&self) -> u64 {
        self.metadata().lamport_time
    }

    /// Returns true if this value happened-before the given timestamp.
    /// Such values are considered "stale" and should be filtered by late subscribers.
    ///
    /// The happened-before relation uses Lamport clock semantics:
    /// if value.lamport_time <= subscription_time, the value existed before
    /// the subscription was created and should not be replayed.
    pub fn happened_before(&self, timestamp: u64) -> bool {
        self.lamport_time() <= timestamp
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
            Value::Text(text, _) => {
                serde_json::Value::String(text.text().to_string())
            }
            Value::Tag(tag, _) => {
                let mut obj = serde_json::Map::new();
                obj.insert("_tag".to_string(), serde_json::Value::String(tag.tag().to_string()));
                serde_json::Value::Object(obj)
            }
            Value::Number(number, _) => {
                serde_json::json!(number.number())
            }
            Value::Object(object, _) => {
                let mut obj = serde_json::Map::new();
                for variable in object.variables() {
                    let value = variable.value_actor().current_value().await.ok();
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::TaggedObject(tagged_object, _) => {
                let mut obj = serde_json::Map::new();
                obj.insert("_tag".to_string(), serde_json::Value::String(tagged_object.tag().to_string()));
                for variable in tagged_object.variables() {
                    let value = variable.value_actor().current_value().await.ok();
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::List(list, _) => {
                let first_change = list.clone().stream().next().await;
                if let Some(ListChange::Replace { items }) = first_change {
                    let mut json_items = Vec::new();
                    for item in items.iter() {
                        let value = item.current_value().await.ok();
                        if let Some(value) = value {
                            let json_value = Box::pin(value.to_json()).await;
                            json_items.push(json_value);
                        }
                    }
                    serde_json::Value::Array(json_items)
                } else {
                    serde_json::Value::Array(Vec::new())
                }
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
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Text from JSON",
                );
                Text::new_value(construct_info, construct_context, idempotency_key, s.clone())
            }
            serde_json::Value::Number(n) => {
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Number from JSON",
                );
                let number = n.as_f64().unwrap_or(0.0);
                Number::new_value(construct_info, construct_context, idempotency_key, number)
            }
            serde_json::Value::Object(obj) => {
                if let Some(serde_json::Value::String(tag)) = obj.get("_tag") {
                    // TaggedObject or Tag
                    let other_fields: Vec<_> = obj.iter()
                        .filter(|(k, _)| *k != "_tag")
                        .collect();

                    if other_fields.is_empty() {
                        // Just a Tag
                        let construct_info = ConstructInfo::new(
                            construct_id,
                            None,
                            "Tag from JSON",
                        );
                        Tag::new_value(construct_info, construct_context, idempotency_key, tag.clone())
                    } else {
                        // TaggedObject
                        let construct_info = ConstructInfo::new(
                            construct_id.clone(),
                            None,
                            "TaggedObject from JSON",
                        );
                        let variables: Vec<Arc<Variable>> = other_fields.iter()
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
                                    construct_context.clone(),
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
                    let construct_info = ConstructInfo::new(
                        construct_id.clone(),
                        None,
                        "Object from JSON",
                    );
                    let variables: Vec<Arc<Variable>> = obj.iter()
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
                                construct_context.clone(),
                                name.clone(),
                                value_actor,
                                parser::PersistenceId::new(),
                                actor_context.scope.clone(),
                            )
                        })
                        .collect();
                    Object::new_value(construct_info, construct_context, idempotency_key, variables)
                }
            }
            serde_json::Value::Array(arr) => {
                let construct_info = ConstructInfo::new(
                    construct_id.clone(),
                    None,
                    "List from JSON",
                );
                let items: Vec<ActorHandle> = arr.iter()
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
                List::new_value(construct_info, construct_context, idempotency_key, actor_context, items)
            }
            serde_json::Value::Bool(b) => {
                // Represent booleans as tags
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Tag from JSON bool",
                );
                let tag = if *b { "True" } else { "False" };
                Tag::new_value(construct_info, construct_context, idempotency_key, tag)
            }
            serde_json::Value::Null => {
                // Represent null as a tag
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Tag from JSON null",
                );
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
    let actor_construct_info = ConstructInfo::new(
        construct_id.with_child_id("value_actor"),
        None,
        "ValueActor from JSON",
    ).complete(ConstructType::ValueActor);
    let scope_id = actor_context.scope_id();
    create_actor_complete(
        actor_construct_info,
        actor_context,
        constant(value),
        parser::PersistenceId::new(),
        scope_id,
    )
}

/// Saves a list of ValueActors to JSON for persistence.
/// Used by List persistence functions.
pub async fn save_list_items_to_json(items: &[ActorHandle]) -> Vec<serde_json::Value> {
    let mut json_items = Vec::new();
    for item in items {
        if let Ok(value) = item.current_value().await {
            json_items.push(value.to_json().await);
        }
    }
    json_items
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
    let scope_id = actor_context.scope_id();
    match value {
        Value::Object(object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in object.variables() {
                // Await the variable's current value (use value() to avoid subscription churn)
                let var_value = variable.value_actor().current_value().await.ok();
                if let Some(var_value) = var_value {
                    // Recursively materialize nested values
                    let materialized = Box::pin(materialize_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )).await;
                    // Create a constant ValueActor for the materialized value
                    let value_actor = create_actor_complete(
                        ConstructInfo::new(
                            format!("materialized_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ).complete(ConstructType::ValueActor),
                        actor_context.clone(),
                        constant(materialized),
                        parser::PersistenceId::new(),
                        scope_id,
                    );
                    // Create new Variable with the constant actor
                    let new_var = Variable::new_arc(
                        ConstructInfo::new(
                            format!("materialized_var_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ),
                        construct_context.clone(),
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
                let var_value = variable.value_actor().current_value().await.ok();
                if let Some(var_value) = var_value {
                    let materialized = Box::pin(materialize_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )).await;
                    let value_actor = create_actor_complete(
                        ConstructInfo::new(
                            format!("materialized_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ).complete(ConstructType::ValueActor),
                        actor_context.clone(),
                        constant(materialized),
                        parser::PersistenceId::new(),
                        scope_id,
                    );
                    let new_var = Variable::new_arc(
                        ConstructInfo::new(
                            format!("materialized_var_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ),
                        construct_context.clone(),
                        variable.name().to_string(),
                        value_actor,
                        parser::PersistenceId::new(),
                        actor_context.scope.clone(),
                    );
                    materialized_vars.push(new_var);
                }
            }
            let new_tagged_object = TaggedObject::new_arc(
                ConstructInfo::new("materialized_tagged_object", None, "Materialized tagged object"),
                construct_context,
                tagged_object.tag().to_string(),
                materialized_vars,
            );
            Value::TaggedObject(new_tagged_object, metadata)
        }
        Value::Flushed(inner, metadata) => {
            let materialized_inner = Box::pin(materialize_value(
                *inner,
                construct_context,
                actor_context,
            )).await;
            Value::Flushed(Box::new(materialized_inner), metadata)
        }
        // Scalars don't need materialization
        other => other,
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
        construct_context: ConstructContext,
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

    /// Create a Value with a pre-captured Lamport timestamp from DOM callback.
    pub fn new_value_with_lamport_time(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        lamport_time: u64,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Value {
        Value::Object(
            Self::new_arc(construct_info, construct_context, variables),
            ValueMetadata::with_lamport_time(idempotency_key, lamport_time),
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> TypedStream<impl Stream<Item = Value>, Infinite> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            variables,
        ))
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
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant object wrapper");

        // Create the initial value first
        let initial_value = Self::new_value(
            object_construct_info,
            construct_context,
            idempotency_key,
            variables.into(),
        );

        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());

        // Use create_actor_with_initial_value so the value is immediately available
        // This is critical for WASM single-threaded runtime where spawned tasks
        // don't run until we yield - subscriptions need immediate access to values.
        let scope_id = actor_context.scope_id();
        create_actor_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
            scope_id,
        )
    }

    pub fn variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .iter()
            .position(|variable| variable.name == name)
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

impl Drop for Object {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
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
        construct_context: ConstructContext,
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

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> TypedStream<impl Stream<Item = Value>, Infinite> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            tag,
            variables,
        ))
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
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Tagged object wrapper");

        // Create the initial value first
        let initial_value = Self::new_value(
            tagged_object_construct_info,
            construct_context,
            idempotency_key,
            tag.into(),
            variables.into(),
        );

        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());

        // Use create_actor_with_initial_value so the value is immediately available
        // This is critical for WASM single-threaded runtime where spawned tasks
        // don't run until we yield - subscriptions need immediate access to values.
        let scope_id = actor_context.scope_id();
        create_actor_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
            scope_id,
        )
    }

    pub fn variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .iter()
            .position(|variable| variable.name == name)
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

impl Drop for TaggedObject {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
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
        construct_context: ConstructContext,
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

    /// Create a Value with a pre-captured Lamport timestamp from DOM callback.
    pub fn new_value_with_lamport_time(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        lamport_time: u64,
        text: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Text(
            Self::new_arc(construct_info, construct_context, text),
            ValueMetadata::with_lamport_time(idempotency_key, lamport_time),
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        text: impl Into<Cow<'static, str>>,
    ) -> TypedStream<impl Stream<Item = Value>, Infinite> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            text,
        ))
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
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant text wrapper");
        // Create the initial value directly (avoid cloning ConstructInfo)
        let initial_value = Value::Text(
            Self::new_arc(construct_info, construct_context, text.clone()),
            ValueMetadata::new(idempotency_key),
        );
        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());
        // Use the new method that pre-sets initial value
        let scope_id = actor_context.scope_id();
        create_actor_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
            scope_id,
        )
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
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
        construct_context: ConstructContext,
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

    /// Create a Value with a pre-captured Lamport timestamp from DOM callback.
    pub fn new_value_with_lamport_time(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        lamport_time: u64,
        tag: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Tag(
            Self::new_arc(construct_info, construct_context, tag),
            ValueMetadata::with_lamport_time(idempotency_key, lamport_time),
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
    ) -> TypedStream<impl Stream<Item = Value>, Infinite> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            tag,
        ))
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
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant tag wrapper");
        // Create the initial value directly (avoid cloning ConstructInfo)
        let initial_value = Value::Tag(
            Self::new_arc(construct_info, construct_context, tag.clone()),
            ValueMetadata::new(idempotency_key),
        );
        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());
        // Use the new method that pre-sets initial value
        let scope_id = actor_context.scope_id();
        create_actor_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
            scope_id,
        )
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }
}

impl Drop for Tag {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
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
        construct_context: ConstructContext,
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

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        number: impl Into<f64>,
    ) -> TypedStream<impl Stream<Item = Value>, Infinite> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            number,
        ))
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
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant number wrapper)");
        // Create the initial value directly (avoid cloning ConstructInfo)
        let initial_value = Value::Number(
            Self::new_arc(construct_info, construct_context, number),
            ValueMetadata::new(idempotency_key),
        );
        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());
        // Use the new method that pre-sets initial value
        let scope_id = actor_context.scope_id();
        create_actor_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
            scope_id,
        )
    }

    pub fn number(&self) -> f64 {
        self.number
    }
}

impl Drop for Number {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- List ---

/// Query types for List's diff history actor
enum DiffHistoryQuery {
    GetUpdateSince {
        subscriber_version: u64,
        reply: oneshot::Sender<ValueUpdate>,
    },
    Snapshot {
        reply: oneshot::Sender<Vec<(ItemId, ActorHandle)>>,
    },
}

pub struct List {
    construct_info: Arc<ConstructInfoComplete>,
    actor_loop: ActorLoop,
    change_sender_sender: NamedChannel<NamedChannel<ListChange>>,
    /// Current version (increments on each change)
    current_version: Arc<AtomicU64>,
    /// Channel for registering diff subscribers (bounded channels)
    notify_sender_sender: NamedChannel<mpsc::Sender<()>>,
    /// Channel for querying diff history (actor-owned, no RefCell)
    diff_query_sender: NamedChannel<DiffHistoryQuery>,
    /// Optional persistence actor loop - keeps alive for list's lifetime
    persistence_loop: Option<ActorLoop>,
}

impl List {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        items: impl Into<Vec<ActorHandle>>,
    ) -> Self {
        let change_stream = constant(ListChange::Replace {
            items: Arc::from(items.into()),
        });
        Self::new_with_change_stream(construct_info, actor_context, change_stream, ())
    }

    pub fn new_with_change_stream<EOD: 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        change_stream: impl Stream<Item = ListChange> + 'static,
        extra_owned_data: EOD,
    ) -> Self {
        let construct_info = Arc::new(construct_info.complete(ConstructType::List));
        let (change_sender_sender, mut change_sender_receiver) =
            NamedChannel::<NamedChannel<ListChange>>::new("list.change_subscribers", 32);

        // Version tracking for push-pull architecture
        let current_version = Arc::new(AtomicU64::new(0));
        let current_version_for_loop = current_version.clone();

        // Channel for diff subscriber registration (bounded channels)
        let (notify_sender_sender, notify_sender_receiver) =
            NamedChannel::<mpsc::Sender<()>>::new("list.diff_subscribers", 32);

        // Channel for diff history queries (actor-owned, no RefCell)
        let (diff_query_sender, diff_query_receiver) =
            NamedChannel::<DiffHistoryQuery>::new("list.diff_queries", 16);

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            async move {
                // Diff history is owned by the actor loop - no RefCell needed
                let mut diff_history = DiffHistory::new(DiffHistoryConfig::default());

                let output_valve_signal = output_valve_signal;
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.stream().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                let mut change_stream = pin!(change_stream.fuse());
                let mut notify_sender_receiver = pin!(notify_sender_receiver.fuse());
                let mut diff_query_receiver = pin!(diff_query_receiver.fuse());
                let mut change_senders = Vec::<NamedChannel<ListChange>>::new();
                // Diff subscriber notification senders (bounded channels)
                let mut notify_senders: Vec<mpsc::Sender<()>> = Vec::new();
                let mut list: Option<Vec<ActorHandle>> = None;
                // Cached Arc slice for sending Replace without re-cloning the Vec each time.
                // Invalidated when `list` is mutated via `apply_to_vec`.
                let mut list_arc_cache: Option<Arc<[ActorHandle]>> = None;
                // Queue for subscribers that register before list is initialized
                let mut pending_subscribers: Vec<NamedChannel<ListChange>> = Vec::new();
                loop {
                    select! {
                        // Handle diff history queries
                        query = diff_query_receiver.next() => {
                            if let Some(query) = query {
                                match query {
                                    DiffHistoryQuery::GetUpdateSince { subscriber_version, reply } => {
                                        let update = diff_history.get_update_since(subscriber_version);
                                        if reply.send(update).is_err() {
                                            zoon::println!("[LIST] GetUpdateSince reply receiver dropped");
                                        }
                                    }
                                    DiffHistoryQuery::Snapshot { reply } => {
                                        let snapshot = diff_history.snapshot().to_vec();
                                        if reply.send(snapshot).is_err() {
                                            zoon::println!("[LIST] Snapshot reply receiver dropped");
                                        }
                                    }
                                }
                            }
                        }

                        // Handle new diff subscriber registrations
                        sender = notify_sender_receiver.next() => {
                            if let Some(mut sender) = sender {
                                // Immediately notify - subscriber will check version and see current diffs if any
                                // May fail if subscriber dropped immediately, which is fine
                                sender.try_send(()).ok();
                                notify_senders.push(sender);
                            }
                        }

                        change = change_stream.next() => {
                            let Some(change) = change else { break };
                            if output_valve_signal.is_none() {
                                // Send to all change subscribers, silently removing any that are gone.
                                // Subscribers being dropped is normal during WHILE arm switches.
                                // Keep senders that are just full (backpressure), remove disconnected.
                                change_senders.retain(|change_sender| {
                                    if let ListChange::Replace { ref items } = change {
                                        inc_metric!(REPLACE_FANOUT_SENDS);
                                        inc_metric!(REPLACE_PAYLOAD_TOTAL_ITEMS, items.len() as u64);
                                    }
                                    match change_sender.try_send(change.clone()) {
                                        Ok(()) => true,
                                        Err(e) => !e.is_disconnected(),
                                    }
                                });
                            }

                            // Convert to diff and add to history (before modifying list)
                            let diff = change.to_diff(diff_history.snapshot());
                            diff_history.add(diff);

                            if let Some(list) = &mut list {
                                change.clone().apply_to_vec(list);
                                list_arc_cache = None; // Invalidate cache on mutation
                            } else {
                                if let ListChange::Replace { items } = &change {
                                    list = Some(items.to_vec());
                                    list_arc_cache = Some(items.clone()); // Cache the Arc directly
                                    // Flush pending subscribers that registered before initialization
                                    for pending_sender in pending_subscribers.drain(..) {
                                        inc_metric!(REPLACE_PAYLOADS_SENT);
                                        let first_change_to_send = ListChange::Replace { items: items.clone() };
                                        match pending_sender.try_send(first_change_to_send) {
                                            Ok(()) => change_senders.push(pending_sender),
                                            Err(e) if !e.is_disconnected() => change_senders.push(pending_sender),
                                            Err(_) => {} // Disconnected, don't add
                                        }
                                    }
                                } else {
                                    panic!("Failed to initialize {construct_info}: The first change has to be 'ListChange::Replace'")
                                }
                            }
                            // Increment version
                            current_version_for_loop.fetch_add(1, Ordering::SeqCst);
                            // Notify diff subscribers via bounded channels
                            notify_senders.retain_mut(|sender| {
                                match sender.try_send(()) {
                                    Ok(()) => true,
                                    Err(e) => !e.is_disconnected(),
                                }
                            });
                        }
                        change_sender = change_sender_receiver.select_next_some() => {
                            if output_valve_signal.is_none() {
                                if let Some(list) = list.as_ref() {
                                    // Send initial state to new subscriber.
                                    // If receiver is already gone (race during WHILE switch), just skip.
                                    inc_metric!(REPLACE_PAYLOADS_SENT);
                                    let items_arc = list_arc_cache.get_or_insert_with(|| Arc::from(list.as_slice())).clone();
                                    let first_change_to_send = ListChange::Replace { items: items_arc };
                                    match change_sender.try_send(first_change_to_send) {
                                        Ok(()) => change_senders.push(change_sender),
                                        Err(e) if !e.is_disconnected() => change_senders.push(change_sender),
                                        Err(_) => {} // Disconnected, don't add
                                    }
                                } else {
                                    // List not initialized yet - queue subscriber to receive initial state later
                                    pending_subscribers.push(change_sender);
                                }
                            } else {
                                change_senders.push(change_sender);
                            }
                        }
                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() {
                                break
                            }
                            if let Some(list) = list.as_ref() {
                                // Send to all subscribers on impulse, silently removing dropped ones.
                                // Keep senders that are just full (backpressure), remove disconnected.
                                let items_arc = list_arc_cache.get_or_insert_with(|| Arc::from(list.as_slice())).clone();
                                change_senders.retain(|change_sender| {
                                    inc_metric!(REPLACE_FANOUT_SENDS);
                                    let change_to_send = ListChange::Replace { items: items_arc.clone() };
                                    match change_sender.try_send(change_to_send) {
                                        Ok(()) => true,
                                        Err(e) => !e.is_disconnected(),
                                    }
                                });
                            }
                        }
                    }
                }
                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("Loop ended {construct_info}");
                }
                drop(extra_owned_data);
            }
        });
        Self {
            construct_info,
            actor_loop,
            change_sender_sender,
            current_version,
            notify_sender_sender,
            diff_query_sender,
            persistence_loop: None,
        }
    }

    /// Set the persistence actor loop. The loop will be kept alive for the list's lifetime.
    pub fn set_persistence_loop(&mut self, loop_: ActorLoop) {
        self.persistence_loop = Some(loop_);
    }

    /// Get current version.
    pub fn version(&self) -> u64 {
        self.current_version.load(Ordering::SeqCst)
    }

    /// Get optimal update for subscriber at given version (async).
    /// Returns diffs if subscriber is close, snapshot if too far behind.
    pub async fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.diff_query_sender.try_send(DiffHistoryQuery::GetUpdateSince {
            subscriber_version,
            reply: tx,
        }) {
            zoon::println!("[LIST] Failed to send GetUpdateSince query: {e}");
            return ValueUpdate::Current;
        }
        rx.await.unwrap_or(ValueUpdate::Current)
    }

    /// Get current snapshot of items with their stable IDs (async).
    pub async fn snapshot(&self) -> Vec<(ItemId, ActorHandle)> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.diff_query_sender.try_send(DiffHistoryQuery::Snapshot {
            reply: tx,
        }) {
            zoon::println!("[LIST] Failed to send Snapshot query: {e}");
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Subscribe to list updates with diff support.
    ///
    /// Uses bounded(1) channels - pure dataflow, no RefCell.
    ///
    /// Takes ownership of the Arc to keep the list alive for the subscription lifetime.
    /// Callers should use `.clone().subscribe_diffs()` if they need to retain a reference.
    pub fn subscribe_diffs(self: Arc<Self>) -> ListDiffSubscription {
        // Create bounded(1) channel
        let (sender, receiver) = mpsc::channel::<()>(1);
        // Register with list loop
        if let Err(e) = self.notify_sender_sender.try_send(sender) {
            zoon::eprintln!("Failed to register diff subscriber: {e:#}");
        }
        ListDiffSubscription {
            last_seen_version: 0,
            notify_receiver: receiver,
            list: self,
        }
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

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<ActorHandle>>,
    ) -> TypedStream<impl Stream<Item = Value>, Infinite> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            actor_context,
            items,
        ))
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
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant list wrapper")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            actor_context.clone(),
            items.into(),
        );
        let scope_id = actor_context.scope_id();
        create_actor_complete(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            scope_id,
        )
    }

    /// Subscribe to this list's changes.
    ///
    /// The returned `ListSubscription` stream keeps the list alive for the
    /// duration of the subscription. When the stream is dropped, the
    /// list reference is released.
    ///
    /// Takes ownership of the Arc to keep the list alive for the subscription lifetime.
    /// Callers should use `.clone().stream()` if they need to retain a reference.
    pub fn stream(self: Arc<Self>) -> ListSubscription {
        // ListChange events use bounded channel with backpressure - keeps senders that are just full
        let (change_sender, change_receiver) = NamedChannel::new("list.changes", 64);
        if let Err(error) = self.change_sender_sender.try_send(change_sender) {
            zoon::eprintln!("Failed to subscribe to {}: {error:#}", self.construct_info);
        }
        ListSubscription {
            receiver: change_receiver,
            _list: self,
        }
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
            /// Restored from storage - need to skip first source Replace
            Restored {
                pid: parser::PersistenceId,
                change_stream: std::pin::Pin<Box<dyn Stream<Item = ListChange>>>,
                current_items: Vec<ActorHandle>,
                ctx: ConstructContext,
                actor_ctx: ActorContext,
                cid: ConstructId,
            },
            /// Running normally - forward and save changes
            Running {
                pid: parser::PersistenceId,
                change_stream: std::pin::Pin<Box<dyn Stream<Item = ListChange>>>,
                current_items: Vec<ActorHandle>,
                ctx: ConstructContext,
                actor_ctx: ActorContext,
                cid: ConstructId,
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
                        PersistState::Init { storage, pid, mut change_stream, ctx, actor_ctx, cid } => {
                            // Check for saved state
                            let loaded_items: Option<Vec<serde_json::Value>> = storage.clone().load_state(pid).await;

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

                                    let restored_change = ListChange::Replace { items: Arc::from(items.clone()) };
                                    return Some((
                                        restored_change,
                                        PersistState::Restored { pid, change_stream, current_items: items, ctx, actor_ctx, cid },
                                    ));
                                }
                            } else if loaded_items.is_some() && LOG_DEBUG {
                                zoon::println!("[DEBUG] Skipping JSON restoration for list with complex objects - use recorded-call restoration instead");
                            }

                            // No saved state OR skipped restoration - forward first change from source
                            if let Some(change) = change_stream.next().await {
                                let mut items = Vec::new();
                                change.clone().apply_to_vec(&mut items);

                                // Save immediately
                                let json_items = save_list_items_to_json(&items).await;
                                storage_for_save.save_state(pid, &json_items);

                                Some((
                                    change,
                                    PersistState::Running { pid, change_stream, current_items: items, ctx, actor_ctx, cid },
                                ))
                            } else {
                                None
                            }
                        }
                        PersistState::Restored { pid, mut change_stream, current_items, ctx, actor_ctx, cid } => {
                            // Need to skip the first Replace from source (their initial state)
                            if let Some(change) = change_stream.next().await {
                                if matches!(&change, ListChange::Replace { .. }) {
                                    // Skip source's Replace, emit our restored items instead
                                    Some((
                                        ListChange::Replace { items: Arc::from(current_items.clone()) },
                                        PersistState::Running { pid, change_stream, current_items, ctx, actor_ctx, cid },
                                    ))
                                } else {
                                    // Non-Replace change, process normally
                                    let mut items = current_items;
                                    change.clone().apply_to_vec(&mut items);

                                    let json_items = save_list_items_to_json(&items).await;
                                    storage_for_save.save_state(pid, &json_items);

                                    Some((
                                        change,
                                        PersistState::Running { pid, change_stream, current_items: items, ctx, actor_ctx, cid },
                                    ))
                                }
                            } else {
                                None
                            }
                        }
                        PersistState::Running { pid, mut change_stream, current_items, ctx, actor_ctx, cid } => {
                            // Forward changes and save
                            if let Some(change) = change_stream.next().await {
                                let mut items = current_items;
                                change.clone().apply_to_vec(&mut items);

                                let json_items = save_list_items_to_json(&items).await;
                                storage_for_save.save_state(pid, &json_items);

                                Some((
                                    change,
                                    PersistState::Running { pid, change_stream, current_items: items, ctx, actor_ctx, cid },
                                ))
                            } else {
                                None
                            }
                        }
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

        let ConstructInfo {
            id: actor_id,
            persistence: _,
            description: list_description,
        } = construct_info;

        // Create a stream that:
        // 1. First emits loaded items from storage (or code items if nothing saved)
        // 2. Then keeps the persistence loop alive
        let construct_context_for_load = construct_context.clone();
        let actor_context_for_load = actor_context.clone();
        let actor_id_for_load = actor_id.clone();

        // State for unfold: Some(init_data) means not yet initialized, None means already initialized
        enum InitState {
            NotInitialized {
                construct_storage: Arc<ConstructStorage>,
                persistence_id: parser::PersistenceId,
                code_items: Vec<ActorHandle>,
                actor_id: ConstructId,
                persistence_data: parser::Persistence,
                construct_context: ConstructContext,
                actor_context: ActorContext,
                idempotency_key: ValueIdempotencyKey,
            },
            Initialized {
                persistence_loop: ActorLoop,
            },
        }

        let value_stream = stream::unfold(
            InitState::NotInitialized {
                construct_storage,
                persistence_id,
                code_items,
                actor_id: actor_id_for_load,
                persistence_data,
                construct_context: construct_context_for_load,
                actor_context: actor_context_for_load,
                idempotency_key,
            },
            |state| async move {
                match state {
                    InitState::NotInitialized {
                        construct_storage,
                        persistence_id,
                        code_items,
                        actor_id,
                        persistence_data,
                        construct_context,
                        actor_context,
                        idempotency_key,
                    } => {
                        // Try to load from storage (clone the Arc since load_state takes ownership)
                        let loaded_items: Option<Vec<serde_json::Value>> = construct_storage
                            .clone()
                            .load_state(persistence_id)
                            .await;

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
                                            actor_id.with_child_id(format!("loaded_item_{i}")),
                                            construct_context.clone(),
                                            parser::PersistenceId::new(),
                                            actor_context.clone(),
                                        )
                                    }
                                })
                                .collect()
                        } else {
                            // Use code-defined items
                            code_items
                        };

                        // Create the inner list
                        let inner_construct_info = ConstructInfo::new(
                            actor_id.with_child_id("persistent_list"),
                            Some(persistence_data),
                            "Persistent List",
                        );
                        let list = List::new(
                            inner_construct_info,
                            construct_context.clone(),
                            actor_context.clone(),
                            initial_items,
                        );

                        // Create persistence actor loop to save changes
                        // Using ActorLoop instead of Task::start for pure actor model compliance
                        let list_arc = Arc::new(list);
                        let list_for_save = list_arc.clone();
                        let construct_storage_for_save = construct_storage;
                        let persistence_loop = ActorLoop::new(async move {
                            let mut change_stream = pin!(list_for_save.clone().stream());
                            while let Some(change) = change_stream.next().await {
                                // After any change, serialize and save the current list
                                if let ListChange::Replace { ref items } = change {
                                    let mut json_items = Vec::new();
                                    for item in items.iter() {
                                        // Use current_value() to get current value without subscription churn
                                        if let Ok(value) = item.current_value().await {
                                            json_items.push(value.to_json().await);
                                        }
                                    }
                                    construct_storage_for_save.save_state(persistence_id, &json_items);
                                } else {
                                    // For incremental changes, we need to get the full list and save it
                                    let mut list_stream = pin!(list_for_save.clone().stream());
                                    if let Some(ListChange::Replace { items }) = list_stream.next().await {
                                        let mut json_items = Vec::new();
                                        for item in items.iter() {
                                            // Use current_value() to get current value without subscription churn
                                            if let Ok(value) = item.current_value().await {
                                                json_items.push(value.to_json().await);
                                            }
                                        }
                                        construct_storage_for_save.save_state(persistence_id, &json_items);
                                    }
                                }
                            }
                        });

                        let value = Value::List(list_arc, ValueMetadata::new(idempotency_key));
                        Some((value, InitState::Initialized { persistence_loop }))
                    }
                    InitState::Initialized { persistence_loop } => {
                        // Keep persistence loop alive forever - never emit another value
                        let _keep_alive = persistence_loop;
                        future::pending::<Option<(Value, InitState)>>().await
                    }
                }
            }
        );

        let actor_construct_info = ConstructInfo::new(
            actor_id,
            Some(persistence_data),
            "Persistent list wrapper",
        ).complete(ConstructType::ValueActor);

        let scope_id = actor_context.scope_id();
        create_actor_complete(
            actor_construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
            scope_id,
        )
    }
}

impl Drop for List {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- ListSubscription ---

/// A subscription stream that keeps its source List alive.
/// The `_list` field holds an `Arc<List>` reference that is automatically
/// dropped when the subscription stream is dropped, ensuring proper lifecycle management.
pub struct ListSubscription {
    receiver: mpsc::Receiver<ListChange>,
    _list: Arc<List>,
}

impl Stream for ListSubscription {
    type Item = ListChange;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(cx)
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

/// Configuration for diff history ring buffer.
pub struct DiffHistoryConfig {
    /// Maximum number of diffs to keep before oldest are dropped
    pub max_entries: usize,
    /// When to prefer snapshot over diffs (if gap > threshold * current_len)
    pub snapshot_threshold: f64,
}

impl Default for DiffHistoryConfig {
    fn default() -> Self {
        Self {
            max_entries: 1500,
            snapshot_threshold: 0.5, // Snapshot if catching up > 50% of list
        }
    }
}

/// Ring buffer storing recent diffs for efficient subscriber updates.
/// Subscribers close to current version get diffs; those far behind get snapshots.
pub struct DiffHistory {
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
    pub fn new(config: DiffHistoryConfig) -> Self {
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
                    Some(after_id) => {
                        self.current_snapshot
                            .iter()
                            .position(|(id, _)| id == after_id)
                            .map(|i| i + 1)
                            .unwrap_or(self.current_snapshot.len())
                    }
                };
                self.current_snapshot.insert(pos, (*id, value.clone()));
            }
            ListDiff::Remove { id } => {
                self.current_snapshot.retain(|(item_id, _)| item_id != id);
            }
            ListDiff::Update { id, value } => {
                if let Some((_, v)) = self.current_snapshot.iter_mut().find(|(item_id, _)| item_id == id) {
                    *v = value.clone();
                }
            }
            ListDiff::Replace { items } => {
                self.current_snapshot = items.clone();
            }
        }

        // Store diff
        self.diffs.push_back((self.current_version, Arc::new(diff)));

        // Trim old diffs if over capacity
        while self.diffs.len() > self.config.max_entries {
            if let Some((version, _)) = self.diffs.pop_front() {
                self.oldest_version = version;
            }
        }
    }

    /// Get optimal update for subscriber at given version.
    pub fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        if subscriber_version >= self.current_version {
            return ValueUpdate::Current;
        }

        // If subscriber is too far behind or before our history, send snapshot
        if subscriber_version < self.oldest_version {
            return self.snapshot_update();
        }

        // Calculate how many diffs needed
        let diffs_needed: Vec<_> = self.diffs
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
            ValueUpdate::Current
        } else {
            ValueUpdate::Diffs(diffs_needed)
        }
    }

    fn snapshot_update(&self) -> ValueUpdate {
        // Return a Replace diff containing the full snapshot
        let items: Vec<_> = self.current_snapshot
            .iter()
            .map(|(id, actor)| (*id, actor.clone()))
            .collect();
        ValueUpdate::Diffs(vec![Arc::new(ListDiff::Replace { items })])
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
                    zoon::eprintln!("ListChange::InsertAt index {} out of bounds (len: {})", index, vec.len());
                }
            }
            Self::UpdateAt { index, item } => {
                if index < vec.len() {
                    vec[index] = item;
                } else {
                    zoon::eprintln!("ListChange::UpdateAt index {} out of bounds (len: {})", index, vec.len());
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
                    zoon::eprintln!("ListChange::Move old_index {} out of bounds (len: {})", old_index, vec.len());
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
                ListDiff::Replace { items: items_with_ids }
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
                let id = snapshot.get(*index).map(|(id, _)| *id).unwrap_or_else(ItemId::new);
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
                let item_id = snapshot.iter()
                    .find(|(_, actor)| actor.persistence_id() == *persistence_id)
                    .map(|(id, _)| *id)
                    .unwrap_or_else(ItemId::new);
                ListDiff::Remove { id: item_id }
            }
            Self::Move { old_index, new_index } => {
                // Move is Remove + Insert
                // For simplicity, we model it as Remove followed by Insert
                // The caller should handle this as two separate diffs if needed
                if let Some((id, value)) = snapshot.get(*old_index).map(|(id, v)| (*id, v.clone())) {
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
                    zoon::eprintln!("ListChange::Move old_index {} out of bounds in to_diff (snapshot len: {})", old_index, snapshot.len());
                    // Return a no-op by replacing with current snapshot
                    ListDiff::Replace {
                        items: snapshot.iter().map(|(id, v)| (*id, v.clone())).collect()
                    }
                }
            }
            Self::Pop => {
                let id = snapshot.last().map(|(id, _)| *id).unwrap_or_else(ItemId::new);
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

use crate::parser::static_expression::{Expression as StaticExpression, Spanned as StaticSpanned};
use crate::parser::StrSlice;

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
#[derive(Clone, Debug)]
pub enum SortKey {
    Number(f64),
    Text(String),
    Tag(String),
    /// Fallback for unsupported types - sorts last
    Unsupported,
}

impl SortKey {
    /// Extract a sortable key from a Value
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Number(num, _) => SortKey::Number(num.number()),
            Value::Text(text, _) => SortKey::Text(text.text().to_string()),
            Value::Tag(tag, _) => SortKey::Tag(tag.tag().to_string()),
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
                if a.is_nan() && b.is_nan() { true }
                else { a == b }
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

impl ListBindingFunction {
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
            ListBindingOperation::Map => {
                Self::create_map_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                )
            }
            ListBindingOperation::Retain => {
                Self::create_retain_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                )
            }
            ListBindingOperation::Remove => {
                Self::create_remove_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                    persistence_id,
                )
            }
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
            ListBindingOperation::SortBy => {
                Self::create_sort_by_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                )
            }
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
            source_list_actor.clone().stream()
            // Check if subscription scope is cancelled (e.g., WHILE arm switched)
            .take_while(move |_| {
                let is_active = subscription_scope.as_ref().map_or(true, |s| !s.is_cancelled());
                future::ready(is_active)
            })
            .filter_map(move |value| {
                future::ready(match value {
                    Value::List(list, _) => Some(list),
                    _ => None,
                })
            })
            // Deduplicate by list ID - only emit when we see a genuinely new list
            .scan(None, move |prev_id: &mut Option<ConstructId>, list| {
                let list_id = list.construct_info.id.clone();
                if prev_id.as_ref() == Some(&list_id) {
                    // Same list re-emitted, skip to avoid cancelling LINK connections
                    future::ready(Some(None))
                } else {
                    *prev_id = Some(list_id);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready),
            move |list| {
            use std::collections::HashMap;
            let config = config_for_stream.clone();
            let construct_context = construct_context_for_stream.clone();
            let actor_context = actor_context_for_stream.clone();

            // Track length, PersistenceId mapping, transform cache, AND item order.
            // The transform cache (B1 optimization) caches transformed actors by source PersistenceId
            // to avoid re-transforming unchanged items on Replace events.
            // Item order (Vec) is needed to clean cache on Pop (we need to know which item was last).
            type MapState = (
                usize,
                HashMap<parser::PersistenceId, parser::PersistenceId>,
                HashMap<parser::PersistenceId, ActorHandle>, // B1: Transform cache
                Vec<parser::PersistenceId>, // Item order for Pop handling
            );
            list.stream().scan((0usize, HashMap::new(), HashMap::new(), Vec::new()), move |state: &mut MapState, change| {
                let (length, pid_map, transform_cache, item_order) = state;
                let (transformed_change, new_length) = Self::transform_list_change_for_map_with_tracking(
                    change,
                    *length,
                    pid_map,
                    transform_cache,
                    item_order,
                    &config,
                    construct_context.clone(),
                    actor_context.clone(),
                );
                *length = new_length;
                future::ready(Some(transformed_change))
            })
        });

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
        create_actor_complete(
            construct_info,
            actor_context,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            )),
            parser::PersistenceId::new(),
            scope_id,
        )
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
            source_list_actor.clone().stream()
            .take_while(move |_| {
                let is_active = subscription_scope.as_ref().map_or(true, |s| !s.is_cancelled());
                future::ready(is_active)
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::List(list, _) => Some(list),
                    _ => None,
                })
            })
            // Deduplicate by list ID
            .scan(None, |prev_id: &mut Option<ConstructId>, list| {
                let list_id = list.construct_info.id.clone();
                if prev_id.as_ref() == Some(&list_id) {
                    future::ready(Some(None))
                } else {
                    *prev_id = Some(list_id);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready),
            move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();

            // Use stream::unfold with select for fine-grained control
            // Uses PersistenceId-based tracking to avoid index shift bugs on Remove/Pop

            // State: (items, predicates, predicate_results, list_stream, merged_predicate_stream)
            // Note: Using HashMap keyed by PersistenceId for predicates and results
            // to avoid index misalignment when items are removed
            type RetainState = (
                Vec<ActorHandle>,                    // items (order matters for output)
                HashMap<parser::PersistenceId, ActorHandle>,  // predicates by PersistenceId
                HashMap<parser::PersistenceId, bool>,    // predicate_results by PersistenceId
                Pin<Box<dyn Stream<Item = ListChange>>>, // list_stream
                Option<Pin<Box<dyn Stream<Item = (parser::PersistenceId, bool)>>>>, // merged predicates
            );

            let list_stream: Pin<Box<dyn Stream<Item = ListChange>>> = Box::pin(list.stream());

            stream::unfold(
                (
                    Vec::<ActorHandle>::new(),
                    HashMap::<parser::PersistenceId, ActorHandle>::new(),
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
                move |(mut items, mut predicates, mut predicate_results, mut list_stream, mut merged_predicates, mut last_emitted_pids, config, construct_context, actor_context)| {
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
                                                predicates.clear();
                                                predicate_results.clear();

                                                // Rebuild merged predicate stream
                                                if items.is_empty() {
                                                    merged_predicates = None;
                                                    last_emitted_pids.clear(); // A2: Clear for empty list
                                                    return Some((
                                                        Some(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) }),
                                                        (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                } else {
                                                    // Build predicates keyed by PersistenceId
                                                    for (idx, item) in new_items.iter().enumerate() {
                                                        let pid = item.persistence_id();
                                                        let pred = Self::transform_item(
                                                            item.clone(),
                                                            idx,
                                                            &config,
                                                            construct_context.clone(),
                                                            actor_context.clone(),
                                                        );
                                                        predicates.insert(pid.clone(), pred.clone());

                                                        // Query initial value (use .value() to wait for reactive predicates)
                                                        if let Ok(value) = pred.clone().value().await {
                                                            let is_true = matches!(&value, Value::Tag(tag, _) if tag.tag() == "True");
                                                            predicate_results.insert(pid, is_true);
                                                        }
                                                    }

                                                    // Use stream_from_now for future updates, keyed by PersistenceId
                                                    // B2: Add boolean deduplication to each predicate stream
                                                    let pred_streams: Vec<_> = predicates.iter()
                                                        .map(|(pid, pred)| {
                                                            let pid = pid.clone();
                                                            pred.clone().stream_from_now()
                                                                .map(move |v| {
                                                                    let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                                    (pid.clone(), is_true)
                                                                })
                                                                // B2: Deduplicate booleans - skip emission if same as previous
                                                                .scan(None::<bool>, |last_bool, (pid, is_true)| {
                                                                    if Some(is_true) == *last_bool {
                                                                        future::ready(Some(None)) // Skip duplicate
                                                                    } else {
                                                                        *last_bool = Some(is_true);
                                                                        future::ready(Some(Some((pid, is_true))))
                                                                    }
                                                                })
                                                                .filter_map(future::ready)
                                                        })
                                                        .collect();
                                                    // A3: Coalesce to batch all synchronously-available predicate updates
                                                    merged_predicates = Some(Box::pin(coalesce(stream::select_all(pred_streams))));

                                                    // Emit initial filtered result immediately
                                                    let filtered: Vec<_> = items.iter()
                                                        .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                                        .cloned()
                                                        .collect();

                                                    // A2: Update last_emitted_pids for future deduplication
                                                    last_emitted_pids = filtered.iter()
                                                        .map(|item| item.persistence_id())
                                                        .collect();

                                                    return Some((
                                                        Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                        (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                }
                                            }
                                            ListChange::Push { item } => {
                                                let idx = items.len();
                                                let pid = item.persistence_id();
                                                let pred = Self::transform_item(
                                                    item.clone(),
                                                    idx,
                                                    &config,
                                                    construct_context.clone(),
                                                    actor_context.clone(),
                                                );
                                                items.push(item);
                                                predicates.insert(pid.clone(), pred.clone());

                                                // Query new predicate value (use .value() to wait for reactive predicates)
                                                let is_true = if let Ok(value) = pred.clone().value().await {
                                                    matches!(&value, Value::Tag(tag, _) if tag.tag() == "True")
                                                } else {
                                                    false
                                                };
                                                predicate_results.insert(pid.clone(), is_true);

                                                // B3: Incremental merge - only add the new predicate stream, O(1) instead of O(N)
                                                let new_pid = pid.clone();
                                                let new_pred_stream: LocalBoxStream<'static, Vec<(parser::PersistenceId, bool)>> = Box::pin(coalesce(
                                                    pred.clone().stream_from_now()
                                                        .map(move |v| {
                                                            let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                            (new_pid.clone(), is_true)
                                                        })
                                                        .scan(None::<bool>, |last_bool, (pid, is_true)| {
                                                            if Some(is_true) == *last_bool {
                                                                future::ready(Some(None))
                                                            } else {
                                                                *last_bool = Some(is_true);
                                                                future::ready(Some(Some((pid, is_true))))
                                                            }
                                                        })
                                                        .filter_map(future::ready)
                                                ));

                                                merged_predicates = match merged_predicates.take() {
                                                    Some(existing) => Some(Box::pin(
                                                        stream::select(existing, new_pred_stream)
                                                    )),
                                                    None => Some(new_pred_stream),
                                                };

                                                // Emit updated filtered result
                                                let filtered: Vec<_> = items.iter()
                                                    .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                                    .cloned()
                                                    .collect();

                                                // A2: Output deduplication - skip if same as last emitted
                                                let current_pids: Vec<_> = filtered.iter()
                                                    .map(|item| item.persistence_id())
                                                    .collect();
                                                if current_pids == last_emitted_pids {
                                                    continue; // Skip redundant emission (pushed item was filtered out)
                                                }
                                                last_emitted_pids = current_pids;

                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                    (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                ));
                                            }
                                            ListChange::Remove { id } => {
                                                // Remove item and its predicate by PersistenceId
                                                items.retain(|item| item.persistence_id() != id);
                                                predicates.remove(&id);
                                                predicate_results.remove(&id);

                                                // DON'T rebuild streams - stale updates for removed PersistenceIds
                                                // will be automatically ignored (HashMap lookup returns None)

                                                if items.is_empty() {
                                                    merged_predicates = None;
                                                }

                                                // Emit filtered result
                                                let filtered: Vec<_> = items.iter()
                                                    .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                                    .cloned()
                                                    .collect();

                                                // A2: Output deduplication - skip if same as last emitted
                                                let current_pids: Vec<_> = filtered.iter()
                                                    .map(|item| item.persistence_id())
                                                    .collect();
                                                if current_pids == last_emitted_pids {
                                                    continue; // Skip redundant emission (removed item was filtered out)
                                                }
                                                last_emitted_pids = current_pids;

                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                    (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                ));
                                            }
                                            ListChange::Pop => {
                                                if let Some(popped_item) = items.pop() {
                                                    let pid = popped_item.persistence_id();
                                                    predicates.remove(&pid);
                                                    predicate_results.remove(&pid);

                                                    // DON'T rebuild streams - stale updates for removed PersistenceIds
                                                    // will be automatically ignored (HashMap lookup returns None)

                                                    if items.is_empty() {
                                                        merged_predicates = None;
                                                    }

                                                    // Emit filtered result
                                                    let filtered: Vec<_> = items.iter()
                                                        .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                                        .cloned()
                                                        .collect();

                                                    // A2: Output deduplication - skip if same as last emitted
                                                    let current_pids: Vec<_> = filtered.iter()
                                                        .map(|item| item.persistence_id())
                                                        .collect();
                                                    if current_pids == last_emitted_pids {
                                                        continue; // Skip redundant emission (popped item was filtered out)
                                                    }
                                                    last_emitted_pids = current_pids;

                                                    return Some((
                                                        Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                        (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                }
                                                continue;
                                            }
                                            ListChange::Clear => {
                                                items.clear();
                                                predicates.clear();
                                                predicate_results.clear();
                                                merged_predicates = None;
                                                last_emitted_pids.clear(); // A2: Clear for empty list
                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) }),
                                                    (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
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
                                        // A3: Process entire batch of predicate updates at once
                                        // C1: Track visibility changes for smart diffing
                                        let mut visibility_changes: Vec<(parser::PersistenceId, bool, bool)> = Vec::new(); // (pid, old_visible, new_visible)

                                        for (pid, is_true) in batch {
                                            // Stale updates for removed items will be ignored (contains_key returns false)
                                            if let Some(&old_visible) = predicate_results.get(&pid) {
                                                if old_visible != is_true {
                                                    visibility_changes.push((pid.clone(), old_visible, is_true));
                                                }
                                                predicate_results.insert(pid, is_true);
                                            }
                                        }

                                        if visibility_changes.is_empty() {
                                            continue; // No visibility changes
                                        }

                                        // C1: Smart diffing - emit InsertAt/Remove for small changes, Replace for large changes
                                        // Threshold: if more than 25% of items changed, use Replace (simpler for downstream)
                                        let use_smart_diffing = visibility_changes.len() <= items.len() / 4 + 1;

                                        if use_smart_diffing {
                                            let mut changes: Vec<ListChange> = Vec::with_capacity(visibility_changes.len());
                                            let mut ok = true;

                                            // Process removals first (items that became hidden), in reverse index order
                                            // to keep indices stable for subsequent removals
                                            let mut removals: Vec<_> = visibility_changes.iter()
                                                .filter(|(_, was_visible, is_visible)| *was_visible && !*is_visible)
                                                .collect();
                                            removals.sort_by(|a, b| {
                                                // Sort by position in last_emitted_pids, reverse order
                                                let pos_a = last_emitted_pids.iter().position(|p| *p == a.0);
                                                let pos_b = last_emitted_pids.iter().position(|p| *p == b.0);
                                                pos_b.cmp(&pos_a)
                                            });
                                            for (pid, _, _) in &removals {
                                                last_emitted_pids.retain(|p| p != pid);
                                                changes.push(ListChange::Remove { id: pid.clone() });
                                            }

                                            // Process insertions (items that became visible), in source order
                                            let insertions: Vec<_> = visibility_changes.iter()
                                                .filter(|(_, was_visible, is_visible)| !*was_visible && *is_visible)
                                                .collect();
                                            for (pid, _, _) in &insertions {
                                                if let Some((source_idx, item)) = items.iter().enumerate()
                                                    .find(|(_, item)| item.persistence_id() == *pid)
                                                {
                                                    // Compute filtered index against current last_emitted_pids state
                                                    let filtered_idx = items.iter().take(source_idx)
                                                        .filter(|i| predicate_results.get(&i.persistence_id()) == Some(&true))
                                                        .count();
                                                    last_emitted_pids.insert(filtered_idx, pid.clone());
                                                    changes.push(ListChange::InsertAt { index: filtered_idx, item: item.clone() });
                                                } else {
                                                    // Item not found - fall back to Replace
                                                    ok = false;
                                                    break;
                                                }
                                            }

                                            if ok && !changes.is_empty() {
                                                // Emit the first change, buffer the rest for subsequent iterations
                                                // Since the unfold returns one change at a time, we return the first
                                                // and re-process will pick up the state change. For multiple changes,
                                                // we still emit them as individual changes sequentially.
                                                if changes.len() == 1 {
                                                    return Some((
                                                        Some(changes.into_iter().next().expect("checked non-empty")),
                                                        (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                    ));
                                                }
                                                // Multiple changes: last_emitted_pids is already up to date.
                                                // Emit Replace with the current filtered state (equivalent to all the individual ops).
                                                let filtered: Vec<_> = items.iter()
                                                    .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                                    .cloned()
                                                    .collect();
                                                return Some((
                                                    Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                    (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                                ));
                                            }
                                            // ok == false: fall through to Replace below
                                            // Recompute last_emitted_pids since we may have partially updated it
                                        }

                                        // Fallback: emit Replace for many changes or complex cases
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
                                            (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                        ));
                                    }
                                    Either::Right((None, _)) => {
                                        // Predicate stream ended (shouldn't happen normally)
                                        merged_predicates = None;
                                        continue;
                                    }
                                }
                            } else {
                                // No predicate streams yet, wait for list change
                                match list_stream.next().await {
                                    Some(ListChange::Replace { items: new_items }) => {
                                        items = new_items.to_vec();
                                        predicates.clear();
                                        predicate_results.clear();

                                        if items.is_empty() {
                                            last_emitted_pids.clear(); // A2: Clear for empty list
                                            return Some((
                                                Some(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) }),
                                                (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                            ));
                                        } else {
                                            // Build predicates keyed by PersistenceId
                                            for (idx, item) in new_items.iter().enumerate() {
                                                let pid = item.persistence_id();
                                                let pred = Self::transform_item(
                                                    item.clone(),
                                                    idx,
                                                    &config,
                                                    construct_context.clone(),
                                                    actor_context.clone(),
                                                );
                                                predicates.insert(pid.clone(), pred.clone());

                                                // Query initial value (use .value() to wait for reactive predicates)
                                                if let Ok(value) = pred.clone().value().await {
                                                    let is_true = matches!(&value, Value::Tag(tag, _) if tag.tag() == "True");
                                                    predicate_results.insert(pid, is_true);
                                                }
                                            }

                                            // Use stream_from_now for future updates, keyed by PersistenceId
                                            // B2: Add boolean deduplication to each predicate stream
                                            let pred_streams: Vec<_> = predicates.iter()
                                                .map(|(pid, pred)| {
                                                    let pid = pid.clone();
                                                    pred.clone().stream_from_now()
                                                        .map(move |v| {
                                                            let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                            (pid.clone(), is_true)
                                                        })
                                                        // B2: Deduplicate booleans - skip emission if same as previous
                                                        .scan(None::<bool>, |last_bool, (pid, is_true)| {
                                                            if Some(is_true) == *last_bool {
                                                                future::ready(Some(None)) // Skip duplicate
                                                            } else {
                                                                *last_bool = Some(is_true);
                                                                future::ready(Some(Some((pid, is_true))))
                                                            }
                                                        })
                                                        .filter_map(future::ready)
                                                })
                                                .collect();
                                            // A3: Coalesce to batch all synchronously-available predicate updates
                                            merged_predicates = Some(Box::pin(coalesce(stream::select_all(pred_streams))));

                                            // Emit initial filtered result immediately
                                            let filtered: Vec<_> = items.iter()
                                                .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                                .cloned()
                                                .collect();

                                            // A2: Update last_emitted_pids for future deduplication
                                            last_emitted_pids = filtered.iter()
                                                .map(|item| item.persistence_id())
                                                .collect();

                                            return Some((
                                                Some(ListChange::Replace { items: Arc::from(filtered) }),
                                                (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
                                            ));
                                        }
                                    }
                                    Some(ListChange::Push { item }) => {
                                        // BUG FIX: Handle Push when merged_predicates is None
                                        // This happens after Clear completed empties the list,
                                        // then a new todo is added
                                        let idx = items.len();
                                        let pid = item.persistence_id();
                                        let pred = Self::transform_item(
                                            item.clone(),
                                            idx,
                                            &config,
                                            construct_context.clone(),
                                            actor_context.clone(),
                                        );
                                        items.push(item);
                                        predicates.insert(pid.clone(), pred.clone());

                                        // Query initial predicate value
                                        let is_true = if let Ok(value) = pred.clone().value().await {
                                            matches!(&value, Value::Tag(tag, _) if tag.tag() == "True")
                                        } else {
                                            false
                                        };
                                        predicate_results.insert(pid.clone(), is_true);

                                        // Build merged predicate stream (now we have items)
                                        let pred_streams: Vec<_> = predicates.iter()
                                            .map(|(pid, pred)| {
                                                let pid = pid.clone();
                                                pred.clone().stream_from_now()
                                                    .map(move |v| {
                                                        let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                        (pid.clone(), is_true)
                                                    })
                                                    // B2: Deduplicate booleans
                                                    .scan(None::<bool>, |last_bool, (pid, is_true)| {
                                                        if Some(is_true) == *last_bool {
                                                            future::ready(Some(None))
                                                        } else {
                                                            *last_bool = Some(is_true);
                                                            future::ready(Some(Some((pid, is_true))))
                                                        }
                                                    })
                                                    .filter_map(future::ready)
                                            })
                                            .collect();
                                        merged_predicates = Some(Box::pin(coalesce(stream::select_all(pred_streams))));

                                        // Emit filtered result
                                        let filtered: Vec<_> = items.iter()
                                            .filter(|item| predicate_results.get(&item.persistence_id()) == Some(&true))
                                            .cloned()
                                            .collect();

                                        let current_pids: Vec<_> = filtered.iter()
                                            .map(|item| item.persistence_id())
                                            .collect();
                                        if current_pids == last_emitted_pids {
                                            continue; // Skip redundant emission
                                        }
                                        last_emitted_pids = current_pids;

                                        return Some((
                                            Some(ListChange::Replace { items: Arc::from(filtered) }),
                                            (items, predicates, predicate_results, list_stream, merged_predicates, last_emitted_pids, config, construct_context, actor_context)
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
        });

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
        create_actor_complete(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            )),
            parser::PersistenceId::new(),
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
        let removed_set_key: Option<String> = persistence_id.as_ref().map(|pid| {
            format!("list_removed:{}", pid)
        });

        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();
        let construct_context_for_persistence = construct_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling
        let value_stream = switch_map(
            source_list_actor.clone().stream()
            // Check if subscription scope is cancelled (e.g., WHILE arm switched)
            .take_while(move |_| {
                let is_active = subscription_scope.as_ref().map_or(true, |s| !s.is_cancelled());
                future::ready(is_active)
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::List(list, _) => Some(list),
                    _ => None,
                })
            })
            // Deduplicate by list ID
            .scan(None, |prev_id: &mut Option<ConstructId>, list| {
                let list_id = list.construct_info.id.clone();
                if prev_id.as_ref() == Some(&list_id) {
                    future::ready(Some(None))
                } else {
                    *prev_id = Some(list_id);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready),
            move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let removed_set_key = removed_set_key.clone();

            // Load persisted removed set for restoration (call_ids that were previously removed)
            let persisted_removed: HashSet<String> = removed_set_key.as_ref()
                .map(|key| load_removed_set(key).into_iter().collect())
                .unwrap_or_default();

            // Create channel for removal events (PersistenceId of item to remove)
            let (remove_tx, remove_rx) = mpsc::channel::<parser::PersistenceId>(64);

            // Event type for merged streams
            enum RemoveEvent {
                ListChange(ListChange),
                RemoveItem(parser::PersistenceId),
            }

            // Create list change stream
            let list_changes = list.clone().stream().map(RemoveEvent::ListChange);

            // Create removal event stream
            let removal_events = remove_rx.map(RemoveEvent::RemoveItem);

            // State: track items and which PersistenceIds have been removed by THIS List/remove
            // removed_persistence_ids is bounded: items are removed from this set when they're
            // removed from upstream (no longer in Replace payload or via Remove { id }).
            type ItemEntry = (usize, ActorHandle, ActorHandle);
            // (internal_idx, item, when_actor) — no TaskHandle needed with stream fan-in

            /// B6: Create a trigger stream for a single when_actor.
            /// Emits the persistence_id once when the when_actor produces a value, then ends.
            fn make_trigger_stream(
                when_actor: ActorHandle,
                persistence_id: parser::PersistenceId,
            ) -> LocalBoxStream<'static, parser::PersistenceId> {
                when_actor.stream_from_now()
                    .map(move |_| persistence_id.clone())
                    .take(1)
                    .boxed_local()
            }

            // Merge list changes and removal events
            stream::select(list_changes, removal_events).scan(
                (
                    Vec::<ItemEntry>::new(),
                    HashSet::<parser::PersistenceId>::new(), // removed_persistence_ids
                    remove_tx,
                    config.clone(),
                    construct_context.clone(),
                    actor_context.clone(),
                    0usize, // next_idx for assigning unique internal IDs
                    removed_set_key.clone(), // storage key for this branch's removed set
                    persisted_removed, // call_ids from storage for restoration filtering
                    // B6: Task handles for trigger stream drivers.
                    // Replace creates one merged task; Push adds individual tasks.
                    // All cleared on next Replace.
                    Vec::<TaskHandle>::new(),
                ),
                move |state, event| {
                    let (items, removed_pids, remove_tx, config, construct_context, actor_context, next_idx, removed_set_key, persisted_removed, trigger_tasks) = state;

                    match event {
                        RemoveEvent::ListChange(change) => {
                            match change {
                                ListChange::Replace { items: new_items } => {
                                    // Filter out items we've already removed (by PersistenceId)
                                    // and rebuild tracking for remaining items
                                    if LOG_DEBUG {
                                        zoon::println!("[DEBUG] List/remove Replace: {} items incoming, persisted_removed={:?}",
                                            new_items.len(), persisted_removed);
                                    }
                                    items.clear();
                                    // B6: Drop old trigger tasks (cancels all old trigger streams)
                                    trigger_tasks.clear();
                                    let mut filtered_items = Vec::new();
                                    let mut trigger_streams: Vec<LocalBoxStream<'static, parser::PersistenceId>> = Vec::new();

                                    for item in new_items.iter() {
                                        let persistence_id = item.persistence_id();
                                        if removed_pids.contains(&persistence_id) {
                                            // This item was removed by this List/remove, skip it
                                            if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Replace: filtering by removed_pids"); }
                                            continue;
                                        }

                                        // Check persisted removals (for restoration after reload)
                                        if let Some(origin) = item.list_item_origin() {
                                            if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Replace: item has origin call_id={}", origin.call_id); }
                                            if persisted_removed.contains(&origin.call_id) {
                                                // This item was previously removed, skip it
                                                if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Replace: FILTERING out call_id={}", origin.call_id); }
                                                continue;
                                            }
                                        } else {
                                            // Check for persistence_id-based removal (LIST literal items)
                                            let id_str = format!("pid:{}", persistence_id);
                                            if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Replace: item has NO origin, checking pid={}", id_str); }
                                            if persisted_removed.contains(&id_str) {
                                                if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Replace: FILTERING out pid={}", id_str); }
                                                continue;
                                            }
                                        }

                                        let idx = *next_idx;
                                        *next_idx += 1;
                                        let when_actor = Self::transform_item(
                                            item.clone(),
                                            idx,
                                            config,
                                            construct_context.clone(),
                                            actor_context.clone(),
                                        );
                                        // B6: Create trigger stream instead of spawning a task
                                        trigger_streams.push(make_trigger_stream(when_actor.clone(), persistence_id));
                                        items.push((idx, item.clone(), when_actor));
                                        filtered_items.push(item.clone());
                                    }

                                    // B6: Spawn a single task to drive all trigger streams
                                    if !trigger_streams.is_empty() {
                                        let mut tx = remove_tx.clone();
                                        let merged = stream::select_all(trigger_streams);
                                        trigger_tasks.push(Task::start_droppable(async move {
                                            let mut merged = std::pin::pin!(merged);
                                            while let Some(persistence_id) = merged.next().await {
                                                // Channel may be closed if filter was removed - that's fine
                                                let _ = tx.send(persistence_id).await;
                                            }
                                        }));
                                    }

                                    // Clean up removed_pids: remove entries for items no longer in upstream
                                    // This ensures bounded growth - we only track items that exist upstream
                                    let upstream_pids: HashSet<_> = new_items.iter()
                                        .map(|item| item.persistence_id())
                                        .collect();
                                    removed_pids.retain(|pid| upstream_pids.contains(pid));

                                    return future::ready(Some(Some(ListChange::Replace { items: Arc::from(filtered_items) })));
                                }
                                ListChange::Push { item } => {
                                    let persistence_id = item.persistence_id();
                                    // If this item was previously removed, don't add it back
                                    if removed_pids.contains(&persistence_id) {
                                        if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Push: filtering by removed_pids"); }
                                        return future::ready(Some(None));
                                    }

                                    // Check persisted removals (for restoration after reload)
                                    if let Some(origin) = item.list_item_origin() {
                                        if LOG_DEBUG {
                                            zoon::println!("[DEBUG] List/remove Push: item has origin call_id={}, persisted_removed={:?}",
                                                origin.call_id, persisted_removed);
                                        }
                                        if persisted_removed.contains(&origin.call_id) {
                                            if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Push: FILTERING out call_id={}", origin.call_id); }
                                            return future::ready(Some(None));
                                        }
                                    } else {
                                        // Check for persistence_id-based removal (LIST literal items)
                                        let id_str = format!("pid:{}", persistence_id);
                                        if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Push: item has NO origin, checking pid={}", id_str); }
                                        if persisted_removed.contains(&id_str) {
                                            if LOG_DEBUG { zoon::println!("[DEBUG] List/remove Push: FILTERING out pid={}", id_str); }
                                            return future::ready(Some(None));
                                        }
                                    }

                                    let idx = *next_idx;
                                    *next_idx += 1;
                                    let when_actor = Self::transform_item(
                                        item.clone(),
                                        idx,
                                        config,
                                        construct_context.clone(),
                                        actor_context.clone(),
                                    );
                                    // B6: For Push, spawn a dedicated task for the new trigger.
                                    // Cleared on next Replace along with all other trigger tasks.
                                    let mut tx = remove_tx.clone();
                                    let trigger = make_trigger_stream(when_actor.clone(), persistence_id);
                                    trigger_tasks.push(Task::start_droppable(async move {
                                        let mut trigger = std::pin::pin!(trigger);
                                        if let Some(pid) = trigger.next().await {
                                            let _ = tx.send(pid).await;
                                        }
                                    }));
                                    items.push((idx, item.clone(), when_actor));
                                    return future::ready(Some(Some(ListChange::Push { item })));
                                }
                                ListChange::Remove { id } => {
                                    // Upstream removed an item - remove from our tracking
                                    // Also remove from removed_pids (item is gone from upstream)
                                    removed_pids.remove(&id);
                                    if let Some(pos) = items.iter().position(|(_, item, _)| item.persistence_id() == id) {
                                        items.remove(pos);
                                    }
                                    // Forward the Remove downstream
                                    return future::ready(Some(Some(ListChange::Remove { id })));
                                }
                                ListChange::Clear => {
                                    items.clear();
                                    trigger_tasks.clear();
                                    removed_pids.clear();
                                    *next_idx = 0;
                                    return future::ready(Some(Some(ListChange::Clear)));
                                }
                                ListChange::Pop => {
                                    if let Some((_, item, _)) = items.pop() {
                                        // If this was a removed item, take it out of tracking
                                        removed_pids.remove(&item.persistence_id());
                                    }
                                    return future::ready(Some(Some(ListChange::Pop)));
                                }
                                _ => {
                                    return future::ready(Some(None));
                                }
                            }
                        }
                        RemoveEvent::RemoveItem(persistence_id) => {
                            // Local removal triggered by `when` event
                            if let Some(pos) = items.iter().position(|(_, item, _)| item.persistence_id() == persistence_id) {
                                // Get the item before removing to check its origin
                                let (_, item, _) = &items[pos];

                                // Debug logging
                                if LOG_DEBUG {
                                    zoon::println!("[DEBUG] RemoveItem: item has origin: {}, removed_set_key: {:?}",
                                        item.list_item_origin().is_some(), removed_set_key);
                                }

                                // Persist removal to this branch's removed set
                                if let Some(key) = removed_set_key.as_ref() {
                                    if let Some(origin) = item.list_item_origin() {
                                        if LOG_DEBUG { zoon::println!("[DEBUG] Adding to removed set: key={}, call_id={}", key, origin.call_id); }
                                        add_to_removed_set(key, &origin.call_id);
                                    } else {
                                        // Fallback: use persistence_id for items without origin (LIST literal items)
                                        let id_str = format!("pid:{}", persistence_id);
                                        if LOG_DEBUG { zoon::println!("[DEBUG] Adding to removed set (no origin): key={}, id={}", key, id_str); }
                                        add_to_removed_set(key, &id_str);
                                    }
                                }

                                items.remove(pos);
                                // Track this removal so item doesn't reappear on upstream Replace
                                removed_pids.insert(persistence_id);
                                // Emit identity-based Remove
                                return future::ready(Some(Some(ListChange::Remove { id: persistence_id })));
                            }
                            return future::ready(Some(None));
                        }
                    }
                }
            ).filter_map(future::ready)
        });

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
        create_actor_complete(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            )),
            parser::PersistenceId::new(),
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
            source_list_actor.clone().stream()
            // Check if subscription scope is cancelled (e.g., WHILE arm switched)
            .take_while(move |_| {
                let is_active = subscription_scope.as_ref().map_or(true, |s| !s.is_cancelled());
                future::ready(is_active)
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::List(list, _) => Some(list),
                    _ => None,
                })
            })
            // Deduplicate by list ID - only emit when we see a genuinely new list
            .scan(None, |prev_id: &mut Option<ConstructId>, list| {
                let list_id = list.construct_info.id.clone();
                if prev_id.as_ref() == Some(&list_id) {
                    // Same list re-emitted, skip
                    future::ready(Some(None))
                } else {
                    *prev_id = Some(list_id);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready),
            move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();
            let construct_context_inner = construct_context.clone();

            list.stream().scan(
                Vec::<(ActorHandle, ActorHandle)>::new(),
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
                                (item.clone(), predicate)
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
                            item_predicates.push((item.clone(), predicate));
                        }
                        ListChange::Clear => {
                            item_predicates.clear();
                        }
                        ListChange::Pop => {
                            item_predicates.pop();
                        }
                        ListChange::Remove { id } => {
                            // Find item by PersistenceId
                            if let Some(index) = item_predicates.iter().position(|(item, _)| item.persistence_id() == *id) {
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
                let construct_info_id = construct_info_id_inner.clone();
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
                coalesce(stream::select_all(predicate_streams))
                    .scan(
                        vec![None::<bool>; item_predicates.len()],
                        move |states, batch| {
                            // A3: Process entire batch of predicate updates at once
                            for (idx, is_true) in batch {
                                if idx < states.len() {
                                    states[idx] = Some(is_true);
                                }
                            }

                            let all_evaluated = states.iter().all(|r| r.is_some());
                            if all_evaluated {
                                let result = if is_every {
                                    states.iter().all(|r| r == &Some(true))
                                } else {
                                    states.iter().any(|r| r == &Some(true))
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
        });

        // Deduplicate: only emit when the boolean result actually changes
        let deduplicated_stream = value_stream.scan(None::<bool>, |last_result, value| {
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
        }).filter_map(future::ready);

        let scope_id = actor_context_for_result.scope_id();
        create_actor_complete(
            construct_info,
            actor_context_for_result,
            TypedStream::infinite(deduplicated_stream),
            parser::PersistenceId::new(),
            scope_id,
        )
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
            source_list_actor.clone().stream()
            // Check if subscription scope is cancelled (e.g., WHILE arm switched)
            .take_while(move |_| {
                let is_active = subscription_scope.as_ref().map_or(true, |s| !s.is_cancelled());
                future::ready(is_active)
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::List(list, _) => Some(list),
                    _ => None,
                })
            })
            // Deduplicate by list ID - only emit when we see a genuinely new list
            .scan(None, |prev_id: &mut Option<ConstructId>, list| {
                let list_id = list.construct_info.id.clone();
                if prev_id.as_ref() == Some(&list_id) {
                    // Same list re-emitted, skip
                    future::ready(Some(None))
                } else {
                    *prev_id = Some(list_id);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready),
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
                let construct_info_id = construct_info_id_inner.clone();

                if item_keys.is_empty() {
                    // Empty list - emit empty Replace
                    return stream::once(future::ready(ListChange::Replace { items: Arc::from(Vec::<ActorHandle>::new()) })).boxed_local();
                }

                // Subscribe to all keys and emit sorted list when any changes
                // A3: Use coalesce to batch simultaneous key updates
                let key_streams: Vec<_> = item_keys.iter().enumerate().map(|(idx, (item, key_actor))| {
                    let item = item.clone();
                    key_actor.clone().stream().map(move |value| (idx, item.clone(), value))
                }).collect();

                coalesce(stream::select_all(key_streams))
                    .scan(
                        item_keys.iter().map(|(item, _)| (item.clone(), None::<SortKey>)).collect::<Vec<_>>(),
                        move |states, batch| {
                            // A3: Process entire batch of key updates at once
                            for (idx, item, value) in batch {
                                // Extract sortable key from value
                                let sort_key = SortKey::from_value(&value);
                                if idx < states.len() {
                                    states[idx] = (item, Some(sort_key));
                                }
                            }

                            // If all items have key results, emit sorted list
                            let all_evaluated = states.iter().all(|(_, result)| result.is_some());
                            if all_evaluated {
                                // Sort by key, preserving original order for equal keys (stable sort)
                                let mut indexed_items: Vec<_> = states.iter().enumerate()
                                    .map(|(orig_idx, (item, key))| (orig_idx, item.clone(), key.clone().unwrap()))
                                    .collect();
                                indexed_items.sort_by(|(orig_a, _, key_a), (orig_b, _, key_b)| {
                                    match key_a.cmp(key_b) {
                                        std::cmp::Ordering::Equal => orig_a.cmp(orig_b), // stable sort
                                        other => other,
                                    }
                                });
                                let sorted: Vec<ActorHandle> = indexed_items.into_iter()
                                    .map(|(_, item, _)| item)
                                    .collect();
                                future::ready(Some(Some(ListChange::Replace { items: Arc::from(sorted) })))
                            } else {
                                future::ready(Some(None))
                            }
                        }
                    )
                    .filter_map(future::ready)
                    .boxed_local()
            })
        });

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
        create_actor_complete(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            )),
            parser::PersistenceId::new(),
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
        let mut new_params = actor_context.parameters.clone();

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
        let pid_suffix = format!("{}", item_actor.persistence_id().as_u128());
        let scope_id = format!("list_item_{}_{}", index, pid_suffix);
        let child_scope = actor_context.with_child_scope(&scope_id);
        // Create a child registry scope for this list item so all actors created
        // within the transform expression are owned by this scope.
        // When the parent scope is destroyed (e.g., WHILE arm switch, program teardown),
        // all list item scopes are recursively destroyed.
        let item_registry_scope = child_scope.registry_scope_id.map(|parent_scope| {
            REGISTRY.with(|reg: &std::cell::RefCell<ActorRegistry>| {
                reg.borrow_mut().create_scope(Some(parent_scope))
            })
        });
        let new_actor_context = ActorContext {
            parameters: new_params,
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
                // Wrap the result in a new ValueActor with the ORIGINAL item's PersistenceId.
                let original_pid = item_actor.persistence_id();
                let scope_id = new_actor_context.scope_id();
                create_actor(
                    ConstructInfo::new(
                        ConstructId::new("List/map mapped item"),
                        None,
                        "List/map mapped item",
                    ),
                    new_actor_context,
                    TypedStream::infinite(result_actor.stream()),
                    original_pid,  // Preserve original PersistenceId!
                    scope_id,
                )
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
                (ListChange::Replace { items: Arc::from(transformed_items) }, new_length)
            }
            ListChange::InsertAt { index, item } => {
                let transformed_item = Self::transform_item(
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
                (ListChange::InsertAt { index, item: transformed_item }, current_length + 1)
            }
            ListChange::UpdateAt { index, item } => {
                let transformed_item = Self::transform_item(
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
                (ListChange::UpdateAt { index, item: transformed_item }, current_length)
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
                (ListChange::Push { item: transformed_item }, current_length + 1)
            }
            // These operations don't involve new items, pass through with updated length
            ListChange::Remove { id } => (ListChange::Remove { id }, current_length.saturating_sub(1)),
            ListChange::Move { old_index, new_index } => (ListChange::Move { old_index, new_index }, current_length),
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
        transform_cache: &mut std::collections::HashMap<parser::PersistenceId, ActorHandle>,
        item_order: &mut Vec<parser::PersistenceId>,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> (ListChange, usize) {
        match change {
            ListChange::Replace { items } => {
                let new_length = items.len();
                // Clear old PID mapping and item order (but keep transform cache for reuse)
                pid_map.clear();
                item_order.clear();

                // B1: Track which items are still present for cache cleanup
                let current_pids: std::collections::HashSet<_> = items.iter()
                    .map(|item| item.persistence_id())
                    .collect();

                let transformed_items: Vec<ActorHandle> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let original_pid = item.persistence_id();
                        item_order.push(original_pid.clone());

                        // B1: Check cache first - reuse transformed actor if available
                        let transformed = if let Some(cached) = transform_cache.get(&original_pid) {
                            cached.clone()
                        } else {
                            let new_transformed = Self::transform_item(
                                item.clone(),
                                index,
                                config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            transform_cache.insert(original_pid.clone(), new_transformed.clone());
                            new_transformed
                        };

                        let mapped_pid = transformed.persistence_id();
                        pid_map.insert(original_pid, mapped_pid);
                        transformed
                    })
                    .collect();

                // B1: Clean up cache - remove items no longer in the list
                transform_cache.retain(|pid, _| current_pids.contains(pid));

                (ListChange::Replace { items: Arc::from(transformed_items) }, new_length)
            }
            ListChange::InsertAt { index, item } => {
                let original_pid = item.persistence_id();
                // Track item order
                if index <= item_order.len() {
                    item_order.insert(index, original_pid.clone());
                }
                // B1: Check cache first, add to cache if miss
                let transformed_item = if let Some(cached) = transform_cache.get(&original_pid) {
                    cached.clone()
                } else {
                    let new_transformed = Self::transform_item(
                        item,
                        index,
                        config,
                        construct_context,
                        actor_context,
                    );
                    transform_cache.insert(original_pid.clone(), new_transformed.clone());
                    new_transformed
                };
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (ListChange::InsertAt { index, item: transformed_item }, current_length + 1)
            }
            ListChange::UpdateAt { index, item } => {
                let original_pid = item.persistence_id();
                // B1: For updates, always re-transform (the item content changed)
                let transformed_item = Self::transform_item(
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
                transform_cache.insert(original_pid.clone(), transformed_item.clone());
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (ListChange::UpdateAt { index, item: transformed_item }, current_length)
            }
            ListChange::Push { item } => {
                let original_pid = item.persistence_id();
                // Track item order
                item_order.push(original_pid.clone());
                // B1: Check cache first, add to cache if miss
                let transformed_item = if let Some(cached) = transform_cache.get(&original_pid) {
                    cached.clone()
                } else {
                    let new_transformed = Self::transform_item(
                        item,
                        current_length,
                        config,
                        construct_context,
                        actor_context,
                    );
                    transform_cache.insert(original_pid.clone(), new_transformed.clone());
                    new_transformed
                };
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (ListChange::Push { item: transformed_item }, current_length + 1)
            }
            ListChange::Remove { id } => {
                // B1: Remove from cache and item order
                transform_cache.remove(&id);
                item_order.retain(|pid| *pid != id);
                // Translate the original PersistenceId to the mapped PersistenceId
                if let Some(mapped_pid) = pid_map.remove(&id) {
                    (ListChange::Remove { id: mapped_pid }, current_length.saturating_sub(1))
                } else {
                    // Item not found in mapping - this shouldn't happen, but pass through
                    zoon::println!("[List/map] WARNING: Remove for unknown PersistenceId {:?}", id);
                    (ListChange::Remove { id }, current_length.saturating_sub(1))
                }
            }
            ListChange::Move { old_index, new_index } => {
                // Update item order for Move
                if old_index < item_order.len() && new_index < item_order.len() {
                    let pid = item_order.remove(old_index);
                    item_order.insert(new_index, pid);
                }
                (ListChange::Move { old_index, new_index }, current_length)
            }
            ListChange::Pop => {
                // B1: Now we can clean cache because we track item order
                if let Some(popped_pid) = item_order.pop() {
                    transform_cache.remove(&popped_pid);
                    pid_map.remove(&popped_pid);
                }
                (ListChange::Pop, current_length.saturating_sub(1))
            }
            ListChange::Clear => {
                pid_map.clear();
                transform_cache.clear(); // B1: Clear cache
                item_order.clear();
                (ListChange::Clear, 0)
            }
        }
    }
}
