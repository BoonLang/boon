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
            // Log at debug level - this is expected behavior for high-frequency events
            #[cfg(feature = "debug-channels")]
            zoon::eprintln!(
                "[BACKPRESSURE DROP] '{}' dropped event (full, capacity: {})",
                self.name, self.capacity
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
    /// Keep input actors alive
    inputs: Vec<Arc<ValueActor>>,
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
        inputs: Vec<Arc<ValueActor>>,
    ) -> Self {
        let construct_info = Arc::new(construct_info);
        let (request_tx, request_rx) = NamedChannel::new("lazy_value_actor.requests", 16);

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            // Keep inputs alive in the task
            let _inputs = inputs.clone();
            Self::internal_loop(construct_info, source_stream, request_rx)
        });

        Self {
            construct_info,
            request_tx,
            subscriber_counter: std::sync::atomic::AtomicUsize::new(0),
            inputs,
            actor_loop,
        }
    }

    /// Create a new Arc<LazyValueActor>.
    pub fn new_arc<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        source_stream: S,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info.complete(ConstructType::LazyValueActor),
            source_stream,
            inputs,
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

/// Messages that can be sent to a ValueActor.
/// All actor communication happens via these typed messages.
pub enum ActorMessage {
    // === Value Updates ===
    /// New value from the input stream (used during migration forwarding)
    StreamValue(Value),

    // === Migration Protocol (Phase 3) ===
    /// Request to migrate state to a new actor
    MigrateTo {
        target: Arc<ValueActor>,
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
        target: Arc<ValueActor>,
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
        target: Arc<ValueActor>,
        /// Optional transform to apply to values
        transform: Option<Box<dyn Fn(Value) -> Value + Send>>,
        /// IDs of batches that haven't been acknowledged yet
        pending_batches: HashSet<u64>,
        /// Values received during migration that need forwarding
        buffered_writes: Vec<Value>,
    },

