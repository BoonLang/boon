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
static SWITCH_MAP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn switch_map<S, F, U>(outer: S, f: F) -> LocalBoxStream<'static, U::Item>
where
    S: Stream + 'static,
    F: Fn(S::Item) -> U + 'static,
    U: Stream + 'static,
    U::Item: 'static,
{
    use zoon::futures_util::stream::FusedStream;

    let switch_id = SWITCH_MAP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Use type aliases to avoid complex generic inference issues
    type FusedOuter<T> = stream::Fuse<LocalBoxStream<'static, T>>;
    type FusedInner<T> = stream::Fuse<LocalBoxStream<'static, T>>;

    // State as tuple: (outer_stream, inner_stream_opt, map_fn, switch_id, pending_outer_value)
    // pending_outer_value: When outer emits while inner is active, we store the outer value
    // and drain the inner stream before switching. This prevents losing in-flight events.
    let initial: (FusedOuter<S::Item>, Option<FusedInner<U::Item>>, F, usize, Option<S::Item>) = (
        outer.boxed_local().fuse(),
        None,
        f,
        switch_id,
        None, // pending_outer_value
    );

    stream::unfold(initial, |state| async move {
        use zoon::futures_util::future::Either;
        use std::task::Poll;

        // Destructure state - we need to rebuild it for the return
        let (mut outer_stream, mut inner_opt, map_fn, switch_id, mut pending_outer) = state;

        loop {
            // If we have a pending outer value and the inner stream is done/empty, switch now
            if let Some(pending_value) = pending_outer.take() {
                zoon::println!("[SWITCH_MAP:{}] Switching to pending outer value (inner drained)", switch_id);
                inner_opt = Some(map_fn(pending_value).boxed_local().fuse());
                continue;
            }

            match &mut inner_opt {
                Some(inner) if !inner.is_terminated() => {
                    // Both streams active - race between them
                    let outer_fut = outer_stream.next();
                    let inner_fut = inner.next();

                    match future::select(pin!(outer_fut), pin!(inner_fut)).await {
                        Either::Left((outer_opt, inner_fut_incomplete)) => {
                            match outer_opt {
                                Some(new_outer_value) => {
                                    // Outer emitted - but don't switch immediately!
                                    // First, try to drain any ready items from inner stream.
                                    zoon::println!("[SWITCH_MAP:{}] Outer value arrived, draining inner before switch", switch_id);

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
                                        zoon::println!("[SWITCH_MAP:{}] Drained item from inner, storing outer for later", switch_id);
                                        pending_outer = Some(new_outer_value);
                                        return Some((item, (outer_stream, inner_opt, map_fn, switch_id, pending_outer)));
                                    } else {
                                        // No ready items in inner - safe to switch now
                                        // Drop the old inner stream explicitly for logging
                                        zoon::println!("[SWITCH_MAP:{}] Inner empty, DROPPING old inner and switching", switch_id);
                                        drop(inner_opt.take());
                                        zoon::println!("[SWITCH_MAP:{}] Old inner dropped, creating new inner", switch_id);
                                        inner_opt = Some(map_fn(new_outer_value).boxed_local().fuse());
                                    }
                                }
                                None => {
                                    // Outer ended - drain inner then finish
                                    zoon::println!("[SWITCH_MAP:{}] Outer ended, draining inner", switch_id);
                                    while let Some(item) = inner.next().await {
                                        return Some((item, (outer_stream, inner_opt, map_fn, switch_id, None)));
                                    }
                                    return None;
                                }
                            }
                        }
                        Either::Right((inner_opt_val, _)) => {
                            match inner_opt_val {
                                Some(item) => {
                                    zoon::println!("[SWITCH_MAP:{}] Inner stream emitted value", switch_id);
                                    return Some((item, (outer_stream, inner_opt, map_fn, switch_id, pending_outer)));
                                }
                                None => {
                                    // Inner ended - clear it
                                    zoon::println!("[SWITCH_MAP:{}] Inner stream ended, waiting for new outer value", switch_id);
                                    inner_opt = None;
                                }
                            }
                        }
                    }
                }
                _ => {
                    // No active inner stream - wait for outer value
                    match outer_stream.next().await {
                        Some(_value) => {
                            zoon::println!("[SWITCH_MAP:{}] Initial outer value, creating inner stream", switch_id);
                            inner_opt = Some(map_fn(_value).boxed_local().fuse());
                        }
                        None => {
                            zoon::println!("[SWITCH_MAP:{}] Outer ended with no active inner", switch_id);
                            return None; // Outer ended
                        }
                    }
                }
            }
        }
    })
    .boxed_local()
}

