// @TODO remove
#![allow(dead_code)]

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
use super::evaluator::evaluate_static_expression;

use ulid::Ulid;

use zoon::IntoCowStr;
use zoon::future;
use zoon::futures_channel::{mpsc, oneshot};
use zoon::futures_util::select;
use zoon::futures_util::stream::{self, LocalBoxStream, Stream, StreamExt};
use zoon::{Deserialize, DeserializeOwned, Serialize, serde, serde_json};
use zoon::{Task, TaskHandle};
use zoon::{WebStorage, local_storage};
use zoon::{eprintln, println};

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
const LOG_DROPS_AND_LOOP_ENDS: bool = true;

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

    // State as tuple: (outer_stream, inner_stream_opt, map_fn)
    let initial: (FusedOuter<S::Item>, Option<FusedInner<U::Item>>, F) = (
        outer.boxed_local().fuse(),
        None,
        f,
    );

    stream::unfold(initial, |state| async move {
        use zoon::futures_util::future::Either;

        // Destructure state - we need to rebuild it for the return
        let (mut outer_stream, mut inner_opt, map_fn) = state;

        loop {
            match &mut inner_opt {
                Some(inner) if !inner.is_terminated() => {
                    // Both streams active - race between them
                    let outer_fut = outer_stream.next();
                    let inner_fut = inner.next();

                    match future::select(pin!(outer_fut), pin!(inner_fut)).await {
                        Either::Left((outer_opt, _)) => {
                            match outer_opt {
                                Some(value) => {
                                    // Switch! Drop old inner by replacing
                                    inner_opt = Some(map_fn(value).boxed_local().fuse());
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
                        None => return None, // Outer ended
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
/// # Usage
/// ```ignore
/// let backpressured = BackpressuredStream::new(source_stream);
/// let demand_tx = backpressured.demand_sender();
/// backpressured.signal_initial_demand(); // Allow first value
///
/// // In processing loop, after handling each value:
/// let _ = demand_tx.unbounded_send(()); // Signal ready for next
/// ```
#[pin_project::pin_project]
pub struct BackpressuredStream<S> {
    #[pin]
    inner: S,
    #[pin]
    demand_rx: mpsc::UnboundedReceiver<()>,
    demand_tx: mpsc::UnboundedSender<()>,
    awaiting_demand: bool,
}

impl<S> BackpressuredStream<S> {
    pub fn new(inner: S) -> Self {
        let (demand_tx, demand_rx) = mpsc::unbounded();
        Self {
            inner,
            demand_rx,
            demand_tx,
            awaiting_demand: true,
        }
    }

    /// Get a cloneable sender to signal demand.
    /// Call `demand_sender.unbounded_send(())` to allow next value through.
    pub fn demand_sender(&self) -> mpsc::UnboundedSender<()> {
        self.demand_tx.clone()
    }

    /// Signal initial demand (call once after creation to start flow).
    pub fn signal_initial_demand(&self) {
        let _ = self.demand_tx.unbounded_send(());
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

// --- BackpressurePermit ---

// RefCell removed - using actor model with channels instead
use std::task::Waker;

/// A permit-based synchronization primitive for backpressure between producer and consumer.
///
/// Used by HOLD to ensure THEN processes one value at a time AND waits for state update.
///
/// # How it works:
/// 1. HOLD creates permit with initial count = 1
/// 2. THEN acquires permit before each body evaluation (blocks if count = 0)
/// 3. THEN evaluates body and emits result (permit count stays at 0)
/// 4. HOLD receives result, updates state, releases permit (count = 0 → 1)
/// 5. THEN can now acquire for next body evaluation
///
/// This guarantees that HOLD's state update completes before THEN's next body starts.
///
/// Uses thread-safe primitives (Arc + AtomicUsize + Mutex) instead of Rc<Cell>/Rc<RefCell>
/// to support WebWorkers and to allow callbacks in Arc<dyn Fn>.
#[derive(Clone)]
pub struct BackpressurePermit {
    available: Arc<std::sync::atomic::AtomicUsize>,
    waker: Arc<std::sync::Mutex<Option<Waker>>>,
}

impl BackpressurePermit {
    /// Create a new permit with the given initial count.
    /// For HOLD/THEN synchronization, use initial = 1.
    pub fn new(initial: usize) -> Self {
        use std::sync::atomic::AtomicUsize;
        BackpressurePermit {
            available: Arc::new(AtomicUsize::new(initial)),
            waker: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Release a permit, incrementing the available count.
    /// Wakes the waiting task if one exists.
    /// Called by HOLD after updating state.
    pub fn release(&self) {
        use std::sync::atomic::Ordering;
        let old = self.available.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.waker.lock() {
            if let Some(waker) = guard.take() {
                waker.wake();
            }
        }
    }

    /// Acquire a permit asynchronously.
    /// If no permit is available (count = 0), waits until one is released.
    /// Called by THEN before each body evaluation.
    pub async fn acquire(&self) {
        use std::sync::atomic::Ordering;
        std::future::poll_fn(|cx| {
            // Try to decrement if available > 0
            loop {
                let available = self.available.load(Ordering::SeqCst);
                if available > 0 {
                    // Try to atomically decrement
                    if self.available.compare_exchange(
                        available,
                        available - 1,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ).is_ok() {
                        return Poll::Ready(());
                    }
                    // CAS failed, retry
                    continue;
                } else {
                    if let Ok(mut guard) = self.waker.lock() {
                        *guard = Some(cx.waker().clone());
                    }
                    return Poll::Pending;
                }
            }
        }).await
    }
}

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
    /// Channel for subscribers to request the next value
    request_tx: mpsc::UnboundedSender<LazyValueRequest>,
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
        let (request_tx, request_rx) = mpsc::unbounded::<LazyValueRequest>();

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
        mut request_rx: mpsc::UnboundedReceiver<LazyValueRequest>,
    ) {
        let mut source = Box::pin(source_stream);
        let mut buffer: Vec<Value> = Vec::new();
        let mut cursors: HashMap<usize, usize> = HashMap::new();
        let mut source_exhausted = false;
        const CLEANUP_THRESHOLD: usize = 100;

        println!("[DEBUG LazyValueActor] Loop started: {}", construct_info.description);

        while let Some(request) = request_rx.next().await {
            let cursor = cursors.entry(request.subscriber_id).or_insert(0);

            let value = if *cursor < buffer.len() {
                // Return buffered value (replay for this subscriber)
                println!("[DEBUG LazyValueActor] Returning buffered value at cursor {} for subscriber {}",
                    *cursor, request.subscriber_id);
                Some(buffer[*cursor].clone())
            } else if source_exhausted {
                // Source is exhausted, no more values
                println!("[DEBUG LazyValueActor] Source exhausted for subscriber {}",
                    request.subscriber_id);
                None
            } else {
                // Poll source for next value (demand-driven pull!)
                println!("[DEBUG LazyValueActor] Pulling from source for subscriber {}",
                    request.subscriber_id);
                match source.next().await {
                    Some(value) => {
                        println!("[DEBUG LazyValueActor] Got value from source, buffering");
                        buffer.push(value.clone());
                        Some(value)
                    }
                    None => {
                        println!("[DEBUG LazyValueActor] Source exhausted");
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
                    println!("[DEBUG LazyValueActor] Cleaning buffer, min_cursor={}", min_cursor);
                    // Remove consumed values from buffer
                    buffer.drain(0..min_cursor);
                    // Adjust all cursors
                    for c in cursors.values_mut() {
                        *c -= min_cursor;
                    }
                }
            }

            // Send response to subscriber
            let _ = request.response_tx.send(value);
        }

        if LOG_DROPS_AND_LOOP_ENDS {
            println!("LazyValueActor loop ended: {}", construct_info);
        }
    }

    /// Subscribe to this lazy actor's values.
    ///
    /// Returns a stream that pulls values on demand.
    /// Each call to .next() on the stream will request the next value from the actor.
    ///
    /// Takes ownership of the Arc to keep the actor alive for the subscription lifetime.
    /// Callers should use `.clone().subscribe()` if they need to retain a reference.
    pub fn subscribe(self: Arc<Self>) -> LazySubscription {
        let subscriber_id = self.subscriber_counter.fetch_add(1, Ordering::SeqCst);
        println!("[DEBUG LazyValueActor] New subscription, subscriber_id={}, construct={}",
            subscriber_id, self.construct_info.description);
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
            println!("Dropped LazyValueActor: {}", self.construct_info);
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

        // Try to send the request
        if this.actor.request_tx.unbounded_send(request).is_err() {
            // Actor dropped
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
    request_sender: mpsc::UnboundedSender<FsRequest>,
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
        let (tx, mut rx) = mpsc::unbounded::<FsRequest>();

        let actor_loop = ActorLoop::new(async move {
            let mut files: HashMap<String, String> = initial_files;

            while let Some(req) = rx.next().await {
                match req {
                    FsRequest::ReadText { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let result = files.get(&normalized).cloned();
                        let _ = reply.send(result);
                    }
                    FsRequest::WriteText { path, content } => {
                        let normalized = Self::normalize_path(&path);
                        files.insert(normalized, content);
                    }
                    FsRequest::Exists { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let exists = files.contains_key(&normalized);
                        let _ = reply.send(exists);
                    }
                    FsRequest::Delete { path, reply } => {
                        let normalized = Self::normalize_path(&path);
                        let was_present = files.remove(&normalized).is_some();
                        let _ = reply.send(was_present);
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
                        let _ = reply.send(entries);
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
        let _ = self.request_sender.unbounded_send(FsRequest::ReadText {
            path: path.to_string(),
            reply: tx,
        });
        rx.await.ok().flatten()
    }

    /// Write text content to a file (fire-and-forget)
    ///
    /// This is synchronous because it just sends a message to the actor.
    /// The actual write happens asynchronously in the actor loop.
    pub fn write_text(&self, path: &str, content: String) {
        let _ = self.request_sender.unbounded_send(FsRequest::WriteText {
            path: path.to_string(),
            content,
        });
    }

    /// List entries in a directory (async)
    pub async fn list_directory(&self, path: &str) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        let _ = self.request_sender.unbounded_send(FsRequest::ListDirectory {
            path: path.to_string(),
            reply: tx,
        });
        rx.await.unwrap_or_default()
    }

    /// Check if a file exists (async)
    pub async fn exists(&self, path: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        let _ = self.request_sender.unbounded_send(FsRequest::Exists {
            path: path.to_string(),
            reply: tx,
        });
        rx.await.unwrap_or(false)
    }

    /// Delete a file (async)
    pub async fn delete(&self, path: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        let _ = self.request_sender.unbounded_send(FsRequest::Delete {
            path: path.to_string(),
            reply: tx,
        });
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
    state_inserter_sender: mpsc::UnboundedSender<(
        parser::PersistenceId,
        serde_json::Value,
        oneshot::Sender<()>,
    )>,
    state_getter_sender: mpsc::UnboundedSender<(
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
        let (state_inserter_sender, mut state_inserter_receiver) = mpsc::unbounded();
        let (state_getter_sender, mut state_getter_receiver) = mpsc::unbounded();
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
                                eprintln!("Failed to save states: {error:#}");
                            }
                            if confirmation_sender.send(()).is_err() {
                                eprintln!("Failed to send save confirmation from construct storage");
                            }
                        },
                        (persistence_id, state_sender) = state_getter_receiver.select_next_some() => {
                            // @TODO Cheaper cloning? Replace get with remove?
                            let state = states.get(&persistence_id.to_string()).cloned();
                            if state_sender.send(state).is_err() {
                                eprintln!("Failed to send state from construct storage");
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
                eprintln!("Failed to save state: {error:#}");
                return;
            }
        };
        let (confirmation_sender, confirmation_receiver) = oneshot::channel::<()>();
        if let Err(error) = self.state_inserter_sender.unbounded_send((
            persistence_id,
            json_value,
            confirmation_sender,
        )) {
            eprintln!("Failed to save state: {error:#}")
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
        if let Err(error) = self
            .state_getter_sender
            .unbounded_send((persistence_id, state_sender))
        {
            eprintln!("Failed to load state: {error:#}")
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
    /// When true, code should use `.snapshot()` instead of `.stream()` for subscriptions.
    ///
    /// This flag propagates through function calls:
    /// - THEN/WHEN bodies set this to `true` (snapshot context)
    /// - WHILE bodies keep this `false` (streaming context)
    /// - User-defined function bodies inherit caller's context
    ///
    /// Code that needs values (variable references, API functions, operators) checks
    /// this flag and calls `.snapshot()` or `.stream()` accordingly.
    ///
    /// Default: false (streaming context - continuous updates).
    pub is_snapshot_context: bool,
}

// --- ActorOutputValveSignal ---

/// Actor for broadcasting impulses to multiple subscribers.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct ActorOutputValveSignal {
    impulse_sender_sender: mpsc::UnboundedSender<mpsc::UnboundedSender<()>>,
    actor_loop: ActorLoop,
}

impl ActorOutputValveSignal {
    pub fn new(impulse_stream: impl Stream<Item = ()> + 'static) -> Self {
        let (impulse_sender_sender, mut impulse_sender_receiver) =
            mpsc::unbounded::<mpsc::UnboundedSender<()>>();
        Self {
            impulse_sender_sender,
            actor_loop: ActorLoop::new(async move {
                let mut impulse_stream = pin!(impulse_stream.fuse());
                let mut impulse_senders = Vec::<mpsc::UnboundedSender<()>>::new();
                loop {
                    select! {
                        impulse = impulse_stream.next() => {
                            if impulse.is_none() { break };
                            impulse_senders.retain(|impulse_sender| {
                                if let Err(error) = impulse_sender.unbounded_send(()) {
                                    false
                                } else {
                                    true
                                }
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

    pub fn subscribe(&self) -> impl Stream<Item = ()> {
        let (impulse_sender, impulse_receiver) = mpsc::unbounded();
        if let Err(error) = self.impulse_sender_sender.unbounded_send(impulse_sender) {
            eprintln!("Failed to subscribe to actor output valve signal: {error:#}");
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

#[derive(Clone)]
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
    ThenCombinator,
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
    persistence_id: Option<parser::PersistenceId>,
    name: Cow<'static, str>,
    value_actor: Arc<ValueActor>,
    link_value_sender: Option<mpsc::UnboundedSender<Value>>,
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
        persistence_id: Option<parser::PersistenceId>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Variable),
            persistence_id,
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
        persistence_id: Option<parser::PersistenceId>,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info,
            construct_context,
            name,
            value_actor,
            persistence_id,
        ))
    }

    /// Create a new Arc<Variable> with a forwarding actor loop.
    /// The loop will be kept alive as long as the Variable exists.
    pub fn new_arc_with_forwarding_loop(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        value_actor: Arc<ValueActor>,
        persistence_id: Option<parser::PersistenceId>,
        forwarding_loop: ActorLoop,
    ) -> Arc<Self> {
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::Variable),
            persistence_id,
            name: name.into(),
            value_actor,
            link_value_sender: None,
            forwarding_loop: Some(forwarding_loop),
        })
    }

    pub fn persistence_id(&self) -> Option<parser::PersistenceId> {
        self.persistence_id
    }

    pub fn new_link_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        actor_context: ActorContext,
        persistence_id: Option<parser::PersistenceId>,
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
        let (link_value_sender, link_value_receiver) = mpsc::unbounded();
        // UnboundedReceiver is infinite - it never terminates unless sender is dropped
        let value_actor =
            ValueActor::new(actor_construct_info, actor_context, TypedStream::infinite(link_value_receiver), persistence_id);
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            name: name.into(),
            value_actor: Arc::new(value_actor),
            link_value_sender: Some(link_value_sender),
            forwarding_loop: None,
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
    /// Callers should use `.clone().subscribe()` if they need to retain a reference.
    pub fn subscribe(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        let subscription = self.value_actor.clone().subscribe();
        // Subscription keeps the actor alive; we also need to keep the Variable alive
        stream::unfold(
            (subscription, self),
            |(mut subscription, variable)| async move {
                subscription.next().await.map(|value| (value, (subscription, variable)))
            }
        ).boxed_local()
    }

    pub fn value_actor(&self) -> Arc<ValueActor> {
        self.value_actor.clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn link_value_sender(&self) -> Option<mpsc::UnboundedSender<Value>> {
        self.link_value_sender.clone()
    }

    pub fn expect_link_value_sender(&self) -> mpsc::UnboundedSender<Value> {
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
            println!("Dropped: {}", self.construct_info);
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
        // Capture snapshot mode flag before closures
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
        // The `subscribe()` method returns a `Subscription` that keeps the actor alive
        // for the duration of the subscription, so we no longer need the manual unfold pattern.
        let mut value_stream = stream::once(async move {
            root_value_actor.await
        })
            .flat_map(move |actor| {
                // Use snapshot() or stream() based on context - type-safe subscription
                if use_snapshot {
                    // Snapshot: convert Future to single-item Stream
                    stream::once(actor.snapshot())
                        .filter_map(|v| async { v })
                        .boxed_local()
                } else {
                    // Streaming: continuous updates
                    actor.stream()
                }
            })
            .boxed_local();
        // Collect parts to detect the last one
        let parts_vec: Vec<_> = alias_parts.into_iter().skip(skip_alias_parts).collect();
        let num_parts = parts_vec.len();

        for (idx, alias_part) in parts_vec.into_iter().enumerate() {
            let alias_part = alias_part.to_string();
            let _is_last = idx == num_parts - 1;

            // Process each field in the path using switch_map semantics:
            // When the outer stream emits a new value, cancel the old inner subscription
            // and start a new one. This is essential for LINK fields (when a new event arrives,
            // stop processing the old event's fields) but also correct for all fields.
            //
            // This makes LINK transparent: it's just a channel that forwards values.
            // The switch semantics come from the path traversal, not from LINK itself.
            value_stream = switch_map(value_stream, move |value| {
                let alias_part = alias_part.clone();
                match value {
                    Value::Object(object, _) => {
                        let variable = object.expect_variable(&alias_part);
                        let variable_actor = variable.value_actor();
                        // Use snapshot() or stream() based on context - type-safe subscription
                        if use_snapshot {
                            // Snapshot: get one value using the type-safe Future API
                            stream::once(async move {
                                let value = variable_actor.snapshot().await;
                                // Keep object and variable alive for the value's lifetime
                                let _ = (&object, &variable);
                                value
                            })
                            .filter_map(|v| async { v })
                            .boxed_local()
                        } else {
                            // Streaming: continuous updates
                            let subscription = variable_actor.stream();
                            stream::unfold(
                                (subscription, object, variable),
                                move |(mut subscription, object, variable)| async move {
                                    let value = subscription.next().await;
                                    value.map(|value| (value, (subscription, object, variable)))
                                }
                            ).boxed_local()
                        }
                    }
                    Value::TaggedObject(tagged_object, _) => {
                        let variable = tagged_object.expect_variable(&alias_part);
                        let variable_actor = variable.value_actor();
                        // Use snapshot() or stream() based on context - type-safe subscription
                        if use_snapshot {
                            // Snapshot: get one value using the type-safe Future API
                            stream::once(async move {
                                let value = variable_actor.snapshot().await;
                                // Keep tagged_object and variable alive for the value's lifetime
                                let _ = (&tagged_object, &variable);
                                value
                            })
                            .filter_map(|v| async { v })
                            .boxed_local()
                        } else {
                            // Streaming: continuous updates
                            let subscription = variable_actor.stream();
                            stream::unfold(
                                (subscription, tagged_object, variable),
                                move |(mut subscription, tagged_object, variable)| async move {
                                    let value = subscription.next().await;
                                    value.map(|value| (value, (subscription, tagged_object, variable)))
                                }
                            ).boxed_local()
                        }
                    }
                    other => panic!(
                        "Failed to get Object or TaggedObject to create VariableOrArgumentReference: The Value has a different type {}",
                        other.construct_info()
                    ),
                }
            }).boxed_local();
        }
        // Subscription-based streams are infinite (subscriptions never terminate first)
        Arc::new(ValueActor::new(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            None,
        ))
    }
}

// --- ReferenceConnector ---

/// Actor for connecting references to actors by span.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct ReferenceConnector {
    referenceable_inserter_sender: mpsc::UnboundedSender<(parser::Span, Arc<ValueActor>)>,
    referenceable_getter_sender:
        mpsc::UnboundedSender<(parser::Span, oneshot::Sender<Arc<ValueActor>>)>,
    actor_loop: ActorLoop,
}

impl ReferenceConnector {
    pub fn new() -> Self {
        let (referenceable_inserter_sender, referenceable_inserter_receiver) =
            mpsc::unbounded();
        let (referenceable_getter_sender, referenceable_getter_receiver) = mpsc::unbounded();
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
                                    println!("[DEBUG ReferenceConnector] Registering actor for span {:?}", span);
                                    if let Some(senders) = referenceable_senders.remove(&span) {
                                        println!("[DEBUG ReferenceConnector] Found {} waiting senders for span {:?}", senders.len(), span);
                                        for sender in senders {
                                            if sender.send(actor.clone()).is_err() {
                                                eprintln!("Failed to send referenceable actor from reference connector");
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
                                    println!("[DEBUG ReferenceConnector] Got request for span {:?}", span);
                                    if let Some(actor) = referenceables.get(&span) {
                                        println!("[DEBUG ReferenceConnector] Found actor for span {:?}", span);
                                        if referenceable_sender.send(actor.clone()).is_err() {
                                            eprintln!("Failed to send referenceable actor from reference connector");
                                        }
                                    } else {
                                        println!("[DEBUG ReferenceConnector] Actor NOT found for span {:?}, deferring", span);
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
                    println!("ReferenceConnector loop ended - all actors dropped");
                }
            }),
        }
    }

    pub fn register_referenceable(&self, span: parser::Span, actor: Arc<ValueActor>) {
        if let Err(error) = self
            .referenceable_inserter_sender
            .unbounded_send((span, actor))
        {
            eprintln!("Failed to register referenceable: {error:#}")
        }
    }

    // @TODO is &self enough?
    pub async fn referenceable(self: Arc<Self>, span: parser::Span) -> Arc<ValueActor> {
        println!("[DEBUG referenceable] Starting lookup for span {:?}", span);
        let (referenceable_sender, referenceable_receiver) = oneshot::channel();
        if let Err(error) = self
            .referenceable_getter_sender
            .unbounded_send((span, referenceable_sender))
        {
            eprintln!("Failed to register referenceable: {error:#}")
        }
        let actor = referenceable_receiver
            .await
            .expect("Failed to get referenceable from ReferenceConnector");

        // DEBUG: Log the actor's stored value
        if let Some(stored) = actor.stored_value() {
            let value_desc = match &stored {
                Value::Text(t, _) => format!("Text('{}')", t.text()),
                Value::Number(n, _) => format!("Number({})", n.number()),
                Value::Tag(_, _) => "Tag".to_string(),
                Value::Object(_, _) => "Object".to_string(),
                Value::TaggedObject(_, _) => "TaggedObject".to_string(),
                Value::List(_, _) => "List".to_string(),
                Value::Flushed(_, _) => "Flushed".to_string(),
            };
            println!("[DEBUG referenceable] Found actor for span {:?} with stored_value: {}", span, value_desc);
        } else {
            println!("[DEBUG referenceable] Found actor for span {:?} with NO stored_value", span);
        }

        actor
    }
}

// --- LinkConnector ---

/// Actor for connecting LINK variables with their setters.
/// Similar to ReferenceConnector but stores mpsc senders for LINK variables.
/// Uses ActorLoop internally to encapsulate the async task.
pub struct LinkConnector {
    link_inserter_sender: mpsc::UnboundedSender<(parser::Span, mpsc::UnboundedSender<Value>)>,
    link_getter_sender:
        mpsc::UnboundedSender<(parser::Span, oneshot::Sender<mpsc::UnboundedSender<Value>>)>,
    actor_loop: ActorLoop,
}

impl LinkConnector {
    pub fn new() -> Self {
        let (link_inserter_sender, link_inserter_receiver) = mpsc::unbounded();
        let (link_getter_sender, link_getter_receiver) = mpsc::unbounded();
        Self {
            link_inserter_sender,
            link_getter_sender,
            actor_loop: ActorLoop::new(async move {
                let mut links = HashMap::<parser::Span, mpsc::UnboundedSender<Value>>::new();
                let mut link_senders =
                    HashMap::<parser::Span, Vec<oneshot::Sender<mpsc::UnboundedSender<Value>>>>::new();
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
                                                eprintln!("Failed to send link sender from link connector");
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
                                            eprintln!("Failed to send link sender from link connector");
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
                    println!("LinkConnector loop ended - all links dropped");
                }
            }),
        }
    }

    /// Register a LINK variable's sender with its span.
    pub fn register_link(&self, span: parser::Span, sender: mpsc::UnboundedSender<Value>) {
        if let Err(error) = self
            .link_inserter_sender
            .unbounded_send((span, sender))
        {
            eprintln!("Failed to register link: {error:#}")
        }
    }

    /// Get a LINK variable's sender by its span.
    pub async fn link_sender(self: Arc<Self>, span: parser::Span) -> mpsc::UnboundedSender<Value> {
        let (link_sender, link_receiver) = oneshot::channel();
        if let Err(error) = self
            .link_getter_sender
            .unbounded_send((span, link_sender))
        {
            eprintln!("Failed to get link sender: {error:#}")
        }
        link_receiver
            .await
            .expect("Failed to get link sender from LinkConnector")
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
            .unwrap_or_else(Ulid::new);

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
                .map(|arg| arg.clone().subscribe().filter(|v| {
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
                None,
                inputs,
            )
        } else {
            Arc::new(ValueActor::new_with_inputs(
                construct_info,
                actor_context,
                TypedStream::infinite(combined_stream),
                None,
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
            .unwrap_or_else(Ulid::new);
        let storage = construct_context.construct_storage.clone();

        let value_stream =
            stream::select_all(inputs.iter().enumerate().map(|(index, value_actor)| {
                // Use subscribe() to properly handle lazy actors in HOLD body context
                value_actor.clone().subscribe().map(move |value| (index, value))
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
            None,
            inputs,
        ))
    }
}

// --- ThenCombinator ---

pub struct ThenCombinator {}

impl ThenCombinator {
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        observed: Arc<ValueActor>,
        impulse_sender: mpsc::UnboundedSender<()>,
        body: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        #[derive(Default, Copy, Clone, Serialize, Deserialize)]
        #[serde(crate = "serde")]
        struct State {
            observed_idempotency_key: Option<ValueIdempotencyKey>,
        }

        let construct_info = construct_info.complete(ConstructType::ThenCombinator);
        // If persistence is None (e.g., for dynamically evaluated expressions),
        // generate a fresh persistence ID at runtime
        let persistent_id = construct_info
            .persistence
            .map(|p| p.id)
            .unwrap_or_else(Ulid::new);
        let storage = construct_context.construct_storage.clone();

        let observed_for_subscribe = observed.clone();
        let send_impulse_loop = ActorLoop::new(
            observed_for_subscribe
                // Use subscribe() to properly handle lazy actors in HOLD body context
                .subscribe()
                .scan(true, {
                    let storage = storage.clone();
                    move |first_run, value| {
                        let storage = storage.clone();
                        let previous_first_run = *first_run;
                        *first_run = false;
                        async move {
                            if previous_first_run {
                                Some((
                                    storage.clone().load_state::<State>(persistent_id).await,
                                    value,
                                ))
                            } else {
                                Some((None, value))
                            }
                        }
                    }
                })
                .scan(State::default(), move |state, (new_state, value)| {
                    if let Some(new_state) = new_state {
                        *state = new_state;
                    }
                    let idempotency_key = value.idempotency_key();
                    let skip_value = state
                        .observed_idempotency_key
                        .is_some_and(|key| key == idempotency_key);
                    if !skip_value {
                        state.observed_idempotency_key = Some(idempotency_key);
                    }
                    let state = *state;
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
                .filter_map(future::ready)
                .for_each({
                    let construct_info = construct_info.clone();
                    move |_| {
                        if let Err(error) = impulse_sender.unbounded_send(()) {
                            eprintln!("Failed to send impulse in {construct_info}: {error:#}")
                        }
                        future::ready(())
                    }
                }),
        );
        // Use subscribe() to properly handle lazy body actors in HOLD context
        let value_stream = body.clone().subscribe().map(|mut value| {
            value.set_idempotency_key(ValueIdempotencyKey::new());
            value
        });
        // Subscription-based streams are infinite (subscriptions never terminate first)
        // Keep both observed and body alive as explicit dependencies
        // Also include the impulse actor loop in the stream state to keep it alive
        let value_stream = stream::unfold(
            (value_stream, send_impulse_loop, observed.clone()),
            |(mut inner_stream, actor_loop, observed)| async move {
                inner_stream.next().await.map(|value| (value, (inner_stream, actor_loop, observed)))
            }
        );
        Arc::new(ValueActor::new_with_inputs(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            None,
            vec![observed, body],
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
        // Use subscribe() to properly handle lazy actors in HOLD body context
        let value_stream = stream::select_all([
            operand_a.clone().subscribe().map(|v| (0usize, v)).boxed_local(),
            operand_b.clone().subscribe().map(|v| (1usize, v)).boxed_local(),
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
            None,
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

// --- WhenCombinator ---

/// Pattern matching combinator for WHEN expressions.
/// Matches an input value against patterns and returns the first matching arm's result.
pub struct WhenCombinator {}

/// A compiled arm for WHEN matching.
pub struct CompiledArm {
    pub matcher: Box<dyn Fn(&Value) -> bool + Send + Sync>,
    pub body: Arc<ValueActor>,
}

impl WhenCombinator {
    /// Creates a ValueActor for WHEN pattern matching.
    /// The arms are tried in order; first matching pattern's body is returned.
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        actor_context: ActorContext,
        input: Arc<ValueActor>,
        arms: Vec<CompiledArm>,
    ) -> Arc<ValueActor> {
        let construct_info = construct_info.complete(ConstructType::ValueActor);

        // Collect all arm bodies as dependencies to keep them alive
        let arm_bodies: Vec<Arc<ValueActor>> = arms.iter().map(|arm| arm.body.clone()).collect();
        let arms = Arc::new(arms);

        // For each input value, find the matching arm and emit its body's value
        // Use subscribe() to properly handle lazy actors in HOLD body context
        let value_stream = input
            .clone()
            .subscribe()
            .flat_map({
                let arms = arms.clone();
                move |input_value| {
                    // Find the first matching arm
                    let matched_arm = arms
                        .iter()
                        .find(|arm| (arm.matcher)(&input_value));

                    if let Some(arm) = matched_arm {
                        // Subscribe to the matching arm's body
                        arm.body.clone().subscribe().boxed_local()
                    } else {
                        // No match - this shouldn't happen if we have a wildcard default
                        // Return an empty stream
                        stream::empty().boxed_local()
                    }
                }
            });

        // Subscription-based streams are infinite (subscriptions never terminate first)
        // Keep input and all arm bodies alive as explicit dependencies
        let mut inputs = vec![input];
        inputs.extend(arm_bodies);
        Arc::new(ValueActor::new_with_inputs(
            construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            None,
            inputs,
        ))
    }
}

/// Create a matcher function for a pattern.
pub fn pattern_to_matcher(pattern: &crate::parser::Pattern) -> Box<dyn Fn(&Value) -> bool + Send + Sync> {
    match pattern {
        crate::parser::Pattern::WildCard => {
            Box::new(|_| true)
        }
        crate::parser::Pattern::Literal(lit) => {
            match lit {
                crate::parser::Literal::Number(n) => {
                    let n = *n;
                    Box::new(move |v| {
                        matches!(v, Value::Number(num, _) if num.number() == n)
                    })
                }
                crate::parser::Literal::Tag(tag) => {
                    let tag = tag.to_string();
                    Box::new(move |v| {
                        match v {
                            Value::Tag(t, _) => t.tag() == tag,
                            Value::TaggedObject(to, _) => to.tag == tag,
                            _ => false,
                        }
                    })
                }
                crate::parser::Literal::Text(text) => {
                    let text = text.to_string();
                    Box::new(move |v| {
                        matches!(v, Value::Text(t, _) if t.text() == text)
                    })
                }
            }
        }
        crate::parser::Pattern::Alias { name: _ } => {
            // Alias just binds the value, so it always matches
            // (Variable binding will be handled separately)
            Box::new(|_| true)
        }
        crate::parser::Pattern::TaggedObject { tag, variables: _ } => {
            let tag = tag.to_string();
            Box::new(move |v| {
                matches!(v, Value::TaggedObject(to, _) if to.tag == tag)
            })
        }
        crate::parser::Pattern::Object { variables: _ } => {
            // Object pattern matches any object
            Box::new(|v| matches!(v, Value::Object(_, _)))
        }
        crate::parser::Pattern::List { items: _ } => {
            // List pattern matches any list (detailed matching would check items)
            Box::new(|v| matches!(v, Value::List(_, _)))
        }
        crate::parser::Pattern::Map { entries: _ } => {
            // Map pattern - not fully supported yet
            Box::new(|_| false)
        }
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
    persistence_id: Option<parser::PersistenceId>,

    /// Message channel for actor communication.
    /// All subscription management happens via messages.
    message_sender: mpsc::UnboundedSender<ActorMessage>,

    /// Explicit dependency tracking - keeps input actors alive.
    /// This replaces the old `extra_owned_data` pattern.
    inputs: Vec<Arc<ValueActor>>,

    /// Current version number - increments on each value change.
    /// Used by Subscription for push-pull architecture.
    current_version: Arc<AtomicU64>,

    /// History of recent values for preventing message loss.
    /// Subscribers can pull all values since their last_seen_version.
    value_history: Arc<std::sync::Mutex<ValueHistory>>,

    /// Channel for registering new subscribers.
    /// Subscribers send their bounded sender here, actor loop stores them locally.
    /// Using bounded(1) channels prevents unbounded memory growth for slow consumers.
    notify_sender_sender: mpsc::UnboundedSender<mpsc::Sender<()>>,

    /// Shared notify senders for use by store_value_directly.
    /// This allows synchronous notification when values are stored directly.
    notify_senders: Arc<std::sync::Mutex<Vec<mpsc::Sender<()>>>>,

    /// The actor's internal loop.
    actor_loop: ActorLoop,

    /// Optional lazy delegate for demand-driven evaluation.
    /// When Some, subscribe() delegates to this lazy actor instead of the normal subscription.
    /// Used in HOLD body context where lazy evaluation is needed for sequential state updates.
    lazy_delegate: Option<Arc<LazyValueActor>>,

    /// Extra ActorLoops that should be kept alive with this actor.
    /// Used by HOLD to keep the driver loop alive - when the ValueActor is dropped,
    /// the extra loops are dropped too, cancelling their async tasks.
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
        persistence_id: Option<parser::PersistenceId>,
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
        persistence_id: Option<parser::PersistenceId>,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Self {
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = mpsc::unbounded::<ActorMessage>();
        let current_version = Arc::new(AtomicU64::new(0));
        // History of recent values for preventing message loss
        let value_history = Arc::new(std::sync::Mutex::new(ValueHistory::new(64)));
        // Channel for subscribers to register their notification senders
        let (notify_sender_sender, notify_sender_receiver) = mpsc::unbounded::<mpsc::Sender<()>>();
        // Shared notify senders - used by both main loop and store_value_directly
        let notify_senders: Arc<std::sync::Mutex<Vec<mpsc::Sender<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        // EAGER SYNCHRONOUS POLLING: TEMPORARILY DISABLED
        // The noop waker causes issues with futures that await channel responses.
        // When such a future returns Pending with noop waker stored, the waker never
        // gets called when the response arrives, leaving the stream stuck.
        // TODO: Either use a proper waker, or detect which streams are safe to sync-poll.
        let boxed_stream: std::pin::Pin<Box<dyn Stream<Item = Value>>> =
            Box::pin(value_stream.inner);
        let stream_ended_sync = false;
        let stream_ever_produced_sync = false;

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let current_version = current_version.clone();
            let value_history = value_history.clone();
            let notify_senders = notify_senders.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            // Keep inputs alive in the spawned task
            let _inputs = inputs.clone();

            async move {
                println!("[DEBUG ValueActor async loop] Started for {}", construct_info);
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.subscribe().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                // Use the boxed stream that was already polled synchronously
                let mut value_stream = boxed_stream.fuse();
                let mut message_receiver = pin!(message_receiver.fuse());
                let mut notify_sender_receiver = pin!(notify_sender_receiver.fuse());
                let mut migration_state = MigrationState::Normal;
                // Track if stream ever produced a value (for SKIP detection)
                // Initialize from sync polling phase
                let mut stream_ever_produced = stream_ever_produced_sync;
                // Track if stream has ended
                // Initialize from sync polling phase
                let mut stream_ended = stream_ended_sync;

                let mut loop_iteration = 0u64;
                loop {
                    loop_iteration += 1;
                    if loop_iteration <= 3 {
                        println!("[DEBUG ValueActor async loop] Iteration {} for {}", loop_iteration, construct_info);
                    }
                    select! {
                        // Handle new subscriber registrations
                        sender = notify_sender_receiver.next() => {
                            if let Some(mut sender) = sender {
                                // If stream ended without producing any value (SKIP case),
                                // don't register subscriber - just drop sender to signal completion
                                if stream_ended && !stream_ever_produced {
                                    // Drop sender - subscriber will see channel closed
                                    drop(sender);
                                } else {
                                    // Immediately notify - subscriber will check version and see current value if any
                                    let _ = sender.try_send(());
                                    notify_senders.lock().unwrap().push(sender);
                                }
                            }
                        }

                        // Handle messages from the message channel
                        // When message_sender is dropped (ValueActor dropped), this returns None
                        msg = message_receiver.next() => {
                            let Some(msg) = msg else {
                                // Channel closed - ValueActor was dropped, exit the loop
                                break;
                            };
                            match msg {
                                ActorMessage::StreamValue(value) => {
                                    // Value received from migration source - treat as new value
                                    // Increment version and store in history
                                    let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                    if let Ok(mut history) = value_history.lock() {
                                        history.add(new_version, value);
                                    }
                                    // Notify subscribers via bounded channels (try_send - if full, skip)
                                    notify_senders.lock().unwrap().retain_mut(|sender| {
                                        // try_send returns Err if receiver dropped or buffer full
                                        // If dropped, remove from list. If full, keep (subscriber has pending notification)
                                        match sender.try_send(()) {
                                            Ok(()) => true,
                                            Err(e) => !e.is_disconnected(),
                                        }
                                    });
                                }
                                ActorMessage::MigrateTo { target, transform } => {
                                    // Phase 3: Migration - not implemented yet
                                    migration_state = MigrationState::Migrating {
                                        target,
                                        transform,
                                        pending_batches: HashSet::new(),
                                        buffered_writes: Vec::new(),
                                    };
                                }
                                ActorMessage::MigrationBatch { batch_id, items, is_final: _ } => {
                                    // Phase 3: Receiving migration data
                                    if let MigrationState::Receiving { received_batches, source: _ } = &mut migration_state {
                                        received_batches.insert(batch_id, items);
                                        // TODO: Send BatchAck, rebuild state when complete
                                    }
                                }
                                ActorMessage::BatchAck { batch_id } => {
                                    // Phase 3: Batch acknowledged
                                    if let MigrationState::Migrating { pending_batches, .. } = &mut migration_state {
                                        pending_batches.remove(&batch_id);
                                    }
                                }
                                ActorMessage::MigrationComplete => {
                                    // Phase 3: Migration complete
                                    migration_state = MigrationState::Normal;
                                }
                                ActorMessage::RedirectSubscribers { target: _ } => {
                                    // Phase 3: Redirect - subscribers will pick up data via version checks
                                    migration_state = MigrationState::ShuttingDown;
                                }
                                ActorMessage::Shutdown => {
                                    break;
                                }
                            }
                        }

                        // Handle values from the input stream
                        new_value = value_stream.next() => {
                            println!("[DEBUG ValueActor loop] Got value from stream for {}", construct_info);
                            // Stream ended - but we DON'T break!
                            // Actors stay alive until explicit Shutdown.
                            // This prevents "receiver is gone" errors.
                            let Some(new_value) = new_value else {
                                // Stream ended
                                stream_ended = true;
                                // If stream ended without ever producing a value (SKIP case),
                                // close all notify channels so subscribers see completion
                                if !stream_ever_produced {
                                    // Clear senders - this drops them, closing channels
                                    // Subscribers will receive Poll::Ready(None)
                                    notify_senders.lock().unwrap().clear();
                                }
                                // Continue listening for messages
                                continue;
                            };

                            stream_ever_produced = true;
                            // Increment version and store in history
                            let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                            if let Ok(mut history) = value_history.lock() {
                                history.add(new_version, new_value.clone());
                            }
                            // Notify subscribers via bounded channels
                            notify_senders.lock().unwrap().retain_mut(|sender| {
                                match sender.try_send(()) {
                                    Ok(()) => true,
                                    Err(e) => !e.is_disconnected(),
                                }
                            });

                            // Handle migration forwarding
                            if let MigrationState::Migrating { buffered_writes, target, .. } = &mut migration_state {
                                buffered_writes.push(new_value.clone());
                                let _ = target.send_message(ActorMessage::StreamValue(new_value));
                            }
                        }

                        // Handle output valve impulses (for output gating)
                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() {
                                // Valve closed - but we DON'T break!
                                continue;
                            }
                            // Subscribers pull on demand - no explicit broadcast needed
                        }
                    }
                }

                // Explicit cleanup - inputs dropped when task ends
                drop(_inputs);

                if LOG_DROPS_AND_LOOP_ENDS {
                    println!("Loop ended {construct_info}");
                }
            }
        });

        Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs,
            current_version,
            value_history,
            notify_sender_sender,
            notify_senders,
            actor_loop,
            lazy_delegate: None,
            extra_loops: Vec::new(),
        }
    }

    /// Send a message to this actor.
    pub fn send_message(&self, msg: ActorMessage) -> Result<(), mpsc::TrySendError<ActorMessage>> {
        self.message_sender.unbounded_send(msg)
    }

    pub fn new_arc<S: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        value_stream: TypedStream<S, Infinite>,
        persistence_id: Option<parser::PersistenceId>,
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
        persistence_id: Option<parser::PersistenceId>,
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
        persistence_id: Option<parser::PersistenceId>,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Arc<Self> {
        // Create the lazy actor that does the actual work
        let lazy_actor = Arc::new(LazyValueActor::new(
            construct_info.clone(),
            value_stream,
            inputs.clone(),
        ));

        // Create a shell ValueActor with a constant empty stream (it won't be used)
        // The lazy_delegate will handle all subscriptions
        let construct_info = Arc::new(construct_info);
        let (message_sender, _message_receiver) = mpsc::unbounded::<ActorMessage>();
        let current_version = Arc::new(AtomicU64::new(0));
        let (notify_sender_sender, _notify_sender_receiver) = mpsc::unbounded::<mpsc::Sender<()>>();
        let value_history = Arc::new(std::sync::Mutex::new(ValueHistory::new(64)));
        let notify_senders = Arc::new(std::sync::Mutex::new(Vec::new()));

        // Create a no-op task (the lazy delegate owns the real processing)
        let actor_loop = ActorLoop::new(async {});

        Arc::new(Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs,
            current_version,
            value_history,
            notify_sender_sender,
            notify_senders,
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
        persistence_id: Option<parser::PersistenceId>,
        initial_value: Value,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Self {
        let value_stream = value_stream.inner;
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = mpsc::unbounded::<ActorMessage>();
        // Start at version 1 since we have an initial value
        let current_version = Arc::new(AtomicU64::new(1));
        let (notify_sender_sender, notify_sender_receiver) = mpsc::unbounded::<mpsc::Sender<()>>();
        // History of recent values for preventing message loss (start with initial value at version 1)
        let value_history = Arc::new(std::sync::Mutex::new({
            let mut history = ValueHistory::new(64);
            history.add(1, initial_value);
            history
        }));
        // Shared notify senders - used by both main loop and store_value_directly
        let notify_senders: Arc<std::sync::Mutex<Vec<mpsc::Sender<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let current_version = current_version.clone();
            let value_history = value_history.clone();
            let notify_senders = notify_senders.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            // Keep inputs alive in the spawned task
            let _inputs = inputs.clone();

            async move {
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.subscribe().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                let mut value_stream = pin!(value_stream.fuse());
                let mut message_receiver = pin!(message_receiver.fuse());
                let mut notify_sender_receiver = pin!(notify_sender_receiver.fuse());
                let migration_state = MigrationState::Normal;
                let mut stream_ever_produced = true; // We have initial value
                let mut stream_ended = false;

                loop {
                    select! {
                        sender = notify_sender_receiver.next() => {
                            if let Some(mut sender) = sender {
                                if stream_ended && !stream_ever_produced {
                                    drop(sender);
                                } else {
                                    let _ = sender.try_send(());
                                    notify_senders.lock().unwrap().push(sender);
                                }
                            }
                        }

                        msg = message_receiver.next() => {
                            let Some(msg) = msg else {
                                break;
                            };
                            match msg {
                                ActorMessage::StreamValue(value) => {
                                    // Increment version and store in history
                                    let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                    if let Ok(mut history) = value_history.lock() {
                                        history.add(new_version, value);
                                    }
                                    notify_senders.lock().unwrap().retain_mut(|sender| {
                                        match sender.try_send(()) {
                                            Ok(()) => true,
                                            Err(e) => !e.is_disconnected(),
                                        }
                                    });
                                }
                                _ => {} // Ignore other messages for simplicity
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
                            // Increment version and store in history
                            let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                            if let Ok(mut history) = value_history.lock() {
                                history.add(new_version, new_value);
                            }
                            notify_senders.lock().unwrap().retain_mut(|sender| {
                                match sender.try_send(()) {
                                    Ok(()) => true,
                                    Err(e) => !e.is_disconnected(),
                                }
                            });
                        }

                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() {
                                continue;
                            }
                        }
                    }
                }

                drop(_inputs);

                if LOG_DROPS_AND_LOOP_ENDS {
                    println!("Loop ended {construct_info}");
                }
            }
        });

        Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs,
            current_version,
            value_history,
            notify_sender_sender,
            notify_senders,
            actor_loop,
            lazy_delegate: None,
            extra_loops: Vec::new(),
        }
    }

    pub fn persistence_id(&self) -> Option<parser::PersistenceId> {
        self.persistence_id
    }

    /// Get the current version of this actor's value.
    pub fn version(&self) -> u64 {
        self.current_version.load(Ordering::SeqCst)
    }

    /// Get all values since a given version from the history buffer.
    /// Returns (values, oldest_available_version).
    /// If the requested version is older than the oldest in history,
    /// the caller should fall back to the current snapshot.
    pub fn get_values_since(&self, since_version: u64) -> (Vec<Value>, Option<u64>) {
        if let Ok(history) = self.value_history.lock() {
            let result = history.get_values_since(since_version);
            result
        } else {
            (Vec::new(), None)
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
        persistence_id: Option<parser::PersistenceId>,
    ) -> (Arc<Self>, mpsc::UnboundedSender<Value>) {
        let (sender, receiver) = mpsc::unbounded::<Value>();
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
        forwarding_sender: mpsc::UnboundedSender<Value>,
        source_actor: Arc<ValueActor>,
        initial_value: Option<Value>,
    ) -> ActorLoop {
        // Send initial value synchronously if provided
        if let Some(value) = initial_value {
            let _ = forwarding_sender.unbounded_send(value);
        }

        ActorLoop::new(async move {
            let mut subscription = source_actor.subscribe();
            while let Some(value) = subscription.next().await {
                if forwarding_sender.unbounded_send(value).is_err() {
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
        persistence_id: Option<parser::PersistenceId>,
        initial_value: Value,
    ) -> Arc<Self> {
        let construct_info = construct_info.complete(ConstructType::ValueActor);
        let value_stream = value_stream.inner;
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = mpsc::unbounded::<ActorMessage>();
        // Start at version 1 since we have an initial value
        let current_version = Arc::new(AtomicU64::new(1));
        let (notify_sender_sender, notify_sender_receiver) = mpsc::unbounded::<mpsc::Sender<()>>();
        // History of recent values for preventing message loss (start with initial value at version 1)
        let value_history = Arc::new(std::sync::Mutex::new({
            let mut history = ValueHistory::new(64);
            history.add(1, initial_value);
            history
        }));

        // Shared notify_senders for use by store_value_directly
        let notify_senders: Arc<std::sync::Mutex<Vec<mpsc::Sender<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let current_version = current_version.clone();
            let value_history = value_history.clone();
            let output_valve_signal = actor_context.output_valve_signal.clone();
            let notify_senders = notify_senders.clone();

            async move {
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.subscribe().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                let mut value_stream = pin!(value_stream.fuse());
                let mut message_receiver = pin!(message_receiver.fuse());
                let mut notify_sender_receiver = pin!(notify_sender_receiver.fuse());
                let stream_ever_produced = true; // We have an initial value
                let mut stream_ended = false;

                loop {
                    select! {
                        sender = notify_sender_receiver.next() => {
                            if let Some(mut sender) = sender {
                                if stream_ended && !stream_ever_produced {
                                    drop(sender);
                                } else {
                                    let _ = sender.try_send(());
                                    notify_senders.lock().unwrap().push(sender);
                                }
                            }
                        }

                        msg = message_receiver.next() => {
                            let Some(msg) = msg else {
                                break;
                            };
                            match msg {
                                ActorMessage::StreamValue(new_value) => {
                                    let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                    if let Ok(mut history) = value_history.lock() {
                                        history.add(new_version, new_value);
                                    }
                                    notify_senders.lock().unwrap().retain_mut(|sender| {
                                        match sender.try_send(()) {
                                            Ok(()) => true,
                                            Err(e) => !e.is_disconnected(),
                                        }
                                    });
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
                                if let Ok(mut history) = value_history.lock() {
                                    history.add(new_version, new_value);
                                }
                                notify_senders.lock().unwrap().retain_mut(|sender| {
                                    match sender.try_send(()) {
                                        Ok(()) => true,
                                        Err(e) => !e.is_disconnected(),
                                    }
                                });
                            } else {
                                stream_ended = true;
                            }
                        }
                    }
                }
                if LOG_DROPS_AND_LOOP_ENDS {
                    println!("Loop ended {construct_info}");
                }
            }
        });

        Arc::new(Self {
            construct_info,
            persistence_id,
            message_sender,
            inputs: Vec::new(),
            current_version,
            value_history,
            notify_sender_sender,
            notify_senders,
            actor_loop,
            lazy_delegate: None,
            extra_loops: Vec::new(),
        })
    }

    /// Read stored value from value history.
    /// Used by Subscription within engine.rs and for synchronous initial value storage.
    /// Prefer subscribe().next().await for async reactive semantics.
    pub fn stored_value(&self) -> Option<Value> {
        self.value_history.lock().ok()?.get_latest()
    }

    /// Directly store a value, bypassing the async stream.
    /// Used by HOLD to ensure state is visible before next body evaluation.
    /// This also increments the version so subscribers can see the new value.
    pub fn store_value_directly(&self, value: Value) {
        // Increment version
        let new_version = self.current_version.fetch_add(1, Ordering::SeqCst) + 1;
        // Store in history (also serves as current value storage)
        if let Ok(mut history) = self.value_history.lock() {
            history.add(new_version, value);
        }
        // Notify subscribers
        if let Ok(mut senders) = self.notify_senders.lock() {
            senders.retain_mut(|sender| {
                match sender.try_send(()) {
                    Ok(()) => true,
                    Err(e) => !e.is_disconnected(),
                }
            });
        }
    }

    /// Subscribe to this actor's values.
    ///
    /// Automatically handles both eager and lazy actors:
    /// - Eager actors: Returns efficient bounded-channel subscription
    /// - Lazy actors: Returns demand-driven subscription from lazy delegate
    ///
    /// Uses `Either` to avoid boxing while supporting both subscription types.
    ///
    /// Takes ownership of the Arc to keep the actor alive for the subscription lifetime.
    /// Callers should use `.clone().subscribe()` if they need to retain a reference.
    ///
    /// This method yields control once before subscribing, allowing the actor's async
    /// loop to start producing values. This is critical when subscribing to actors
    /// that were just created (e.g., in list operations like map, retain, etc.) - without
    /// the yield, the subscription would wait forever for values that haven't been produced yet.
    pub fn subscribe(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        // Yield control once to allow the actor's loop to start
        stream::once(async move {
            use std::task::Poll;
            let mut yielded = false;
            std::future::poll_fn(|cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }).await;
            self
        })
        .flat_map(|actor| actor.subscribe_immediate())
        .boxed_local()
    }

    /// Subscribe without yielding first. Only use this when you KNOW the actor's loop
    /// has already started (e.g., for internal chaining within an already-yielded stream).
    fn subscribe_immediate(self: Arc<Self>) -> impl Stream<Item = Value> {
        if let Some(ref lazy_delegate) = self.lazy_delegate {
            lazy_delegate.clone().subscribe().left_stream()
        } else {
            self.subscribe_eager().right_stream()
        }
    }

    /// Subscribe to eager actor's values using bounded channel notifications.
    ///
    /// WARNING: Only use this for actors you KNOW are eager (no lazy_delegate).
    /// For general use, prefer `subscribe()` which handles both cases.
    ///
    /// Memory-efficient subscription using bounded(1) channels.
    /// - O(1) memory per subscriber regardless of speed
    /// - Slow subscribers automatically skip to latest value
    /// - No RefCell or internal mutability - pure dataflow
    fn subscribe_eager(self: Arc<Self>) -> Subscription {
        // Create bounded(1) channel - at most 1 pending notification
        let (mut sender, receiver) = mpsc::channel::<()>(1);

        // CRITICAL: Add sender directly to notify_senders for IMMEDIATE registration.
        // This ensures store_value_directly() can notify this subscriber right away,
        // even before the main loop has a chance to poll and process the registration.
        // This is essential for synchronous value delivery (e.g., HOLD with Stream/pulses).
        //
        // Send an immediate notification so subscriber can check current version.
        let _ = sender.try_send(());
        self.notify_senders.lock().unwrap().push(sender);

        let _current_version = self.version();

        // NOTE: We no longer send to notify_sender_sender because we're registering directly.
        // The main loop's registration logic is now only for actors that don't use
        // store_value_directly() (i.e., actors that receive values through their stream).

        // Start at version 0 so subscriber can pull ALL historical values from the buffer.
        // This gives STREAM semantics: new subscriptions see all values from history.
        // Combined with the ValueHistory ring buffer (64 entries), this prevents message loss
        // for synchronous event streams like Stream/pulses().
        // If subscriber is too far behind (history full), it falls back to snapshot.
        Subscription {
            last_seen_version: 0,
            notify_receiver: receiver,
            // Strong reference keeps actor alive as long as subscription exists.
            // Circular reference chains in HOLD are broken via Weak in HOLD closures (evaluator.rs).
            actor: self,
            pending_values: VecDeque::new(),
        }
    }

    /// Check if this actor has a lazy delegate.
    pub fn has_lazy_delegate(&self) -> bool {
        self.lazy_delegate.is_some()
    }

    // === New Type-Safe Subscription API ===
    //
    // These methods replace the confusing subscribe_*() family with type-distinct return values:
    // - snapshot() returns Future<Option<Value>> - exactly ONE value, can't be misused
    // - stream() returns Stream<Item=Value> - continuous updates
    //
    // The return type itself enforces correct usage - no more needing .take(1) externally.

    /// Get exactly ONE value - the current snapshot.
    ///
    /// Returns a `Future`, not a `Stream`. This makes it **impossible to misuse**:
    /// - Future resolves once → you get one value
    /// - No need for `.take(1)` - the type itself enforces single-value semantics
    ///
    /// Use this in THEN/WHEN bodies where you need a point-in-time snapshot of values.
    ///
    /// If the actor has no value yet (version 0), waits for the first value.
    pub fn snapshot(self: Arc<Self>) -> Snapshot {
        Snapshot {
            actor: self,
            resolved: false,
            notify_receiver: None,
        }
    }

    /// Subscribe to continuous stream of all values from version 0.
    ///
    /// Use this for WHILE bodies, LATEST inputs, and reactive bindings where you
    /// need continuous updates.
    ///
    /// This is an alias for the existing `subscribe()` method - same behavior,
    /// clearer name that contrasts with `snapshot()`.
    pub fn stream(self: Arc<Self>) -> LocalBoxStream<'static, Value> {
        self.subscribe()
    }

    /// Get optimal update for subscriber at given version.
    ///
    /// For scalar values, always returns a snapshot (cheap to copy).
    /// For collections with DiffHistory (future), may return diffs if
    /// subscriber is close enough to current version.
    pub fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        let current = self.version();
        if subscriber_version >= current {
            return ValueUpdate::Current;
        }
        // For now, always return snapshot. Phase 4 will add diff support for LIST.
        match self.stored_value() {
            Some(value) => ValueUpdate::Snapshot(value),
            None => ValueUpdate::Current,
        }
    }
}

impl Drop for ValueActor {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
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

// --- Subscription ---

/// Bounded-channel subscription that pulls data on demand.
///
/// Uses bounded(1) channels for notifications - pure dataflow, no RefCell.
/// O(1) memory per subscriber regardless of speed.
///
/// # Memory Characteristics
/// - Notifications: bounded(1) channel - at most 1 pending signal
/// - Data: Pulled from history buffer
/// - Slow consumer: Falls back to latest if too far behind
pub struct Subscription {
    /// Strong reference to the subscribed actor.
    /// This keeps the actor alive as long as the subscription exists.
    actor: Arc<ValueActor>,
    last_seen_version: u64,
    notify_receiver: mpsc::Receiver<()>,
    /// Local buffer for values pulled from history, yielded one at a time.
    pending_values: VecDeque<Value>,
}

impl Subscription {
    /// Wait for next value.
    pub async fn next_value(&mut self) -> Option<Value> {
        // Wait for version to change
        loop {
            let current = self.actor.version();
            if current > self.last_seen_version {
                break;
            }
            // Wait for notification from actor loop
            if self.notify_receiver.next().await.is_none() {
                // Actor dropped - no more values
                return None;
            }
        }

        self.last_seen_version = self.actor.version();
        self.actor.stored_value()
    }

    /// Get current value immediately without waiting.
    pub fn current(&self) -> Option<Value> {
        self.actor.stored_value()
    }

    /// Check if there are pending updates without consuming them.
    pub fn has_pending(&self) -> bool {
        self.actor.version() > self.last_seen_version
    }

    /// Get the actor being subscribed to.
    pub fn actor(&self) -> &Arc<ValueActor> {
        &self.actor
    }
}

impl Stream for Subscription {
    type Item = Value;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // 1. First yield from local buffer if we have pending values
        if let Some(value) = self.pending_values.pop_front() {
            return Poll::Ready(Some(value));
        }

        loop {
            let current_version = self.actor.version();

            // 2. Check if new values are available
            if current_version > self.last_seen_version {
                let (values, _oldest_available) = self.actor.get_values_since(self.last_seen_version);

                if values.is_empty() {
                    // History exhausted or subscriber too far behind - fall back to snapshot
                    self.last_seen_version = current_version;
                    if let Some(value) = self.actor.stored_value() {
                        return Poll::Ready(Some(value));
                    }
                } else {
                    // Got values from history - buffer them and yield first one
                    // Update last_seen_version based on how many values we got, not current_version
                    // This prevents skipping values if the actor is still processing more
                    let values_count = values.len() as u64;
                    self.last_seen_version = self.last_seen_version.saturating_add(values_count);
                    let mut iter = values.into_iter();
                    let first = iter.next();
                    self.pending_values.extend(iter);

                    if let Some(value) = first {
                        return Poll::Ready(Some(value));
                    }
                }
            }

            // 3. Wait for notification
            match Pin::new(&mut self.notify_receiver).poll_next(cx) {
                Poll::Ready(Some(())) => {
                    // Got notification, loop to check version again
                    continue;
                }
                Poll::Ready(None) => {
                    // Channel closed, actor dropped
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

// --- Snapshot Future ---

/// Future that resolves to exactly ONE value from an actor.
///
/// This is the type returned by `ValueActor::snapshot()`. Unlike a Stream which
/// can emit many values, this Future resolves exactly once, making it impossible
/// to accidentally create ongoing subscriptions in THEN/WHEN bodies.
///
/// # Semantics
/// - If the actor has a stored value (version > 0), resolves immediately with that value
/// - If the actor has no value yet (version 0), waits for the first value
/// - After resolving once, the Future is complete (returns None on subsequent polls)
pub struct Snapshot {
    actor: Arc<ValueActor>,
    resolved: bool,
    /// Lazily initialized channel for waiting on notifications.
    /// Only created if we need to wait (version 0 case).
    notify_receiver: Option<mpsc::Receiver<()>>,
}

impl Future for Snapshot {
    type Output = Option<Value>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Already resolved - return None (Future complete)
        if self.resolved {
            return Poll::Ready(None);
        }

        // Check if value is available
        if self.actor.version() > 0 {
            self.resolved = true;
            return Poll::Ready(self.actor.stored_value());
        }

        // No value yet - register for notification and wait
        // Lazily initialize the notification channel
        if self.notify_receiver.is_none() {
            let (mut sender, receiver) = mpsc::channel::<()>(1);
            // Pre-send so we'll wake up when first value arrives
            let _ = sender.try_send(());
            self.actor.notify_senders.lock().unwrap().push(sender);
            self.notify_receiver = Some(receiver);
        }

        // Poll the notification channel
        if let Some(ref mut receiver) = self.notify_receiver {
            match Pin::new(receiver).poll_next(cx) {
                Poll::Ready(Some(())) => {
                    // Got notification - check if value is now available
                    if self.actor.version() > 0 {
                        self.resolved = true;
                        return Poll::Ready(self.actor.stored_value());
                    }
                    // Spurious wakeup, register waker and wait again
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Ready(None) => {
                    // Channel closed, actor dropped
                    self.resolved = true;
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

// --- ListDiffSubscription ---

/// Bounded-channel subscription for List that returns diffs.
///
/// Unlike the legacy ListSubscription which queues ListChange in unbounded channels,
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

pub type ValueIdempotencyKey = Ulid;

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
            idempotency_key: Ulid::new(),
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
                    let value = variable.value_actor().subscribe().next().await;
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
                    let value = variable.value_actor().subscribe().next().await;
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::List(list, _) => {
                let first_change = list.clone().subscribe().next().await;
                if let Some(ListChange::Replace { items }) = first_change {
                    let mut json_items = Vec::new();
                    for item in items {
                        let value = item.subscribe().next().await;
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
                                    Ulid::new(),
                                    actor_context.clone(),
                                );
                                Variable::new_arc(
                                    var_construct_info,
                                    construct_context.clone(),
                                    (*name).clone(),
                                    value_actor,
                                    None,
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
                                Ulid::new(),
                                actor_context.clone(),
                            );
                            Variable::new_arc(
                                var_construct_info,
                                construct_context.clone(),
                                name.clone(),
                                value_actor,
                                None,
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
                            Ulid::new(),
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
        None,
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
                let var_value = variable.value_actor().subscribe().next().await;
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
                        None,
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
                        None,
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
                let var_value = variable.value_actor().subscribe().next().await;
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
                        None,
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
                        None,
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
            None,
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
            println!("Dropped: {}", self.construct_info);
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
            None,
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
            println!("Dropped: {}", self.construct_info);
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
            None,
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
            println!("Dropped: {}", self.construct_info);
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
            None,
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
            println!("Dropped: {}", self.construct_info);
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
            None,
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
            println!("Dropped: {}", self.construct_info);
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
    change_sender_sender: mpsc::UnboundedSender<mpsc::UnboundedSender<ListChange>>,
    /// Current version (increments on each change)
    current_version: Arc<AtomicU64>,
    /// Channel for registering diff subscribers (bounded channels)
    notify_sender_sender: mpsc::UnboundedSender<mpsc::Sender<()>>,
    /// Channel for querying diff history (actor-owned, no RefCell)
    diff_query_sender: mpsc::UnboundedSender<DiffHistoryQuery>,
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
            mpsc::unbounded::<mpsc::UnboundedSender<ListChange>>();

        // Version tracking for push-pull architecture
        let current_version = Arc::new(AtomicU64::new(0));
        let current_version_for_loop = current_version.clone();

        // Channel for diff subscriber registration (bounded channels)
        let (notify_sender_sender, notify_sender_receiver) = mpsc::unbounded::<mpsc::Sender<()>>();

        // Channel for diff history queries (actor-owned, no RefCell)
        let (diff_query_sender, diff_query_receiver) = mpsc::unbounded::<DiffHistoryQuery>();

        let actor_loop = ActorLoop::new({
            let construct_info = construct_info.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            async move {
                // Diff history is owned by the actor loop - no RefCell needed
                let mut diff_history = DiffHistory::new(DiffHistoryConfig::default());

                let output_valve_signal = output_valve_signal;
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.subscribe().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                let mut change_stream = pin!(change_stream.fuse());
                let mut notify_sender_receiver = pin!(notify_sender_receiver.fuse());
                let mut diff_query_receiver = pin!(diff_query_receiver.fuse());
                let mut change_senders = Vec::<mpsc::UnboundedSender<ListChange>>::new();
                // Diff subscriber notification senders (bounded channels)
                let mut notify_senders: Vec<mpsc::Sender<()>> = Vec::new();
                let mut list = None;
                println!("[DEBUG List loop] Started for {}", construct_info);
                loop {
                    select! {
                        // Handle diff history queries
                        query = diff_query_receiver.next() => {
                            if let Some(query) = query {
                                match query {
                                    DiffHistoryQuery::GetUpdateSince { subscriber_version, reply } => {
                                        let update = diff_history.get_update_since(subscriber_version);
                                        let _ = reply.send(update);
                                    }
                                    DiffHistoryQuery::Snapshot { reply } => {
                                        let snapshot = diff_history.snapshot().to_vec();
                                        let _ = reply.send(snapshot);
                                    }
                                }
                            }
                        }

                        // Handle new diff subscriber registrations
                        sender = notify_sender_receiver.next() => {
                            if let Some(mut sender) = sender {
                                // Immediately notify - subscriber will check version and see current diffs if any
                                let _ = sender.try_send(());
                                notify_senders.push(sender);
                            }
                        }

                        change = change_stream.next() => {
                            println!("[DEBUG List loop] Received change from stream for {}", construct_info);
                            let Some(change) = change else { break };
                            if output_valve_signal.is_none() {
                                change_senders.retain(|change_sender| {
                                    if let Err(error) = change_sender.unbounded_send(change.clone()) {
                                        eprintln!("Failed to send new {construct_info} change to subscriber: {error:#}");
                                        false
                                    } else {
                                        true
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
                                    let first_change_to_send = ListChange::Replace { items: list.clone() };
                                    if let Err(error) = change_sender.unbounded_send(first_change_to_send) {
                                        eprintln!("Failed to send {construct_info} change to subscriber: {error:#}");
                                    } else {
                                        change_senders.push(change_sender);
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
                                change_senders.retain(|change_sender| {
                                    let change_to_send = ListChange::Replace { items: list.clone() };
                                    if let Err(error) = change_sender.unbounded_send(change_to_send) {
                                        eprintln!("Failed to send {construct_info} change to subscriber on impulse: {error:#}");
                                        false
                                    } else {
                                        true
                                    }
                                });
                            }
                        }
                    }
                }
                if LOG_DROPS_AND_LOOP_ENDS {
                    println!("Loop ended {construct_info}");
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
        }
    }

    /// Get current version.
    pub fn version(&self) -> u64 {
        self.current_version.load(Ordering::SeqCst)
    }

    /// Get optimal update for subscriber at given version (async).
    /// Returns diffs if subscriber is close, snapshot if too far behind.
    pub async fn get_update_since(&self, subscriber_version: u64) -> ValueUpdate {
        let (tx, rx) = oneshot::channel();
        let _ = self.diff_query_sender.unbounded_send(DiffHistoryQuery::GetUpdateSince {
            subscriber_version,
            reply: tx,
        });
        rx.await.unwrap_or(ValueUpdate::Current)
    }

    /// Get current snapshot of items with their stable IDs (async).
    pub async fn snapshot(&self) -> Vec<(ItemId, Arc<ValueActor>)> {
        let (tx, rx) = oneshot::channel();
        let _ = self.diff_query_sender.unbounded_send(DiffHistoryQuery::Snapshot {
            reply: tx,
        });
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
        if let Err(e) = self.notify_sender_sender.unbounded_send(sender) {
            eprintln!("Failed to register diff subscriber: {e:#}");
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
            None,
        ))
    }

    /// Subscribe to this list's changes.
    ///
    /// The returned `ListSubscription` stream keeps the list alive for the
    /// duration of the subscription. When the stream is dropped, the
    /// list reference is released.
    ///
    /// Takes ownership of the Arc to keep the list alive for the subscription lifetime.
    /// Callers should use `.clone().subscribe()` if they need to retain a reference.
    pub fn subscribe(self: Arc<Self>) -> ListSubscription {
        let (change_sender, change_receiver) = mpsc::unbounded();
        if let Err(error) = self.change_sender_sender.unbounded_send(change_sender) {
            eprintln!("Failed to subscribe to {}: {error:#}", self.construct_info);
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
        // 2. Then wraps further changes to save them
        let construct_context_for_load = construct_context.clone();
        let actor_context_for_load = actor_context.clone();
        let actor_id_for_load = actor_id.clone();

        let value_stream = stream::once(async move {
            // Try to load from storage
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
                                actor_id_for_load.with_child_id(format!("loaded_item_{i}")),
                                construct_context_for_load.clone(),
                                Ulid::new(),
                                actor_context_for_load.clone(),
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
                actor_id_for_load.with_child_id("persistent_list"),
                Some(persistence_data),
                "Persistent List",
            );
            let list = List::new_arc(
                inner_construct_info,
                construct_context_for_load.clone(),
                actor_context_for_load.clone(),
                initial_items,
            );

            // Start a background task to save changes
            let list_for_save = list.clone();
            let construct_storage_for_save = construct_storage;
            Task::start(async move {
                let mut change_stream = pin!(list_for_save.clone().subscribe());
                while let Some(change) = change_stream.next().await {
                    // After any change, serialize and save the current list
                    if let ListChange::Replace { ref items } = change {
                        let mut json_items = Vec::new();
                        for item in items {
                            if let Some(value) = item.clone().subscribe().next().await {
                                json_items.push(value.to_json().await);
                            }
                        }
                        construct_storage_for_save.save_state(persistence_id, &json_items).await;
                    } else {
                        // For incremental changes, we need to get the full list and save it
                        // This is done by getting the next Replace event after the change is applied
                        // But for simplicity, let's re-subscribe to get the current state
                        if let Some(ListChange::Replace { items }) = list_for_save.clone().subscribe().next().await {
                            let mut json_items = Vec::new();
                            for item in &items {
                                if let Some(value) = item.clone().subscribe().next().await {
                                    json_items.push(value.to_json().await);
                                }
                            }
                            construct_storage_for_save.save_state(persistence_id, &json_items).await;
                        }
                    }
                }
            });

            Value::List(list, ValueMetadata { idempotency_key })
        }).chain(stream::pending());

        let actor_construct_info = ConstructInfo::new(
            actor_id,
            Some(persistence_data),
            "Persistent list wrapper",
        ).complete(ConstructType::ValueActor);

        Arc::new(ValueActor::new(
            actor_construct_info,
            actor_context,
            TypedStream::infinite(value_stream),
            None,
        ))
    }
}

impl Drop for List {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- ListSubscription ---

/// A subscription stream that keeps its source List alive.
/// The `_list` field holds an `Arc<List>` reference that is automatically
/// dropped when the subscription stream is dropped, ensuring proper lifecycle management.
pub struct ListSubscription {
    receiver: mpsc::UnboundedReceiver<ListChange>,
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
    RemoveAt { index: usize },
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
                vec.insert(index, item);
            }
            Self::UpdateAt { index, item } => {
                vec[index] = item;
            }
            Self::Push { item } => {
                vec.push(item);
            }
            Self::RemoveAt { index } => {
                vec.remove(index);
            }
            Self::Move {
                old_index,
                new_index,
            } => {
                let item = vec.remove(old_index);
                vec.insert(new_index, item);
            }
            Self::Pop => {
                vec.pop().unwrap();
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
            Self::RemoveAt { index } => {
                let id = snapshot.get(*index).map(|(id, _)| *id).unwrap_or_else(ItemId::new);
                ListDiff::Remove { id }
            }
            Self::Move { old_index, new_index } => {
                // Move is Remove + Insert
                // For simplicity, we model it as Remove followed by Insert
                // The caller should handle this as two separate diffs if needed
                let id = snapshot.get(*old_index).map(|(id, _)| *id).unwrap_or_else(ItemId::new);
                let value = snapshot.get(*old_index).map(|(_, v)| v.clone()).unwrap_or_else(|| {
                    panic!("Move operation with invalid old_index")
                });
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
    /// Source code for creating borrowed expressions
    pub source_code: SourceCode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListBindingOperation {
    Map,
    Retain,
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

        // subscribe() now yields automatically, ensuring source actor's loop has started
        let change_stream = source_list_actor.clone().subscribe()
        .filter_map(move |value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config_for_stream.clone();
            let construct_context = construct_context_for_stream.clone();
            let actor_context = actor_context_for_stream.clone();

            list.subscribe().map(move |change| {
                Self::transform_list_change_for_map(
                    change,
                    &config,
                    construct_context.clone(),
                    actor_context.clone(),
                )
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

        println!("[DEBUG create_map_actor] END - returning map actor");
        Arc::new(ValueActor::new(
            construct_info,
            actor_context,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            None,
        ))
    }

    /// Creates a retain actor that filters list items based on predicate.
    /// When any item's predicate changes, emits an updated filtered list.
    fn create_retain_actor(
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

        // Create a stream that:
        // 1. Subscribes to source list changes (subscribe() yields automatically)
        // 2. For each item, evaluates predicate and subscribes to its changes
        // 3. When list or any predicate changes, emits filtered Replace
        let value_stream = source_list_actor.clone().subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();

            // Track items and their predicates
            list.subscribe().scan(
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(), // (item, predicate)
                move |item_predicates, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    // Apply change and update predicate actors
                    match &change {
                        ListChange::Replace { items } => {
                            *item_predicates = items.iter().map(|item| {
                                let predicate = Self::transform_item(
                                    item.clone(),
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), predicate)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let predicate = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_predicates.push((item.clone(), predicate));
                        }
                        ListChange::InsertAt { index, item } => {
                            let predicate = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            if *index <= item_predicates.len() {
                                item_predicates.insert(*index, (item.clone(), predicate));
                            }
                        }
                        ListChange::RemoveAt { index } => {
                            if *index < item_predicates.len() {
                                item_predicates.remove(*index);
                            }
                        }
                        ListChange::Clear => {
                            item_predicates.clear();
                        }
                        ListChange::Pop => {
                            item_predicates.pop();
                        }
                        _ => {}
                    }

                    future::ready(Some(item_predicates.clone()))
                }
            ).flat_map(move |item_predicates| {
                let construct_info_id = construct_info_id_inner.clone();

                if item_predicates.is_empty() {
                    // Empty list - emit empty Replace
                    return stream::once(future::ready(ListChange::Replace { items: vec![] })).boxed_local();
                }

                // Subscribe to all predicates and emit filtered list when any changes
                let predicate_streams: Vec<_> = item_predicates.iter().enumerate().map(|(idx, (item, pred))| {
                    let item = item.clone();
                    pred.clone().subscribe().map(move |value| (idx, item.clone(), value))
                }).collect();

                stream::select_all(predicate_streams)
                    .scan(
                        item_predicates.iter().map(|(item, _)| (item.clone(), None::<bool>)).collect::<Vec<_>>(),
                        move |states, (idx, item, value)| {
                            // Update the predicate result for this item
                            let is_true = match &value {
                                Value::Tag(tag, _) => tag.tag() == "True",
                                _ => false,
                            };
                            if idx < states.len() {
                                states[idx] = (item, Some(is_true));
                            }

                            // If all items have predicate results, emit filtered list
                            let all_evaluated = states.iter().all(|(_, result)| result.is_some());
                            if all_evaluated {
                                let filtered: Vec<Arc<ValueActor>> = states.iter()
                                    .filter_map(|(item, result)| {
                                        if result == &Some(true) {
                                            Some(item.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                future::ready(Some(Some(ListChange::Replace { items: filtered })))
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
            None,
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

        // subscribe() yields automatically, ensuring source actor's loop has started
        let value_stream = source_list_actor.clone().subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();
            let construct_context_inner = construct_context.clone();

            list.subscribe().scan(
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(),
                move |item_predicates, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    match &change {
                        ListChange::Replace { items } => {
                            *item_predicates = items.iter().map(|item| {
                                let predicate = Self::transform_item(
                                    item.clone(),
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), predicate)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let predicate = Self::transform_item(
                                item.clone(),
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
                        ListChange::RemoveAt { index } => {
                            if *index < item_predicates.len() {
                                item_predicates.remove(*index);
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
                    pred.clone().subscribe().map(move |value| (idx, value))
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
            None,
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

        // Create a stream that:
        // 1. Subscribes to source list changes (subscribe() yields automatically)
        // 2. For each item, evaluates key expression and subscribes to its changes
        // 3. When list or any key changes, emits sorted Replace
        let value_stream = source_list_actor.clone().subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();

            // Track items and their keys
            list.subscribe().scan(
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(), // (item, key_actor)
                move |item_keys, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    // Apply change and update key actors
                    match &change {
                        ListChange::Replace { items } => {
                            *item_keys = items.iter().map(|item| {
                                let key_actor = Self::transform_item(
                                    item.clone(),
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), key_actor)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let key_actor = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_keys.push((item.clone(), key_actor));
                        }
                        ListChange::InsertAt { index, item } => {
                            let key_actor = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            if *index <= item_keys.len() {
                                item_keys.insert(*index, (item.clone(), key_actor));
                            }
                        }
                        ListChange::RemoveAt { index } => {
                            if *index < item_keys.len() {
                                item_keys.remove(*index);
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
                    key_actor.clone().subscribe().map(move |value| (idx, item.clone(), value))
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
            None,
        ))
    }

    /// Transform a single list item using the config's transform expression.
    fn transform_item(
        item_actor: Arc<ValueActor>,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> Arc<ValueActor> {
        // Create a new ActorContext with the binding variable set
        let binding_name = config.binding_name.to_string();
        let mut new_params = actor_context.parameters.clone();

        // Debug: log what item value we're transforming
        if let Some(stored) = item_actor.stored_value() {
            let value_desc = match &stored {
                Value::Text(t, _) => format!("Text('{}')", t.text()),
                Value::Number(n, _) => format!("Number({})", n.number()),
                Value::Tag(_, _) => "Tag".to_string(),
                Value::Object(_, _) => "Object".to_string(),
                Value::TaggedObject(_, _) => "TaggedObject".to_string(),
                Value::List(_, _) => "List".to_string(),
                Value::Flushed(_, _) => "Flushed".to_string(),
            };
            println!("[DEBUG transform_item] binding_name='{}', stored_value={}", binding_name, value_desc);
        } else {
            println!("[DEBUG transform_item] binding_name='{}', no stored value yet", binding_name);
        }

        new_params.insert(binding_name, item_actor.clone());

        let new_actor_context = ActorContext {
            parameters: new_params,
            ..actor_context
        };

        // Evaluate the transform expression with the binding in scope
        match evaluate_static_expression(
            &config.transform_expr,
            construct_context,
            new_actor_context,
            config.reference_connector.clone(),
            config.link_connector.clone(),
            config.source_code.clone(),
        ) {
            Ok(result_actor) => result_actor,
            Err(e) => {
                eprintln!("Error evaluating transform expression: {e}");
                // Return the original item as fallback
                item_actor
            }
        }
    }

    /// Transform a ListChange by applying the transform expression to affected items.
    /// Only used for map operation.
    fn transform_list_change_for_map(
        change: ListChange,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> ListChange {
        let change_type = match &change {
            ListChange::Replace { items } => format!("Replace({})", items.len()),
            ListChange::InsertAt { index, .. } => format!("InsertAt({})", index),
            ListChange::UpdateAt { index, .. } => format!("UpdateAt({})", index),
            ListChange::Push { .. } => "Push".to_string(),
            ListChange::RemoveAt { index } => format!("RemoveAt({})", index),
            ListChange::Move { old_index, new_index } => format!("Move({} -> {})", old_index, new_index),
            ListChange::Pop => "Pop".to_string(),
            ListChange::Clear => "Clear".to_string(),
        };
        println!("[DEBUG transform_list_change_for_map] Received change: {}", change_type);
        match change {
            ListChange::Replace { items } => {
                let transformed_items: Vec<Arc<ValueActor>> = items
                    .into_iter()
                    .map(|item| {
                        Self::transform_item(
                            item,
                            config,
                            construct_context.clone(),
                            actor_context.clone(),
                        )
                    })
                    .collect();
                ListChange::Replace { items: transformed_items }
            }
            ListChange::InsertAt { index, item } => {
                let transformed_item = Self::transform_item(
                    item,
                    config,
                    construct_context,
                    actor_context,
                );
                ListChange::InsertAt { index, item: transformed_item }
            }
            ListChange::UpdateAt { index, item } => {
                let transformed_item = Self::transform_item(
                    item,
                    config,
                    construct_context,
                    actor_context,
                );
                ListChange::UpdateAt { index, item: transformed_item }
            }
            ListChange::Push { item } => {
                let transformed_item = Self::transform_item(
                    item,
                    config,
                    construct_context,
                    actor_context,
                );
                ListChange::Push { item: transformed_item }
            }
            // These operations don't involve new items, pass through unchanged
            ListChange::RemoveAt { index } => ListChange::RemoveAt { index },
            ListChange::Move { old_index, new_index } => ListChange::Move { old_index, new_index },
            ListChange::Pop => ListChange::Pop,
            ListChange::Clear => ListChange::Clear,
        }
    }
}