    /// Receiving state from old actor
    Receiving {
        source: Arc<ValueActor>,
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

/// Request for subscribing to a ValueActor.
/// The actor loop will send back a Receiver<Value> through the oneshot channel.
pub struct SubscriptionRequest {
    /// Channel to receive the value receiver back
    pub reply: oneshot::Sender<mpsc::Receiver<Value>>,
    /// Starting version - 0 means all history
    pub starting_version: u64,
}

/// A simple push-based subscription that just wraps a receiver.
/// Values are pushed by the actor loop, no polling of shared state needed.
pub struct PushSubscription {
    receiver: mpsc::Receiver<Value>,
    /// Keeps the actor alive for the subscription lifetime
    _actor: Arc<ValueActor>,
}

impl PushSubscription {
    fn new(receiver: mpsc::Receiver<Value>, actor: Arc<ValueActor>) -> Self {
        Self { receiver, _actor: actor }
    }
}

impl Stream for PushSubscription {
    type Item = Value;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(cx)
    }
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
    /// Bounded(32) - state save operations.
    state_inserter_sender: NamedChannel<(
        parser::PersistenceId,
        serde_json::Value,
        oneshot::Sender<()>,
    )>,
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
                let mut states = match local_storage().get(&states_local_storage_key) {
                    None => BTreeMap::<String, serde_json::Value>::new(),
                    Some(Ok(states)) => states,
                    Some(Err(error)) => panic!("Failed to deserialize states: {error:#}"),
                };
                loop {
                    select! {
                        (persistence_id, json_value, confirmation_sender) = state_inserter_receiver.select_next_some() => {
                            // @TODO remove `.to_string()` call when LocalStorage is replaced with IndexedDB (?)
                            states.insert(persistence_id.to_string(), json_value);
                            if let Err(error) = local_storage().insert(&states_local_storage_key, &states) {
                                zoon::eprintln!("Failed to save states: {error:#}");
                            }
                            if confirmation_sender.send(()).is_err() {
                                zoon::eprintln!("Failed to send save confirmation from construct storage");
                            }
                        },
                        (persistence_id, state_sender) = state_getter_receiver.select_next_some() => {
                            // @TODO Cheaper cloning? Replace get with remove?
                            let state = states.get(&persistence_id.to_string()).cloned();
                            if state_sender.send(state).is_err() {
                                zoon::eprintln!("Failed to send state from construct storage");
                            }
                        }
                    }
                }
            }),
        }
    }

    pub async fn save_state<T: Serialize>(&self, persistence_id: parser::PersistenceId, state: &T) {
        let json_value = match serde_json::to_value(state) {
            Ok(json_value) => json_value,
            Err(error) => {
                zoon::eprintln!("Failed to save state: {error:#}");
                return;
            }
        };
        let (confirmation_sender, confirmation_receiver) = oneshot::channel::<()>();
        if self.state_inserter_sender.send((
            persistence_id,
            json_value,
            confirmation_sender,
        )).await.is_err() {
            zoon::eprintln!("Failed to save state: channel closed")
        }
        confirmation_receiver
            .await
            .expect("Failed to get confirmation from ConstructStorage")
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

// --- ActorContext ---

#[derive(Default, Clone)]
pub struct ActorContext {
    pub output_valve_signal: Option<Arc<ActorOutputValveSignal>>,
    /// The piped value from `|>` operator.
    /// Set when evaluating `x |> expr` - the `x` becomes `piped` for `expr`.
    /// Used by function calls to prepend as first argument.
    /// Also used by THEN/WHEN/WHILE/LinkSetter to process the piped stream.
    pub piped: Option<Arc<ValueActor>>,
    /// The PASSED context - implicit context passed through function calls.
    /// Set when calling a function with `PASS: something` argument.
    /// Accessible inside the function via `PASSED` or `PASSED.field`.
    /// Propagates automatically through nested function calls.
    pub passed: Option<Arc<ValueActor>>,
    /// Function parameter bindings - maps parameter names to their values.
    /// Set when calling a user-defined function.
    /// e.g., `fn(param: x)` binds "param" -> x's ValueActor
    pub parameters: HashMap<String, Arc<ValueActor>>,
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
    pub object_locals: HashMap<parser::Span, Arc<ValueActor>>,
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
}

impl ActorContext {
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
    value_actor: Arc<ValueActor>,
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
        value_actor: Arc<ValueActor>,
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
        value_actor: Arc<ValueActor>,
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
        value_actor: Arc<ValueActor>,
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
        let value_actor =
            ValueActor::new(actor_construct_info, actor_context, TypedStream::infinite(link_value_receiver), persistence_id);
        let value_actor = Arc::new(value_actor);

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
        forwarding_actor: Arc<ValueActor>,
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

    /// Subscribe to this variable's value stream (async).
    ///
    /// This method keeps the Variable alive for the lifetime of the returned stream.
    /// This is important because:
    /// - LINK variables have a `link_value_sender` that must stay alive
    /// - The Variable may be the only reference keeping dependent actors alive
    ///
    /// Takes ownership of the Arc to keep the Variable alive for the subscription lifetime.
    /// Callers should use `.clone().stream().await` if they need to retain a reference.
    pub async fn stream(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        let subscription = self.value_actor.clone().stream().await;
        // Subscription keeps the actor alive; we also need to keep the Variable alive
        stream::unfold(
            (subscription, self),
            |(mut subscription, variable)| async move {
                subscription.next().await.map(|value| (value, (subscription, variable)))
            }
        ).boxed_local()
    }

    /// Subscribe to future values only - skips historical replay (async).
    ///
    /// Use this for event triggers (THEN, WHEN, List/remove) where historical
    /// events should NOT be replayed.
    ///
    /// Takes ownership of the Arc to keep the Variable alive for the subscription lifetime.
    pub async fn stream_from_now(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        let subscription = self.value_actor.clone().stream_from_now().await;
        // Subscription keeps the actor alive; we also need to keep the Variable alive
        stream::unfold(
            (subscription, self),
            |(mut subscription, variable)| async move {
                subscription.next().await.map(|value| (value, (subscription, variable)))
            }
        ).boxed_local()
    }

    /// Sync wrapper for stream_from_now() - returns stream that emits only future values.
    ///
    /// Use this when you need stream_from_now() semantics but can't await.
    pub fn stream_from_now_sync(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        stream::once(async move { self.stream_from_now().await }).flatten().boxed_local()
    }

    /// Sync wrapper for stream() - returns continuous stream from version 0.
    ///
    /// Use this when you need stream() semantics but can't await.
    pub fn stream_sync(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        stream::once(async move { self.stream().await }).flatten().boxed_local()
    }

    pub fn value_actor(&self) -> Arc<ValueActor> {
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
        root_value_actor: impl Future<Output = Arc<ValueActor>> + 'static,
    ) -> Arc<ValueActor> {
        let construct_info = construct_info.complete(ConstructType::VariableOrArgumentReference);
        // Capture context flags before closures
        let use_snapshot = actor_context.is_snapshot_context;
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
                .then(|actor| async move { actor.stream().await })
                .flatten()
                .boxed_local()
        };
        // Collect parts to detect the last one
        let parts_vec: Vec<_> = alias_parts.into_iter().skip(skip_alias_parts).collect();
        let num_parts = parts_vec.len();

        for (idx, alias_part) in parts_vec.into_iter().enumerate() {
            let alias_part = alias_part.to_string();
            let _is_last = idx == num_parts - 1;

            // Process each field in the path using switch_map.
            // switch_map switches to a new inner stream whenever the outer emits.
            // It drains any in-flight items before switching to prevent losing events.
            //
            // WHILE re-render works because:
            // 1. When WHILE switches away from an arm, old Variables are dropped
            // 2. Subscription stream ends (receiver gone)
            // 3. switch_map creates new subscription when outer emits again
            // 4. When WHILE switches back, new Variables get new subscription
            value_stream = switch_map(
                value_stream,
                move |value| {
                let alias_part = alias_part.clone();
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
                            // Streaming: continuous updates - use stream() and keep alive
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), object, variable),
                                move |(subscription_opt, actor_opt, object, variable)| {
                                    async move {
                                        let mut subscription = match subscription_opt {
                                            Some(s) => s,
                                            None => {
                                                let actor = actor_opt.unwrap();
                                                actor.stream().await
                                            }
                                        };
                                        let value = subscription.next().await;
                                        value.map(|value| (value, (Some(subscription), None, object, variable)))
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
                            // Streaming: continuous updates - use stream() and keep alive
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), tagged_object, variable),
                                move |(subscription_opt, actor_opt, tagged_object, variable)| {
                                    async move {
                                        let mut subscription = match subscription_opt {
                                            Some(s) => s,
                                            None => {
                                                let actor = actor_opt.unwrap();
                                                actor.stream().await
                                            }
                                        };
                                        let value = subscription.next().await;
                                        value.map(|value| (value, (Some(subscription), None, tagged_object, variable)))
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
        Arc::new(ValueActor::new(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
        ))
    }
}

// --- ReferenceConnector ---

/// Actor for connecting references to actors by span.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct ReferenceConnector {
    referenceable_inserter_sender: NamedChannel<(parser::Span, Arc<ValueActor>)>,
    referenceable_getter_sender:
        NamedChannel<(parser::Span, oneshot::Sender<Arc<ValueActor>>)>,
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
                let mut referenceables = HashMap::<parser::Span, Arc<ValueActor>>::new();
                let mut referenceable_senders =
                    HashMap::<parser::Span, Vec<oneshot::Sender<Arc<ValueActor>>>>::new();
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

    pub fn register_referenceable(&self, span: parser::Span, actor: Arc<ValueActor>) {
        if let Err(error) = self
            .referenceable_inserter_sender
            .try_send((span, actor))
        {
            zoon::eprintln!("Failed to register referenceable: {error:#}")
        }
    }

    // @TODO is &self enough?
    pub async fn referenceable(self: Arc<Self>, span: parser::Span) -> Arc<ValueActor> {
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

/// Actor for connecting LINK variables with their setters.
/// Similar to ReferenceConnector but stores mpsc senders for LINK variables.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct LinkConnector {
    link_inserter_sender: NamedChannel<(parser::Span, NamedChannel<Value>)>,
    link_getter_sender:
        NamedChannel<(parser::Span, oneshot::Sender<NamedChannel<Value>>)>,
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
                let mut links = HashMap::<parser::Span, NamedChannel<Value>>::new();
                let mut link_senders =
                    HashMap::<parser::Span, Vec<oneshot::Sender<NamedChannel<Value>>>>::new();
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

    /// Register a LINK variable's sender with its span.
    pub fn register_link(&self, span: parser::Span, sender: NamedChannel<Value>) {
        if let Err(error) = self
            .link_inserter_sender
            .try_send((span, sender))
        {
            zoon::eprintln!("Failed to register link: {error:#}")
        }
    }

    /// Get a LINK variable's sender by its span.
    pub async fn link_sender(self: Arc<Self>, span: parser::Span) -> NamedChannel<Value> {
        let (link_sender, link_receiver) = oneshot::channel();
        if let Err(error) = self
            .link_getter_sender
            .try_send((span, link_sender))
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
    getter_sender: NamedChannel<(PassThroughKey, oneshot::Sender<Option<Arc<ValueActor>>>)>,
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
        actor: Arc<ValueActor>,
    },
    /// Forward a value to an existing pass-through
    Forward {
        key: PassThroughKey,
        value: Value,
    },
    /// Add a forwarder to keep alive for an existing pass-through
    AddForwarder {
        key: PassThroughKey,
        forwarder: Arc<ValueActor>,
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
                let mut pass_throughs = HashMap::<PassThroughKey, (mpsc::Sender<Value>, Arc<ValueActor>, Vec<Arc<ValueActor>>)>::new();
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
    pub fn register(&self, key: PassThroughKey, value_sender: mpsc::Sender<Value>, actor: Arc<ValueActor>) {
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
    pub fn add_forwarder(&self, key: PassThroughKey, forwarder: Arc<ValueActor>) {
        if let Err(e) = self.op_sender.try_send(PassThroughOp::AddForwarder { key, forwarder }) {
            zoon::eprintln!("[PASS_THROUGH] Failed to send AddForwarder: {e}");
        }
    }

    /// Get an existing pass-through actor if it exists
    pub async fn get(&self, key: PassThroughKey) -> Option<Arc<ValueActor>> {
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
            Arc<Vec<Arc<ValueActor>>>,
            ConstructId,
            parser::PersistenceId,
            ConstructContext,
            ActorContext,
        ) -> FR
        + 'static,
        arguments: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
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
                .map(|arg| arg.clone().stream_sync().filter(|v| {
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
        // Keep arguments alive as explicit dependencies
        let inputs: Vec<Arc<ValueActor>> = arguments.iter().cloned().collect();

        // In lazy mode, use LazyValueActor for demand-driven evaluation.
        // This is critical for HOLD body context where sequential state updates are needed.
        if actor_context.use_lazy_actors {
            ValueActor::new_arc_lazy(
                construct_info,
                combined_stream,
                parser::PersistenceId::new(),
                inputs,
            )
        } else {
            Arc::new(ValueActor::new_with_inputs(
                construct_info,
                actor_context,
                TypedStream::infinite(combined_stream),
                parser::PersistenceId::new(),
                inputs,
            ))
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
        inputs: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
        #[derive(Default, Clone, Serialize, Deserialize)]
        #[serde(crate = "serde")]
        struct State {
            input_idempotency_keys: BTreeMap<usize, ValueIdempotencyKey>,
        }

        let construct_info = construct_info.complete(ConstructType::LatestCombinator);
        let inputs: Vec<Arc<ValueActor>> = inputs.into();
        // If persistence is None (e.g., for dynamically evaluated expressions),
        // generate a fresh persistence ID at runtime
        let persistent_id = construct_info
            .persistence
            .map(|p| p.id)
            .unwrap_or_else(parser::PersistenceId::new);
        let storage = construct_context.construct_storage.clone();

        let value_stream =
            stream::select_all(inputs.iter().enumerate().map(|(index, value_actor)| {
                // Use stream_sync() to properly handle lazy actors in HOLD body context
                value_actor.clone().stream_sync().map(move |value| (index, value))
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
                        storage.save_state(persistent_id, &state).await;
                        Some(Some(value))
                    }
                }
            })
            .filter_map(future::ready);

        // Subscription-based streams are infinite (subscriptions never terminate first)
        // Pass inputs as explicit dependencies to keep them alive
        Arc::new(ValueActor::new_with_inputs(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
            inputs,
        ))
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
        operation: F,
    ) -> Arc<ValueActor>
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Value + 'static,
    {
        let construct_info = construct_info.complete(ConstructType::ValueActor);

        // Merge both operand streams, tracking which operand changed
        // Use stream_sync() to properly handle lazy actors in HOLD body context
        let value_stream = stream::select_all([
            operand_a.clone().stream_sync().map(|v| (0usize, v)).boxed_local(),
            operand_b.clone().stream_sync().map(|v| (1usize, v)).boxed_local(),
        ])
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
        // Keep both operands alive as explicit dependencies
        Arc::new(ValueActor::new_with_inputs(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
            vec![operand_a, operand_b],
        ))
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
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

// --- ValueActor ---

/// A message-based actor that manages a reactive value stream.
///
/// ValueActor uses explicit message passing for all communication:
/// - Subscriptions are managed via Subscribe/Unsubscribe messages
/// - Values flow from the input stream to subscribers
/// - Input actors are explicitly tracked in the `inputs` field
///
/// This design prevents "receiver is gone" errors by:
/// 1. Keeping input actors alive via explicit `inputs` Vec
/// 2. Never terminating the internal loop when input stream ends
/// 3. Only shutting down via explicit Shutdown message
pub struct ValueActor {
    construct_info: Arc<ConstructInfoComplete>,
    persistence_id: parser::PersistenceId,

    /// Message channel for actor communication (migration, shutdown).
    /// Bounded(16) - low frequency control messages.
    message_sender: NamedChannel<ActorMessage>,

    /// Explicit dependency tracking - keeps input actors alive.
    inputs: Vec<Arc<ValueActor>>,

    /// Current version number - increments on each value change.
    current_version: Arc<AtomicU64>,

    /// Channel for subscription requests.
    /// Subscribers send SubscriptionRequest, receive back a Receiver<Value>.
    /// Bounded(32) - subscription bursts during initialization.
    subscription_sender: NamedChannel<SubscriptionRequest>,

    /// Channel for direct value storage.
    /// Used by HOLD to store values without going through the input stream.
    /// Bounded(64) - high frequency state updates from HOLD.
    direct_store_sender: NamedChannel<Value>,

    /// Channel for stored value queries.
    /// Used by stored_value() and snapshot() to get current value.
    /// Bounded(8) - low frequency queries.
    stored_value_query_sender: NamedChannel<StoredValueQuery>,

    /// Signal that fires when actor has processed at least one value.
    /// Used by stream() to wait for initial value instead of arbitrary yields.
    ready_signal: Shared<oneshot::Receiver<()>>,

    /// The actor's internal loop.
    actor_loop: ActorLoop,

    /// Optional lazy delegate for demand-driven evaluation.
    /// When Some, subscribe() delegates to this lazy actor instead of the normal subscription.
    lazy_delegate: Option<Arc<LazyValueActor>>,

    /// Extra ActorLoops that should be kept alive with this actor.
    extra_loops: Vec<ActorLoop>,
}

impl ValueActor {
    /// Create a new ValueActor from a stream.
    ///
    /// # Arguments
    /// - `construct_info`: Metadata about this construct
    /// - `actor_context`: Context including output valve signal
    /// - `value_stream`: The input stream of values (should be infinite)
    /// - `persistence_id`: Optional ID for state persistence
    ///
    /// # Notes
    /// The stream SHOULD be infinite for best results, but the actor will
    /// NOT terminate when the stream ends - it continues waiting for messages.
    pub fn new<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfoComplete,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: parser::PersistenceId,
    ) -> Self {
        Self::new_with_inputs(
            construct_info,
            actor_context,
            value_stream,
            persistence_id,
            Vec::new(),
        )
    }

    /// Create a new ValueActor with explicit input dependencies.
    ///
    /// The `inputs` Vec keeps the input actors alive for the lifetime of this actor.
    /// This is the primary mechanism for preventing "receiver is gone" errors.
    pub fn new_with_inputs<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfoComplete,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: parser::PersistenceId,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Self {
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = NamedChannel::new("value_actor.messages", 16);
        let current_version = Arc::new(AtomicU64::new(0));

        // Bounded channel for subscription requests
        let (subscription_sender, subscription_receiver) = NamedChannel::new("value_actor.subscriptions", 32);

        // Bounded channel for direct value storage
        let (direct_store_sender, direct_store_receiver) = NamedChannel::<Value>::new("value_actor.direct_store", 64);

        // Bounded channel for stored value queries
        let (stored_value_query_sender, stored_value_query_receiver) = NamedChannel::new("value_actor.queries", 8);

        // Oneshot channel for ready signal - fires when first value is processed
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let ready_signal = ready_rx.shared();

        let boxed_stream: std::pin::Pin<Box<dyn Stream<Item = Value>>> =
            Box::pin(value_stream.inner);

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let current_version = current_version.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            // Keep inputs alive in the spawned task
            let _inputs = inputs.clone();

            async move {
                // Actor-local state (no Mutex needed!)
                let mut value_history = ValueHistory::new(64);
                let mut subscribers: Vec<mpsc::Sender<Value>> = Vec::new();
                let mut stream_ever_produced = false;
                let mut stream_ended = false;
                // Ready signal sender - consumed on first value
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
                        // Handle new subscription requests
                        req = subscription_receiver.next() => {
                            if let Some(SubscriptionRequest { reply, starting_version }) = req {
                                // If stream ended without producing any value (SKIP case),
                                // don't register subscriber - send empty receiver to signal completion
                                if stream_ended && !stream_ever_produced {
                                    let (tx, rx) = mpsc::channel(1);
                                    drop(tx); // Close immediately
                                    if reply.send(rx).is_err() {
                                        zoon::println!("[VALUE_ACTOR] Subscription reply receiver dropped (SKIP case) for '{construct_info}'");
                                    }
                                } else {
                                    // Create channel for this subscriber (capacity 32 for buffering)
                                    let (mut tx, rx) = mpsc::channel(32);

                                    // Send historical values from starting_version
                                    let historical_values = value_history.get_values_since(starting_version).0;
                                    let history_count = historical_values.len();
                                    let mut sent_count = 0;
                                    for value in historical_values {
                                        // Best effort - if channel full, subscriber is too slow
                                        if tx.try_send(value.clone()).is_ok() {
                                            sent_count += 1;
                                        }
                                    }

                                    // Add to live subscribers list
                                    subscribers.push(tx);

                                    // Reply with the receiver
                                    if reply.send(rx).is_err() {
                                        zoon::println!("[VALUE_ACTOR] Subscription reply receiver dropped for '{construct_info}'");
                                    }
                                }
                            }
                        }

                        // Handle direct value storage (from store_value_directly)
                        value = direct_store_receiver.next() => {
                            if let Some(value) = value {
                                if !stream_ever_produced {
                                    stream_ever_produced = true;
                                    if let Some(tx) = ready_tx.take() {
                                        // Ready signal receiver dropped is fine - caller may not care about readiness
                                        tx.send(()).ok();
                                    }
                                }
                                let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                value_history.add(new_version, value.clone());

                                // Push value to all subscribers (only remove on disconnect, not backpressure)
                                subscribers.retain_mut(|tx| {
                                    match tx.try_send(value.clone()) {
                                        Ok(()) => true,
                                        Err(e) if e.is_disconnected() => false,
                                        Err(_) => true,
                                    }
                                });
                            }
                        }

                        // Handle stored value queries
                        query = stored_value_query_receiver.next() => {
                            if let Some(StoredValueQuery { reply }) = query {
                                let current_value = value_history.get_latest();
                                if reply.send(current_value).is_err() {
                                    zoon::println!("[VALUE_ACTOR] Stored value query reply receiver dropped");
                                }
                            }
                        }

                        // Handle messages from the message channel
                        msg = message_receiver.next() => {
                            let Some(msg) = msg else {
                                // Channel closed - ValueActor was dropped, exit the loop
                                break;
                            };
                            match msg {
                                ActorMessage::StreamValue(value) => {
                                    // Value received from migration source
                                    if !stream_ever_produced {
                                        stream_ever_produced = true;
                                        if let Some(tx) = ready_tx.take() {
                                            // Ready signal receiver dropped is fine - caller may not care about readiness
                                            tx.send(()).ok();
                                        }
                                    }
                                    let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                    value_history.add(new_version, value.clone());

                                    // Push value to all subscribers (only remove on disconnect, not backpressure)
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

                        // Handle values from the input stream
                        new_value = value_stream.next() => {
                            let Some(new_value) = new_value else {
                                // Stream ended - but we DON'T break!
                                stream_ended = true;
                                if !stream_ever_produced {
                                    // Clear subscribers - they'll see channel closed
                                    subscribers.clear();
                                }
                                continue;
                            };

                            if !stream_ever_produced {
                                stream_ever_produced = true;
                                if let Some(tx) = ready_tx.take() {
                                    // Ready signal receiver dropped is fine - caller may not care about readiness
                                    tx.send(()).ok();
                                }
                            }
                            let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                            value_history.add(new_version, new_value.clone());

                            // Push value to all subscribers
                            // Only remove on Disconnected (receiver dropped), NOT on Full (backpressure)
                            subscribers.retain_mut(|tx| {
                                match tx.try_send(new_value.clone()) {
                                    Ok(()) => true,
                                    Err(e) if e.is_disconnected() => false, // Remove dead subscribers
                                    Err(e) if e.is_full() => true, // Keep on backpressure (Full)
                                    Err(_) => true,
                                }
                            });

                            // Handle migration forwarding
                            if let MigrationState::Migrating { buffered_writes, target, .. } = &mut migration_state {
                                buffered_writes.push(new_value.clone());
                                if let Err(e) = target.send_message(ActorMessage::StreamValue(new_value)) {
                                    zoon::println!("[VALUE_ACTOR] Migration forward failed: {e}");
                                }
                            }
                        }

                        // Handle output valve impulses
                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() {
                                continue;
                            }
                        }
                    }
                }

                // Explicit cleanup
                drop(_inputs);

                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("Loop ended {construct_info}");
                }
            }
        });

        Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs,
            current_version,
            subscription_sender,
            direct_store_sender,
            stored_value_query_sender,
            ready_signal,
            actor_loop,
            lazy_delegate: None,
            extra_loops: Vec::new(),
        }
    }

    /// Send a message to this actor.
    /// Uses try_send (non-blocking) - returns error if channel full or closed.
    pub fn send_message(&self, msg: ActorMessage) -> Result<(), mpsc::TrySendError<ActorMessage>> {
        self.message_sender.try_send(msg)
    }

    pub fn new_arc<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: parser::PersistenceId,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info.complete(ConstructType::ValueActor),
            actor_context,
            value_stream,
            persistence_id,
        ))
    }

    /// Create a new Arc<ValueActor> with explicit input dependencies.
    pub fn new_arc_with_inputs<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: parser::PersistenceId,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Arc<Self> {
        Arc::new(Self::new_with_inputs(
            construct_info.complete(ConstructType::ValueActor),
            actor_context,
            value_stream,
            persistence_id,
            inputs,
        ))
    }

    /// Create a ValueActor that delegates to a LazyValueActor for demand-driven evaluation.
    ///
    /// Used in HOLD body context where lazy evaluation is needed for sequential state updates.
    /// The returned ValueActor is a shell that forwards subscribe() calls to the lazy actor.
    ///
    /// Note: The shell actor doesn't have its own loop - all subscription happens through
    /// the lazy delegate. The shell exists so the return type is Arc<ValueActor>.
    pub fn new_arc_lazy<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfoComplete,
        value_stream: S,
        persistence_id: parser::PersistenceId,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Arc<Self> {
        // Create the lazy actor that does the actual work
        let lazy_actor = Arc::new(LazyValueActor::new(
            construct_info.clone(),
            value_stream,
            inputs.clone(),
        ));

        // Create a shell ValueActor - the lazy_delegate handles all subscriptions
        let construct_info = Arc::new(construct_info);
        let (message_sender, _message_receiver) = NamedChannel::new("value_actor.lazy.messages", 16);
        let current_version = Arc::new(AtomicU64::new(0));

        // Dummy channels (won't be used since lazy_delegate handles subscriptions)
        let (subscription_sender, _subscription_receiver) = NamedChannel::new("value_actor.lazy.subscriptions", 32);
        let (direct_store_sender, _direct_store_receiver) = NamedChannel::new("value_actor.lazy.direct_store", 64);
        let (stored_value_query_sender, _stored_value_query_receiver) = NamedChannel::new("value_actor.lazy.queries", 8);

        // Dummy ready signal (lazy actors bypass it in stream())
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        drop(ready_tx); // Won't be used
        let ready_signal = ready_rx.shared();

        // Create a no-op task (the lazy delegate owns the real processing)
        let actor_loop = ActorLoop::new(async {});

        Arc::new(Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs,
            current_version,
            subscription_sender,
            direct_store_sender,
            stored_value_query_sender,
            ready_signal,
            actor_loop,
            lazy_delegate: Some(lazy_actor),
            extra_loops: Vec::new(),
        })
    }

    /// Create a new ValueActor with both an initial value and input dependencies.
    /// Combines the benefits of `new_with_inputs` (keeps inputs alive) and
    /// `new_arc_with_initial_value` (immediate value availability).
    ///
    /// Use this for combinators that have both:
    /// - Input dependencies that must stay alive
    /// - An initial value that can be computed synchronously
    pub fn new_with_initial_value_and_inputs<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfoComplete,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: parser::PersistenceId,
        initial_value: Value,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Self {
        let value_stream = value_stream.inner;
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = NamedChannel::new("value_actor.initial.messages", 16);
        // Start at version 1 since we have an initial value
        let current_version = Arc::new(AtomicU64::new(1));

        // Bounded channels for subscription, direct store, and stored value queries
        let (subscription_sender, subscription_receiver) = NamedChannel::new("value_actor.initial.subscriptions", 32);
        let (direct_store_sender, direct_store_receiver) = NamedChannel::<Value>::new("value_actor.initial.direct_store", 64);
        let (stored_value_query_sender, stored_value_query_receiver) = NamedChannel::new("value_actor.initial.queries", 8);

        // Oneshot channel for ready signal - immediately fire since we have initial value
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        // Fire immediately - receiver will always be there since we just created it
        ready_tx.send(()).ok();
        let ready_signal = ready_rx.shared();

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let current_version = current_version.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            let _inputs = inputs.clone();

            async move {
                // Actor-local state with initial value
                let mut value_history = {
                    let mut history = ValueHistory::new(64);
                    history.add(1, initial_value);
                    history
                };
                let mut subscribers: Vec<mpsc::Sender<Value>> = Vec::new();
                let mut stream_ever_produced = true; // We have initial value
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
                        req = subscription_receiver.next() => {
                            if let Some(SubscriptionRequest { reply, starting_version }) = req {
                                if stream_ended && !stream_ever_produced {
                                    let (tx, rx) = mpsc::channel(1);
                                    drop(tx);
                                    if reply.send(rx).is_err() {
                                        zoon::println!("[VALUE_ACTOR] Subscription reply receiver dropped (SKIP case) for '{construct_info}'");
                                    }
                                } else {
                                    let (mut tx, rx) = mpsc::channel(32);
                                    for value in value_history.get_values_since(starting_version).0 {
                                        // Best effort historical send - if full, skip
                                        tx.try_send(value.clone()).ok();
                                    }
                                    subscribers.push(tx);
                                    if reply.send(rx).is_err() {
                                        zoon::println!("[VALUE_ACTOR] Subscription reply receiver dropped for '{construct_info}'");
                                    }
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

                drop(_inputs);

                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("Loop ended {construct_info}");
                }
            }
        });

        Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs,
            current_version,
            subscription_sender,
            direct_store_sender,
            stored_value_query_sender,
            ready_signal,
            actor_loop,
            lazy_delegate: None,
            extra_loops: Vec::new(),
        }
    }

    pub fn persistence_id(&self) -> parser::PersistenceId {
        self.persistence_id
    }

    /// Get the current version of this actor's value.
    pub fn version(&self) -> u64 {
        self.current_version.load(Ordering::SeqCst)
    }

    /// Get the current stored value (async).
    ///
    /// Returns `Ok(value)` if the actor has a stored value.
    /// Returns `Err(NoValueYet)` if the actor exists but hasn't stored a value yet.
    /// Returns `Err(ActorDropped)` if the actor was dropped.
    ///
    /// For properties with Boon-level initial values, use `.expect("reason")`.
    /// For LINK variables (user interaction), handle the error gracefully.
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

    /// Create a new ValueActor with a channel-based stream for forwarding values.
    /// Returns the actor and a sender that can be used to forward values to it.
    ///
    /// This is useful when you need to register an actor with ReferenceConnector
    /// before the actual value-producing expression is evaluated. The sender
    /// can then be used to forward values from the evaluated expression.
    ///
    /// # Usage Pattern
    /// 1. Create the forwarding actor and register it immediately
    /// 2. Evaluate the source expression to get its actor
    /// 3. Subscribe to the source actor and forward values through the sender
    pub fn new_arc_forwarding(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        persistence_id: parser::PersistenceId,
    ) -> (Arc<Self>, NamedChannel<Value>) {
        let (sender, receiver) = NamedChannel::new("forwarding.values", 64);
        let stream = TypedStream::infinite(receiver);
        let actor = Self::new_arc(
            construct_info,
            actor_context,
            stream,
            persistence_id,
        );
        (actor, sender)
    }

    /// Connect a forwarding actor to its source actor.
    ///
    /// This creates an ActorLoop that subscribes to the source actor and forwards
    /// all values through the provided sender to the forwarding actor.
    /// Optionally sends an initial value synchronously before starting the async forwarding.
    ///
    /// # Arguments
    /// - `forwarding_sender`: The sender from `new_arc_forwarding()` to send values to
    /// - `source_actor`: The source actor to subscribe to
    /// - `initial_value`: Optional initial value to send synchronously before async forwarding
    ///
    /// # Returns
    /// An ActorLoop that must be kept alive for forwarding to continue.
    ///
    /// # Usage Pattern
    /// ```ignore
    /// let (forwarding_actor, sender) = ValueActor::new_arc_forwarding(...);
    /// // Register forwarding_actor immediately
    /// // Later, when source_actor is available:
    /// let actor_loop = ValueActor::connect_forwarding(sender, source_actor, initial_value);
    /// // Store actor_loop to keep forwarding alive
    /// ```
    pub fn connect_forwarding(
        forwarding_sender: NamedChannel<Value>,
        source_actor: Arc<ValueActor>,
        initial_value_future: impl Future<Output = Option<Value>> + 'static,
    ) -> ActorLoop {
        ActorLoop::new(async move {
            // Send initial value first (awaiting if needed)
            if let Some(value) = initial_value_future.await {
                if let Err(e) = forwarding_sender.send(value).await {
                    zoon::println!("[VALUE_ACTOR] Initial forwarding failed: {e}");
                    return;
                }
            }

            let mut subscription = source_actor.stream().await;
            while let Some(value) = subscription.next().await {
                if forwarding_sender.send(value).await.is_err() {
                    break;
                }
            }
        })
    }

    /// Create a new Arc<ValueActor> with an initial value pre-set.
    /// This ensures the value is immediately available to subscribers without
    /// waiting for the async task to poll the stream.
    ///
    /// Use this for constant values where the initial value is known synchronously.
    pub fn new_arc_with_initial_value<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: parser::PersistenceId,
        initial_value: Value,
    ) -> Arc<Self> {
        let construct_info = construct_info.complete(ConstructType::ValueActor);
        let value_stream = value_stream.inner;
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = NamedChannel::new("value_actor.arc_initial.messages", 16);
        // Start at version 1 since we have an initial value
        let current_version = Arc::new(AtomicU64::new(1));

        // Bounded channels for subscription, direct store, and stored value queries
        let (subscription_sender, subscription_receiver) = NamedChannel::new("value_actor.arc_initial.subscriptions", 32);
        let (direct_store_sender, direct_store_receiver) = NamedChannel::<Value>::new("value_actor.arc_initial.direct_store", 64);
        let (stored_value_query_sender, stored_value_query_receiver) = NamedChannel::new("value_actor.arc_initial.queries", 8);

        // Oneshot channel for ready signal - immediately fire since we have initial value
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        // Fire immediately - receiver will always be there since we just created it
        ready_tx.send(()).ok();
        let ready_signal = ready_rx.shared();

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let current_version = current_version.clone();
            let output_valve_signal = actor_context.output_valve_signal.clone();

            async move {
                // Actor-local state with initial value
                let mut value_history = {
                    let mut history = ValueHistory::new(64);
                    history.add(1, initial_value);
                    history
                };
                let mut subscribers: Vec<mpsc::Sender<Value>> = Vec::new();
                let stream_ever_produced = true; // We have an initial value
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

                loop {
                    select! {
                        query = stored_value_query_receiver.next() => {
                            if let Some(StoredValueQuery { reply }) = query {
                                if reply.send(value_history.get_latest()).is_err() {
                                    zoon::println!("[VALUE_ACTOR] Stored value query reply receiver dropped");
                                }
                            }
                        }

                        req = subscription_receiver.next() => {
                            if let Some(SubscriptionRequest { reply, starting_version }) = req {
                                if stream_ended && !stream_ever_produced {
                                    let (tx, rx) = mpsc::channel(1);
                                    drop(tx);
                                    if reply.send(rx).is_err() {
                                        zoon::println!("[VALUE_ACTOR] Subscription reply receiver dropped (SKIP case) for '{construct_info}'");
                                    }
                                } else {
                                    let (mut tx, rx) = mpsc::channel(32);
                                    for value in value_history.get_values_since(starting_version).0 {
                                        // Best effort historical send - if full, skip
                                        tx.try_send(value.clone()).ok();
                                    }
                                    subscribers.push(tx);
                                    if reply.send(rx).is_err() {
                                        zoon::println!("[VALUE_ACTOR] Subscription reply receiver dropped for '{construct_info}'");
                                    }
                                }
                            }
                        }

                        value = direct_store_receiver.next() => {
                            if let Some(value) = value {
                                let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                value_history.add(new_version, value.clone());
                                // Only remove on Disconnected (receiver dropped), NOT on Full (backpressure)
                                subscribers.retain_mut(|tx| {
                                    match tx.try_send(value.clone()) {
                                        Ok(()) => true,
                                        Err(e) if e.is_disconnected() => false,
                                        Err(_) => true,
                                    }
                                });
                            }
                        }

                        msg = message_receiver.next() => {
                            let Some(msg) = msg else { break; };
                            match msg {
                                ActorMessage::StreamValue(new_value) => {
                                    let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                    value_history.add(new_version, new_value.clone());
                                    subscribers.retain_mut(|tx| match tx.try_send(new_value.clone()) { Ok(()) => true, Err(e) if e.is_disconnected() => false, Err(_) => true });
                                }
                                ActorMessage::Shutdown => break,
                                _ => {}
                            }
                        }

                        _ = output_valve_impulse_stream.next() => {
                            break;
                        }

                        new_value = value_stream.next() => {
                            if let Some(new_value) = new_value {
                                let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                value_history.add(new_version, new_value.clone());
                                subscribers.retain_mut(|tx| match tx.try_send(new_value.clone()) { Ok(()) => true, Err(e) if e.is_disconnected() => false, Err(_) => true });
                            } else {
                                stream_ended = true;
                            }
                        }
                    }
                }
                if LOG_DROPS_AND_LOOP_ENDS {
                    zoon::println!("Loop ended {construct_info}");
                }
            }
        });

        Arc::new(Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs: Vec::new(),
            current_version,
            subscription_sender,
            direct_store_sender,
            stored_value_query_sender,
            ready_signal,
            actor_loop,
            lazy_delegate: None,
            extra_loops: Vec::new(),
        })
    }

    /// Directly store a value, bypassing the async input stream.
    /// Used by HOLD to update state between body evaluations.
    ///
    /// The actor loop processes the value and notifies all subscribers.
    /// Uses send_or_drop (non-blocking) - logs if channel is full.
    pub fn store_value_directly(&self, value: Value) {
        self.direct_store_sender.send_or_drop(value);
    }

    /// Check if this actor has a lazy delegate.
    pub fn has_lazy_delegate(&self) -> bool {
        self.lazy_delegate.is_some()
    }

    // === Clean Subscription API ===
    //
    // Primary methods for getting values:
    // - value() returns Future<Result<Value, ValueError>> - exactly ONE value
    // - stream() returns Stream<Item=Value> - continuous updates
    //
    // Deprecated wrappers are provided for incremental migration.

    /// Get exactly ONE value - waiting if necessary.
    ///
    /// Returns a `Future`, not a `Stream`. This makes it **impossible to misuse**:
    /// - Future resolves once → you get one value
    /// - No need for `.take(1)` - the type itself enforces single-value semantics
    ///
    /// Use this in THEN/WHEN bodies where you need a point-in-time value.
    ///
    /// Returns `Ok(value)` when a value is available.
    /// Returns `Err(ActorDropped)` if the actor dies before producing a value.
    ///
    /// For LINK variables (user interaction), handle ActorDropped gracefully -
    /// the user may navigate away without triggering the event.
    pub async fn value(self: Arc<Self>) -> Result<Value, ValueError> {
        // If we already have a value, return it directly
        if self.version() > 0 {
            return self.current_value().await.map_err(|e| match e {
                CurrentValueError::NoValueYet => unreachable!("version > 0 implies value exists"),
                CurrentValueError::ActorDropped => ValueError::ActorDropped,
            });
        }
        // Otherwise stream and wait for first value
        let mut s = self.stream().await;
        s.next().await.ok_or(ValueError::ActorDropped)
    }

    /// Subscribe to continuous stream of all values from version 0 (async).
    ///
    /// Use this for WHILE bodies, LATEST inputs, and reactive bindings where you
    /// need continuous updates.
    ///
    /// Takes ownership of the Arc to keep the actor alive for the subscription lifetime.
    /// Callers should use `.clone().stream().await` if they need to retain a reference.
    pub async fn stream(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        // Handle lazy actors
        if let Some(ref lazy_delegate) = self.lazy_delegate {
            return lazy_delegate.clone().stream().boxed_local();
        }

        // Wait for actor to be ready (has processed at least one value).
        // This ensures constant() values are in value_history before we subscribe.
        // The ready_signal fires when the actor loop processes its first value.
        if self.version() == 0 {
            // Clone the shared future and await it - errors mean sender dropped (actor dead)
            if self.ready_signal.clone().await.is_err() {
                zoon::println!("[VALUE_ACTOR] Ready signal sender dropped - actor may be dead");
            }
        }

        // Send subscription request to actor loop
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.subscription_sender.send(SubscriptionRequest {
            reply: reply_tx,
            starting_version: 0,
        }).await.is_err() {
            zoon::eprintln!("Failed to stream: actor dropped");
            // Return empty stream
            return stream::empty().boxed_local();
        }

        // Wait for the receiver from actor loop
        match reply_rx.await {
            Ok(receiver) => {
                PushSubscription::new(receiver, self).boxed_local()
            }
            Err(_) => {
                zoon::eprintln!("Failed to stream: actor dropped before reply");
                stream::empty().boxed_local()
            }
        }
    }

    /// Subscribe starting from current version - only future values (async).
    ///
    /// Use this for event triggers (THEN, WHEN, List/remove) where historical
    /// events should NOT be replayed. Subscribers will only receive values
    /// emitted AFTER this subscription is created.
    ///
    /// Takes ownership of the Arc to keep the actor alive for the subscription lifetime.
    pub async fn stream_from_now(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        // Handle lazy actors
        if let Some(ref lazy_delegate) = self.lazy_delegate {
            return lazy_delegate.clone().stream().boxed_local();
        }

        // Capture current version BEFORE subscribing - this is the key difference from stream()
        let current_version = self.version();

        // Send subscription request to actor loop with current version as starting point
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.subscription_sender.send(SubscriptionRequest {
            reply: reply_tx,
            starting_version: current_version,
        }).await.is_err() {
            zoon::eprintln!("Failed to stream_from_now: actor dropped");
            return stream::empty().boxed_local();
        }

        // Wait for the receiver from actor loop
        match reply_rx.await {
            Ok(receiver) => {
                PushSubscription::new(receiver, self).boxed_local()
            }
            Err(_) => {
                zoon::eprintln!("Failed to stream_from_now: actor dropped before reply");
                stream::empty().boxed_local()
            }
        }
    }

    /// Sync wrapper for stream_from_now() - returns stream that emits only future values.
    ///
    /// Use this when you need stream_from_now() semantics but can't await.
    pub fn stream_from_now_sync(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        stream::once(async move { self.stream_from_now().await }).flatten().boxed_local()
    }

    /// Sync wrapper for stream() - returns continuous stream from version 0.
    ///
    /// Use this when you need stream() semantics but can't await.
    /// Equivalent to: `stream::once(async move { actor.stream().await }).flatten()`
    pub fn stream_sync(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        stream::once(async move { self.stream().await }).flatten().boxed_local()
    }

    /// Get optimal update for subscriber at given version (async).
    ///
    /// For scalar values, always returns a snapshot (cheap to copy).
    /// For collections with DiffHistory (future), may return diffs if
    /// subscriber is close enough to current version.
    pub async fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        let current = self.version();
        if subscriber_version >= current {
            return ValueUpdate::Current;
        }
        // For now, always return snapshot. Phase 4 will add diff support for LIST.
        match self.current_value().await {
            Ok(value) => ValueUpdate::Snapshot(value),
            Err(_) => ValueUpdate::Current,
        }
    }
}