/// A key-aware switch_map that only switches the inner stream when the key changes.
/// If the same key is produced, it keeps the existing inner stream subscription,
/// preventing race conditions where events could be lost during subscription recreation.
///
/// This is critical for LINK subscriptions in alias paths like `item.elements.checkbox.event.click`.
/// When `item` changes but the underlying `click` LINK Variable is the same (same Arc pointer),
/// we must NOT drop and recreate the subscription, as events could arrive during the transition.
pub fn switch_map_by_key<S, K, F, U>(
    outer: S,
    key_fn: impl Fn(&S::Item) -> K + 'static,
    f: F,
) -> LocalBoxStream<'static, U::Item>
where
    S: Stream + 'static,
    K: PartialEq + Clone + std::fmt::Debug + 'static,
    F: Fn(S::Item) -> U + 'static,
    U: Stream + 'static,
    U::Item: 'static,
{
    use zoon::futures_util::stream::FusedStream;

    let switch_id = SWITCH_MAP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    type FusedOuter<T> = stream::Fuse<LocalBoxStream<'static, T>>;
    type FusedInner<T> = stream::Fuse<LocalBoxStream<'static, T>>;

    // State: (outer_stream, inner_stream_opt, map_fn, key_fn, switch_id, current_key)
    let initial: (
        FusedOuter<S::Item>,
        Option<FusedInner<U::Item>>,
        F,
        Box<dyn Fn(&S::Item) -> K>,
        usize,
        Option<K>,
    ) = (
        outer.boxed_local().fuse(),
        None,
        f,
        Box::new(key_fn),
        switch_id,
        None,
    );

    stream::unfold(initial, |state| async move {
        use zoon::futures_util::future::Either;

        let (mut outer_stream, mut inner_opt, map_fn, key_fn, switch_id, mut current_key) = state;

        loop {
            match &mut inner_opt {
                Some(inner) if !inner.is_terminated() => {
                    // Both streams active - race between them
                    let outer_fut = outer_stream.next();
                    let inner_fut = inner.next();

                    match future::select(pin!(outer_fut), pin!(inner_fut)).await {
                        Either::Left((outer_opt, _)) => {
                            match outer_opt {
                                Some(new_outer_value) => {
                                    let new_key = key_fn(&new_outer_value);

                                    // Check if key changed
                                    let key_changed = current_key.as_ref().map_or(true, |k| k != &new_key);

                                    if key_changed {
                                        zoon::println!("[SWITCH_MAP_BY_KEY:{}] Key changed from {:?} to {:?}, switching", switch_id, current_key, new_key);
                                        current_key = Some(new_key);
                                        inner_opt = Some(map_fn(new_outer_value).boxed_local().fuse());
                                    } else {
                                        zoon::println!("[SWITCH_MAP_BY_KEY:{}] Same key {:?}, keeping subscription", switch_id, new_key);
                                        // Don't create new inner - keep existing subscription!
                                    }
                                }
                                None => {
                                    // Outer ended - drain inner then finish
                                    zoon::println!("[SWITCH_MAP_BY_KEY:{}] Outer ended, draining inner", switch_id);
                                    while let Some(item) = inner.next().await {
                                        return Some((item, (outer_stream, inner_opt, map_fn, key_fn, switch_id, current_key)));
                                    }
                                    return None;
                                }
                            }
                        }
                        Either::Right((inner_opt_val, _)) => {
                            match inner_opt_val {
                                Some(item) => {
                                    return Some((item, (outer_stream, inner_opt, map_fn, key_fn, switch_id, current_key)));
                                }
                                None => {
                                    zoon::println!("[SWITCH_MAP_BY_KEY:{}] Inner stream ended, waiting for new outer value", switch_id);
                                    inner_opt = None;
                                    current_key = None; // Reset key since inner ended
                                }
                            }
                        }
                    }
                }
                _ => {
                    // No active inner stream - wait for outer value
                    match outer_stream.next().await {
                        Some(_value) => {
                            let new_key = key_fn(&_value);
                            zoon::println!("[SWITCH_MAP_BY_KEY:{}] Initial outer value with key {:?}, creating inner stream", switch_id, new_key);
                            current_key = Some(new_key);
                            inner_opt = Some(map_fn(_value).boxed_local().fuse());
                        }
                        None => {
                            zoon::println!("[SWITCH_MAP_BY_KEY:{}] Outer ended with no active inner", switch_id);
                            return None;
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
    /// Debug ID for tracking drops
    debug_id: usize,
}

static PUSH_SUBSCRIPTION_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ACTOR_INSTANCE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl PushSubscription {
    fn new(receiver: mpsc::Receiver<Value>, actor: Arc<ValueActor>) -> Self {
        let debug_id = PUSH_SUBSCRIPTION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        zoon::println!("[PUSH_SUB:{}] Created for: {} (id: {:?})", debug_id, actor.construct_info.description, actor.construct_info.id);
        Self { receiver, _actor: actor, debug_id }
    }
}

impl Stream for PushSubscription {
    type Item = Value;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(cx)
    }
}

impl Drop for PushSubscription {
    fn drop(&mut self) {
        zoon::println!("[PUSH_SUB:{}] DROPPED for: {}", self.debug_id, self._actor.construct_info.description);
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

    pub fn stream(&self) -> impl Stream<Item = ()> {
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
    /// Persistence identity from parser - REQUIRED for all Variables.
    /// Combined with scope using `persistence_id.in_scope(&scope)` to create unique keys.
    persistence_id: parser::PersistenceId,
    /// Scope context from List/map evaluation - either Root or Nested with a unique prefix.
    /// Combined with persistence_id to create a unique key per list item.
    /// This is how we distinguish item1.completed from item2.completed.
    scope: parser::Scope,
    /// Unique ID generated when this Variable is created.
    /// Each re-evaluation of an expression creates a new Variable with new evaluation_id.
    /// Used in subscription keys to detect Variable recreation (e.g., WHILE re-render).
    evaluation_id: ulid::Ulid,
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
        persistence_id: parser::PersistenceId,
        scope: parser::Scope,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Variable),
            persistence_id,
            scope,
            evaluation_id: ulid::Ulid::new(),
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
            evaluation_id: ulid::Ulid::new(),
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

    pub fn evaluation_id(&self) -> ulid::Ulid {
        self.evaluation_id
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
        let variable_description_for_log = variable_description.clone();
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Variable"),
            persistence,
            variable_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id.clone(), persistence, "Link variable value actor")
                .complete(ConstructType::ValueActor);
        let (link_value_sender, link_value_receiver) = mpsc::unbounded::<Value>();
        // UnboundedReceiver is infinite - it never terminates unless sender is dropped
        let link_stream_with_logging = link_value_receiver.map(move |value| {
            zoon::println!("[LINK] Received value: {} -> {:?}", variable_description_for_log, value.construct_info());
            value
        });
        // Capture the scope before actor_context is moved
        let scope = actor_context.scope.clone();
        let value_actor =
            ValueActor::new(actor_construct_info, actor_context, TypedStream::infinite(link_stream_with_logging), Some(persistence_id));
        let value_actor = Arc::new(value_actor);

        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            scope,
            evaluation_id: ulid::Ulid::new(),
            name: name.into(),
            value_actor,
            link_value_sender: Some(link_value_sender),
            forwarding_loop: None,
        })
    }

    /// Create a LINK variable that reuses an existing sender and ValueActor.
    ///
    /// NOTE: This function is currently unused but kept for potential future use.
    /// The evaluation_id fix in subscription keys handles WHILE re-render correctly.
    pub fn new_link_arc_reusing(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        actor_context: ActorContext,
        persistence_id: parser::PersistenceId,
        existing_sender: mpsc::UnboundedSender<Value>,
        existing_value_actor: Arc<ValueActor>,
    ) -> Arc<Self> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: variable_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Variable (reused)"),
            persistence,
            variable_description,
        );

        // Use existing sender (clone) and value_actor
        let link_value_sender = existing_sender.clone();
        let scope = actor_context.scope.clone();

        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            scope,
            evaluation_id: ulid::Ulid::new(),
            name: name.into(),
            value_actor: existing_value_actor,
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
        link_value_sender: mpsc::UnboundedSender<Value>,
        forwarding_loop: ActorLoop,
    ) -> Arc<Self> {
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            persistence_id,
            scope,
            evaluation_id: ulid::Ulid::new(),
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
        // The `subscribe()` method returns a `Subscription` that keeps the actor alive
        // for the duration of the subscription, so we no longer need the manual unfold pattern.
        let mut value_stream = stream::once(async move {
            root_value_actor.await
        })
            .flat_map(move |actor| {
                // Use value() or stream() based on context - type-safe subscription
                if use_snapshot {
                    // Value: convert Future to single-item Stream
                    stream::once(actor.value())
                        .filter_map(|v| async { v.ok() })
                        .boxed_local()
                } else {
                    // Streaming: continuous updates - lazy wrapper for sync context
                    stream::once(async move { actor.stream().await }).flatten().boxed_local()
                }
            })
            .boxed_local();
        // Collect parts to detect the last one
        let parts_vec: Vec<_> = alias_parts.into_iter().skip(skip_alias_parts).collect();
        let num_parts = parts_vec.len();

        for (idx, alias_part) in parts_vec.into_iter().enumerate() {
            let alias_part = alias_part.to_string();
            let _is_last = idx == num_parts - 1;
            let alias_part_for_log = alias_part.clone();
            let alias_part_for_key = alias_part.clone();
            let idx_for_log = idx;

            // Process each field in the path using switch_map_by_key semantics:
            // Use the Variable's Arc pointer as the key - if the same Variable is returned
            // (same underlying Arc), we keep the existing subscription to prevent race conditions.
            //
            // This is critical for LINK subscriptions: when filter changes in HOLD, the outer
            // objects change but the inner LINK Variables may be the same. If we unconditionally
            // dropped subscriptions, events could be lost during the recreation window.
            //
            // The key uses (persistence_id.in_scope(scope), evaluation_id):
            // - persistence_id + scope: distinguishes list items from each other
            // - evaluation_id: changes when WHILE/WHEN re-evaluates, triggering reconnection
            value_stream = switch_map_by_key(
                value_stream,
                move |value: &Value| {
                    match value {
                        Value::Object(object, _) => {
                            let variable = object.expect_variable(&alias_part_for_key);
                            // Include evaluation_id to detect Variable recreation (WHILE re-render)
                            // persistence_id.in_scope(&scope) ensures uniqueness across list items
                            // evaluation_id ensures reconnection when same item is re-evaluated
                            let key = (
                                variable.persistence_id().in_scope(variable.scope()),
                                variable.evaluation_id(),
                            );
                            zoon::println!("[ALIAS_KEY] '.{}' key={:?}", alias_part_for_key, key);
                            key
                        }
                        Value::TaggedObject(tagged_object, _) => {
                            let variable = tagged_object.expect_variable(&alias_part_for_key);
                            // Include evaluation_id to detect Variable recreation (WHILE re-render)
                            let key = (
                                variable.persistence_id().in_scope(variable.scope()),
                                variable.evaluation_id(),
                            );
                            zoon::println!("[ALIAS_KEY:Tagged] '.{}' key={:?}", alias_part_for_key, key);
                            key
                        }
                        // Non-object values get default key (will cause panic in map_fn anyway)
                        _ => (parser::PersistenceId::default(), ulid::Ulid::nil()),
                    }
                },
                move |value| {
                let alias_part = alias_part.clone();
                zoon::println!("[ALIAS:{}] switch_map_by_key triggered for '.{}', value type: {}",
                    idx_for_log, alias_part_for_log,
                    match &value {
                        Value::Object(_, _) => "Object",
                        Value::TaggedObject(t, _) => t.tag(),
                        Value::Text(_, _) => "Text",
                        Value::Tag(t, _) => t.tag(),
                        Value::Number(_, _) => "Number",
                        Value::List(_, _) => "List",
                        Value::Flushed(_, _) => "Flushed",
                    });
                match value {
                    Value::Object(object, _) => {
                        let variable = object.expect_variable(&alias_part);
                        let variable_actor = variable.value_actor();
                        // Use value() or stream() based on context - type-safe subscription
                        if use_snapshot {
                            // Value: get one value using the type-safe Future API
                            stream::once(async move {
                                let value = variable_actor.value().await;
                                // Keep object and variable alive for the value's lifetime
                                let _ = (&object, &variable);
                                value
                            })
                            .filter_map(|v| async { v.ok() })
                            .boxed_local()
                        } else {
                            // Streaming: continuous updates - use stream() and keep alive
                            let alias_part_for_log = alias_part.clone();
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), object, variable),
                                move |(subscription_opt, actor_opt, object, variable)| {
                                    let alias_part_log = alias_part_for_log.clone();
                                    async move {
                                        let (mut subscription, is_new) = match subscription_opt {
                                            Some(s) => (s, false),
                                            None => {
                                                zoon::println!("[ALIAS_UNFOLD] Creating new subscription for '.{}'", alias_part_log);
                                                let actor = actor_opt.unwrap();
                                                (actor.stream().await, true)
                                            }
                                        };
                                        zoon::println!("[ALIAS_UNFOLD] Waiting for value from '.{}' (new_sub={})", alias_part_log, is_new);
                                        let value = subscription.next().await;
                                        if let Some(ref v) = value {
                                            let type_name = match v {
                                                Value::Object(_, _) => "Object",
                                                Value::TaggedObject(t, _) => t.tag(),
                                                Value::Text(_, _) => "Text",
                                                Value::Tag(t, _) => t.tag(),
                                                Value::Number(_, _) => "Number",
                                                Value::List(_, _) => "List",
                                                Value::Flushed(_, _) => "Flushed",
                                            };
                                            zoon::println!("[ALIAS_UNFOLD] Got value from '.{}': {}", alias_part_log, type_name);
                                        } else {
                                            zoon::println!("[ALIAS_UNFOLD] '.{}' subscription ended (None)", alias_part_log);
                                        }
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
                            // Value: get one value using the type-safe Future API
                            stream::once(async move {
                                let value = variable_actor.value().await;
                                // Keep tagged_object and variable alive for the value's lifetime
                                let _ = (&tagged_object, &variable);
                                value
                            })
                            .filter_map(|v| async { v.ok() })
                            .boxed_local()
                        } else {
                            // Streaming: continuous updates - use stream() and keep alive
                            let alias_part_for_log = alias_part.clone();
                            stream::unfold(
                                (None::<LocalBoxStream<'static, Value>>, Some(variable_actor), tagged_object, variable),
                                move |(subscription_opt, actor_opt, tagged_object, variable)| {
                                    let alias_part_log = alias_part_for_log.clone();
                                    async move {
                                        let (mut subscription, is_new) = match subscription_opt {
                                            Some(s) => (s, false),
                                            None => {
                                                zoon::println!("[ALIAS_UNFOLD:Tagged] Creating new subscription for '.{}'", alias_part_log);
                                                let actor = actor_opt.unwrap();
                                                (actor.stream().await, true)
                                            }
                                        };
                                        zoon::println!("[ALIAS_UNFOLD:Tagged] Waiting for value from '.{}' (new_sub={})", alias_part_log, is_new);
                                        let value = subscription.next().await;
                                        if let Some(ref v) = value {
                                            let type_name = match v {
                                                Value::Object(_, _) => "Object",
                                                Value::TaggedObject(t, _) => t.tag(),
                                                Value::Text(_, _) => "Text",
                                                Value::Tag(t, _) => t.tag(),
                                                Value::Number(_, _) => "Number",
                                                Value::List(_, _) => "List",
                                                Value::Flushed(_, _) => "Flushed",
                                            };
                                            zoon::println!("[ALIAS_UNFOLD:Tagged] Got value from '.{}': {}", alias_part_log, type_name);
                                        } else {
                                            zoon::println!("[ALIAS_UNFOLD:Tagged] '.{}' subscription ended (None)", alias_part_log);
                                        }
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
                                    if let Some(senders) = referenceable_senders.remove(&span) {
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
                                    if let Some(actor) = referenceables.get(&span) {
                                        if referenceable_sender.send(actor.clone()).is_err() {
                                            eprintln!("Failed to send referenceable actor from reference connector");
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
        let (referenceable_sender, referenceable_receiver) = oneshot::channel();
        if let Err(error) = self
            .referenceable_getter_sender
            .unbounded_send((span, referenceable_sender))
        {
            eprintln!("Failed to register referenceable: {error:#}")
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
            .unwrap_or_else(parser::PersistenceId::new);
        let storage = construct_context.construct_storage.clone();

        let value_stream =
            stream::select_all(inputs.iter().enumerate().map(|(index, value_actor)| {
                // Use stream_sync() to properly handle lazy actors in HOLD body context
                value_actor.clone().stream_sync().map(move |value| {
                    // DEBUG: Log what LATEST receives from each arm
                    let value_desc = match &value {
                        Value::Text(t, _) => format!("Text('{}')", t.text()),
                        Value::Tag(t, _) => format!("Tag('{}')", t.tag()),
                        Value::Object(_, _) => "Object".to_string(),
                        Value::TaggedObject(t, _) => format!("TaggedObject('{}')", t.tag()),
                        Value::Number(n, _) => format!("Number({})", n.number()),
                        Value::List(_, _) => "List".to_string(),
                        Value::Flushed(_, _) => "Flushed".to_string(),
                    };
                    let _ = value_desc;  // Used for debugging
                    (index, value)
                })
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
            .unwrap_or_else(parser::PersistenceId::new);
        let storage = construct_context.construct_storage.clone();

        let observed_for_subscribe = observed.clone();
        let send_impulse_loop = ActorLoop::new(
            observed_for_subscribe
                // Use stream_sync() to properly handle lazy actors in HOLD body context
                .stream_sync()
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

                    // DEBUG: Log idempotency key comparison for THEN
                    zoon::println!("[THEN:idem] incoming_key={:?}, stored_key={:?}, skip={}",
                        idempotency_key,
                        state.observed_idempotency_key,
                        skip_value);

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
        // Use stream_sync() to properly handle lazy body actors in HOLD context
        let value_stream = body.clone().stream_sync().map(|mut value| {
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
        // Use stream_sync() to properly handle lazy actors in HOLD body context
        let value_stream = input
            .clone()
            .stream_sync()
            .flat_map({
                let arms = arms.clone();
                move |input_value| {
                    // Find the first matching arm
                    let matched_arm = arms
                        .iter()
                        .find(|arm| (arm.matcher)(&input_value));

                    if let Some(arm) = matched_arm {
                        // Subscribe to the matching arm's body
                        arm.body.clone().stream_sync()
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

    /// Message channel for actor communication (migration, shutdown).
    message_sender: mpsc::UnboundedSender<ActorMessage>,

    /// Explicit dependency tracking - keeps input actors alive.
    inputs: Vec<Arc<ValueActor>>,

    /// Current version number - increments on each value change.
    current_version: Arc<AtomicU64>,

    /// Channel for subscription requests.
    /// Subscribers send SubscriptionRequest, receive back a Receiver<Value>.
    subscription_sender: mpsc::UnboundedSender<SubscriptionRequest>,

    /// Channel for direct value storage.
    /// Used by HOLD to store values without going through the input stream.
    direct_store_sender: mpsc::UnboundedSender<Value>,

    /// Channel for stored value queries.
    /// Used by stored_value() and snapshot() to get current value.
    stored_value_query_sender: mpsc::UnboundedSender<StoredValueQuery>,

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

        // Unbounded channel for subscription requests
        let (subscription_sender, subscription_receiver) = mpsc::unbounded::<SubscriptionRequest>();

        // Unbounded channel for direct value storage
        let (direct_store_sender, direct_store_receiver) = mpsc::unbounded::<Value>();

        // Unbounded channel for stored value queries
        let (stored_value_query_sender, stored_value_query_receiver) = mpsc::unbounded::<StoredValueQuery>();

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
            let actor_instance_id = ACTOR_INSTANCE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
                                    let _ = reply.send(rx);
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
                                    if construct_info.description.contains("Link") || construct_info.description.contains("click") {
                                        zoon::println!("[SUBSCRIBE:inst#{}] Added sender, count now: {}, history_count: {}, sent: {}, current_version: {} for: {}",
                                            actor_instance_id, subscribers.len(), history_count, sent_count, current_version.load(Ordering::SeqCst), construct_info.description);
                                    }

                                    // Reply with the receiver
                                    let _ = reply.send(rx);
                                }
                            }
                        }

                        // Handle direct value storage (from store_value_directly)
                        value = direct_store_receiver.next() => {
                            if let Some(value) = value {
                                if !stream_ever_produced {
                                    stream_ever_produced = true;
                                    if let Some(tx) = ready_tx.take() { let _ = tx.send(()); }
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
                                let _ = reply.send(current_value);
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
                                        if let Some(tx) = ready_tx.take() { let _ = tx.send(()); }
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
                                if let Some(tx) = ready_tx.take() { let _ = tx.send(()); }
                            }
                            let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                            value_history.add(new_version, new_value.clone());

                            // Push value to all subscribers
                            // Only remove on Disconnected (receiver dropped), NOT on Full (backpressure)
                            let sub_count_before = subscribers.len();
                            let is_link_or_click = construct_info.description.contains("Link") || construct_info.description.contains("click");
                            let mut ok_count = 0;
                            let mut full_count = 0;
                            let mut disconnected_count = 0;
                            subscribers.retain_mut(|tx| {
                                match tx.try_send(new_value.clone()) {
                                    Ok(()) => { ok_count += 1; true }
                                    Err(e) if e.is_disconnected() => { disconnected_count += 1; false } // Remove dead subscribers
                                    Err(e) if e.is_full() => { full_count += 1; true } // Keep on backpressure (Full)
                                    Err(_) => true,
                                }
                            });
                            if is_link_or_click {
                                zoon::println!("[VALUE_ACTOR:inst#{}] Pushed to {}/{} subscribers (ok={}, full={}, disconnected={}): {}",
                                    actor_instance_id, subscribers.len(), sub_count_before, ok_count, full_count, disconnected_count, construct_info.description);
                            }

                            // Handle migration forwarding
                            if let MigrationState::Migrating { buffered_writes, target, .. } = &mut migration_state {
                                buffered_writes.push(new_value.clone());
                                let _ = target.send_message(ActorMessage::StreamValue(new_value));
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

        // Create a shell ValueActor - the lazy_delegate handles all subscriptions
        let construct_info = Arc::new(construct_info);
        let (message_sender, _message_receiver) = mpsc::unbounded::<ActorMessage>();
        let current_version = Arc::new(AtomicU64::new(0));

        // Dummy channels (won't be used since lazy_delegate handles subscriptions)
        let (subscription_sender, _subscription_receiver) = mpsc::unbounded::<SubscriptionRequest>();
        let (direct_store_sender, _direct_store_receiver) = mpsc::unbounded::<Value>();
        let (stored_value_query_sender, _stored_value_query_receiver) = mpsc::unbounded::<StoredValueQuery>();

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
        persistence_id: Option<parser::PersistenceId>,
        initial_value: Value,
        inputs: Vec<Arc<ValueActor>>,
    ) -> Self {
        let value_stream = value_stream.inner;
        let construct_info = Arc::new(construct_info);
        let (message_sender, message_receiver) = mpsc::unbounded::<ActorMessage>();
        // Start at version 1 since we have an initial value
        let current_version = Arc::new(AtomicU64::new(1));

        // Unbounded channels for subscription, direct store, and stored value queries
        let (subscription_sender, subscription_receiver) = mpsc::unbounded::<SubscriptionRequest>();
        let (direct_store_sender, direct_store_receiver) = mpsc::unbounded::<Value>();
        let (stored_value_query_sender, stored_value_query_receiver) = mpsc::unbounded::<StoredValueQuery>();

        // Oneshot channel for ready signal - immediately fire since we have initial value
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let _ = ready_tx.send(()); // Fire immediately
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
                    // Check if this is a click-related actor (for debug logging)
                    let construct_str = construct_info.to_string();
                    let is_click_related = construct_str.contains("click") || construct_str.contains("checkbox");

                    select! {
                        req = subscription_receiver.next() => {
                            if let Some(SubscriptionRequest { reply, starting_version }) = req {
                                if stream_ended && !stream_ever_produced {
                                    let (tx, rx) = mpsc::channel(1);
                                    drop(tx);
                                    let _ = reply.send(rx);
                                } else {
                                    let (mut tx, rx) = mpsc::channel(32);
                                    for value in value_history.get_values_since(starting_version).0 {
                                        let _ = tx.try_send(value.clone());
                                    }
                                    subscribers.push(tx);
                                    zoon::println!("[SUBSCRIBE:Lazy] Added sender, count now: {} for: {}",
                                        subscribers.len(), construct_str);
                                    let _ = reply.send(rx);
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
                                let _ = reply.send(current_value);
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

                            // Debug logging for click-related actors
                            let construct_str = construct_info.to_string();
                            let is_click_related = construct_str.contains("click") || construct_str.contains("checkbox");
                            let sub_count_before = subscribers.len();

                            subscribers.retain_mut(|tx| match tx.try_send(new_value.clone()) { Ok(()) => true, Err(e) if e.is_disconnected() => false, Err(_) => true });

                            if is_click_related {
                                zoon::println!("[BROADCAST] {} -> {} subscribers before, {} after",
                                    construct_str, sub_count_before, subscribers.len());
                            }
                        }

                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() { continue; }
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
            subscription_sender,
            direct_store_sender,
            stored_value_query_sender,
            ready_signal,
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
        if self.stored_value_query_sender.unbounded_send(StoredValueQuery { reply: reply_tx }).is_err() {
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
        initial_value_future: impl Future<Output = Option<Value>> + 'static,
    ) -> ActorLoop {
        ActorLoop::new(async move {
            // Send initial value first (awaiting if needed)
            if let Some(value) = initial_value_future.await {
                let _ = forwarding_sender.unbounded_send(value);
            }

            let mut subscription = source_actor.stream().await;
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

        // Unbounded channels for subscription, direct store, and stored value queries
        let (subscription_sender, subscription_receiver) = mpsc::unbounded::<SubscriptionRequest>();
        let (direct_store_sender, direct_store_receiver) = mpsc::unbounded::<Value>();
        let (stored_value_query_sender, stored_value_query_receiver) = mpsc::unbounded::<StoredValueQuery>();

        // Oneshot channel for ready signal - immediately fire since we have initial value
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let _ = ready_tx.send(()); // Fire immediately
        let ready_signal = ready_rx.shared();

        let actor_instance_id = ACTOR_INSTANCE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                                let _ = reply.send(value_history.get_latest());
                            }
                        }

                        req = subscription_receiver.next() => {
                            if let Some(SubscriptionRequest { reply, starting_version }) = req {
                                if stream_ended && !stream_ever_produced {
                                    let (tx, rx) = mpsc::channel(1);
                                    drop(tx);
                                    let _ = reply.send(rx);
                                } else {
                                    let (mut tx, rx) = mpsc::channel(32);
                                    let historical_values = value_history.get_values_since(starting_version).0;
                                    let history_count = historical_values.len();
                                    let mut sent_count = 0;
                                    for value in historical_values {
                                        if tx.try_send(value.clone()).is_ok() {
                                            sent_count += 1;
                                        }
                                    }
                                    subscribers.push(tx);
                                    if construct_info.description.contains("Link") || construct_info.description.contains("click") {
                                        zoon::println!("[SUBSCRIBE:InitVal:inst#{}] Added sender, count now: {}, history: {}, sent: {}, version: {} for: {}",
                                            actor_instance_id, subscribers.len(), history_count, sent_count, current_version.load(Ordering::SeqCst), construct_info.description);
                                    }
                                    let _ = reply.send(rx);
                                }
                            }
                        }

                        value = direct_store_receiver.next() => {
                            if let Some(value) = value {
                                let new_version = current_version.fetch_add(1, Ordering::SeqCst) + 1;
                                value_history.add(new_version, value.clone());
                                let sub_count_before = subscribers.len();
                                // Only remove on Disconnected (receiver dropped), NOT on Full (backpressure)
                                subscribers.retain_mut(|tx| {
                                    match tx.try_send(value.clone()) {
                                        Ok(()) => true,
                                        Err(e) if e.is_disconnected() => false,
                                        Err(_) => true,
                                    }
                                });
                                if sub_count_before > 0 || construct_info.description.contains("click") {
                                    zoon::println!("[VALUE_ACTOR:InitVal] Pushed to {}/{} subscribers: {} (id: {:?})",
                                        subscribers.len(), sub_count_before, construct_info.description, construct_info.id);
                                }
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
    pub fn store_value_directly(&self, value: Value) {
        if let Err(e) = self.direct_store_sender.unbounded_send(value) {
            eprintln!("Failed to store value directly: {e:#}");
        }
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
            // Clone the shared future and await it - ignoring errors (sender dropped means actor dead)
            let _ = self.ready_signal.clone().await;
        }

        // Send subscription request to actor loop
        let (reply_tx, reply_rx) = oneshot::channel();
        if let Err(e) = self.subscription_sender.unbounded_send(SubscriptionRequest {
            reply: reply_tx,
            starting_version: 0,
        }) {
            eprintln!("Failed to stream: actor dropped: {e:#}");
            // Return empty stream
            return stream::empty().boxed_local();
        }

        // Wait for the receiver from actor loop
        match reply_rx.await {
            Ok(receiver) => {
                PushSubscription::new(receiver, self).boxed_local()
            }
            Err(_) => {
                eprintln!("Failed to stream: actor dropped before reply");
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
        if let Err(e) = self.subscription_sender.unbounded_send(SubscriptionRequest {
            reply: reply_tx,
            starting_version: current_version,
        }) {
            eprintln!("Failed to stream_from_now: actor dropped: {e:#}");
            return stream::empty().boxed_local();
        }

        // Wait for the receiver from actor loop
        match reply_rx.await {
            Ok(receiver) => {
                PushSubscription::new(receiver, self).boxed_local()
            }
            Err(_) => {
                eprintln!("Failed to stream_from_now: actor dropped before reply");
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
                        output_valve_signal.stream().left_stream()
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
    /// Callers should use `.clone().stream()` if they need to retain a reference.
    pub fn stream(self: Arc<Self>) -> ListSubscription {
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
                let mut change_stream = pin!(list_for_save.clone().stream());
                while let Some(change) = change_stream.next().await {
                    // After any change, serialize and save the current list
                    if let ListChange::Replace { ref items } = change {
                        let mut json_items = Vec::new();
                        for item in items {
                            if let Some(value) = item.clone().stream().await.next().await {
                                json_items.push(value.to_json().await);
                            }
                        }
                        construct_storage_for_save.save_state(persistence_id, &json_items).await;
                    } else {
                        // For incremental changes, we need to get the full list and save it
                        // This is done by getting the next Replace event after the change is applied
                        // But for simplicity, let's re-subscribe to get the current state
                        if let Some(ListChange::Replace { items }) = list_for_save.clone().stream().next().await {
                            let mut json_items = Vec::new();
                            for item in &items {
                                if let Some(value) = item.clone().stream().await.next().await {
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
                if index <= vec.len() {
                    vec.insert(index, item);
                } else {
                    eprintln!("ListChange::InsertAt index {} out of bounds (len: {})", index, vec.len());
                }
            }
            Self::UpdateAt { index, item } => {
                if index < vec.len() {
                    vec[index] = item;
                } else {
                    eprintln!("ListChange::UpdateAt index {} out of bounds (len: {})", index, vec.len());
                }
            }
            Self::Push { item } => {
                vec.push(item);
            }
            Self::RemoveAt { index } => {
                if index < vec.len() {
                    vec.remove(index);
                } else {
                    eprintln!("ListChange::RemoveAt index {} out of bounds (len: {})", index, vec.len());
                }
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
                    eprintln!("ListChange::Move old_index {} out of bounds (len: {})", old_index, vec.len());
                }
            }
            Self::Pop => {
                if vec.pop().is_none() {
                    eprintln!("ListChange::Pop on empty vec");
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
            Self::RemoveAt { index } => {
                let id = snapshot.get(*index).map(|(id, _)| *id).unwrap_or_else(ItemId::new);
                ListDiff::Remove { id }
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
                    eprintln!("ListChange::Move old_index {} out of bounds in to_diff (snapshot len: {})", old_index, snapshot.len());
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
        let op_name = match config.operation {
            ListBindingOperation::Map => "Map",
            ListBindingOperation::Retain => "Retain",
            ListBindingOperation::Remove => "Remove",
            ListBindingOperation::Every => "Every",
            ListBindingOperation::Any => "Any",
            ListBindingOperation::SortBy => "SortBy",
        };
        zoon::println!("[LIST_BINDING] new_arc_value_actor called: binding='{}', operation={}, source={}",
            config.binding_name, op_name, source_list_actor.construct_info);
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
        zoon::println!("[LIST_MAP] create_map_actor: binding='{}', source={}",
            config.binding_name, source_list_actor.construct_info);

        let config_for_stream = config.clone();
        let construct_context_for_stream = construct_context.clone();
        let actor_context_for_stream = actor_context.clone();

        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // LINK connections unnecessarily.
        let binding_name_for_filter_log = config.binding_name.clone();
        let binding_name_for_dedup_log = config.binding_name.clone();
        let change_stream = switch_map(
            source_list_actor.clone().stream_sync()
            .inspect(move |value| {
                let value_type = match value {
                    Value::List(_, _) => "List",
                    Value::Object(_, _) => "Object",
                    Value::TaggedObject(_, _) => "TaggedObject",
                    Value::Text(_, _) => "Text",
                    Value::Tag(_, _) => "Tag",
                    Value::Number(_, _) => "Number",
                    Value::Flushed(_, _) => "Flushed",
                };
                zoon::println!("[LIST_MAP] source emitted value for binding='{}': {}",
                    binding_name_for_filter_log, value_type);
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
                    zoon::println!("[LIST_MAP] skipping duplicate list for binding='{}', id={:?}",
                        binding_name_for_dedup_log, list_id);
                    future::ready(Some(None))
                } else {
                    zoon::println!("[LIST_MAP] new list for binding='{}', id={:?} (prev={:?})",
                        binding_name_for_dedup_log, list_id, prev_id);
                    *prev_id = Some(list_id);
                    future::ready(Some(Some(list)))
                }
            })
            .filter_map(future::ready),
            move |list| {
            let config = config_for_stream.clone();
            let construct_context = construct_context_for_stream.clone();
            let actor_context = actor_context_for_stream.clone();
            let binding_for_log = config.binding_name.clone();
            let list_info = list.construct_info.to_string();
            zoon::println!("[LIST_MAP] flat_map received new List for binding='{}': {}",
                binding_for_log, list_info);

            // Use scan to track list length for proper Push index assignment.
            // The length is updated after each change is processed.
            list.stream().scan(0usize, move |length, change| {
                let change_type = match &change {
                    ListChange::Replace { items } => format!("Replace({} items)", items.len()),
                    ListChange::Push { .. } => "Push".to_string(),
                    ListChange::Pop => "Pop".to_string(),
                    ListChange::InsertAt { index, .. } => format!("InsertAt({})", index),
                    ListChange::RemoveAt { index } => format!("RemoveAt({})", index),
                    ListChange::UpdateAt { index, .. } => format!("UpdateAt({})", index),
                    ListChange::Clear => "Clear".to_string(),
                    ListChange::Move { old_index, new_index } => format!("Move({} -> {})", old_index, new_index),
                };
                zoon::println!("[LIST_MAP] received change '{}' for binding='{}', source={}, current_length={}",
                    change_type, binding_for_log, list_info, *length);

                let (transformed_change, new_length) = Self::transform_list_change_for_map(
                    change,
                    *length,
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

        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // subscriptions unnecessarily.
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync().filter_map(|value| {
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

            // Create channels for predicate results
            let (predicate_tx, predicate_rx) = mpsc::unbounded::<(usize, bool)>();

            // Event type for merged streams
            enum RetainEvent {
                ListChange(ListChange),
                PredicateResult(usize, bool),
            }

            // Create list change stream
            let list_changes = list.clone().stream().map(RetainEvent::ListChange);

            // Create predicate result stream
            let predicate_results = predicate_rx.map(|(idx, result)| RetainEvent::PredicateResult(idx, result));

            // Merge list changes and predicate results
            stream::select(list_changes, predicate_results).scan(
                (
                    Vec::<(Arc<ValueActor>, Arc<ValueActor>, Option<bool>, Option<TaskHandle>)>::new(), // (item, predicate, result, task_handle)
                    predicate_tx,
                    config.clone(),
                    construct_context.clone(),
                    actor_context.clone(),
                    0usize, // next_predicate_idx
                ),
                move |state, event| {
                    let (items, predicate_tx, config, construct_context, actor_context, next_idx) = state;

                    match event {
                        RetainEvent::ListChange(change) => {
                            match change {
                                ListChange::Replace { items: new_items } => {
                                    // Reset the index counter
                                    *next_idx = 0;
                                    *items = new_items.iter().map(|item| {
                                        let idx = *next_idx;
                                        *next_idx += 1;
                                        let predicate = Self::transform_item(
                                            item.clone(),
                                            idx,
                                            config,
                                            construct_context.clone(),
                                            actor_context.clone(),
                                        );
                                        // Spawn a task to forward predicate results - store handle to keep alive
                                        let tx = predicate_tx.clone();
                                        let pred_clone = predicate.clone();
                                        let task_handle = Task::start_droppable(async move {
                                            let mut stream = pred_clone.stream_sync();
                                            while let Some(value) = stream.next().await {
                                                let is_true = match &value {
                                                    Value::Tag(tag, _) => tag.tag() == "True",
                                                    _ => false,
                                                };
                                                if tx.unbounded_send((idx, is_true)).is_err() {
                                                    break;
                                                }
                                            }
                                        });
                                        (item.clone(), predicate, None, Some(task_handle))
                                    }).collect();
                                }
                                ListChange::Push { item } => {
                                    let idx = *next_idx;
                                    *next_idx += 1;
                                    let predicate = Self::transform_item(
                                        item.clone(),
                                        idx,
                                        config,
                                        construct_context.clone(),
                                        actor_context.clone(),
                                    );
                                    // Spawn a task to forward predicate results - store handle to keep alive
                                    let tx = predicate_tx.clone();
                                    let pred_clone = predicate.clone();
                                    let task_handle = Task::start_droppable(async move {
                                        let mut stream = pred_clone.stream_sync();
                                        while let Some(value) = stream.next().await {
                                            let is_true = match &value {
                                                Value::Tag(tag, _) => tag.tag() == "True",
                                                _ => false,
                                            };
                                            if tx.unbounded_send((idx, is_true)).is_err() {
                                                break;
                                            }
                                        }
                                    });
                                    items.push((item, predicate, None, Some(task_handle)));
                                }
                                ListChange::Clear => {
                                    items.clear();
                                    *next_idx = 0;
                                }
                                ListChange::Pop => {
                                    items.pop();
                                }
                                _ => {}
                            }
                        }
                        RetainEvent::PredicateResult(idx, is_true) => {
                            zoon::println!("[RETAIN] PredicateResult: idx={}, is_true={}", idx, is_true);
                            if idx < items.len() {
                                items[idx].2 = Some(is_true);
                            }
                        }
                    }

                    // Check if all predicates are evaluated
                    let all_evaluated = !items.is_empty() && items.iter().all(|(_, _, result, _)| result.is_some());
                    if all_evaluated || items.is_empty() {
                        let filtered: Vec<Arc<ValueActor>> = items.iter()
                            .filter_map(|(item, _, result, _)| {
                                if result == &Some(true) {
                                    Some(item.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        zoon::println!("[RETAIN] Emitting filtered list with {} items", filtered.len());
                        future::ready(Some(Some(ListChange::Replace { items: filtered })))
                    } else {
                        future::ready(Some(None))
                    }
                }
            ).filter_map(future::ready)
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

    /// Creates a remove actor that removes items when their `when` event fires.
    /// Unlike retain (which filters based on a boolean predicate), remove listens
    /// to event streams and removes items when those streams emit any value.
    fn create_remove_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Arc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        // Clone for use after the chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Use switch_map for proper list replacement handling
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync().filter_map(|value| {
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

            // Create channel for removal events (index of item to remove)
            let (remove_tx, remove_rx) = mpsc::unbounded::<usize>();

            // Event type for merged streams
            enum RemoveEvent {
                ListChange(ListChange),
                RemoveItem(usize),
            }

            // Create list change stream
            let list_changes = list.clone().stream().map(RemoveEvent::ListChange);

            // Create removal event stream
            let removal_events = remove_rx.map(RemoveEvent::RemoveItem);

            // Merge list changes and removal events
            stream::select(list_changes, removal_events).scan(
                (
                    Vec::<(usize, Arc<ValueActor>, Arc<ValueActor>, bool, Option<TaskHandle>)>::new(), // (original_idx, item, when_actor, removed, task_handle)
                    remove_tx,
                    config.clone(),
                    construct_context.clone(),
                    actor_context.clone(),
                    0usize, // next_idx for assigning unique IDs
                ),
                move |state, event| {
                    let (items, remove_tx, config, construct_context, actor_context, next_idx) = state;

                    match event {
                        RemoveEvent::ListChange(change) => {
                            match change {
                                ListChange::Replace { items: new_items } => {
                                    // Reset and rebuild
                                    *next_idx = 0;
                                    *items = new_items.iter().map(|item| {
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
                                        // Use stream_from_now_stream to avoid replaying historical events
                                        let tx = remove_tx.clone();
                                        let when_clone = when_actor.clone();
                                        let task_handle = Task::start_droppable(async move {
                                            let mut stream = when_clone.stream_from_now_sync();
                                            // Wait for ANY emission - that triggers removal
                                            if stream.next().await.is_some() {
                                                let _ = tx.unbounded_send(idx);
                                            }
                                        });
                                        (idx, item.clone(), when_actor, false, Some(task_handle))
                                    }).collect();
                                }
                                ListChange::Push { item } => {
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
                                    // Use stream_from_now_stream to avoid replaying historical events
                                    let tx = remove_tx.clone();
                                    let when_clone = when_actor.clone();
                                    let task_handle = Task::start_droppable(async move {
                                        let mut stream = when_clone.stream_from_now_sync();
                                        if stream.next().await.is_some() {
                                            let _ = tx.unbounded_send(idx);
                                        }
                                    });
                                    items.push((idx, item, when_actor, false, Some(task_handle)));
                                }
                                ListChange::Clear => {
                                    items.clear();
                                    *next_idx = 0;
                                }
                                ListChange::Pop => {
                                    items.pop();
                                }
                                _ => {}
                            }
                            // Emit current list state (all non-removed items)
                            let current: Vec<Arc<ValueActor>> = items.iter()
                                .filter(|(_, _, _, removed, _)| !removed)
                                .map(|(_, item, _, _, _)| item.clone())
                                .collect();
                            future::ready(Some(Some(ListChange::Replace { items: current })))
                        }
                        RemoveEvent::RemoveItem(idx) => {
                            zoon::println!("[REMOVE] RemoveItem event for idx={}", idx);
                            // Mark the item as removed
                            if let Some(item_entry) = items.iter_mut().find(|(i, _, _, _, _)| *i == idx) {
                                item_entry.3 = true; // Mark as removed
                            }
                            // Emit updated list without removed items
                            let remaining: Vec<Arc<ValueActor>> = items.iter()
                                .filter(|(_, _, _, removed, _)| !removed)
                                .map(|(_, item, _, _, _)| item.clone())
                                .collect();
                            zoon::println!("[REMOVE] Emitting list with {} remaining items", remaining.len());
                            future::ready(Some(Some(ListChange::Replace { items: remaining })))
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

        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // subscriptions unnecessarily.
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync().filter_map(|value| {
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
        // 1. Subscribes to source list changes (stream_sync() yields automatically)
        // 2. For each item, evaluates key expression and subscribes to its changes
        // 3. When list or any key changes, emits sorted Replace
        // Use switch_map for proper list replacement handling:
        // When source emits a new list, cancel old subscription and start fresh.
        // But filter out re-emissions of the same list by ID to avoid cancelling
        // subscriptions unnecessarily.
        let value_stream = switch_map(
            source_list_actor.clone().stream_sync().filter_map(|value| {
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
            None,
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
        let pid_suffix = if let Some(pid) = item_actor.persistence_id() {
            format!("{}", pid.as_u128())
        } else if let Some(persistence) = item_actor.construct_info.persistence() {
            format!("{}", persistence.id.as_u128())
        } else {
            format!("{:?}", item_actor.construct_info.id)
        };
        let scope_id = format!("list_item_{}_{}", index, pid_suffix);
        zoon::println!("[LIST_MAP] transform_item: binding='{}', scope_id='{}', parent_scope={:?}",
            config.binding_name, scope_id, actor_context.scope);
        let new_actor_context = ActorContext {
            parameters: new_params,
            ..actor_context.with_child_scope(&scope_id)
        };
        zoon::println!("[LIST_MAP] transform_item: new_scope={:?}", new_actor_context.scope);

        // Evaluate the transform expression with the binding in scope
        // Pass the function registry snapshot to enable user-defined function calls
        match evaluate_static_expression_with_registry(
            &config.transform_expr,
            construct_context,
            new_actor_context,
            config.reference_connector.clone(),
            config.link_connector.clone(),
            config.source_code.clone(),
            config.function_registry_snapshot.clone(),
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
            ListChange::RemoveAt { index } => (ListChange::RemoveAt { index }, current_length.saturating_sub(1)),
            ListChange::Move { old_index, new_index } => (ListChange::Move { old_index, new_index }, current_length),
            ListChange::Pop => (ListChange::Pop, current_length.saturating_sub(1)),
            ListChange::Clear => (ListChange::Clear, 0),
        }
    }
}