impl Drop for ValueActor {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            zoon::println!("Dropped: {}", self.construct_info);
        }
    }
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
    pub async fn snapshot(&self) -> Vec<(ItemId, Arc<ValueActor>)> {
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
        let metadata = ValueMetadata {
            idempotency_key: parser::PersistenceId::new(),
        };
        Value::Flushed(Box::new(self), metadata)
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
                    let value = variable.value_actor().clone().stream().await.next().await;
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
                    let value = variable.value_actor().clone().stream().await.next().await;
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
                    for item in items {
                        let value = item.clone().stream().await.next().await;
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
                let items: Vec<Arc<ValueActor>> = arr.iter()
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
) -> Arc<ValueActor> {
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
    Arc::new(ValueActor::new(
        actor_construct_info,
        actor_context,
        constant(value),
        parser::PersistenceId::new(),
    ))
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
    match value {
        Value::Object(object, metadata) => {
            let mut materialized_vars: Vec<Arc<Variable>> = Vec::new();
            for variable in object.variables() {
                // Await the variable's current value through subscription (proper async)
                let var_value = variable.value_actor().clone().stream().await.next().await;
                if let Some(var_value) = var_value {
                    // Recursively materialize nested values
                    let materialized = Box::pin(materialize_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )).await;
                    // Create a constant ValueActor for the materialized value
                    let value_actor = Arc::new(ValueActor::new(
                        ConstructInfo::new(
                            format!("materialized_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ).complete(ConstructType::ValueActor),
                        actor_context.clone(),
                        constant(materialized),
                        parser::PersistenceId::new(),
                    ));
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
                let var_value = variable.value_actor().clone().stream().await.next().await;
                if let Some(var_value) = var_value {
                    let materialized = Box::pin(materialize_value(
                        var_value,
                        construct_context.clone(),
                        actor_context.clone(),
                    )).await;
                    let value_actor = Arc::new(ValueActor::new(
                        ConstructInfo::new(
                            format!("materialized_{}", variable.name()),
                            None,
                            format!("Materialized variable {}", variable.name()),
                        ).complete(ConstructType::ValueActor),
                        actor_context.clone(),
                        constant(materialized),
                        parser::PersistenceId::new(),
                    ));
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
            ValueMetadata { idempotency_key },
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
    ) -> Arc<ValueActor> {
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

        // Use new_arc_with_initial_value so the value is immediately available
        // This is critical for WASM single-threaded runtime where spawned tasks
        // don't run until we yield - subscriptions need immediate access to values.
        ValueActor::new_arc_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
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
            ValueMetadata { idempotency_key },
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
    ) -> Arc<ValueActor> {
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

        // Use new_arc_with_initial_value so the value is immediately available
        // This is critical for WASM single-threaded runtime where spawned tasks
        // don't run until we yield - subscriptions need immediate access to values.
        ValueActor::new_arc_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
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
            ValueMetadata { idempotency_key },
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
    ) -> Arc<ValueActor> {
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
            ValueMetadata { idempotency_key },
        );
        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());
        // Use the new method that pre-sets initial value
        ValueActor::new_arc_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
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
            ValueMetadata { idempotency_key },
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
    ) -> Arc<ValueActor> {
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
            ValueMetadata { idempotency_key },
        );
        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());
        // Use the new method that pre-sets initial value
        ValueActor::new_arc_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
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
            ValueMetadata { idempotency_key },
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
    ) -> Arc<ValueActor> {
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
            ValueMetadata { idempotency_key },
        );
        // Use pending() stream - initial value is already set, no need for stream to emit
        // Using constant() here would cause duplicate emissions (version 1 from initial,
        // version 2 from stream)
        let value_stream = TypedStream::infinite(stream::pending());
        // Use the new method that pre-sets initial value
        ValueActor::new_arc_with_initial_value(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
            initial_value,
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
        reply: oneshot::Sender<Vec<(ItemId, Arc<ValueActor>)>>,
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
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Self {
        let change_stream = constant(ListChange::Replace {
            items: items.into(),
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
                let mut list = None;
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
                            } else {
                                if let ListChange::Replace { items } = &change {
                                    list = Some(items.clone());
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
                                    let first_change_to_send = ListChange::Replace { items: list.clone() };
                                    match change_sender.try_send(first_change_to_send) {
                                        Ok(()) => change_senders.push(change_sender),
                                        Err(e) if !e.is_disconnected() => change_senders.push(change_sender),
                                        Err(_) => {} // Disconnected, don't add
                                    }
                                } else {
                                    change_senders.push(change_sender);
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
                                change_senders.retain(|change_sender| {
                                    let change_to_send = ListChange::Replace { items: list.clone() };
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
    pub async fn snapshot(&self) -> Vec<(ItemId, Arc<ValueActor>)> {
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
        items: impl Into<Vec<Arc<ValueActor>>>,
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
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Value {
        Value::List(
            Self::new_arc(construct_info, construct_context, actor_context, items),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<Arc<ValueActor>>>,
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
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
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
        Arc::new(ValueActor::new(
            actor_construct_info,
            actor_context,
            value_stream,
            parser::PersistenceId::new(),
        ))
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

    /// Creates a List with persistence support.
    /// - If saved data exists, it's loaded and used as initial items (code items are ignored)
    /// - On any change, the current list state is saved to storage
    pub fn new_arc_value_actor_with_persistence(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        code_items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
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
                code_items: Vec<Arc<ValueActor>>,
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
                                    for item in items {
                                        // ValueActor::stream() returns a Future, await it to get the Stream
                                        if let Some(value) = item.clone().stream().await.next().await {
                                            json_items.push(value.to_json().await);
                                        }
                                    }
                                    construct_storage_for_save.save_state(persistence_id, &json_items).await;
                                } else {
                                    // For incremental changes, we need to get the full list and save it
                                    let mut list_stream = pin!(list_for_save.clone().stream());
                                    if let Some(ListChange::Replace { items }) = list_stream.next().await {
                                        let mut json_items = Vec::new();
                                        for item in &items {
                                            // ValueActor::stream() returns a Future, await it to get the Stream
                                            if let Some(value) = item.clone().stream().await.next().await {
                                                json_items.push(value.to_json().await);
                                            }
                                        }
                                        construct_storage_for_save.save_state(persistence_id, &json_items).await;
                                    }
                                }
                            }
                        });

                        let value = Value::List(list_arc, ValueMetadata { idempotency_key });
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

        Arc::new(ValueActor::new(
            actor_construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
        ))
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
        value: Arc<ValueActor>,
    },
    /// Remove item by its stable ID
    Remove { id: ItemId },
    /// Update item's value (ID stays the same)
    Update { id: ItemId, value: Arc<ValueActor> },
    /// Full replacement (when diffs would be larger than snapshot)
    Replace { items: Vec<(ItemId, Arc<ValueActor>)> },
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
    current_snapshot: Vec<(ItemId, Arc<ValueActor>)>,
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
    pub fn snapshot(&self) -> &[(ItemId, Arc<ValueActor>)] {
        &self.current_snapshot
    }
}

// --- ListChange ---

#[derive(Clone)]
pub enum ListChange {
    Replace { items: Vec<Arc<ValueActor>> },
    InsertAt { index: usize, item: Arc<ValueActor> },
    UpdateAt { index: usize, item: Arc<ValueActor> },
    Remove { id: parser::PersistenceId },
    Move { old_index: usize, new_index: usize },
    Push { item: Arc<ValueActor> },
    Pop,
    Clear,
}

impl ListChange {
    pub fn apply_to_vec(self, vec: &mut Vec<Arc<ValueActor>>) {
        match self {
            Self::Replace { items } => {
                *vec = items;
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
    pub fn to_diff(&self, snapshot: &[(ItemId, Arc<ValueActor>)]) -> ListDiff {
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
        source_list_actor: Arc<ValueActor>,
        config: ListBindingConfig,
    ) -> Arc<ValueActor> {
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
        source_list_actor: Arc<ValueActor>,
        config: Arc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
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
            source_list_actor.clone().stream_sync()
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

            // Track both length AND mapping from original PersistenceIds to mapped PersistenceIds.
            // This is needed because mapped items get new PersistenceIds, so Remove { id }
            // must be translated to use the mapped item's PersistenceId.
            type MapState = (usize, HashMap<parser::PersistenceId, parser::PersistenceId>);
            list.stream().scan((0usize, HashMap::new()), move |state: &mut MapState, change| {
                let (length, pid_map) = state;
                let (transformed_change, new_length) = Self::transform_list_change_for_map_with_tracking(
                    change,
                    *length,
                    pid_map,
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

        Arc::new(ValueActor::new(
            construct_info,
            actor_context,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            parser::PersistenceId::new(),
        ))
    }

    /// Creates a retain actor that filters items based on predicate evaluation.
    /// Uses pure stream combinators (no spawned tasks, no channels) following
    /// the same pattern as `create_remove_actor`.
    fn create_retain_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Arc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync()
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
            // This avoids recreating all streams on every list change

            // State: (items, predicates, predicate_results, list_stream, merged_predicate_stream)
            type RetainState = (
                Vec<Arc<ValueActor>>,                    // items
                Vec<Arc<ValueActor>>,                    // predicates
                Vec<Option<bool>>,                       // predicate_results
                Pin<Box<dyn Stream<Item = ListChange>>>, // list_stream
                Option<Pin<Box<dyn Stream<Item = (usize, bool)>>>>, // merged predicates
            );

            let list_stream: Pin<Box<dyn Stream<Item = ListChange>>> = Box::pin(list.stream());

            stream::unfold(
                (
                    Vec::<Arc<ValueActor>>::new(),
                    Vec::<Arc<ValueActor>>::new(),
                    Vec::<Option<bool>>::new(),
                    list_stream,
                    None::<Pin<Box<dyn Stream<Item = (usize, bool)>>>>,
                    config,
                    construct_context,
                    actor_context,
                ),
                move |(mut items, mut predicates, mut predicate_results, mut list_stream, mut merged_predicates, config, construct_context, actor_context)| {
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
                                        // List structure changed - need to rebuild predicates
                                        match change {
                                            ListChange::Replace { items: new_items } => {
                                                items = new_items.clone();
                                                predicates = new_items.iter().enumerate().map(|(idx, item)| {
                                                    Self::transform_item(
                                                        item.clone(),
                                                        idx,
                                                        &config,
                                                        construct_context.clone(),
                                                        actor_context.clone(),
                                                    )
                                                }).collect();
                                                predicate_results = vec![None; items.len()];

                                                // Rebuild merged predicate stream
                                                if items.is_empty() {
                                                    merged_predicates = None;
                                                    return Some((
                                                        Some(ListChange::Replace { items: vec![] }),
                                                        (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                    ));
                                                } else {
                                                    // Query initial values directly (no channel overhead)
                                                    for (idx, pred) in predicates.iter().enumerate() {
                                                        if let Ok(value) = pred.clone().value().await {
                                                            let is_true = matches!(&value, Value::Tag(tag, _) if tag.tag() == "True");
                                                            predicate_results[idx] = Some(is_true);
                                                        }
                                                    }

                                                    // Use stream_from_now_sync for future updates only
                                                    let pred_streams: Vec<_> = predicates.iter()
                                                        .enumerate()
                                                        .map(|(idx, pred)| {
                                                            pred.clone().stream_from_now_sync()
                                                                .map(move |v| {
                                                                    let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                                    (idx, is_true)
                                                                })
                                                        })
                                                        .collect();
                                                    merged_predicates = Some(Box::pin(stream::select_all(pred_streams)));

                                                    // Emit initial filtered result immediately
                                                    let filtered: Vec<_> = items.iter()
                                                        .zip(predicate_results.iter())
                                                        .filter_map(|(item, r)| if r == &Some(true) { Some(item.clone()) } else { None })
                                                        .collect();
                                                    return Some((
                                                        Some(ListChange::Replace { items: filtered }),
                                                        (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                    ));
                                                }
                                            }
                                            ListChange::Push { item } => {
                                                let idx = items.len();
                                                let pred = Self::transform_item(
                                                    item.clone(),
                                                    idx,
                                                    &config,
                                                    construct_context.clone(),
                                                    actor_context.clone(),
                                                );
                                                items.push(item);
                                                predicates.push(pred.clone());

                                                // Query new predicate value directly
                                                let is_true = if let Ok(value) = pred.clone().value().await {
                                                    matches!(&value, Value::Tag(tag, _) if tag.tag() == "True")
                                                } else {
                                                    false
                                                };
                                                predicate_results.push(Some(is_true));

                                                // Use stream_from_now_sync for future updates only
                                                let pred_streams: Vec<_> = predicates.iter()
                                                    .enumerate()
                                                    .map(|(idx, pred)| {
                                                        pred.clone().stream_from_now_sync()
                                                            .map(move |v| {
                                                                let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                                (idx, is_true)
                                                            })
                                                    })
                                                    .collect();
                                                merged_predicates = Some(Box::pin(stream::select_all(pred_streams)));

                                                // Emit updated filtered result
                                                let filtered: Vec<_> = items.iter()
                                                    .zip(predicate_results.iter())
                                                    .filter_map(|(item, r)| if r == &Some(true) { Some(item.clone()) } else { None })
                                                    .collect();
                                                return Some((
                                                    Some(ListChange::Replace { items: filtered }),
                                                    (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                ));
                                            }
                                            ListChange::Remove { id } => {
                                                // Find item by PersistenceId
                                                if let Some(index) = items.iter().position(|item| item.persistence_id() == id) {
                                                    items.remove(index);
                                                    predicates.remove(index);
                                                    predicate_results.remove(index);

                                                    if items.is_empty() {
                                                        merged_predicates = None;
                                                        return Some((
                                                            Some(ListChange::Replace { items: vec![] }),
                                                            (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                        ));
                                                    } else {
                                                        // DON'T rebuild streams - keep existing merged_predicates
                                                        // Stale updates from removed indices will be ignored (idx >= len check below)

                                                        // Emit filtered result (predicate_results already cached)
                                                        let filtered: Vec<_> = items.iter()
                                                            .zip(predicate_results.iter())
                                                            .filter_map(|(item, r)| if r == &Some(true) { Some(item.clone()) } else { None })
                                                            .collect();
                                                        return Some((
                                                            Some(ListChange::Replace { items: filtered }),
                                                            (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                        ));
                                                    }
                                                }
                                                continue;
                                            }
                                            ListChange::Pop => {
                                                if !items.is_empty() {
                                                    items.pop();
                                                    predicates.pop();
                                                    predicate_results.pop();

                                                    if items.is_empty() {
                                                        merged_predicates = None;
                                                        return Some((
                                                            Some(ListChange::Replace { items: vec![] }),
                                                            (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                        ));
                                                    } else {
                                                        // DON'T rebuild streams - keep existing merged_predicates
                                                        // Stale updates from popped indices will be ignored (idx >= len check below)

                                                        // Emit filtered result (predicate_results already cached)
                                                        let filtered: Vec<_> = items.iter()
                                                            .zip(predicate_results.iter())
                                                            .filter_map(|(item, r)| if r == &Some(true) { Some(item.clone()) } else { None })
                                                            .collect();
                                                        return Some((
                                                            Some(ListChange::Replace { items: filtered }),
                                                            (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                        ));
                                                    }
                                                }
                                                continue;
                                            }
                                            ListChange::Clear => {
                                                items.clear();
                                                predicates.clear();
                                                predicate_results.clear();
                                                merged_predicates = None;
                                                return Some((
                                                    Some(ListChange::Replace { items: vec![] }),
                                                    (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                                ));
                                            }
                                            _ => continue,
                                        }
                                    }
                                    Either::Left((None, _)) => {
                                        // List stream ended
                                        return None;
                                    }
                                    Either::Right((Some((idx, is_true)), _)) => {
                                        // Predicate update
                                        if idx < predicate_results.len() {
                                            predicate_results[idx] = Some(is_true);
                                        }

                                        // Emit if all predicates evaluated
                                        if predicate_results.iter().all(|r| r.is_some()) {
                                            let filtered: Vec<_> = items.iter()
                                                .zip(predicate_results.iter())
                                                .filter_map(|(item, r)| if r == &Some(true) { Some(item.clone()) } else { None })
                                                .collect();
                                            return Some((
                                                Some(ListChange::Replace { items: filtered }),
                                                (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                            ));
                                        }
                                        continue;
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
                                        items = new_items.clone();
                                        predicates = new_items.iter().enumerate().map(|(idx, item)| {
                                            Self::transform_item(
                                                item.clone(),
                                                idx,
                                                &config,
                                                construct_context.clone(),
                                                actor_context.clone(),
                                            )
                                        }).collect();
                                        predicate_results = vec![None; items.len()];

                                        if items.is_empty() {
                                            return Some((
                                                Some(ListChange::Replace { items: vec![] }),
                                                (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                            ));
                                        } else {
                                            // Query initial values directly (no channel overhead)
                                            for (idx, pred) in predicates.iter().enumerate() {
                                                if let Ok(value) = pred.clone().value().await {
                                                    let is_true = matches!(&value, Value::Tag(tag, _) if tag.tag() == "True");
                                                    predicate_results[idx] = Some(is_true);
                                                }
                                            }

                                            // Use stream_from_now_sync for future updates only
                                            let pred_streams: Vec<_> = predicates.iter()
                                                .enumerate()
                                                .map(|(idx, pred)| {
                                                    pred.clone().stream_from_now_sync()
                                                        .map(move |v| {
                                                            let is_true = matches!(&v, Value::Tag(tag, _) if tag.tag() == "True");
                                                            (idx, is_true)
                                                        })
                                                })
                                                .collect();
                                            merged_predicates = Some(Box::pin(stream::select_all(pred_streams)));

                                            // Emit initial filtered result immediately
                                            let filtered: Vec<_> = items.iter()
                                                .zip(predicate_results.iter())
                                                .filter_map(|(item, r)| if r == &Some(true) { Some(item.clone()) } else { None })
                                                .collect();
                                            return Some((
                                                Some(ListChange::Replace { items: filtered }),
                                                (items, predicates, predicate_results, list_stream, merged_predicates, config, construct_context, actor_context)
                                            ));
                                        }
                                    }
                                    Some(_) => continue,
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

        Arc::new(ValueActor::new(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            parser::PersistenceId::new(),
        ))
    }

    /// Creates a remove actor that removes items when their `when` event fires.
    /// Tracks removed items by PersistenceId so they don't reappear on upstream Replace.
    fn create_remove_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Arc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        use std::collections::HashSet;

        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Use switch_map for proper list replacement handling
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync()
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
            type ItemEntry = (usize, Arc<ValueActor>, Arc<ValueActor>, Option<TaskHandle>);
            // (internal_idx, item, when_actor, task_handle)

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
                ),
                move |state, event| {
                    let (items, removed_pids, remove_tx, config, construct_context, actor_context, next_idx) = state;

                    match event {
                        RemoveEvent::ListChange(change) => {
                            match change {
                                ListChange::Replace { items: new_items } => {
                                    // Filter out items we've already removed (by PersistenceId)
                                    // and rebuild tracking for remaining items
                                    items.clear();
                                    let mut filtered_items = Vec::new();

                                    for item in new_items.iter() {
                                        let persistence_id = item.persistence_id();
                                        if removed_pids.contains(&persistence_id) {
                                            // This item was removed by this List/remove, skip it
                                            continue;
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
                                        // Spawn task to listen for `when` event
                                        let mut tx = remove_tx.clone();
                                        let when_clone = when_actor.clone();
                                        let task_handle = Task::start_droppable(async move {
                                            let mut stream = when_clone.stream_from_now_sync();
                                            // Wait for ANY emission - that triggers removal
                                            if stream.next().await.is_some() {
                                                // Channel may be closed if filter was removed - that's fine
                                                let _ = tx.send(persistence_id).await;
                                            }
                                        });
                                        items.push((idx, item.clone(), when_actor, Some(task_handle)));
                                        filtered_items.push(item.clone());
                                    }

                                    // Clean up removed_pids: remove entries for items no longer in upstream
                                    // This ensures bounded growth - we only track items that exist upstream
                                    let upstream_pids: HashSet<_> = new_items.iter()
                                        .map(|item| item.persistence_id())
                                        .collect();
                                    removed_pids.retain(|pid| upstream_pids.contains(pid));

                                    return future::ready(Some(Some(ListChange::Replace { items: filtered_items })));
                                }
                                ListChange::Push { item } => {
                                    let persistence_id = item.persistence_id();
                                    // If this item was previously removed, don't add it back
                                    if removed_pids.contains(&persistence_id) {
                                        return future::ready(Some(None));
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
                                    // Spawn task to listen for `when` event
                                    let mut tx = remove_tx.clone();
                                    let when_clone = when_actor.clone();
                                    let task_handle = Task::start_droppable(async move {
                                        let mut stream = when_clone.stream_from_now_sync();
                                        if stream.next().await.is_some() {
                                            let _ = tx.send(persistence_id).await;
                                        }
                                    });
                                    items.push((idx, item.clone(), when_actor, Some(task_handle)));
                                    return future::ready(Some(Some(ListChange::Push { item })));
                                }
                                ListChange::Remove { id } => {
                                    // Upstream removed an item - remove from our tracking
                                    // Also remove from removed_pids (item is gone from upstream)
                                    removed_pids.remove(&id);
                                    if let Some(pos) = items.iter().position(|(_, item, _, _)| item.persistence_id() == id) {
                                        items.remove(pos);
                                    }
                                    // Forward the Remove downstream
                                    return future::ready(Some(Some(ListChange::Remove { id })));
                                }
                                ListChange::Clear => {
                                    items.clear();
                                    removed_pids.clear();
                                    *next_idx = 0;
                                    return future::ready(Some(Some(ListChange::Clear)));
                                }
                                ListChange::Pop => {
                                    if let Some((_, item, _, _)) = items.pop() {
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
                            if let Some(pos) = items.iter().position(|(_, item, _, _)| item.persistence_id() == persistence_id) {
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

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/remove result",
            ),
            actor_context_for_list,
            value_stream,
            source_list_actor.clone(),
        );

        Arc::new(ValueActor::new(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            parser::PersistenceId::new(),
        ))
    }

    /// Creates an every/any actor that produces True/False based on predicates.
    fn create_every_any_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Arc<ListBindingConfig>,
        is_every: bool, // true = every, false = any
    ) -> Arc<ValueActor> {
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
            source_list_actor.clone().stream_sync()
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
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(),
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
                        _ => {}
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

                let predicate_streams: Vec<_> = item_predicates.iter().enumerate().map(|(idx, (_, pred))| {
                    pred.clone().stream_sync().map(move |value| (idx, value))
                }).collect();

                stream::select_all(predicate_streams)
                    .scan(
                        vec![None::<bool>; item_predicates.len()],
                        move |states, (idx, value)| {
                            let is_true = match &value {
                                Value::Tag(tag, _) => tag.tag() == "True",
                                _ => false,
                            };
                            if idx < states.len() {
                                states[idx] = Some(is_true);
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

        Arc::new(ValueActor::new(
            construct_info,
            actor_context_for_result,
            TypedStream::infinite(value_stream),
            parser::PersistenceId::new(),
        ))
    }

    /// Creates a sort_by actor that sorts list items based on a key expression.
    /// When any item's key changes, emits an updated sorted list.
    fn create_sort_by_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Arc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        let construct_info_id = construct_info.id.clone();

        // Clone for use after the flat_map chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Clone subscription scope for scope cancellation check
        let subscription_scope = actor_context.subscription_scope.clone();

        // Create a stream that:
        // 1. Subscribes to source list changes (stream_sync() yields automatically)
        // 2. For each item, evaluates key expression and subscribes to its changes
        // 3. When list or any key changes, emits sorted Replace
        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // subscriptions unnecessarily.
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync()
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
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(), // (item, key_actor)
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
                        _ => {}
                    }

                    future::ready(Some(item_keys.clone()))
                }
            ).flat_map(move |item_keys| {
                let construct_info_id = construct_info_id_inner.clone();

                if item_keys.is_empty() {
                    // Empty list - emit empty Replace
                    return stream::once(future::ready(ListChange::Replace { items: vec![] })).boxed_local();
                }

                // Subscribe to all keys and emit sorted list when any changes
                let key_streams: Vec<_> = item_keys.iter().enumerate().map(|(idx, (item, key_actor))| {
                    let item = item.clone();
                    key_actor.clone().stream_sync().map(move |value| (idx, item.clone(), value))
                }).collect();

                stream::select_all(key_streams)
                    .scan(
                        item_keys.iter().map(|(item, _)| (item.clone(), None::<SortKey>)).collect::<Vec<_>>(),
                        move |states, (idx, item, value)| {
                            // Extract sortable key from value
                            let sort_key = SortKey::from_value(&value);
                            if idx < states.len() {
                                states[idx] = (item, Some(sort_key));
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
                                let sorted: Vec<Arc<ValueActor>> = indexed_items.into_iter()
                                    .map(|(_, item, _)| item)
                                    .collect();
                                future::ready(Some(Some(ListChange::Replace { items: sorted })))
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

        Arc::new(ValueActor::new(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            parser::PersistenceId::new(),
        ))
    }

    /// Transform a single list item using the config's transform expression.
    fn transform_item(
        item_actor: Arc<ValueActor>,
        index: usize,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> Arc<ValueActor> {
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
        let new_actor_context = ActorContext {
            parameters: new_params,
            ..actor_context.with_child_scope(&scope_id)
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
                ValueActor::new_arc(
                    ConstructInfo::new(
                        result_actor.construct_info.id.clone().with_child_id("mapped"),
                        None,
                        "List/map mapped item",
                    ),
                    new_actor_context,
                    TypedStream::infinite(result_actor.stream_sync()),
                    original_pid,  // Preserve original PersistenceId!
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
                let transformed_items: Vec<Arc<ValueActor>> = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        Self::transform_item(
                            item,
                            index,
                            config,
                            construct_context.clone(),
                            actor_context.clone(),
                        )
                    })
                    .collect();
                (ListChange::Replace { items: transformed_items }, new_length)
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
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> (ListChange, usize) {
        match change {
            ListChange::Replace { items } => {
                let new_length = items.len();
                // Clear old mapping and build new one
                pid_map.clear();
                let transformed_items: Vec<Arc<ValueActor>> = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let original_pid = item.persistence_id();
                        let transformed = Self::transform_item(
                            item,
                            index,
                            config,
                            construct_context.clone(),
                            actor_context.clone(),
                        );
                        let mapped_pid = transformed.persistence_id();
                        pid_map.insert(original_pid, mapped_pid);
                        transformed
                    })
                    .collect();
                (ListChange::Replace { items: transformed_items }, new_length)
            }
            ListChange::InsertAt { index, item } => {
                let original_pid = item.persistence_id();
                let transformed_item = Self::transform_item(
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (ListChange::InsertAt { index, item: transformed_item }, current_length + 1)
            }
            ListChange::UpdateAt { index, item } => {
                let original_pid = item.persistence_id();
                let transformed_item = Self::transform_item(
                    item,
                    index,
                    config,
                    construct_context,
                    actor_context,
                );
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (ListChange::UpdateAt { index, item: transformed_item }, current_length)
            }
            ListChange::Push { item } => {
                let original_pid = item.persistence_id();
                let transformed_item = Self::transform_item(
                    item,
                    current_length,
                    config,
                    construct_context,
                    actor_context,
                );
                let mapped_pid = transformed_item.persistence_id();
                pid_map.insert(original_pid, mapped_pid);
                (ListChange::Push { item: transformed_item }, current_length + 1)
            }
            ListChange::Remove { id } => {
                // Translate the original PersistenceId to the mapped PersistenceId
                if let Some(mapped_pid) = pid_map.remove(&id) {
                    (ListChange::Remove { id: mapped_pid }, current_length.saturating_sub(1))
                } else {
                    // Item not found in mapping - this shouldn't happen, but pass through
                    zoon::println!("[List/map] WARNING: Remove for unknown PersistenceId {:?}", id);
                    (ListChange::Remove { id }, current_length.saturating_sub(1))
                }
            }
            ListChange::Move { old_index, new_index } => (ListChange::Move { old_index, new_index }, current_length),
            ListChange::Pop => {
                // Pop removes the last item, but we don't know its PersistenceId here
                // This is a limitation - Pop should be avoided with identity-based Remove
                (ListChange::Pop, current_length.saturating_sub(1))
            }
            ListChange::Clear => {
                pid_map.clear();
                (ListChange::Clear, 0)
            }
        }
    }
}
