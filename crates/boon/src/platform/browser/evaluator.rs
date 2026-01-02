use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::Poll;

use chumsky::Parser as ChumskyParser;
use chumsky::input::{Input as ChumskyInput, Stream as ChumskyStream};
use ulid::Ulid;
use zoon::futures_channel::oneshot;
use zoon::futures_util::select;
use zoon::futures_util::future;
use zoon::futures_util::stream::{self, LocalBoxStream};
use zoon::{Stream, StreamExt, SinkExt, mpsc, Task, TaskHandle};

/// Yields control to the executor, allowing other tasks to run.
/// This is a simple implementation that returns Pending once and schedules a wake.
async fn yield_once() {
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
    }).await
}

use super::super::super::parser::{
    Persistence, PersistenceId, Scope, SourceCode, Span, span_at, static_expression, lexer, parser, resolve_references, Token, Spanned,
};
use super::api;
use super::engine::*;

/// Creates a persistence-wrapped stream for a variable.
///
/// The stream:
/// 1. First tries to load stored value from storage (only for primitives)
/// 2. If found, emits the stored value first and SKIPS first source emission
/// 3. Then subscribes to source actor and forwards all subsequent values
/// 4. Each forwarded primitive value is saved to storage
///
/// IMPORTANT: Only primitive values (Text, Number, Tag) and Lists are persisted.
/// Complex values (Object, TaggedObject) are NOT persisted because:
/// - They contain nested Variables that need to be created by code evaluation
/// - Each nested Variable handles its own persistence
/// - Restoring complex structures from JSON would lose Variable references
fn create_variable_persistence_stream(
    source_actor: Arc<ValueActor>,
    storage: Arc<ConstructStorage>,
    persistence_id: PersistenceId,
    scope: Scope,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let scoped_id = persistence_id.in_scope(&scope);

    // Inside HOLD body (sequential_processing), skip restoration logic.
    // HOLD itself handles state persistence. Variables inside HOLD body are
    // recreated each iteration with fresh values that should NOT be overwritten
    // by stored values from previous iterations.
    if actor_context.sequential_processing {
        return source_actor.stream()
            .then(move |value| {
                let storage = storage.clone();
                async move {
                    save_value_if_applicable(&value, scoped_id, &storage).await;
                    value
                }
            })
            .left_stream();
    }

    // Use unfold to manage state: first load stored value, then forward source
    // Returns Option<Value> so we can skip emissions (None) when needed
    stream::unfold(
        PersistenceState::LoadingStored,
        move |state| {
            let storage = storage.clone();
            let source_actor = source_actor.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();

            async move {
                match state {
                    PersistenceState::LoadingStored => {
                        // Try to load stored value
                        let loaded: Option<zoon::serde_json::Value> = storage.clone().load_state(scoped_id).await;

                        let restored_value = loaded.and_then(|json| {
                            match &json {
                                zoon::serde_json::Value::String(_) |
                                zoon::serde_json::Value::Number(_) |
                                zoon::serde_json::Value::Bool(_) |
                                zoon::serde_json::Value::Null => {
                                    Some(Value::from_json(
                                        &json,
                                        ConstructId::new("variable restored from storage"),
                                        construct_context.clone(),
                                        ValueIdempotencyKey::new(),
                                        actor_context.clone(),
                                    ))
                                }
                                zoon::serde_json::Value::Object(obj) => {
                                    if obj.len() == 1 && obj.contains_key("_tag") {
                                        Some(Value::from_json(
                                            &json,
                                            ConstructId::new("variable restored from storage"),
                                            construct_context.clone(),
                                            ValueIdempotencyKey::new(),
                                            actor_context.clone(),
                                        ))
                                    } else {
                                        None
                                    }
                                }
                                zoon::serde_json::Value::Array(_) => {
                                    // Don't restore Lists from variable persistence.
                                    // Restoring creates a NEW List disconnected from the reactive chain.
                                    // List items need to be persisted at the List actor level instead.
                                    None
                                }
                            }
                        });

                        let mut source_subscription = source_actor.stream();

                        if let Some(value) = restored_value {
                            // Emit restored value, then skip first source emission
                            Some((Some(value), PersistenceState::SkipFirstSource {
                                source_subscription,
                                storage,
                            }))
                        } else {
                            // No stored value, wait for first source emission and forward it
                            let first_value = source_subscription.next().await?;
                            save_value_if_applicable(&first_value, scoped_id, &storage).await;
                            Some((Some(first_value), PersistenceState::ForwardingSource {
                                source_subscription,
                                storage,
                            }))
                        }
                    }
                    PersistenceState::SkipFirstSource { mut source_subscription, storage } => {
                        // Skip first emission from source (it's the initial empty state)
                        // Return None to skip this iteration, but continue with ForwardingSource
                        let _ = source_subscription.next().await?;
                        Some((None, PersistenceState::ForwardingSource {
                            source_subscription,
                            storage,
                        }))
                    }
                    PersistenceState::ForwardingSource { mut source_subscription, storage } => {
                        let value = source_subscription.next().await?;
                        save_value_if_applicable(&value, scoped_id, &storage).await;
                        Some((Some(value), PersistenceState::ForwardingSource {
                            source_subscription,
                            storage,
                        }))
                    }
                }
            }
        }
    ).filter_map(future::ready).right_stream()
}

enum PersistenceState {
    LoadingStored,
    SkipFirstSource {
        source_subscription: LocalBoxStream<'static, Value>,
        storage: Arc<ConstructStorage>,
    },
    ForwardingSource {
        source_subscription: LocalBoxStream<'static, Value>,
        storage: Arc<ConstructStorage>,
    },
}

async fn save_value_if_applicable(value: &Value, scoped_id: PersistenceId, storage: &ConstructStorage) {
    match value {
        // Don't persist Lists at variable level - they need List-actor-level persistence
        // Restoring a List from JSON creates a disconnected List
        Value::List(..) => {}
        Value::Text(_, _) | Value::Number(..) | Value::Tag(..) => {
            let json = value.to_json().await;
            storage.save_state(scoped_id, &json);
        }
        _ => {}
    }
}

// =============================================================================
// WORK QUEUE TYPES (Stack-safe evaluator)
// =============================================================================

/// Bundles all evaluation parameters into one struct.
/// This replaces passing 8 separate parameters through recursive calls.
///
/// NOTE: reference_connector and link_connector are stored as Weak to break
/// reference cycles. The playground/caller must hold the strong Arc references,
/// and when those are dropped, the connectors will be freed, causing all actors
/// to be cleaned up.
#[derive(Clone)]
pub struct EvaluationContext {
    pub construct_context: ConstructContext,
    pub actor_context: ActorContext,
    pub reference_connector: Weak<ReferenceConnector>,
    pub link_connector: Weak<LinkConnector>,
    pub pass_through_connector: Weak<PassThroughConnector>,
    pub module_loader: ModuleLoader,
    pub source_code: SourceCode,
    /// Optional snapshot of function registry for nested evaluations (closures).
    /// None during main evaluation (uses EvaluationState.function_registry instead).
    /// Some(arc) for nested evaluations with immutable snapshot.
    pub function_registry_snapshot: Option<Arc<HashMap<String, StaticFunctionDefinition>>>,
}

impl EvaluationContext {
    /// Create a new EvaluationContext with all parameters.
    pub fn new(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        reference_connector: Arc<ReferenceConnector>,
        link_connector: Arc<LinkConnector>,
        pass_through_connector: Arc<PassThroughConnector>,
        module_loader: ModuleLoader,
        source_code: SourceCode,
    ) -> Self {
        Self {
            construct_context,
            actor_context,
            reference_connector: Arc::downgrade(&reference_connector),
            link_connector: Arc::downgrade(&link_connector),
            pass_through_connector: Arc::downgrade(&pass_through_connector),
            module_loader,
            source_code,
            function_registry_snapshot: None,
        }
    }

    /// Create a derived context with updated actor_context.
    pub fn with_actor_context(&self, actor_context: ActorContext) -> Self {
        Self {
            actor_context,
            ..self.clone()
        }
    }

    /// Create a derived context with piped value set.
    pub fn with_piped(&self, piped: Arc<ValueActor>) -> Self {
        self.with_actor_context(ActorContext {
            piped: Some(piped),
            ..self.actor_context.clone()
        })
    }

    /// Create a derived context with passed value set.
    pub fn with_passed(&self, passed: Arc<ValueActor>) -> Self {
        self.with_actor_context(ActorContext {
            passed: Some(passed),
            ..self.actor_context.clone()
        })
    }

    /// Create a derived context with additional parameters.
    pub fn with_parameters(&self, params: HashMap<String, Arc<ValueActor>>) -> Self {
        let mut new_params = self.actor_context.parameters.clone();
        new_params.extend(params);
        self.with_actor_context(ActorContext {
            parameters: new_params,
            ..self.actor_context.clone()
        })
    }

    /// Create a derived context with lazy actor mode.
    /// When use_lazy_actors is true, expression evaluation creates LazyValueActors
    /// instead of eager ValueActors. Lazy actors only poll their source stream
    /// when a subscriber requests values (demand-driven).
    pub fn with_lazy_actors(&self, use_lazy_actors: bool) -> Self {
        self.with_actor_context(ActorContext {
            use_lazy_actors,
            ..self.actor_context.clone()
        })
    }

    /// Try to upgrade the weak reference_connector to a strong Arc.
    /// Returns None if the connector has been dropped (program shutting down).
    pub fn try_reference_connector(&self) -> Option<Arc<ReferenceConnector>> {
        self.reference_connector.upgrade()
    }

    /// Try to upgrade the weak link_connector to a strong Arc.
    /// Returns None if the connector has been dropped (program shutting down).
    pub fn try_link_connector(&self) -> Option<Arc<LinkConnector>> {
        self.link_connector.upgrade()
    }

    /// Try to upgrade the weak pass_through_connector to a strong Arc.
    /// Returns None if the connector has been dropped (program shutting down).
    pub fn try_pass_through_connector(&self) -> Option<Arc<PassThroughConnector>> {
        self.pass_through_connector.upgrade()
    }
}

/// Slot ID for result storage in work queue evaluation.
pub type SlotId = u32;

/// Binary operation kinds (unified arithmetic and comparison).
#[derive(Clone, Copy, Debug)]
pub enum BinaryOpKind {
    // Arithmetic
    Add,
    Subtract,
    Multiply,
    Divide,
    // Comparison
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

/// A work item represents a pending evaluation step.
/// This replaces recursive calls with heap-allocated work.
pub enum WorkItem {
    /// Evaluate an expression, store result at given slot.
    Evaluate {
        expr: static_expression::Spanned<static_expression::Expression>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// After evaluating operands, apply binary operation.
    BinaryOp {
        op: BinaryOpKind,
        operand_a_slot: SlotId,
        operand_b_slot: SlotId,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build a List from evaluated items.
    BuildList {
        item_slots: Vec<SlotId>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build an Object from evaluated variables.
    BuildObject {
        variable_data: Vec<ObjectVariableData>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build a TaggedObject from evaluated variables.
    BuildTaggedObject {
        tag: String,
        variable_data: Vec<ObjectVariableData>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build LATEST combinator after evaluating inputs.
    BuildLatest {
        input_slots: Vec<SlotId>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build THEN after piped expression is ready.
    BuildThen {
        piped_slot: SlotId,
        body: Box<static_expression::Spanned<static_expression::Expression>>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build WHEN after piped expression is ready.
    BuildWhen {
        piped_slot: SlotId,
        arms: Vec<static_expression::Arm>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build WHILE after piped expression is ready.
    BuildWhile {
        piped_slot: SlotId,
        arms: Vec<static_expression::Arm>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build HOLD after initial value is ready.
    BuildHold {
        initial_slot: SlotId,
        state_param: String,
        body: Box<static_expression::Spanned<static_expression::Expression>>,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Build Block after variables and output are evaluated.
    /// The object_slot contains the Object with Variables that must be kept alive.
    BuildBlock {
        object_slot: SlotId,
        output_slot: SlotId,
        result_slot: SlotId,
    },

    /// Evaluate pipe step with piped value from previous slot.
    EvaluateWithPiped {
        expr: static_expression::Spanned<static_expression::Expression>,
        prev_slot: SlotId,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Call function after arguments are evaluated.
    CallFunction {
        path: Vec<String>,
        arg_slots: Vec<(String, SlotId)>,
        passed_slot: Option<SlotId>,
        passed_context: Option<SlotId>,
        /// If true, prepend the piped value as first argument for builtin functions.
        /// This should only be true when the function is the direct target of a pipe (`|>`).
        use_piped_for_builtin: bool,
        span: Span,
        persistence: Option<Persistence>,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },

    /// Connect forwarding actors to their evaluated results.
    /// Used for referenced function arguments.
    ConnectForwardingActors {
        connections: Vec<(SlotId, NamedChannel<Value>)>,
    },

    /// Wrap an evaluated slot with variable persistence.
    /// Used for function arguments that have persistence IDs.
    WrapWithPersistence {
        source_slot: SlotId,
        persistence_id: PersistenceId,
        ctx: EvaluationContext,
        result_slot: SlotId,
    },
}

impl WorkItem {
    /// Get a debug name for this work item type
    pub fn debug_name(&self) -> &'static str {
        match self {
            WorkItem::Evaluate { .. } => "Evaluate",
            WorkItem::BinaryOp { .. } => "BinaryOp",
            WorkItem::BuildList { .. } => "BuildList",
            WorkItem::BuildObject { .. } => "BuildObject",
            WorkItem::BuildTaggedObject { .. } => "BuildTaggedObject",
            WorkItem::BuildLatest { .. } => "BuildLatest",
            WorkItem::BuildThen { .. } => "BuildThen",
            WorkItem::BuildWhen { .. } => "BuildWhen",
            WorkItem::BuildWhile { .. } => "BuildWhile",
            WorkItem::BuildHold { .. } => "BuildHold",
            WorkItem::BuildBlock { .. } => "BuildBlock",
            WorkItem::EvaluateWithPiped { .. } => "EvaluateWithPiped",
            WorkItem::CallFunction { .. } => "CallFunction",
            WorkItem::ConnectForwardingActors { .. } => "ConnectForwardingActors",
            WorkItem::WrapWithPersistence { .. } => "WrapWithPersistence",
        }
    }
}

/// Data for building object variables.
#[derive(Clone)]
pub struct ObjectVariableData {
    pub name: String,
    pub value_slot: SlotId,
    pub is_link: bool,
    pub is_referenced: bool,
    pub span: Span,
    pub persistence: Option<Persistence>,
    /// For referenced fields, holds a pre-created forwarding actor and its sender.
    /// This allows the actor to be registered with ReferenceConnector before
    /// the field expression is evaluated, fixing forward reference race conditions.
    pub forwarding_actor: Option<(Arc<ValueActor>, NamedChannel<Value>)>,
}

/// Holds the state of an ongoing work queue evaluation.
pub struct EvaluationState {
    /// Work queue (LIFO for depth-first evaluation).
    work_queue: Vec<WorkItem>,

    /// Results storage - maps slot IDs to completed ValueActors.
    /// If a slot is not in the map, it means SKIP (no value).
    results: HashMap<SlotId, Arc<ValueActor>>,

    /// Next available slot ID.
    next_slot: SlotId,

    /// Function registry - stores user-defined functions during evaluation.
    /// Owned by the evaluation state, no sharing or interior mutability needed.
    function_registry: HashMap<String, StaticFunctionDefinition>,

    /// Forwarding loops for referenced arguments.
    /// These must be kept alive for the duration of the evaluation to ensure
    /// that forwarding actors receive their values from source actors.
    forwarding_loops: Vec<ActorLoop>,
}

impl EvaluationState {
    /// Create a new empty evaluation state.
    pub fn new() -> Self {
        Self {
            work_queue: Vec::new(),
            results: HashMap::new(),
            next_slot: 0,
            function_registry: HashMap::new(),
            forwarding_loops: Vec::new(),
        }
    }

    /// Create evaluation state with pre-populated function registry.
    pub fn with_functions(functions: HashMap<String, StaticFunctionDefinition>) -> Self {
        Self {
            work_queue: Vec::new(),
            results: HashMap::new(),
            next_slot: 0,
            function_registry: functions,
            forwarding_loops: Vec::new(),
        }
    }

    /// Add a forwarding loop to keep alive.
    pub fn add_forwarding_loop(&mut self, loop_: ActorLoop) {
        self.forwarding_loops.push(loop_);
    }

    /// Take ownership of all forwarding loops.
    /// Used to transfer loops to the final result for lifetime management.
    pub fn take_forwarding_loops(&mut self) -> Vec<ActorLoop> {
        std::mem::take(&mut self.forwarding_loops)
    }

    /// Register a function in the registry.
    pub fn register_function(&mut self, name: String, def: StaticFunctionDefinition) {
        self.function_registry.insert(name, def);
    }

    /// Look up a function by name.
    pub fn get_function(&self, name: &str) -> Option<&StaticFunctionDefinition> {
        self.function_registry.get(name)
    }

    /// Allocate a new result slot.
    pub fn alloc_slot(&mut self) -> SlotId {
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }

    /// Store a result in a slot.
    pub fn store(&mut self, slot: SlotId, actor: Arc<ValueActor>) {
        self.results.insert(slot, actor);
    }

    /// Get a result from a slot. Returns None if the slot was SKIP (not stored).
    pub fn get(&self, slot: SlotId) -> Option<Arc<ValueActor>> {
        self.results.get(&slot).cloned()
    }

    /// Push work item onto the queue.
    pub fn push(&mut self, item: WorkItem) {
        self.work_queue.push(item);
    }

    /// Pop work item from the queue.
    pub fn pop(&mut self) -> Option<WorkItem> {
        self.work_queue.pop()
    }
}

// =============================================================================
// STACK-SAFE EVALUATION FUNCTIONS
// =============================================================================

/// Stack-safe expression evaluation using a work queue.
/// This is the main entry point that replaces the recursive `static_spanned_expression_into_value_actor`.
/// Returns `Ok(None)` for SKIP expressions - callers must handle this case appropriately.
pub fn evaluate_expression(
    expression: static_expression::Spanned<static_expression::Expression>,
    ctx: EvaluationContext,
) -> Result<Option<Arc<ValueActor>>, String> {
    let mut state = EvaluationState::new();
    let final_slot = state.alloc_slot();

    // Debug: log the expression type

    // Schedule the root expression
    schedule_expression(&mut state, expression, ctx, final_slot)?;

    // Process work items until the queue is empty
    while let Some(item) = state.pop() {
        process_work_item(&mut state, item)?;
    }

    // Get the final result - SKIP returns None (no actor)
    match state.get(final_slot) {
        Some(actor) => Ok(Some(actor)),
        None => {
            // SKIP evaluated - return None to signal "no value"
            // Callers handle this by returning stream::empty() (for THEN/WHEN/WHILE)
            // or by not updating state (for HOLD), etc.
            Ok(None)
        }
    }
}

/// Schedule an expression for evaluation.
/// This converts an expression into work items and pushes them onto the queue.
/// For expressions with sub-expressions, it schedules the sub-expressions first,
/// then adds a work item to combine the results.
fn schedule_expression(
    state: &mut EvaluationState,
    expression: static_expression::Spanned<static_expression::Expression>,
    ctx: EvaluationContext,
    result_slot: SlotId,
) -> Result<(), String> {
    let static_expression::Spanned {
        span,
        node: expr,
        persistence,
    } = expression;

    // If persistence is None (e.g., for expressions inside arm bodies that weren't
    // assigned persistence during parsing), generate a fresh ID at runtime.
    let (persistence, persistence_id) = match persistence {
        Some(p) => (Some(p), p.id),
        None => {
            let fresh_id = PersistenceId::new();
            (None, fresh_id)
        }
    };
    let idempotency_key = persistence_id;

    match expr {
        // ============================================================
        // IMMEDIATE VALUES (no sub-expressions to evaluate)
        // ============================================================

        static_expression::Expression::Variable(_) => {
            return Err("Failed to evaluate the variable in this context.".to_string());
        }

        // Link expressions don't produce a value directly - they're handled via is_link flag
        // in Object/Block/TaggedObject builders. If we encounter one here, it's an error.
        static_expression::Expression::Link => {
            return Err("LINK expression should only appear as a variable value in objects/blocks, not evaluated directly.".to_string());
        }

        static_expression::Expression::Literal(literal) => {
            // Literals are evaluated immediately and stored
            let actor = match literal {
                static_expression::Literal::Number(number) => Number::new_arc_value_actor(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; Number {number}"),
                    ),
                    ctx.construct_context,
                    idempotency_key,
                    ctx.actor_context,
                    number,
                ),
                static_expression::Literal::Tag(tag) => {
                    let tag = tag.to_string();
                    Tag::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; Tag {tag}"),
                        ),
                        ctx.construct_context,
                        idempotency_key,
                        ctx.actor_context,
                        tag,
                    )
                }
                static_expression::Literal::Text(text) => {
                    let text = text.to_string();
                    Text::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; Text {text}"),
                        ),
                        ctx.construct_context,
                        idempotency_key,
                        ctx.actor_context,
                        text,
                    )
                }
            };
            state.store(result_slot, actor);
        }

        static_expression::Expression::Skip => {
            // SKIP means "no value" - don't store anything in the slot.
            // Work items that depend on this slot will see None and propagate SKIP.
        }

        // ============================================================
        // COLLECTIONS (schedule items first, then build)
        // ============================================================

        static_expression::Expression::List { items } => {
            // Allocate slots for each item
            let item_slots: Vec<SlotId> = items.iter().map(|_| state.alloc_slot()).collect();

            // Push the build work item first (will be processed last due to LIFO)
            state.push(WorkItem::BuildList {
                item_slots: item_slots.clone(),
                span,
                persistence,
                ctx: ctx.clone(),
                result_slot,
            });

            // Schedule each item with a unique scope
            // This ensures LINKs inside each LIST item have unique registry keys,
            // preventing cross-item contamination (e.g., clicking one checkbox affects another)
            //
            // IMPORTANT: Use persistence_id (if available) instead of Ulid::new()!
            // - persistence_id is STABLE across WHILE re-renders (derived from source position)
            // - Ulid::new() creates a new random ID each time
            //
            // Stable scope_id ensures each list item gets a consistent key for subscriptions.
            for (idx, (item, slot)) in items.into_iter().zip(item_slots.into_iter()).enumerate() {
                // Use the helper method to create a properly isolated child scope
                let scope_id = if let Some(ref persistence) = item.persistence {
                    // Use stable persistence_id - stays same across WHILE re-renders
                    format!("list_item_{}_{}", idx, persistence.id.as_u128())
                } else {
                    // Fallback to random ULID for items without persistence_id
                    format!("list_item_{}_{}", idx, Ulid::new())
                };
                let item_ctx = EvaluationContext {
                    actor_context: ctx.actor_context.with_child_scope(&scope_id),
                    ..ctx.clone()
                };
                schedule_expression(state, item, item_ctx, slot)?;
            }
        }

        // ============================================================
        // BINARY OPERATIONS (schedule operands first, then combine)
        // ============================================================

        static_expression::Expression::ArithmeticOperator(op) => {
            schedule_arithmetic_op(state, op, span, persistence, ctx, result_slot)?;
        }

        static_expression::Expression::Comparator(cmp) => {
            schedule_comparator(state, cmp, span, persistence, ctx, result_slot)?;
        }

        // ============================================================
        // ALIAS (variable/parameter references)
        // ============================================================

        static_expression::Expression::Alias(alias) => {
            // For now, fall back to the recursive function for complex alias handling
            // This will be migrated in a future phase
            let actor = evaluate_alias_immediate(
                alias,
                span,
                persistence,
                persistence_id,
                ctx,
            )?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // CONTROL FLOW (THEN, WHEN, WHILE, HOLD)
        // These are special because their bodies are evaluated at runtime
        // ============================================================

        static_expression::Expression::Then { body } => {
            // THEN creates an actor that evaluates body at runtime for each piped value
            // We can build it immediately since the body is evaluated lazily
            // Merge context's snapshot (top-level functions) with state's registry (local functions)
            let mut merged_registry = ctx.function_registry_snapshot
                .as_ref()
                .map(|s| (**s).clone())
                .unwrap_or_default();
            for (name, def) in &state.function_registry {
                merged_registry.insert(name.clone(), def.clone());
            }
            let registry_snapshot = Arc::new(merged_registry);
            let actor = build_then_actor(
                *body,
                span,
                persistence,
                persistence_id,
                ctx,
                registry_snapshot,
            )?;
            state.store(result_slot, actor);
        }

        static_expression::Expression::When { arms } => {
            // Merge context's snapshot (top-level functions) with state's registry (local functions)
            let mut merged_registry = ctx.function_registry_snapshot
                .as_ref()
                .map(|s| (**s).clone())
                .unwrap_or_default();
            for (name, def) in &state.function_registry {
                merged_registry.insert(name.clone(), def.clone());
            }
            let registry_snapshot = Arc::new(merged_registry);
            let actor = build_when_actor(
                arms,
                span,
                persistence,
                persistence_id,
                ctx,
                registry_snapshot,
            )?;
            state.store(result_slot, actor);
        }

        static_expression::Expression::While { arms } => {
            // Merge context's snapshot (top-level functions) with state's registry (local functions)
            let mut merged_registry = ctx.function_registry_snapshot
                .as_ref()
                .map(|s| (**s).clone())
                .unwrap_or_default();
            for (name, def) in &state.function_registry {
                merged_registry.insert(name.clone(), def.clone());
            }
            let registry_snapshot = Arc::new(merged_registry);
            let actor = build_while_actor(
                arms,
                span,
                persistence,
                persistence_id,
                ctx,
                registry_snapshot,
            )?;
            state.store(result_slot, actor);
        }

        static_expression::Expression::Hold { state_param, body } => {
            // HOLD needs the piped value for initial state, then body is evaluated at runtime
            let piped = ctx.actor_context.piped.clone()
                .ok_or("HOLD requires a piped value for initial state")?;

            // Allocate slot before pushing to avoid double borrow
            let initial_slot = state.alloc_slot();
            state.push(WorkItem::BuildHold {
                initial_slot,
                state_param: state_param.to_string(),
                body,
                span,
                persistence,
                ctx: ctx.clone(),
                result_slot,
            });

            // The initial value comes from piped - store it directly
            state.store(initial_slot, piped);
        }

        // ============================================================
        // PIPE (chain of expressions)
        // ============================================================

        static_expression::Expression::Pipe { from, to } => {
            // Flatten nested pipes into a chain
            let mut steps = Vec::new();
            collect_pipe_steps(*from, &mut steps);
            steps.push(*to);

            if steps.is_empty() {
                return Err("Empty pipe chain".to_string());
            }

            // Extract first step (to be scheduled last for LIFO)
            let first_step = steps.remove(0);
            let first_slot = state.alloc_slot();

            // First pass: allocate slots and collect EvaluateWithPiped items
            let mut pipe_items = Vec::new();
            let mut prev_slot = first_slot;
            let steps_len = steps.len();
            for (i, step) in steps.into_iter().enumerate() {
                let is_last = i == steps_len - 1;
                let step_slot = if is_last { result_slot } else { state.alloc_slot() };

                pipe_items.push((step, prev_slot, step_slot));
                prev_slot = step_slot;
            }

            // Push EvaluateWithPiped items in REVERSE order (last step first)
            // so LIFO processes them in correct order
            for (step, prev, step_slot) in pipe_items.into_iter().rev() {
                state.push(WorkItem::EvaluateWithPiped {
                    expr: step,
                    prev_slot: prev,
                    ctx: ctx.clone(),
                    result_slot: step_slot,
                });
            }

            // Schedule first step LAST (will be processed first due to LIFO)
            schedule_expression(state, first_step, ctx.clone(), first_slot)?;
        }

        // ============================================================
        // BLOCK (local variables + output)
        // ============================================================

        static_expression::Expression::Block { variables, output } => {
            // Build object from variables first, then evaluate output
            // Use separate slots for the object and the output expression
            let object_slot = state.alloc_slot();
            let output_expr_slot = state.alloc_slot();

            // First pass: collect variable data and allocate slots (don't schedule yet)
            let mut variable_data = Vec::new();
            let mut vars_to_schedule = Vec::new();

            // Build object_locals map from forwarding actors
            // This allows sibling field expressions to resolve references locally
            // instead of relying on the global ReferenceConnector (which can be overwritten
            // when multiple Objects are created from the same function definition)
            let mut object_locals = ctx.actor_context.object_locals.clone();

            for var in variables {
                let var_slot = state.alloc_slot();
                let name = var.node.name.to_string();
                let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
                let is_referenced = var.node.is_referenced;
                let var_span = var.span;
                let var_persistence = var.persistence.clone();

                // For referenced fields (including LINKs), create a forwarding actor BEFORE scheduling expressions
                // This allows sibling fields to look up this field's actor immediately
                // FIX: Include LINK variables to prevent span-based ReferenceConnector overwrites
                // when multiple Objects are created from the same function definition
                let forwarding_actor = if is_referenced {
                    let var_persistence_id = var_persistence.as_ref().expect("variable persistence should be set by resolver").id;
                    let (actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            var_persistence.clone(),
                            format!("{}: (forwarding field)", name),
                        ),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );
                    // Store in object_locals for local resolution
                    object_locals.insert(var_span, actor.clone());
                    // Also register with ReferenceConnector for backward compatibility
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(var_span, actor.clone());
                    }
                    Some((actor, sender))
                } else {
                    None
                };

                variable_data.push(ObjectVariableData {
                    name,
                    value_slot: var_slot,
                    is_link,
                    is_referenced,
                    span: var_span,
                    persistence: var_persistence.clone(),
                    forwarding_actor,
                });

                // Collect for later scheduling (skip Link)
                if !is_link {
                    vars_to_schedule.push((var.node.value, var_slot));
                }
            }

            // Create context with object_locals for variable expression evaluation
            // IMPORTANT: BLOCK variables should always be reactive (not snapshot mode),
            // even if the calling context has is_snapshot_context: true.
            // Snapshot semantics should only apply to the immediate output of THEN/WHEN bodies,
            // not to intermediate data structures like BLOCKs.
            let ctx_with_locals = EvaluationContext {
                actor_context: ActorContext {
                    object_locals: object_locals.clone(),
                    is_snapshot_context: false,
                    ..ctx.actor_context.clone()
                },
                ..ctx.clone()
            };

            // Push BuildBlock first (will be processed last due to LIFO)
            // BuildBlock takes the output expression result and keeps the Object alive
            // (the Object contains the Variables which must not be dropped)
            state.push(WorkItem::BuildBlock {
                object_slot,
                output_slot: output_expr_slot,
                result_slot,
            });

            // Schedule output expression second - these work items will be processed AFTER BuildObject
            // This is important because the output may reference block variables (like `state.iteration`)
            // which need to be registered with the reference_connector by BuildObject first
            schedule_expression(state, *output, ctx_with_locals.clone(), output_expr_slot)?;

            // Push BuildObject third - will be processed AFTER variable expressions but BEFORE output
            // This registers variables with reference_connector so output can resolve them
            state.push(WorkItem::BuildObject {
                variable_data,
                span,
                persistence,
                ctx: ctx_with_locals.clone(),
                result_slot: object_slot,
            });

            // Schedule variable expressions last (will be processed first due to LIFO)
            for (var_expr, var_slot) in vars_to_schedule {
                schedule_expression(state, var_expr, ctx_with_locals.clone(), var_slot)?;
            }
        }

        // ============================================================
        // OBJECTS (schedule values first, then build)
        // ============================================================

        static_expression::Expression::Object(object) => {
            // First pass: collect variable data and allocate slots (don't schedule yet)
            let mut variable_data = Vec::new();
            let mut vars_to_schedule = Vec::new();

            // Build object_locals map from forwarding actors
            let mut object_locals = ctx.actor_context.object_locals.clone();

            for var in object.variables {
                let var_slot = state.alloc_slot();
                let name = var.node.name.to_string();
                let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
                let is_referenced = var.node.is_referenced;
                let var_span = var.span;
                let var_persistence = var.persistence.clone();

                // For referenced fields (including LINKs), create a forwarding actor BEFORE scheduling expressions
                // This allows sibling fields to look up this field's actor immediately
                // FIX: Include LINK variables to prevent span-based ReferenceConnector overwrites
                // when multiple Objects are created from the same function definition
                let forwarding_actor = if is_referenced {
                    let var_persistence_id = var_persistence.as_ref().expect("variable persistence should be set by resolver").id;
                    let (actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            var_persistence.clone(),
                            format!("{}: (forwarding field)", name),
                        ),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );
                    // Store in object_locals for local resolution
                    object_locals.insert(var_span, actor.clone());
                    // Also register with ReferenceConnector for backward compatibility
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(var_span, actor.clone());
                    }
                    Some((actor, sender))
                } else {
                    None
                };

                variable_data.push(ObjectVariableData {
                    name,
                    value_slot: var_slot,
                    is_link,
                    is_referenced,
                    span: var_span,
                    persistence: var_persistence.clone(),
                    forwarding_actor,
                });

                // Collect for later scheduling (skip Link)
                if !is_link {
                    vars_to_schedule.push((var.node.value, var_slot, is_referenced));
                }
            }

            // Create context with object_locals for variable expression evaluation
            let ctx_with_locals = EvaluationContext {
                actor_context: ActorContext {
                    object_locals: object_locals.clone(),
                    ..ctx.actor_context.clone()
                },
                ..ctx.clone()
            };

            // Push BuildObject first (will be processed last due to LIFO)
            state.push(WorkItem::BuildObject {
                variable_data,
                span,
                persistence,
                ctx: ctx_with_locals.clone(),
                result_slot,
            });

            // Schedule variable expressions: referenced fields LAST so they're processed FIRST (LIFO).
            // This ensures that when `count: prev + 1` is evaluated, the `prev` field's forwarding
            // actor already has its value, because `prev: state.count` was processed first.

            // First: schedule NON-referenced fields (processed last due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule.iter() {
                if !*is_referenced {
                    schedule_expression(state, var_expr.clone(), ctx_with_locals.clone(), *var_slot)?;
                }
            }
            // Last: schedule referenced fields (processed first due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule {
                if is_referenced {
                    schedule_expression(state, var_expr, ctx_with_locals.clone(), var_slot)?;
                }
            }
        }

        static_expression::Expression::TaggedObject { tag, object } => {
            let tag_str = tag.to_string();
            // First pass: collect variable data and allocate slots (don't schedule yet)
            let mut variable_data = Vec::new();
            let mut vars_to_schedule = Vec::new();

            // Build object_locals map from forwarding actors
            // This allows sibling field expressions to resolve references locally
            // instead of relying on the global ReferenceConnector (which can be overwritten
            // when multiple Objects are created from the same function definition)
            let mut object_locals = ctx.actor_context.object_locals.clone();

            for var in object.variables {
                let var_slot = state.alloc_slot();
                let name = var.node.name.to_string();
                let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
                let is_referenced = var.node.is_referenced;
                let var_span = var.span;
                let var_persistence = var.persistence.clone();

                // For referenced fields (including LINKs), create a forwarding actor BEFORE scheduling expressions
                // This allows sibling fields to look up this field's actor immediately
                // FIX: Include LINK variables to prevent span-based ReferenceConnector overwrites
                // when multiple Objects are created from the same function definition
                let forwarding_actor = if is_referenced {
                    let var_persistence_id = var_persistence.as_ref().expect("variable persistence should be set by resolver").id;
                    let (actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            var_persistence.clone(),
                            format!("{}: (forwarding field)", name),
                        ),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );
                    // Store in object_locals for local resolution
                    object_locals.insert(var_span, actor.clone());
                    // Also register with ReferenceConnector for backward compatibility
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(var_span, actor.clone());
                    }
                    Some((actor, sender))
                } else {
                    None
                };

                variable_data.push(ObjectVariableData {
                    name,
                    value_slot: var_slot,
                    is_link,
                    is_referenced,
                    span: var_span,
                    persistence: var_persistence.clone(),
                    forwarding_actor,
                });

                // Collect for later scheduling (skip Link)
                if !is_link {
                    vars_to_schedule.push((var.node.value, var_slot, is_referenced));
                }
            }

            // Create context with object_locals for variable expression evaluation
            let ctx_with_locals = EvaluationContext {
                actor_context: ActorContext {
                    object_locals: object_locals.clone(),
                    ..ctx.actor_context.clone()
                },
                ..ctx.clone()
            };

            // Push BuildTaggedObject first (will be processed last due to LIFO)
            state.push(WorkItem::BuildTaggedObject {
                tag: tag_str,
                variable_data,
                span,
                persistence,
                ctx: ctx_with_locals.clone(),
                result_slot,
            });

            // Schedule variable expressions: referenced fields LAST so they're processed FIRST (LIFO).
            // This ensures that when `count: prev + 1` is evaluated, the `prev` field's forwarding
            // actor already has its value, because `prev: state.count` was processed first.

            // First: schedule NON-referenced fields (processed last due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule.iter() {
                if !*is_referenced {
                    schedule_expression(state, var_expr.clone(), ctx_with_locals.clone(), *var_slot)?;
                }
            }
            // Last: schedule referenced fields (processed first due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule {
                if is_referenced {
                    schedule_expression(state, var_expr, ctx_with_locals.clone(), var_slot)?;
                }
            }
        }

        // ============================================================
        // LATEST (merge multiple streams)
        // ============================================================

        static_expression::Expression::Latest { inputs } => {
            let item_slots: Vec<SlotId> = inputs.iter().map(|_| state.alloc_slot()).collect();

            state.push(WorkItem::BuildLatest {
                input_slots: item_slots.clone(),
                span,
                persistence,
                ctx: ctx.clone(),
                result_slot,
            });

            for (item, slot) in inputs.into_iter().zip(item_slots.into_iter()) {
                schedule_expression(state, item, ctx.clone(), slot)?;
            }
        }

        // ============================================================
        // FUNCTION CALL
        // ============================================================

        static_expression::Expression::FunctionCall { path, arguments } => {
            let path_strs: Vec<String> = path.iter().map(|s| s.to_string()).collect();
            let path_strs_ref: Vec<&str> = path_strs.iter().map(|s| s.as_str()).collect();

            // Special handling for List binding functions (map, retain, remove, every, any, sort_by)
            // These need the unevaluated expression to evaluate per-item with bindings
            match path_strs_ref.as_slice() {
                ["List", "map"] | ["List", "retain"] | ["List", "remove"] | ["List", "every"] | ["List", "any"] | ["List", "sort_by"] => {
                    // Handle List binding functions specially - don't pre-evaluate transform expression
                    if let Some(actor) = build_list_binding_function(
                        &path_strs,
                        arguments,
                        span,
                        persistence,
                        persistence_id,
                        ctx,
                    )? {
                        state.store(result_slot, actor);
                    }
                }
                ["List", "append"] => {
                    // Special handling for List/append - enable call recording for the item expression
                    // This captures function calls that produce list items (e.g., new_todo())
                    // so they can be replayed on restoration
                    if let Some(actor) = build_list_append_with_recording(
                        arguments,
                        span,
                        persistence,
                        persistence_id,
                        ctx,
                    )? {
                        state.store(result_slot, actor);
                    }
                }
                _ => {
                    // Normal function call - pre-evaluate all arguments
                    // First pass: collect argument data and allocate slots (don't schedule yet)
                    let mut arg_slots = Vec::new();
                    let mut args_to_schedule = Vec::new();
                    let mut passed_slot = None;
                    let mut passed_context: Option<SlotId> = None;
                    // Track forwarding actors for referenced arguments
                    let mut forwarding_connections: Vec<(SlotId, NamedChannel<Value>)> = Vec::new();

                    // Build arg_locals map from forwarding actors
                    // This allows subsequent arguments to resolve references locally
                    // instead of relying on the global ReferenceConnector (which can be overwritten
                    // when the same function is called multiple times)
                    let mut arg_locals = ctx.actor_context.object_locals.clone();

                    // Note: piped value is handled in call_function for BUILTIN functions only
                    // User-defined functions don't receive piped as positional arg

                    for arg in arguments {
                        let arg_slot = state.alloc_slot();
                        let arg_name = arg.node.name.to_string();
                        let arg_span = arg.span;
                        let is_referenced = arg.node.is_referenced;

                        // Handle PASS argument - sets implicit context for nested calls
                        if arg_name == "PASS" {
                            if let Some(value) = arg.node.value {
                                args_to_schedule.push((value, arg_slot));
                                passed_context = Some(arg_slot);
                            }
                            continue; // Don't add PASS to positional arguments
                        }

                        // For referenced arguments, create a forwarding actor BEFORE scheduling
                        // This allows subsequent arguments to reference this one
                        if is_referenced {
                            let (forwarding_actor, sender) = ValueActor::new_arc_forwarding(
                                ConstructInfo::new(
                                    format!("arg:{}", arg_name),
                                    None,
                                    format!("{}; (forwarding argument)", arg_span),
                                ),
                                ctx.actor_context.clone(),
                                PersistenceId::new(),
                            );
                            // Store in arg_locals for local resolution
                            // This prevents overwrites when same function is called multiple times
                            arg_locals.insert(arg_span, forwarding_actor.clone());
                            // Also register with ReferenceConnector for backward compatibility
                            if let Some(ref_connector) = ctx.try_reference_connector() {
                                ref_connector.register_referenceable(arg_span, forwarding_actor.clone());
                            }
                            forwarding_connections.push((arg_slot, sender));
                        }

                        // Handle optional argument value (can be None for PASS arguments without value)
                        if let Some(value) = arg.node.value {
                            arg_slots.push((arg_name, arg_slot));
                            args_to_schedule.push((value, arg_slot));
                        } else {
                            // PASS argument without value - use piped value
                            if let Some(piped) = &ctx.actor_context.piped {
                                state.store(arg_slot, piped.clone());
                                arg_slots.push((arg_name, arg_slot));
                                passed_slot = Some(arg_slot);
                            } else {
                                return Err(format!("PASS argument requires piped value at {:?}", span));
                            }
                        }
                    }

                    // Create context with arg_locals for argument expression evaluation.
                    // Snapshot context propagates naturally - function arguments capture values
                    // at trigger time when called inside THEN/WHEN bodies.
                    let ctx_with_arg_locals = EvaluationContext {
                        actor_context: ActorContext {
                            object_locals: arg_locals,
                            ..ctx.actor_context.clone()
                        },
                        ..ctx.clone()
                    };

                    // Push CallFunction first (will be processed last due to LIFO)
                    // Note: use_piped_for_builtin is false because this is a normal function call,
                    // not the direct target of a pipe. EvaluateWithPiped will set it to true.
                    state.push(WorkItem::CallFunction {
                        path: path_strs,
                        arg_slots,
                        passed_slot,
                        passed_context,
                        use_piped_for_builtin: false,
                        span,
                        persistence,
                        ctx: ctx.clone(),
                        result_slot,
                    });

                    // Push ConnectForwardingActors AFTER CallFunction (so it's processed BEFORE due to LIFO)
                    // This ensures forwarding actors are connected to their sources BEFORE the function is called,
                    // allowing subsequent arguments that reference earlier arguments to work correctly.
                    if !forwarding_connections.is_empty() {
                        state.push(WorkItem::ConnectForwardingActors {
                            connections: forwarding_connections,
                        });
                    }

                    // Collect LATEST args that need persistence wrapping.
                    // Only LATEST expressions need value persistence here - other expressions
                    // (Objects, Literals, etc.) handle their own persistence internally.
                    let latest_args_with_persistence: Vec<_> = args_to_schedule.iter()
                        .filter_map(|(expr, slot)| {
                            // Only wrap LATEST expressions with value persistence
                            if matches!(expr.node, static_expression::Expression::Latest { .. }) {
                                expr.persistence.as_ref().map(|p| (*slot, p.id))
                            } else {
                                None
                            }
                        })
                        .collect();

                    // DEBUG: Log LATEST args with persistence
                    if !latest_args_with_persistence.is_empty() {
                        zoon::println!("[DEBUG] FunctionCall has {} LATEST args with persistence", latest_args_with_persistence.len());
                    }

                    // Push persistence wrappers for LATEST args
                    for (arg_slot, persistence_id) in latest_args_with_persistence {
                        zoon::println!("[DEBUG] Pushing WrapWithPersistence for LATEST slot {:?} with id {}", arg_slot, persistence_id);
                        state.push(WorkItem::WrapWithPersistence {
                            source_slot: arg_slot,
                            persistence_id,
                            ctx: ctx_with_arg_locals.clone(),
                            result_slot: arg_slot, // wrap in place
                        });
                    }

                    // Schedule argument expressions last (will be processed first due to LIFO)
                    // Use ctx_with_arg_locals so subsequent args can resolve references to earlier args
                    for (arg_expr, arg_slot) in args_to_schedule {
                        schedule_expression(state, arg_expr, ctx_with_arg_locals.clone(), arg_slot)?;
                    }
                }
            }
        }

        // ============================================================
        // TEXT LITERAL (text with interpolations)
        // ============================================================

        static_expression::Expression::TextLiteral { parts } => {
            let actor = build_text_literal_actor(
                parts,
                span,
                persistence,
                persistence_id,
                ctx,
            )?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // FUNCTION DEFINITION (registers function for later use)
        // ============================================================

        static_expression::Expression::Function { name, parameters, body } => {
            // Register the function in the evaluation state's registry
            let func_name = name.to_string();
            let param_names: Vec<String> = parameters.iter().map(|p| p.node.to_string()).collect();

            let func_def = StaticFunctionDefinition {
                parameters: param_names,
                body: *body,
            };

            state.register_function(func_name.clone(), func_def);

            // Function definitions don't produce a value, return SKIP
            let actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Function '{}'", func_name),
                ),
                ctx.actor_context,
                TypedStream::infinite(stream::pending::<Value>()),
                persistence_id,
            );
            state.store(result_slot, actor);
        }

        // ============================================================
        // LINK SETTER (sets a link on an object)
        // ============================================================

        static_expression::Expression::LinkSetter { alias } => {
            let actor = build_link_setter_actor(
                alias,
                span,
                persistence,
                persistence_id,
                ctx,
            )?;
            state.store(result_slot, actor);
        }


        // ============================================================
        // FIELD ACCESS (.field.subfield at pipe position)
        // Equivalent to WHILE { value => value.field.subfield }
        // ============================================================

        static_expression::Expression::FieldAccess { path } => {
            // Convert StrSlice path to Vec<String> for the function
            let path_strings: Vec<String> = path.iter().map(|s| s.to_string()).collect();
            let actor = build_field_access_actor(
                path_strings,
                span,
                persistence,
                persistence_id,
                ctx,
            )?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // TODO: More expression types to be added
        // ============================================================

        _ => {
            // For now, return an error for unsupported expressions
            // In the final version, all expression types will be handled
            return Err(format!("Expression type not yet supported in stack-safe evaluator: {:?}", span));
        }
    }

    Ok(())
}

/// Helper to flatten nested Pipe expressions into a list of steps.
fn collect_pipe_steps(
    expr: static_expression::Spanned<static_expression::Expression>,
    steps: &mut Vec<static_expression::Spanned<static_expression::Expression>>,
) {
    if let static_expression::Expression::Pipe { from, to } = expr.node {
        collect_pipe_steps(*from, steps);
        steps.push(*to);
    } else {
        steps.push(expr);
    }
}

/// Schedule an arithmetic operation.
fn schedule_arithmetic_op(
    state: &mut EvaluationState,
    op: static_expression::ArithmeticOperator,
    span: Span,
    persistence: Option<Persistence>,
    ctx: EvaluationContext,
    result_slot: SlotId,
) -> Result<(), String> {
    match op {
        static_expression::ArithmeticOperator::Add { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Add, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::ArithmeticOperator::Subtract { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Subtract, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::ArithmeticOperator::Multiply { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Multiply, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::ArithmeticOperator::Divide { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Divide, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::ArithmeticOperator::Negate { operand } => {
            // Negate is implemented as multiply by -1
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;
            let neg_one_slot = state.alloc_slot();
            let operand_slot = state.alloc_slot();

            // Create -1 constant
            let neg_one = Number::new_arc_value_actor(
                ConstructInfo::new("neg_one", None, "-1 constant"),
                ctx.construct_context.clone(),
                persistence_id,
                ctx.actor_context.clone(),
                -1.0,
            );
            state.store(neg_one_slot, neg_one);

            // Schedule operand
            schedule_expression(state, *operand, ctx.clone(), operand_slot)?;

            // Push multiply operation
            state.push(WorkItem::BinaryOp {
                op: BinaryOpKind::Multiply,
                operand_a_slot: neg_one_slot,
                operand_b_slot: operand_slot,
                span,
                persistence,
                ctx,
                result_slot,
            });

            Ok(())
        }
    }
}

/// Schedule a comparator operation.
fn schedule_comparator(
    state: &mut EvaluationState,
    cmp: static_expression::Comparator,
    span: Span,
    persistence: Option<Persistence>,
    ctx: EvaluationContext,
    result_slot: SlotId,
) -> Result<(), String> {
    match cmp {
        static_expression::Comparator::Equal { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Equal, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::Comparator::NotEqual { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::NotEqual, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::Comparator::Greater { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Greater, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::Comparator::GreaterOrEqual { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::GreaterOrEqual, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::Comparator::Less { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::Less, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
        static_expression::Comparator::LessOrEqual { operand_a, operand_b } => {
            schedule_binary_op(state, BinaryOpKind::LessOrEqual, *operand_a, *operand_b, span, persistence, ctx, result_slot)
        }
    }
}

/// Schedule a binary operation (arithmetic or comparison).
fn schedule_binary_op(
    state: &mut EvaluationState,
    op: BinaryOpKind,
    operand_a: static_expression::Spanned<static_expression::Expression>,
    operand_b: static_expression::Spanned<static_expression::Expression>,
    span: Span,
    persistence: Option<Persistence>,
    ctx: EvaluationContext,
    result_slot: SlotId,
) -> Result<(), String> {
    let a_slot = state.alloc_slot();
    let b_slot = state.alloc_slot();

    // Push the combine operation first (will be processed last)
    state.push(WorkItem::BinaryOp {
        op,
        operand_a_slot: a_slot,
        operand_b_slot: b_slot,
        span,
        persistence,
        ctx: ctx.clone(),
        result_slot,
    });

    // Schedule operands (pushed last = processed first)
    schedule_expression(state, operand_a, ctx.clone(), a_slot)?;
    schedule_expression(state, operand_b, ctx, b_slot)?;

    Ok(())
}

/// Process a single work item from the queue.
fn process_work_item(
    state: &mut EvaluationState,
    item: WorkItem,
) -> Result<(), String> {
    match item {
        WorkItem::Evaluate { expr, ctx, result_slot } => {
            // This delegates back to schedule_expression
            schedule_expression(state, expr, ctx, result_slot)?;
        }

        WorkItem::BinaryOp { op, operand_a_slot, operand_b_slot, span, persistence, ctx, result_slot } => {
            // If either operand slot is empty, produce nothing
            let Some(a) = state.get(operand_a_slot) else { return Ok(()); };
            let Some(b) = state.get(operand_b_slot) else { return Ok(()); };
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;

            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; BinaryOp {:?}", op),
            );

            let actor = create_binary_op_actor(op, construct_info, ctx.construct_context, ctx.actor_context, a, b);
            state.store(result_slot, actor);
        }

        WorkItem::BuildList { item_slots, span, persistence, ctx, result_slot } => {
            // Collect items that have values (empty slots are ignored)
            let items: Vec<_> = item_slots.iter()
                .filter_map(|slot| state.get(*slot))
                .collect();
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;

            let actor = List::new_arc_value_actor_with_persistence(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; LIST {{..}}"),
                ),
                ctx.construct_context,
                persistence_id,
                ctx.actor_context,
                items,
            );
            state.store(result_slot, actor);
        }

        WorkItem::BuildObject { variable_data, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;

            // Build variables
            let mut variables = Vec::new();
            for vd in variable_data.iter() {
                let var_persistence_id = vd.persistence.as_ref().expect("persistence should be set by resolver").id;
                let variable = if vd.is_link && vd.forwarding_actor.is_some() {
                    // LINK variable with forwarding actor (referenced by sibling field)
                    // Create the LINK and connect its value_actor to the forwarding actor
                    let (forwarding_actor, forwarding_sender) = vd.forwarding_actor.as_ref().unwrap();

                    // First create a temporary LINK to get the connected sender and value_actor
                    let temp_link = Variable::new_link_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (link variable internal)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );

                    // Get the components we need from the temporary LINK
                    let link_value_actor = temp_link.value_actor();
                    let link_value_sender = temp_link.expect_link_value_sender();

                    // Connect LINK's value_actor to the forwarding actor so sibling fields see LINK values
                    let link_value_actor_for_initial = link_value_actor.clone();
                    let forwarding_loop = ValueActor::connect_forwarding(
                        forwarding_sender.clone(),
                        link_value_actor.clone(),
                        async move { link_value_actor_for_initial.current_value().await.ok() },
                    );

                    // Create the final Variable with all components properly connected
                    // Note: link_value_actor is kept alive by forwarding_loop's subscription
                    Variable::new_link_arc_with_forwarding_loop(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (link variable with forwarding)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                        forwarding_actor.clone(),
                        link_value_sender,
                        forwarding_loop,
                    )
                } else if vd.is_link {
                    // LINK variables don't have pre-evaluated values
                    Variable::new_link_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (link variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    )
                } else if let Some((forwarding_actor, sender)) = &vd.forwarding_actor {
                    // Use the pre-created forwarding actor for referenced fields
                    // Connect forwarding from source actor to forwarding actor
                    let Some(source_actor) = state.get(vd.value_slot) else { continue; };

                    // connect_forwarding sends initial value asynchronously, then forwards async values
                    // Note: We create a 'static future by cloning source_actor into the async block
                    let source_actor_for_initial = source_actor.clone();
                    let forwarding_loop = ValueActor::connect_forwarding(
                        sender.clone(),
                        source_actor.clone(),
                        async move { source_actor_for_initial.current_value().await.ok() },
                    );
                    Variable::new_arc_with_forwarding_loop(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        forwarding_actor.clone(),
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                        forwarding_loop,
                    )
                } else {
                    // If value slot is empty, skip this variable
                    let Some(value_actor) = state.get(vd.value_slot) else { continue; };

                    // Wrap with persistence: load stored value first, save each emitted value
                    let persistence_stream = create_variable_persistence_stream(
                        value_actor.clone(),
                        ctx.construct_context.construct_storage.clone(),
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                        ctx.construct_context.clone(),
                        ctx.actor_context.clone(),
                    );

                    let persisted_actor = Arc::new(ValueActor::new(
                        ConstructInfo::new(
                            format!("PersistenceId: {} (persisted)", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable persistence wrapper)", vd.name),
                        ).complete(ConstructType::ValueActor),
                        ctx.actor_context.clone(),
                        TypedStream::infinite(persistence_stream),
                        var_persistence_id,
                    ));

                    Variable::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        persisted_actor,
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                    )
                };

                // Note: For referenced fields with forwarding actors, registration
                // already happened in schedule_expression, so we skip it here
                // FIX: Always register variables to ensure they're available for
                // nested function argument references. The is_referenced flag from
                // scope resolution doesn't correctly track references inside nested
                // function calls.
                if vd.forwarding_actor.is_none() {
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(vd.span, variable.value_actor());
                    }
                }

                // Register LINK variable senders with LinkConnector
                // IMPORTANT: Include scope to ensure LINK bindings inside functions
                // (like new_todo() in List/map) get unique identities per list item
                if vd.is_link {
                    if let Some(sender) = variable.link_value_sender() {
                        if let Some(link_connector) = ctx.try_link_connector() {
                            link_connector.register_link(vd.span, ctx.actor_context.scope.clone(), sender);
                        }
                    }
                }

                variables.push(variable);
            }

            let actor = Object::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Object {{..}}"),
                ),
                ctx.construct_context,
                persistence_id,
                ctx.actor_context,
                variables,
            );
            state.store(result_slot, actor);
        }

        WorkItem::BuildTaggedObject { tag, variable_data, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;

            // Build variables
            let mut variables = Vec::new();
            for vd in variable_data.iter() {
                let var_persistence_id = vd.persistence.as_ref().expect("persistence should be set by resolver").id;
                let variable = if vd.is_link && vd.forwarding_actor.is_some() {
                    // LINK variable with forwarding actor (referenced by sibling field)
                    let (forwarding_actor, forwarding_sender) = vd.forwarding_actor.as_ref().unwrap();

                    // First create a temporary LINK to get the connected sender and value_actor
                    let temp_link = Variable::new_link_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (link variable internal)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );

                    // Get the components we need from the temporary LINK
                    let link_value_actor = temp_link.value_actor();
                    let link_value_sender = temp_link.expect_link_value_sender();

                    // Connect LINK's value_actor to the forwarding actor
                    let link_value_actor_for_initial = link_value_actor.clone();
                    let forwarding_loop = ValueActor::connect_forwarding(
                        forwarding_sender.clone(),
                        link_value_actor.clone(),
                        async move { link_value_actor_for_initial.current_value().await.ok() },
                    );

                    // Create the final Variable with all components properly connected
                    // Note: link_value_actor is kept alive by forwarding_loop's subscription
                    Variable::new_link_arc_with_forwarding_loop(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (link variable with forwarding)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                        forwarding_actor.clone(),
                        link_value_sender,
                        forwarding_loop,
                    )
                } else if vd.is_link {
                    Variable::new_link_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (link variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    )
                } else if let Some((forwarding_actor, sender)) = &vd.forwarding_actor {
                    // Use the pre-created forwarding actor for referenced fields
                    // Connect forwarding from source actor to forwarding actor
                    let Some(source_actor) = state.get(vd.value_slot) else { continue; };

                    // connect_forwarding sends initial value asynchronously, then forwards async values
                    // Note: We create a 'static future by cloning source_actor into the async block
                    let source_actor_for_initial = source_actor.clone();
                    let forwarding_loop = ValueActor::connect_forwarding(
                        sender.clone(),
                        source_actor.clone(),
                        async move { source_actor_for_initial.current_value().await.ok() },
                    );
                    Variable::new_arc_with_forwarding_loop(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        forwarding_actor.clone(),
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                        forwarding_loop,
                    )
                } else {
                    // If value slot is empty, skip this variable
                    let Some(value_actor) = state.get(vd.value_slot) else { continue; };

                    // Wrap with persistence: load stored value first, save each emitted value
                    let persistence_stream = create_variable_persistence_stream(
                        value_actor.clone(),
                        ctx.construct_context.construct_storage.clone(),
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                        ctx.construct_context.clone(),
                        ctx.actor_context.clone(),
                    );

                    let persisted_actor = Arc::new(ValueActor::new(
                        ConstructInfo::new(
                            format!("PersistenceId: {} (persisted)", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable persistence wrapper)", vd.name),
                        ).complete(ConstructType::ValueActor),
                        ctx.actor_context.clone(),
                        TypedStream::infinite(persistence_stream),
                        var_persistence_id,
                    ));

                    Variable::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        persisted_actor,
                        var_persistence_id,
                        ctx.actor_context.scope.clone(),
                    )
                };

                // Note: For referenced fields with forwarding actors, registration
                // already happened in schedule_expression, so we skip it here
                // FIX: Always register variables to ensure they're available for
                // nested function argument references. The is_referenced flag from
                // scope resolution doesn't correctly track references inside nested
                // function calls.
                if vd.forwarding_actor.is_none() {
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(vd.span, variable.value_actor());
                    }
                }

                // Register LINK variable senders with LinkConnector
                // IMPORTANT: Include scope to ensure LINK bindings inside functions
                // (like new_todo() in List/map) get unique identities per list item
                if vd.is_link {
                    if let Some(sender) = variable.link_value_sender() {
                        if let Some(link_connector) = ctx.try_link_connector() {
                            link_connector.register_link(vd.span, ctx.actor_context.scope.clone(), sender);
                        }
                    }
                }

                variables.push(variable);
            }

            let actor = TaggedObject::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; {} {{..}}", tag),
                ),
                ctx.construct_context,
                persistence_id,
                ctx.actor_context,
                tag,
                variables,
            );
            state.store(result_slot, actor);
        }

        WorkItem::BuildLatest { input_slots, span, persistence, ctx, result_slot } => {
            // Collect inputs that have values (empty slots are ignored)
            let inputs: Vec<_> = input_slots.iter()
                .filter_map(|slot| state.get(*slot))
                .collect();
            let _persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;

            let actor = LatestCombinator::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {_persistence_id}"),
                    persistence,
                    format!("{span}; LATEST {{..}}"),
                ),
                ctx.construct_context,
                ctx.actor_context,
                inputs,
            );
            state.store(result_slot, actor);
        }

        WorkItem::BuildThen { piped_slot: _, body, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;
            // Merge context's snapshot (top-level functions) with state's registry (local functions)
            let mut merged_registry = ctx.function_registry_snapshot
                .as_ref()
                .map(|s| (**s).clone())
                .unwrap_or_default();
            for (name, def) in &state.function_registry {
                merged_registry.insert(name.clone(), def.clone());
            }
            let registry_snapshot = Arc::new(merged_registry);
            let actor = build_then_actor(*body, span, persistence, persistence_id, ctx, registry_snapshot)?;
            state.store(result_slot, actor);
        }

        WorkItem::BuildWhen { piped_slot: _, arms, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;
            // Merge context's snapshot (top-level functions) with state's registry (local functions)
            let mut merged_registry = ctx.function_registry_snapshot
                .as_ref()
                .map(|s| (**s).clone())
                .unwrap_or_default();
            for (name, def) in &state.function_registry {
                merged_registry.insert(name.clone(), def.clone());
            }
            let registry_snapshot = Arc::new(merged_registry);
            let actor = build_when_actor(arms, span, persistence, persistence_id, ctx, registry_snapshot)?;
            state.store(result_slot, actor);
        }

        WorkItem::BuildWhile { piped_slot: _, arms, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;
            // Merge context's snapshot (top-level functions) with state's registry (local functions)
            let mut merged_registry = ctx.function_registry_snapshot
                .as_ref()
                .map(|s| (**s).clone())
                .unwrap_or_default();
            for (name, def) in &state.function_registry {
                merged_registry.insert(name.clone(), def.clone());
            }
            let registry_snapshot = Arc::new(merged_registry);
            let actor = build_while_actor(arms, span, persistence, persistence_id, ctx, registry_snapshot)?;
            state.store(result_slot, actor);
        }

        WorkItem::BuildHold { initial_slot, state_param, body, span, persistence, ctx, result_slot } => {
            // If initial value slot is empty, produce nothing
            let Some(initial_actor) = state.get(initial_slot) else {
                return Ok(());
            };
            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;
            if let Some(actor) = build_hold_actor(initial_actor, state_param, *body, span, persistence, persistence_id, ctx)? {
                state.store(result_slot, actor);
            }
        }

        WorkItem::BuildBlock { object_slot, output_slot, result_slot } => {
            // If output slot is empty, this block produces nothing
            let Some(output_actor) = state.get(output_slot) else { return Ok(()); };

            // Get the Object actor which contains the Variables
            // We need to keep the Object alive so Variables don't get dropped
            let object_actor = state.get(object_slot);

            if let Some(object_actor) = object_actor {
                // Create a wrapper actor that subscribes to output and holds the Object
                // The stream::unfold keeps the Object alive in its closure, which keeps Variables alive
                // Use deferred subscription pattern for async subscribe()
                let value_stream = stream::unfold(
                    (None::<LocalBoxStream<'static, Value>>, Some(output_actor.clone()), object_actor),
                    |(subscription_opt, actor_opt, obj)| async move {
                        let mut subscription = match subscription_opt {
                            Some(s) => s,
                            None => actor_opt.unwrap().stream(),
                        };
                        subscription.next().await.map(|value| (value, (Some(subscription), None, obj)))
                    },
                );
                let wrapper = Arc::new(ValueActor::new(
                    ConstructInfo::new(
                        "Block wrapper".to_string(),
                        None,
                        "Block wrapper keeping variables alive".to_string(),
                    ).complete(ConstructType::ValueActor),
                    ActorContext::default(),
                    TypedStream::infinite(value_stream),
                    PersistenceId::new(),
                ));
                state.store(result_slot, wrapper);
            } else {
                // No Object (block has no variables) - just use output directly
                state.store(result_slot, output_actor);
            }
        }

        WorkItem::EvaluateWithPiped { expr, prev_slot, ctx, result_slot } => {
            // If piped value slot is empty, produce nothing
            let Some(prev_actor) = state.get(prev_slot) else {
                return Ok(());
            };
            let new_ctx = ctx.with_piped(prev_actor.clone());

            // Check if expression is a FunctionCall - these should consume the piped value
            // as their first argument (for builtin functions).
            if let static_expression::Expression::FunctionCall { path, arguments } = &expr.node {
                // Handle FunctionCall specially to set use_piped_for_builtin: true
                let path_strs: Vec<String> = path.iter().map(|s| s.to_string()).collect();
                let path_strs_ref: Vec<&str> = path_strs.iter().map(|s| s.as_str()).collect();

                // Check for List binding functions first
                match path_strs_ref.as_slice() {
                    ["List", "map"] | ["List", "retain"] | ["List", "remove"] | ["List", "every"] | ["List", "any"] | ["List", "sort_by"] | ["List", "append"] => {
                        // Handle List binding functions specially - they have their own handling
                        // These use the piped value from the context
                        // List/append also has special handling for call recording (persistence)
                        schedule_expression(state, expr, new_ctx, result_slot)?;
                    }
                    _ => {
                        // Normal function call - pre-evaluate arguments and create CallFunction with use_piped_for_builtin: true
                        let mut arg_slots = Vec::new();
                        let mut args_to_schedule = Vec::new();
                        let mut passed_slot = None;
                        let mut passed_context: Option<SlotId> = None;
                        let span = expr.span.clone();
                        let persistence = expr.persistence.clone();

                        for arg in arguments {
                            let arg_slot = state.alloc_slot();
                            let arg_name = arg.node.name.to_string();

                            if arg_name == "PASS" {
                                if let Some(value) = arg.node.value.clone() {
                                    args_to_schedule.push((value, arg_slot));
                                    passed_context = Some(arg_slot);
                                }
                                continue;
                            }

                            if let Some(value) = arg.node.value.clone() {
                                arg_slots.push((arg_name, arg_slot));
                                args_to_schedule.push((value, arg_slot));
                            } else {
                                if let Some(piped) = &new_ctx.actor_context.piped {
                                    state.store(arg_slot, piped.clone());
                                    arg_slots.push((arg_name, arg_slot));
                                    passed_slot = Some(arg_slot);
                                } else {
                                    return Err(format!("PASS argument requires piped value at {:?}", span));
                                }
                            }
                        }

                        // Push CallFunction with use_piped_for_builtin: true
                        state.push(WorkItem::CallFunction {
                            path: path_strs,
                            arg_slots,
                            passed_slot,
                            passed_context,
                            use_piped_for_builtin: true,  // This is the key difference!
                            span,
                            persistence,
                            ctx: new_ctx.clone(),
                            result_slot,
                        });

                        // Schedule argument expressions
                        for (arg_expr, arg_slot) in args_to_schedule {
                            schedule_expression(state, arg_expr, new_ctx.clone(), arg_slot)?;
                        }
                    }
                }
            } else if let static_expression::Expression::LinkSetter { alias } = &expr.node {
                // Handle LinkSetter when piped: send piped value TO the LINK, then pass through.
                // When you do `element |> LINK { store.path }`, the element is:
                // 1. Sent to the LINK variable at store.path
                // 2. Passed through unchanged for downstream use
                //
                // IMPORTANT: The pass-through must be STABLE across re-evaluations!
                // When Element re-evaluates (e.g., checkbox state changes), it creates a NEW
                // internal LINK actor. But downstream components (stripe) are subscribed to
                // the OLD pass-through. If we create a new pass-through each time, events
                // get lost. Solution: use PassThroughConnector to maintain stable pass-throughs.
                //
                // KEY FIX: On re-evaluation, we return a "relay" actor that subscribes to the
                // EXISTING pass-through and forwards its values. This ensures that any new
                // downstream code that uses the "current" result will receive values from
                // the stable pass-through.

                // Build key for stable pass-through lookup
                let persistence_id = expr.persistence.as_ref()
                    .expect("LinkSetter should have persistence_id from resolver")
                    .id
                    .clone();
                let pass_through_key = PassThroughKey {
                    persistence_id,
                    scope: new_ctx.actor_context.scope.clone(),
                };

                // Create stable pass-through with bounded mpsc channel
                let (value_tx, value_rx) = mpsc::channel::<Value>(64);

                // Create forwarder stream using async setup + flat_map pattern
                let alias_for_stream = alias.node.clone();
                let ctx_for_stream = new_ctx.clone();
                let prev_actor_for_stream = prev_actor.clone();
                let value_tx_for_stream = value_tx.clone();
                let value_tx_for_registration = value_tx.clone();
                let pass_through_key_for_stream = pass_through_key.clone();
                let pass_through_key_for_registration = pass_through_key.clone();
                let pass_through_key_for_forwarder_storage = pass_through_key.clone();
                let connector_for_stream: Option<Arc<PassThroughConnector>> = new_ctx.try_pass_through_connector().map(|c| c.clone());

                // Oneshot channel to pass the pass_through_actor to the forwarder for registration
                let (actor_tx, actor_rx) = oneshot::channel::<Arc<ValueActor>>();

                // Oneshot channel to pass forwarder actor for storage on re-evaluation
                let (forwarder_tx, forwarder_rx) = oneshot::channel::<Arc<ValueActor>>();

                // Get subscription scope to check if our context is still active
                // When WHILE switches arms, the old arm's scope is cancelled
                let subscription_scope_for_stream = new_ctx.actor_context.subscription_scope.clone();

                // Async setup: get LINK sender, check for existing pass-through, subscribe to prev_actor
                let setup_stream = stream::once(async move {
                    // Check if existing pass-through exists FIRST (before any registration)
                    let existing_sender: Option<mpsc::Sender<Value>> = match &connector_for_stream {
                        Some(conn) => conn.get_sender(pass_through_key_for_stream.clone()).await,
                        None => None,
                    };

                    let is_reeval = existing_sender.is_some();
                    if is_reeval {
                        // On re-evaluation, store the forwarder actor to keep it alive
                        if let (Some(conn), Ok(forwarder)) = (&connector_for_stream, forwarder_rx.await) {
                            conn.add_forwarder(pass_through_key_for_forwarder_storage, forwarder);
                        }
                    } else {
                        // Register this pass-through (first evaluation only)
                        // Wait for the actor to be created and sent to us
                        if let (Some(conn), Ok(actor)) = (&connector_for_stream, actor_rx.await) {
                            conn.register(pass_through_key_for_registration, value_tx_for_registration, actor);
                        }
                    }

                    // Get link sender (async)
                    let link_sender = get_link_sender_from_alias(alias_for_stream, ctx_for_stream).await;

                    // Subscribe to prev_actor
                    let sub = prev_actor_for_stream.clone().stream();

                    (link_sender, existing_sender, sub, value_tx_for_stream, subscription_scope_for_stream)
                });

                // Flatten setup into value forwarding stream
                let forwarder_stream = setup_stream.flat_map(|(link_sender, existing_sender, sub, value_tx, subscription_scope)| {
                    // Check if subscription scope is cancelled (e.g., WHILE arm switched)
                    sub.take_while(move |_| {
                        let is_active = subscription_scope.as_ref().map_or(true, |s| !s.is_cancelled());
                        future::ready(is_active)
                    })
                    .then(move |value| {
                        // Clone senders for async block
                        let link_sender = link_sender.clone();
                        let mut existing_sender = existing_sender.clone();
                        let mut value_tx = value_tx.clone();
                        async move {
                            // Forward to LINK
                            if let Some(ref sender) = link_sender {
                                sender.send_or_drop(value.clone());
                            }
                            // Forward to existing pass-through with backpressure (if re-evaluation)
                            if let Some(ref mut sender) = existing_sender {
                                if let Err(e) = sender.send(value.clone()).await {
                                    zoon::println!("[LINK_SETTER] Failed to forward to existing pass-through: {e}");
                                }
                            }
                            // Forward to our channel with backpressure (only matters on first evaluation)
                            if let Err(e) = value_tx.send(value.clone()).await {
                                zoon::println!("[LINK_SETTER] Failed to forward to value channel: {e}");
                            }
                            value
                        }
                    })
                });

                // Create forwarder actor - this does the actual forwarding work
                let forwarder_actor = ValueActor::new_arc(
                    ConstructInfo::new(
                        "LinkSetter forwarder",
                        expr.persistence.clone(),
                        format!("{:?}; LinkSetter forwarder", expr.span),
                    ),
                    new_ctx.actor_context.clone(),
                    TypedStream::infinite(forwarder_stream.chain(stream::pending())),
                    PersistenceId::new(),
                );

                // Send forwarder to async setup for storage on re-evaluation
                if forwarder_tx.send(forwarder_actor.clone()).is_err() {
                    zoon::println!("[LINK_SETTER] Forwarder receiver dropped");
                }

                // Create pass-through actor that emits from the channel
                // Keep forwarder_actor alive via inputs
                let pass_through_stream = value_rx.chain(stream::pending());
                let pass_through_actor = ValueActor::new_arc_with_inputs(
                    ConstructInfo::new(
                        "LinkSetter stable pass-through",
                        expr.persistence.clone(),
                        format!("{:?}; LinkSetter stable pass-through", expr.span),
                    ),
                    new_ctx.actor_context.clone(),
                    TypedStream::infinite(pass_through_stream),
                    PersistenceId::new(),
                    vec![forwarder_actor],
                );

                // Send actor to forwarder for registration (if first evaluation)
                if actor_tx.send(pass_through_actor.clone()).is_err() {
                    zoon::println!("[LINK_SETTER] Actor receiver dropped");
                }

                // Create relay actor that subscribes to EXISTING pass-through (if any)
                // This ensures any new downstream code receives values from the stable pass-through
                let connector_for_relay = new_ctx.try_pass_through_connector().map(|c| c.clone());
                let pass_through_key_for_relay = pass_through_key.clone();
                let relay_stream = stream::once(async move {
                    // Try to get existing actor
                    match &connector_for_relay {
                        Some(conn) => conn.get(pass_through_key_for_relay).await,
                        None => None,
                    }
                }).filter_map(|opt_actor| future::ready(opt_actor))
                .flat_map(|existing_actor| {
                    existing_actor.stream()
                });

                // Merge: on first eval, relay_stream is empty (no existing actor), so pass_through emits.
                // On re-eval, relay_stream forwards from existing actor.
                let merged_stream = stream::select(
                    pass_through_actor.clone().stream(),
                    relay_stream,
                );

                let result_actor = ValueActor::new_arc_with_inputs(
                    ConstructInfo::new(
                        "LinkSetter result (pass-through or relay)",
                        expr.persistence.clone(),
                        format!("{:?}; LinkSetter result", expr.span),
                    ),
                    new_ctx.actor_context.clone(),
                    TypedStream::infinite(merged_stream.chain(stream::pending())),
                    PersistenceId::new(),
                    vec![pass_through_actor],
                );

                state.store(result_slot, result_actor);
            } else {
                // For non-FunctionCall expressions, just schedule normally
                schedule_expression(state, expr, new_ctx, result_slot)?;
            }
        }

        WorkItem::CallFunction { path, arg_slots, passed_slot: _, passed_context, use_piped_for_builtin, span, persistence, mut ctx, result_slot } => {
            // Collect arguments that have values (empty slots are ignored)
            let args: Vec<(String, Arc<ValueActor>)> = arg_slots.iter()
                .filter_map(|(name, slot)| {
                    state.get(*slot).map(|actor| (name.clone(), actor))
                })
                .collect();

            // Update passed context if PASS argument was provided
            if let Some(passed_slot) = passed_context {
                if let Some(passed_actor) = state.get(passed_slot) {
                    ctx = EvaluationContext {
                        construct_context: ctx.construct_context,
                        actor_context: ActorContext {
                            output_valve_signal: ctx.actor_context.output_valve_signal,
                            piped: ctx.actor_context.piped,
                            passed: Some(passed_actor),
                            parameters: ctx.actor_context.parameters,
                            sequential_processing: ctx.actor_context.sequential_processing,
                            backpressure_permit: ctx.actor_context.backpressure_permit,
                            hold_state_update_callback: ctx.actor_context.hold_state_update_callback,
                            use_lazy_actors: ctx.actor_context.use_lazy_actors,
                            is_snapshot_context: ctx.actor_context.is_snapshot_context,
                            object_locals: ctx.actor_context.object_locals,
                            scope: ctx.actor_context.scope,
                            subscription_scope: ctx.actor_context.subscription_scope.clone(),
                            call_recorder: ctx.actor_context.call_recorder,
                            is_restoring: ctx.actor_context.is_restoring,
                            list_append_storage_key: ctx.actor_context.list_append_storage_key,
                            recording_counter: ctx.actor_context.recording_counter,
                            subscription_time: ctx.actor_context.subscription_time,
                        },
                        reference_connector: ctx.reference_connector,
                        link_connector: ctx.link_connector,
                        pass_through_connector: ctx.pass_through_connector,
                        function_registry_snapshot: ctx.function_registry_snapshot,
                        module_loader: ctx.module_loader,
                        source_code: ctx.source_code,
                    };
                }
            }

            let persistence_id = persistence.as_ref().expect("persistence should be set by resolver").id;
            let actor_opt = call_function(
                path.clone(),
                args,
                span,
                persistence.clone(),
                persistence_id,
                ctx.clone(),
                use_piped_for_builtin,
                &state.function_registry,
            )?;

            // Take forwarding loops from state - they need to stay alive as long as the result actor
            let forwarding_loops = state.take_forwarding_loops();

            // If function returns SKIP (None), don't store anything
            if let Some(actor) = actor_opt {
                // If there are forwarding loops, wrap the result to keep them alive
                if !forwarding_loops.is_empty() {
                    // Create a wrapper actor that forwards values and keeps forwarding loops alive
                    let wrapper = ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", persistence.as_ref().map(|p| p.id)),
                            persistence.clone(),
                            format!("{}; {}(..) (with forwarding)", span, path.join("/")),
                        ),
                        ctx.actor_context.clone(),
                        TypedStream::infinite(actor.stream().scan(forwarding_loops, |_loops, value| {
                            // Keep _loops alive in scan state
                            async move { Some(value) }
                        })),
                        persistence.map(|p| p.id).unwrap_or_else(PersistenceId::new),
                    );
                    state.store(result_slot, wrapper);
                } else {
                    state.store(result_slot, actor);
                }
            }
        }

        WorkItem::ConnectForwardingActors { connections } => {
            // Connect forwarding actors to their evaluated argument results.
            // We need to forward values from the source actor to the forwarding actor's channel.
            // The forwarding actor was created with an empty channel that needs values.
            for (slot, sender) in connections {
                if let Some(source_actor) = state.get(slot) {
                    // Use ValueActor::connect_forwarding which properly subscribes and forwards ALL values.
                    // This creates an ActorLoop that:
                    // 1. Subscribes to source_actor
                    // 2. Forwards every value through sender
                    // 3. Stays alive until sender is dropped
                    //
                    // We store the ActorLoop in the state to keep it alive for the duration
                    // of the function call evaluation.
                    let forwarding_loop = ValueActor::connect_forwarding(
                        sender,
                        source_actor.clone(),
                        async { None }, // No initial value needed - subscription provides it
                    );

                    // Store in state to keep alive
                    state.add_forwarding_loop(forwarding_loop);
                }
            }
        }

        WorkItem::WrapWithPersistence { source_slot, persistence_id, ctx, result_slot } => {
            // Wrap an evaluated function argument with persistence.
            // This enables persistence for LATEST and other constructs used as function arguments.
            zoon::println!("[DEBUG] Processing WrapWithPersistence for slot {:?} with id {}", source_slot, persistence_id);
            if let Some(source_actor) = state.get(source_slot) {
                zoon::println!("[DEBUG] Found source actor, wrapping with persistence");
                let persistence_stream = create_variable_persistence_stream(
                    source_actor.clone(),
                    ctx.construct_context.construct_storage.clone(),
                    persistence_id,
                    ctx.actor_context.scope.clone(),
                    ctx.construct_context.clone(),
                    ctx.actor_context.clone(),
                );

                let persisted_actor = Arc::new(ValueActor::new(
                    ConstructInfo::new(
                        format!("PersistenceId: {} (argument persistence)", persistence_id),
                        None,
                        format!("function argument persistence wrapper"),
                    ).complete(ConstructType::ValueActor),
                    ctx.actor_context.clone(),
                    TypedStream::infinite(persistence_stream),
                    persistence_id,
                ));

                state.store(result_slot, persisted_actor);
            }
        }
    }

    Ok(())
}

/// Create a binary operation actor (arithmetic or comparison).
fn create_binary_op_actor(
    op: BinaryOpKind,
    construct_info: ConstructInfo,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    a: Arc<ValueActor>,
    b: Arc<ValueActor>,
) -> Arc<ValueActor> {
    match op {
        BinaryOpKind::Add => ArithmeticCombinator::new_add(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::Subtract => ArithmeticCombinator::new_subtract(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::Multiply => ArithmeticCombinator::new_multiply(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::Divide => ArithmeticCombinator::new_divide(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::Equal => ComparatorCombinator::new_equal(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::NotEqual => ComparatorCombinator::new_not_equal(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::Greater => ComparatorCombinator::new_greater(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::GreaterOrEqual => ComparatorCombinator::new_greater_or_equal(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::Less => ComparatorCombinator::new_less(construct_info, construct_context, actor_context, a, b),
        BinaryOpKind::LessOrEqual => ComparatorCombinator::new_less_or_equal(construct_info, construct_context, actor_context, a, b),
    }
}

/// Evaluate an Alias expression immediately (for simple cases).
fn evaluate_alias_immediate(
    alias: static_expression::Alias,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    type BoxedFuture = Pin<Box<dyn std::future::Future<Output = Arc<ValueActor>>>>;

    let root_value_actor: BoxedFuture = match &alias {
        static_expression::Alias::WithPassed { extra_parts: _ } => {
            match &ctx.actor_context.passed {
                Some(passed) => {
                    let passed = passed.clone();
                    Box::pin(async move { passed })
                }
                None => {
                    return Err("PASSED is not available in this context".to_string());
                }
            }
        }
        static_expression::Alias::WithoutPassed { parts, referenced_span } => {
            let first_part = parts.first().map(|s| s.to_string()).unwrap_or_default();
            if let Some(param_actor) = ctx.actor_context.parameters.get(&first_part).cloned() {
                // For simple parameter references (no field accesses), return directly
                if parts.len() == 1 {
                    return Ok(param_actor);
                }
                // For multi-part aliases (e.g., state.current), wrap in async Future
                Box::pin(async move { param_actor })
            } else if let Some(ref_span) = referenced_span {
                // First check object_locals for instance-specific resolution
                // This prevents span-based overwrites when multiple Objects are created
                // from the same function definition (Bug 7.2 fix)
                if let Some(local_actor) = ctx.actor_context.object_locals.get(ref_span).cloned() {
                    Box::pin(async move { local_actor })
                } else {
                    // Fall back to async lookup via ReferenceConnector
                    let ref_connector = ctx.try_reference_connector()
                        .ok_or_else(|| "ReferenceConnector dropped - program shutting down".to_string())?;
                    Box::pin(ref_connector.referenceable(*ref_span))
                }
            } else if parts.len() >= 2 {
                // Module variable access - for now fall back to returning an error
                return Err(format!("Module variable access '{}' not yet supported in stack-safe evaluator", first_part));
            } else {
                return Err(format!("Failed to get aliased variable '{}'", first_part));
            }
        }
    };
    Ok(VariableOrArgumentReference::new_arc_value_actor(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; (alias)"),
        ),
        ctx.construct_context,
        ctx.actor_context,
        alias,
        root_value_actor,
    ))
}

/// Build a THEN actor (runtime evaluation of body for each piped value).
fn build_then_actor(
    body: static_expression::Spanned<static_expression::Expression>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    function_registry_snapshot: Arc<HashMap<String, StaticFunctionDefinition>>,
) -> Result<Arc<ValueActor>, String> {
    let piped = ctx.actor_context.piped.clone()
        .ok_or("THEN requires a piped value")?;

    let sequential = ctx.actor_context.sequential_processing;
    let backpressure_permit = ctx.actor_context.backpressure_permit.clone();

    // Clone context components for the async closure
    let construct_context_for_then = ctx.construct_context.clone();
    let actor_context_for_then = ctx.actor_context.clone();
    let reference_connector_for_then = ctx.reference_connector.clone();
    let link_connector_for_then = ctx.link_connector.clone();
    let pass_through_connector_for_then = ctx.pass_through_connector.clone();
    let function_registry_for_then = function_registry_snapshot;
    let module_loader_for_then = ctx.module_loader.clone();
    let source_code_for_then = ctx.source_code.clone();
    let persistence_for_then = persistence.clone();
    let span_for_then = span;

    // Clone backpressure_permit for the closure
    let backpressure_permit_for_then = backpressure_permit.clone();

    // Clone HOLD callback for state updates
    let hold_callback_for_then = actor_context_for_then.hold_state_update_callback.clone();

    // eval_body now returns a Stream instead of Option<Value>
    // This avoids blocking on .next().await which would hang if body returns SKIP
    let eval_body = move |value: Value| -> Pin<Box<dyn Future<Output = Pin<Box<dyn Stream<Item = Value>>>>>> {
        let actor_context_clone = actor_context_for_then.clone();
        let construct_context_clone = construct_context_for_then.clone();
        let reference_connector_clone = reference_connector_for_then.clone();
        let link_connector_clone = link_connector_for_then.clone();
        let pass_through_connector_clone = pass_through_connector_for_then.clone();
        let function_registry_clone = function_registry_for_then.clone();
        let module_loader_clone = module_loader_for_then.clone();
        let source_code_clone = source_code_for_then.clone();
        let persistence_clone = persistence_for_then.clone();
        let body_clone = body.clone();
        let permit_clone = backpressure_permit_for_then.clone();
        let hold_callback_clone = hold_callback_for_then.clone();

        Box::pin(async move {
            // Acquire permit BEFORE body evaluation - this ensures HOLD's state update
            // completes before we read state for the next iteration. Without this,
            // all pulses would run in parallel reading the same initial state.
            if let Some(ref permit) = permit_clone {
                permit.acquire().await;
            }

            let value_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    "THEN input value".to_string(),
                    None,
                    format!("{span_for_then}; THEN input"),
                ),
                actor_context_clone.clone(),
                constant(value),
                PersistenceId::new(),
            );

            // CRITICAL FIX: Freeze parameters for SNAPSHOT semantics.
            // When THEN body references `state` (from HOLD), we want the CURRENT value at the
            // time of body evaluation, not all historical values from the reactive subscription.
            //
            // Without this fix:
            // - Body creates BinaryOpCombinator for `state + 1`
            // - BinaryOpCombinator subscribes to state_actor starting at version 0
            // - Subscription returns ALL historical values: [{value:0}, {value:1}, ...]
            // - First poll returns {value:0} (OLD value!) instead of current {value:1}
            // - Result is 1 instead of 2
            //
            // With this fix:
            // - We create a "frozen" actor for each parameter with just the current value
            // - Body subscribes to this frozen actor, gets only the current value
            // - Computation uses the correct current state
            let mut frozen_parameters: HashMap<String, Arc<ValueActor>> = HashMap::new();
            for (name, actor) in actor_context_clone.parameters.iter() {
                // Create a constant actor from the current stored value (async)
                if let Ok(current_value) = actor.current_value().await {
                    let frozen_actor = ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("frozen param: {name}"),
                            None,
                            format!("frozen parameter {name}"),
                        ),
                        actor_context_clone.clone(),
                        constant(current_value),
                        PersistenceId::new(),
                    );
                    frozen_parameters.insert(name.clone(), frozen_actor);
                } else {
                    // No value yet, keep original actor
                    frozen_parameters.insert(name.clone(), actor.clone());
                }
            }

            let new_actor_context = ActorContext {
                output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                piped: Some(value_actor.clone()),
                passed: actor_context_clone.passed.clone(),
                parameters: frozen_parameters,
                sequential_processing: actor_context_clone.sequential_processing,
                backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                // Don't propagate callback to body - body evaluation is internal
                hold_state_update_callback: None,
                use_lazy_actors: actor_context_clone.use_lazy_actors,
                // THEN body needs snapshot semantics - read current variable values, not history
                is_snapshot_context: true,
                // Inherit object_locals - THEN body may reference Object sibling fields
                object_locals: actor_context_clone.object_locals.clone(),
                scope: actor_context_clone.scope.clone(),
                subscription_scope: actor_context_clone.subscription_scope.clone(),
                call_recorder: actor_context_clone.call_recorder.clone(),
                is_restoring: actor_context_clone.is_restoring,
                list_append_storage_key: actor_context_clone.list_append_storage_key.clone(),
                recording_counter: actor_context_clone.recording_counter.clone(),
                // THEN body uses snapshot semantics for variables - don't filter stale values
                // The filtering should only happen on the piped stream, not all variable refs
                subscription_time: None,
            };

            let new_ctx = EvaluationContext {
                construct_context: construct_context_clone.clone(),
                actor_context: new_actor_context.clone(),
                reference_connector: reference_connector_clone,
                link_connector: link_connector_clone,
                pass_through_connector: pass_through_connector_clone,
                module_loader: module_loader_clone,
                source_code: source_code_clone,
                function_registry_snapshot: Some(function_registry_clone),
            };

            let body_expr = static_expression::Spanned {
                span: body_clone.span,
                persistence: persistence_clone,
                node: body_clone.node.clone(),
            };

            match evaluate_expression(body_expr, new_ctx) {
                Ok(Some(result_actor)) => {
                    // Use value() for type-safe single-value semantics.
                    // value() returns a Future that resolves to exactly ONE value,
                    // making it impossible to accidentally create ongoing subscriptions.
                    // This is critical for THEN bodies which should produce exactly ONE value per input.
                    //
                    // CRITICAL: Keepalive must be in filter_map, NOT in map!
                    // If value() returns Err, filter_map filters it out and map closure never runs.
                    // This would cause actors to be dropped before value() completes.
                    //
                    // We also need to keep new_actor_context alive because result_actor may
                    // have subscriptions to frozen_parameters contained within it.
                    let result_actor_keepalive = result_actor.clone();
                    let value_actor_keepalive = value_actor.clone();
                    let context_keepalive = new_actor_context.clone();
                    let hold_callback_for_map = hold_callback_clone.clone();
                    let result_stream = stream::once(result_actor.value())
                        .filter_map(move |v| {
                            // Prevent drop: these are captured by the `move` closure and live as long as the stream combinator
                            let _result_actor_keepalive = &result_actor_keepalive;
                            let _value_actor_keepalive = &value_actor_keepalive;
                            let _context_keepalive = &context_keepalive;
                            async move { v.ok() }
                        })
                        .map(move |mut result_value| {
                        // Prevent drop: captured by `move` closure, lives as long as stream combinator
                        let _value_actor = &value_actor;
                        result_value.set_idempotency_key(ValueIdempotencyKey::new());
                        // CRITICAL: Call HOLD's callback synchronously if present.
                        // This updates state_actor and releases the permit BEFORE this stream yields,
                        // enabling the next pulse to be processed synchronously during eager polling.
                        if let Some(ref callback) = hold_callback_for_map {
                            callback(result_value.clone());
                        }
                        result_value
                    });
                    Box::pin(result_stream) as Pin<Box<dyn Stream<Item = Value>>>
                }
                Ok(None) => {
                    // SKIP - return finite empty stream (flatten_unordered removes it cleanly)
                    Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
                }
                Err(_e) => {
                    Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
                }
            }
        })
    };

    // =========================================================================
    // REACTIVE SUBSCRIPTION WITH LAMPORT FILTERING
    // =========================================================================
    // Use then + flatten_unordered instead of then + filter_map
    // flatten_unordered processes inner streams concurrently, so even if one stream
    // never emits (SKIP case), others can still produce values
    //
    // NOTE: We use async-only architecture. All values flow through the subscription
    // stream. No synchronous initial processing - let the async runtime handle ordering.
    //
    // LAMPORT FILTERING: Record the current Lamport time when this THEN is created.
    // Any values from the piped stream that happened-before this time are stale
    // (e.g., old click events) and should not trigger body evaluation.
    // This fixes the Toggle All bug where new todos receive old toggle events.
    let subscription_time = lamport_now();
    let filtered_piped = piped.clone().stream().filter(move |value| {
        let should_pass = !value.happened_before(subscription_time);
        future::ready(should_pass)
    });

    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
        // For sequential mode, use regular flatten (processes one stream at a time)
        let stream = filtered_piped
            .then(eval_body)
            .flatten();
        Box::pin(stream)
    } else {
        // For non-sequential mode, use flatten_unordered for concurrent processing
        let stream = filtered_piped
            .then(eval_body)
            .flatten_unordered(None);
        Box::pin(stream)
    };

    // Keep the piped actor alive by including it in inputs
    // Use lazy actor construction when in HOLD body context for sequential state updates
    if ctx.actor_context.use_lazy_actors {
        Ok(ValueActor::new_arc_lazy(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; THEN {{..}}"),
            ).complete(ConstructType::ValueActor),
            flattened_stream,
            persistence_id,
            vec![piped],  // Keep piped actor alive
        ))
    } else {
        Ok(ValueActor::new_arc_with_inputs(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; THEN {{..}}"),
            ),
            ctx.actor_context,
            TypedStream::infinite(flattened_stream),
            persistence_id,
            vec![piped],  // Keep piped actor alive
        ))
    }
}

/// Build a WHEN actor (pattern matching on piped values).
fn build_when_actor(
    arms: Vec<static_expression::Arm>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    function_registry_snapshot: Arc<HashMap<String, StaticFunctionDefinition>>,
) -> Result<Arc<ValueActor>, String> {
    let piped = ctx.actor_context.piped.clone()
        .ok_or("WHEN requires a piped value")?;

    let sequential = ctx.actor_context.sequential_processing;
    let backpressure_permit = ctx.actor_context.backpressure_permit.clone();
    let should_materialize = backpressure_permit.is_some();

    let construct_context_for_when = ctx.construct_context.clone();
    let actor_context_for_when = ctx.actor_context.clone();
    let reference_connector_for_when = ctx.reference_connector.clone();
    let link_connector_for_when = ctx.link_connector.clone();
    let pass_through_connector_for_when = ctx.pass_through_connector.clone();
    let function_registry_for_when = function_registry_snapshot;
    let module_loader_for_when = ctx.module_loader.clone();
    let source_code_for_when = ctx.source_code.clone();
    let persistence_for_when = persistence.clone();
    let span_for_when = span;

    // eval_body returns a Stream instead of Option<Value>
    // This allows nested WHENs that return SKIP to work correctly:
    // - SKIP returns empty stream (no blocking)
    // - Regular values return stream with one item
    // - flat_map naturally handles empty streams
    let eval_body = move |value: Value| -> Pin<Box<dyn Future<Output = Pin<Box<dyn Stream<Item = Value>>>>>> {
        let actor_context_clone = actor_context_for_when.clone();
        let construct_context_clone = construct_context_for_when.clone();
        let reference_connector_clone = reference_connector_for_when.clone();
        let link_connector_clone = link_connector_for_when.clone();
        let pass_through_connector_clone = pass_through_connector_for_when.clone();
        let function_registry_clone = function_registry_for_when.clone();
        let module_loader_clone = module_loader_for_when.clone();
        let source_code_clone = source_code_for_when.clone();
        let persistence_clone = persistence_for_when.clone();
        let arms_clone = arms.clone();

        Box::pin(async move {
            // Debug: log what WHEN receives
            let value_desc = match &value {
                Value::Tag(tag, _) => format!("Tag({})", tag.tag()),
                Value::Object(_, _) => "Object".to_string(),
                Value::Text(t, _) => format!("Text({})", t.text()),
                Value::List(_, _) => "List".to_string(),
                _ => "Other".to_string(),
            };
            zoon::println!("[WHEN] Received value: {}", value_desc);

            // Try to match against each arm
            for arm in &arms_clone {
                // Use async pattern matching to properly extract bindings from Objects
                if let Some(bindings) = match_pattern(&arm.pattern, &value).await {
                    zoon::println!("[WHEN] Pattern MATCHED: {:?}", arm.pattern);
                    let value_actor = ValueActor::new_arc(
                        ConstructInfo::new(
                            "WHEN input value".to_string(),
                            None,
                            format!("{span_for_when}; WHEN input"),
                        ),
                        actor_context_clone.clone(),
                        constant(value.clone()),
                        PersistenceId::new(),
                    );

                    // CRITICAL FIX: Freeze parameters for SNAPSHOT semantics (same as THEN).
                    // When WHEN body references `state` (from HOLD), we want the CURRENT value at the
                    // time of body evaluation, not all historical values from the reactive subscription.
                    let mut frozen_parameters: HashMap<String, Arc<ValueActor>> = HashMap::new();
                    for (name, actor) in actor_context_clone.parameters.iter() {
                        if let Ok(current_value) = actor.current_value().await {
                            let frozen_actor = ValueActor::new_arc(
                                ConstructInfo::new(
                                    format!("frozen param: {name}"),
                                    None,
                                    format!("frozen parameter {name}"),
                                ),
                                actor_context_clone.clone(),
                                constant(current_value),
                                PersistenceId::new(),
                            );
                            frozen_parameters.insert(name.clone(), frozen_actor);
                        } else {
                            frozen_parameters.insert(name.clone(), actor.clone());
                        }
                    }

                    // Create parameter actors for the pattern bindings
                    let mut parameters = frozen_parameters;
                    for (name, bound_value) in bindings {
                        let bound_actor = ValueActor::new_arc(
                            ConstructInfo::new(
                                format!("WHEN binding: {}", name),
                                None,
                                format!("{span_for_when}; WHEN binding"),
                            ),
                            actor_context_clone.clone(),
                            constant(bound_value),
                            PersistenceId::new(),
                        );
                        parameters.insert(name, bound_actor);
                    }

                    let new_actor_context = ActorContext {
                        output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                        piped: Some(value_actor.clone()),
                        passed: actor_context_clone.passed.clone(),
                        parameters,
                        sequential_processing: actor_context_clone.sequential_processing,
                        backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                        // Don't propagate HOLD callback into WHEN arms - each arm is a separate evaluation
                        hold_state_update_callback: None,
                        use_lazy_actors: actor_context_clone.use_lazy_actors,
                        // WHEN body needs snapshot semantics like THEN
                        is_snapshot_context: true,
                        // Inherit object_locals - WHEN body may reference Object sibling fields
                        object_locals: actor_context_clone.object_locals.clone(),
                        scope: actor_context_clone.scope.clone(),
                        subscription_scope: actor_context_clone.subscription_scope.clone(),
                        call_recorder: actor_context_clone.call_recorder.clone(),
                        is_restoring: actor_context_clone.is_restoring,
                        list_append_storage_key: actor_context_clone.list_append_storage_key.clone(),
                        recording_counter: actor_context_clone.recording_counter.clone(),
                        // WHEN body uses snapshot semantics for variables - don't filter stale values
                        // The filtering should only happen on the piped stream, not all variable refs
                        subscription_time: None,
                    };

                    let new_ctx = EvaluationContext {
                        construct_context: construct_context_clone.clone(),
                        actor_context: new_actor_context.clone(),
                        reference_connector: reference_connector_clone.clone(),
                        link_connector: link_connector_clone.clone(),
                        pass_through_connector: pass_through_connector_clone.clone(),
                        module_loader: module_loader_clone.clone(),
                        source_code: source_code_clone.clone(),
                        function_registry_snapshot: Some(function_registry_clone.clone()),
                    };

                    match evaluate_expression(arm.body.clone(), new_ctx) {
                        Ok(Some(result_actor)) => {
                            // Use value() for type-safe single-value semantics.
                            // value() returns a Future that resolves to exactly ONE value,
                            // making it impossible to accidentally create ongoing subscriptions.
                            // This is critical for WHEN bodies which should produce exactly ONE value per input.
                            let result_actor_keepalive = result_actor.clone();
                            let result_stream = stream::once(result_actor.value())
                                .filter_map(|v| future::ready(v.ok()))
                                .map(move |mut result_value| {
                                    // Prevent drop: captured by `move` closure, lives as long as stream combinator
                                    let _value_actor = &value_actor;
                                    let _result_actor_keepalive = &result_actor_keepalive;
                                    result_value.set_idempotency_key(ValueIdempotencyKey::new());
                                    result_value
                                });
                            return Box::pin(result_stream) as Pin<Box<dyn Stream<Item = Value>>>;
                        }
                        Ok(None) => {
                            // SKIP - return finite empty stream (flatten_unordered removes it cleanly)
                            return Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>;
                        }
                        Err(_) => {
                            return Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>;
                        }
                    }
                }
            }
            Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
        })
    };

    // =========================================================================
    // REACTIVE SUBSCRIPTION (NO LAMPORT FILTERING FOR WHEN)
    // =========================================================================
    // Use then + flatten_unordered instead of then + filter_map
    // flatten_unordered processes inner streams concurrently, so even if one stream
    // never emits (SKIP case), others can still produce values
    //
    // NOTE: WHEN does NOT use Lamport filtering because WHEN is typically used
    // with DATA sources (computed values, state) which should provide their
    // current value to new subscribers. Unlike THEN which handles EVENTS,
    // WHEN handles pattern matching on data which needs immediate current values.
    //
    // Example: `count |> WHEN { 1 => "1 item", n => "{n} items" }` needs the
    // current count immediately, not just future changes.
    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
        // For sequential mode, use regular flatten (processes one stream at a time)
        let stream = piped.clone().stream()
            .then(eval_body)
            .flatten();
        Box::pin(stream)
    } else {
        // For non-sequential mode, use flatten_unordered for concurrent processing
        // None = unlimited concurrency
        let stream = piped.clone().stream()
            .then(eval_body)
            .flatten_unordered(None);
        Box::pin(stream)
    };

    // Keep the piped actor alive by including it in inputs
    Ok(ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; WHEN {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(flattened_stream),
        persistence_id,
        vec![piped],  // Keep piped actor alive
    ))
}

/// Build a WHILE actor (continuous processing while pattern matches).
fn build_while_actor(
    arms: Vec<static_expression::Arm>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    function_registry_snapshot: Arc<HashMap<String, StaticFunctionDefinition>>,
) -> Result<Arc<ValueActor>, String> {
    let piped = ctx.actor_context.piped.clone()
        .ok_or("WHILE requires a piped value")?;

    let construct_context_for_while = ctx.construct_context.clone();
    let actor_context_for_while = ctx.actor_context.clone();
    let reference_connector_for_while = ctx.reference_connector.clone();
    let link_connector_for_while = ctx.link_connector.clone();
    let pass_through_connector_for_while = ctx.pass_through_connector.clone();
    let function_registry_for_while = function_registry_snapshot;
    let module_loader_for_while = ctx.module_loader.clone();
    let source_code_for_while = ctx.source_code.clone();
    let persistence_for_while = persistence.clone();
    let span_for_while = span;

    // Use switch_map for proper WHILE semantics - when input changes, cancel old arm and switch to new one.
    // This is essential for reactive UI: when `current_page` changes from `Home` to `About`,
    // we must STOP rendering Home content and START rendering About content.
    // Regular flatten() would merge both streams, causing both to render.
    //
    // The subscription scope cancellation is handled by ScopeGuard:
    // - Each arm creates a ScopeGuard that holds the scope
    // - The guard is kept alive in the stream
    // - When switch_map drops the old inner stream, the guard is dropped
    // - The guard's Drop impl cancels the scope, terminating all subscriptions in that arm
    let stream = switch_map(piped.clone().stream(), move |value| {
        let actor_context_clone = actor_context_for_while.clone();
        let construct_context_clone = construct_context_for_while.clone();
        let reference_connector_clone = reference_connector_for_while.clone();
        let link_connector_clone = link_connector_for_while.clone();
        let pass_through_connector_clone = pass_through_connector_for_while.clone();
        let function_registry_clone = function_registry_for_while.clone();
        let module_loader_clone = module_loader_for_while.clone();
        let source_code_clone = source_code_for_while.clone();
        let persistence_clone = persistence_for_while.clone();
        let arms_clone = arms.clone();

        // Wrap async pattern matching in stream::once().flatten() so switch_map can work with it.
        // When a new input arrives, switch_map will drop this whole inner stream (cancelling
        // any async work and the forwarded body stream) and start a new one.
        stream::once(async move {
            // Find matching arm using async pattern matching
            let mut matched_arm_with_bindings: Option<(usize, &static_expression::Arm, HashMap<String, Value>)> = None;
            for (arm_idx, arm) in arms_clone.iter().enumerate() {
                if let Some(bindings) = match_pattern(&arm.pattern, &value).await {
                    matched_arm_with_bindings = Some((arm_idx, arm, bindings));
                    break;
                }
            }

            if let Some((arm_idx, arm, bindings)) = matched_arm_with_bindings {
                // Create a new subscription scope for this arm
                // The ScopeGuard will cancel the scope when dropped (when switch_map drops the inner stream)
                let arm_scope = Arc::new(SubscriptionScope::new());
                let scope_guard = ScopeGuard::new(arm_scope.clone());

                let value_actor = ValueActor::new_arc(
                    ConstructInfo::new(
                        "WHILE input value".to_string(),
                        None,
                        format!("{span_for_while}; WHILE input"),
                    ),
                    actor_context_clone.clone(),
                    constant(value),
                    PersistenceId::new(),
                );

                let mut parameters = actor_context_clone.parameters.clone();
                for (name, bound_value) in bindings {
                    let bound_actor = ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("WHILE binding: {}", name),
                            None,
                            format!("{span_for_while}; WHILE binding"),
                        ),
                        actor_context_clone.clone(),
                        constant(bound_value),
                        PersistenceId::new(),
                    );
                    parameters.insert(name, bound_actor);
                }

                let new_actor_context = ActorContext {
                    output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                    piped: Some(value_actor),
                    passed: actor_context_clone.passed.clone(),
                    parameters,
                    sequential_processing: actor_context_clone.sequential_processing,
                    backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                    // Propagate HOLD callback through WHILE arms - body might need it
                    hold_state_update_callback: actor_context_clone.hold_state_update_callback.clone(),
                    use_lazy_actors: actor_context_clone.use_lazy_actors,
                    // WHILE is continuous, not snapshot - variables should stream normally
                    is_snapshot_context: false,
                    // Inherit object_locals - WHILE body may reference Object sibling fields
                    object_locals: actor_context_clone.object_locals.clone(),
                    // Use ARM INDEX for scope, not Ulid. This keeps the scope STABLE
                    // across re-evaluations of the same arm, so subscription keys remain
                    // consistent with what the bridge is connected to.
                    //
                    // When WHILE re-evaluates:
                    // 1. Old Variables are dropped (their actors stop)
                    // 2. New Variables are created with same scope (same arm index)
                    // 3. Bridge stays connected to OLD Variable's sender
                    // 4. HOLD's subscription key is same, but inner stream ends when old Variable drops
                    // 5. switch_map_by_key creates new subscription when next value arrives
                    //
                    // The key insight: we want HOLD to subscribe to the Variable that bridge
                    // is connected to. Using stable scope (arm index) ensures key consistency.
                    scope: {
                        use super::super::super::parser::Scope;
                        let scope_id = format!("while_arm_{}", arm_idx);
                        match &actor_context_clone.scope {
                            Scope::Root => Scope::Nested(scope_id),
                            Scope::Nested(existing) => {
                                Scope::Nested(format!("{}:{}", existing, scope_id))
                            }
                        }
                    },
                    // Set the subscription scope for this WHILE arm
                    // All subscriptions created within this arm will check this scope
                    // and terminate when it's cancelled (on arm switch)
                    subscription_scope: Some(arm_scope.clone()),
                    call_recorder: actor_context_clone.call_recorder.clone(),
                    is_restoring: actor_context_clone.is_restoring,
                    list_append_storage_key: actor_context_clone.list_append_storage_key.clone(),
                    recording_counter: actor_context_clone.recording_counter.clone(),
                    // WHILE is streaming context - don't filter stale values, accept all
                    subscription_time: None,
                };

                let new_ctx = EvaluationContext {
                    construct_context: construct_context_clone,
                    actor_context: new_actor_context,
                    reference_connector: reference_connector_clone,
                    link_connector: link_connector_clone,
                    pass_through_connector: pass_through_connector_clone,
                    module_loader: module_loader_clone,
                    source_code: source_code_clone,
                    function_registry_snapshot: Some(function_registry_clone),
                };

                match evaluate_expression(arm.body.clone(), new_ctx) {
                    Ok(Some(result_actor)) => {
                        // Use stream() for continuous streaming semantics
                        // Wrap the stream to keep scope_guard alive - when this stream is dropped
                        // (by switch_map switching to new arm), the guard is dropped, cancelling scope
                        let body_stream = result_actor.stream();
                        stream::unfold((body_stream, Some(scope_guard)), |(mut s, guard)| async move {
                            s.next().await.map(|v| (v, (s, guard)))
                        }).boxed_local()
                    }
                    Ok(None) => {
                        // SKIP - scope_guard dropped here, scope cancelled immediately
                        drop(scope_guard);
                        stream::empty().boxed_local()
                    }
                    Err(_e) => {
                        // Error evaluating body - scope_guard dropped here, scope cancelled
                        drop(scope_guard);
                        stream::empty().boxed_local()
                    }
                }
            } else {
                stream::empty().boxed_local()
            }
        }).flatten()
    });

    // Keep the piped actor alive by including it in inputs
    Ok(ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; WHILE {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(stream),
        persistence_id,
        vec![piped],  // Keep piped actor alive
    ))
}

/// Asynchronously extract a field value from a Value following a path of field names.
/// Returns None if the path cannot be fully resolved (e.g., non-object value, missing field).
async fn extract_field_path(value: &Value, path: &[String]) -> Option<Value> {
    let mut current = value.clone();
    for field_name in path {
        match &current {
            Value::Object(object, _) => {
                let variable_actor = object.expect_variable(field_name).value_actor();
                // Use value() to wait for first value if not yet stored
                if let Ok(val) = variable_actor.clone().value().await {
                    current = val;
                } else {
                    // Field actor dropped
                    return None;
                }
            }
            Value::TaggedObject(tagged_object, _) => {
                let variable_actor = tagged_object.expect_variable(field_name).value_actor();
                // Use value() to wait for first value if not yet stored
                if let Ok(val) = variable_actor.clone().value().await {
                    current = val;
                } else {
                    // Field actor dropped
                    return None;
                }
            }
            _ => {
                // Not an object - cannot extract field
                return None;
            }
        }
    }
    Some(current)
}

/// Build a FieldAccess actor (.field.subfield at pipe position).
/// FieldAccess: `stream |> .field.subfield`
/// Equivalent to: `stream |> WHILE { value => value.field.subfield }`
/// For each value from the piped stream, navigates through the field path
/// and emits the extracted field value.
///
/// Uses chained `switch_map` for EACH field in the path. This ensures that when
/// ANY field emits a new value, downstream subscriptions are automatically cancelled
/// and re-established on the new value. This is critical for reactive UIs where
/// intermediate elements (like a TextInput inside a List) can be recreated.
fn build_field_access_actor(
    path: Vec<String>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    let piped = ctx.actor_context.piped.clone()
        .ok_or("FieldAccess requires a piped value")?;

    let path_display = path.join(".");

    // Start with piped stream as root
    let mut value_stream: LocalBoxStream<'static, Value> = piped.clone().stream();

    // Chain switch_map for EACH field - THE KEY FIX!
    // When ANY field emits a new value, downstream switch_maps automatically
    // cancel old subscriptions and re-subscribe to the new value.
    // This prevents stale subscriptions when intermediate elements are recreated.
    for (idx, field_name) in path.iter().enumerate() {
        let field_name = field_name.clone();
        let path_display_for_log = path_display.clone();
        let field_idx = idx;

        value_stream = switch_map(value_stream, move |value| {
            let field_name = field_name.clone();
            let path_display = path_display_for_log.clone();

            let value_type = match &value {
                Value::Object(_, _) => "Object",
                Value::TaggedObject(tagged, _) => tagged.tag(),
                Value::Tag(tag, _) => tag.tag(),
                Value::Text(_, _) => "Text",
                Value::Number(_, _) => "Number",
                _ => "Other",
            };
            zoon::println!("[FIELD_ACCESS] .{} step {}: received {} for field '{}'",
                path_display, field_idx, value_type, field_name);

            let variable = match &value {
                Value::Object(object, _) => object.variable(&field_name),
                Value::TaggedObject(tagged, _) => tagged.variable(&field_name),
                _ => None,
            };

            if let Some(var) = variable {
                let value_actor = var.value_actor();
                zoon::println!("[FIELD_ACCESS] .{} step {}: found field '{}', subscribing to actor",
                    path_display, field_idx, field_name);
                value_actor.stream()
            } else {
                // Field not found - emit empty stream (switch_map handles this gracefully)
                zoon::println!("[FIELD_ACCESS] .{} step {}: field '{}' NOT FOUND in {}",
                    path_display, field_idx, field_name, value_type);
                stream::empty().boxed_local()
            }
        });
    }

    // Keep the piped actor alive by including it in inputs
    Ok(ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; .{path_display}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(value_stream),
        persistence_id,
        vec![piped],
    ))
}

/// Build a HOLD actor (stateful accumulator).
/// HOLD: `input |> HOLD state_param { body }`
/// The piped value sets/resets the state (not just initial - any emission).
/// The body can reference `state_param` to get the current state.
/// The body expression's result becomes the new state value.
/// CRITICAL: The state is NOT self-reactive - changes to state don't
/// trigger re-evaluation of body. Only external events trigger updates.
fn build_hold_actor(
    initial_actor: Arc<ValueActor>,
    state_param: String,
    body: static_expression::Spanned<static_expression::Expression>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Option<Arc<ValueActor>>, String> {
    // Apply scope to persistence_id so HOLDs inside user-defined functions
    // get unique storage keys for each call site
    let persistence_id = persistence_id.in_scope(&ctx.actor_context.scope);

    // Use a bounded channel to hold current state value and broadcast updates
    // Note: Sender::try_send takes &self, so we can just clone the sender
    let (state_sender, state_receiver) = zoon::futures_channel::mpsc::channel::<Value>(16);
    let state_sender_for_update = state_sender.clone();
    let state_sender_for_reset = state_sender.clone();

    // Get storage for persistence
    let storage = ctx.construct_context.construct_storage.clone();
    let storage_for_state_load = storage.clone();
    let storage_for_state_save = storage.clone();
    let storage_for_initial_save = storage.clone();
    let construct_context_for_state_load = ctx.construct_context.clone();
    let actor_context_for_state_load = ctx.actor_context.clone();

    // Shared state for restoration tracking
    // restored_value_holder: stores the restored value to emit to output after output is created
    let restored_value_holder: Arc<std::sync::OnceLock<Value>> = Arc::new(std::sync::OnceLock::new());
    let restored_value_for_state = restored_value_holder.clone();

    // Create a ValueActor that provides the current state to the body
    // This is what the state_param references
    //
    // State stream: load stored value first (if exists), else initial_actor, then state_receiver
    let initial_actor_for_state = initial_actor.clone();
    let state_stream = stream::unfold(
        (true, state_receiver),  // (is_first, receiver)
        move |(is_first, mut receiver)| {
            let storage = storage_for_state_load.clone();
            let initial_actor = initial_actor_for_state.clone();
            let construct_context = construct_context_for_state_load.clone();
            let actor_context = actor_context_for_state_load.clone();
            let restored_value_holder = restored_value_for_state.clone();
            async move {
                if is_first {
                    // Try storage first - load persisted state
                    let loaded: Option<zoon::serde_json::Value> = storage.load_state(persistence_id).await;
                    if let Some(json) = loaded {
                        // Deserialize stored value
                        let value = Value::from_json(
                            &json,
                            ConstructId::new("HOLD state restored from storage"),
                            construct_context,
                            ValueIdempotencyKey::new(),
                            actor_context,
                        );
                        // Store for output emission
                        let _ = restored_value_holder.set(value.clone());
                        return Some((value, (false, receiver)));
                    }
                    // No stored state - fall back to initial_actor's first value
                    let initial = initial_actor.stream().next().await?;
                    Some((initial, (false, receiver)))
                } else {
                    // Subsequent values from state channel (updates and resets)
                    let value = receiver.next().await?;
                    Some((value, (false, receiver)))
                }
            }
        },
    );

    // Create state actor - initial value will come through the stream asynchronously
    let state_actor = ValueActor::new_arc(
        ConstructInfo::new(
            format!("Hold state actor for {state_param}"),
            None,
            format!("{span}; HOLD state parameter"),
        ),
        ctx.actor_context.clone(),
        TypedStream::infinite(state_stream),
        PersistenceId::new(),
    );

    // Bind the state parameter in the context so body can reference it
    let mut body_parameters = ctx.actor_context.parameters.clone();
    body_parameters.insert(state_param.clone(), state_actor.clone());

    // Clone state_actor for use in state_update_stream to directly update its stored value
    let state_actor_for_update = state_actor.clone();
    // Clone for the synchronous callback that THEN will use
    let state_actor_for_callback = state_actor;

    // Create backpressure coordinator for synchronizing THEN with state updates.
    // The coordinator uses message-based coordination (no shared atomics/Mutex).
    // HOLD's callback releases permit after each state update, allowing next body to run.
    let backpressure_permit = BackpressureCoordinator::new();
    let permit_for_callback = backpressure_permit.clone();

    // Create callback for THEN to update HOLD's state synchronously.
    // This ensures the next body evaluation sees the updated state.
    // NOTE: We do NOT store to output here - state_update_stream handles that.
    // Storing in both places would cause duplicate emissions.
    let hold_state_update_callback: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |new_value: Value| {
        // Update state_actor's stored value directly - THEN will read from here
        state_actor_for_callback.store_value_directly(new_value);
        // Release permit to allow THEN to process next input
        permit_for_callback.release();
    });

    let body_actor_context = ActorContext {
        output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
        piped: None, // Clear piped - the body shouldn't re-use it
        passed: ctx.actor_context.passed.clone(),
        parameters: body_parameters,
        // Force sequential processing in HOLD body to ensure state consistency.
        // Without this, THEN/WHEN would spawn parallel body evaluations that all
        // read stale state (e.g., Stream/pulses(3) |> THEN { counter + 1 } would read counter=0 three times).
        sequential_processing: true,
        // Pass permit to body - THEN will acquire before each evaluation
        backpressure_permit: Some(backpressure_permit),
        // Pass callback to THEN for synchronous state updates during eager polling
        hold_state_update_callback: Some(hold_state_update_callback),
        // Enable lazy actors in HOLD body for demand-driven evaluation.
        // This ensures HOLD pulls values one at a time and updates state between each pull.
        use_lazy_actors: true,
        // Default - THEN/WHEN inside will set their own snapshot flag
        is_snapshot_context: false,
        // Inherit object_locals - HOLD body may reference Object sibling fields
        object_locals: ctx.actor_context.object_locals.clone(),
        scope: ctx.actor_context.scope.clone(),
        subscription_scope: ctx.actor_context.subscription_scope.clone(),
        call_recorder: ctx.actor_context.call_recorder.clone(),
        is_restoring: ctx.actor_context.is_restoring,
        list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
        recording_counter: ctx.actor_context.recording_counter.clone(),
        // Inherit parent's subscription_time - HOLD body is streaming context
        subscription_time: ctx.actor_context.subscription_time,
    };

    // Create new context for body evaluation
    let body_ctx = EvaluationContext {
        construct_context: ctx.construct_context.clone(),
        actor_context: body_actor_context,
        reference_connector: ctx.reference_connector.clone(),
        link_connector: ctx.link_connector.clone(),
        pass_through_connector: ctx.pass_through_connector.clone(),
        function_registry_snapshot: ctx.function_registry_snapshot.clone(),
        module_loader: ctx.module_loader.clone(),
        source_code: ctx.source_code.clone(),
    };

    // Evaluate the body with state parameter bound
    let body_result = match evaluate_expression(body, body_ctx)? {
        Some(actor) => actor,
        None => {
            // Body is SKIP - HOLD produces no values (propagate SKIP)
            return Ok(None);
        }
    };

    // When body produces new values, update the state
    // Note: We avoid self-reactivity by not triggering body re-evaluation
    // from state changes. Body only evaluates when its event sources fire.
    //
    // Subscribe to body - handles both lazy and eager actors.
    // For lazy actors, this enables demand-driven evaluation where HOLD pulls values
    // one at a time and updates state between each pull (sequential state updates).
    let body_subscription = body_result.clone().stream();
    let state_update_stream = body_subscription.then(move |new_value| {
        let mut state_sender = state_sender_for_update.clone();
        let state_actor = state_actor_for_update.clone();
        let storage = storage_for_state_save.clone();
        async move {
            // Send to state channel with backpressure so body can see it on next event
            if let Err(e) = state_sender.send(new_value.clone()).await {
                zoon::println!("[HOLD] Failed to send state update: {e}");
            }
            // DIRECTLY update state_actor's stored value - bypass async channel delay.
            // This ensures the next THEN body evaluation reads the fresh state value.
            // NOTE: The callback already updates state_actor, but we update here too
            // for cases where the value flows through without callback (e.g., non-THEN body).
            state_actor.store_value_directly(new_value.clone());
            // NOTE: Do NOT release permit here! The hold_state_update_callback already
            // releases it after THEN's body evaluation. Releasing twice would cause
            // permit count to grow, defeating backpressure and allowing parallel processing.

            // Save state to persistent storage
            let json = new_value.to_json().await;
            storage.save_state(persistence_id, &json);

            new_value
        }
    });

    // Create output actor FIRST with a pending stream (stays alive, no async stream processing).
    // Values will be stored directly via store_value_directly() from the stream closures below.
    // This ensures values are available in history immediately when Stream/skip subscribes.
    //
    // The driver_loop_holder holds the ActorLoop for the driver task. When the output actor
    // is dropped, the stream is dropped, which drops the Arc, which drops the ActorLoop,
    // which cancels the driver task. This ensures Timer/interval stops when switching examples.
    //
    // Using OnceLock instead of Rc<RefCell> - it's a thread-safe write-once cell.
    let driver_loop_holder: Arc<std::sync::OnceLock<ActorLoop>> = Arc::new(std::sync::OnceLock::new());
    let driver_loop_holder_for_stream = driver_loop_holder.clone();
    let output_stream = stream::poll_fn(move |_cx| {
        // Prevent drop: captured by `move` closure - when dropped, the ActorLoop is dropped
        let _driver_loop_holder = &driver_loop_holder_for_stream;
        Poll::Pending::<Option<Value>>
    });
    // Clone contexts before they're moved into output
    let construct_context_for_initial = ctx.construct_context.clone();
    let actor_context_for_initial = ctx.actor_context.clone();
    let output = ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; HOLD {state_param} {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(output_stream),
        persistence_id,
        vec![body_result.clone(), initial_actor.clone()],
    );

    // NOTE: We do NOT copy body_result's history here anymore.
    // The state_update_stream (below) will emit those values when polled, and it calls
    // store_value_directly() on output. Values flow through async streams.
    // Restoration is handled by initial_stream which checks storage and emits stored value.

    // Reset/passthrough behavior: ALL emissions from input pass through as HOLD output.
    // First emission: state_actor gets it via take(1), so we don't send to state_receiver.
    // Subsequent emissions: send to state_receiver so body sees the reset value.
    //
    // IMPORTANT: Use Weak<ValueActor> instead of Arc to avoid circular reference!
    // The output actor holds (via Arc chain) the driver task, which holds combined_stream,
    // which holds these closures. Using Arc would create a cycle preventing cleanup.
    //
    // Use AtomicBool instead of Rc<RefCell<bool>> for lock-free flag.
    let is_first_input = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let output_weak_for_initial = Arc::downgrade(&output);
    let initial_stream = initial_actor.clone().stream().then(move |value| {
        let is_first_input = is_first_input.clone();
        let output_weak = output_weak_for_initial.clone();
        let mut state_sender = state_sender_for_reset.clone();
        let storage = storage_for_initial_save.clone();
        let construct_context = construct_context_for_initial.clone();
        let actor_context = actor_context_for_initial.clone();
        async move {
            // swap() atomically reads AND sets to false in one operation
            let is_first = is_first_input.swap(false, std::sync::atomic::Ordering::SeqCst);
            if is_first {
                // Check if we have stored state - if so, emit stored value instead of initial
                // We check storage directly here to avoid race condition with state_stream
                let stored: Option<zoon::serde_json::Value> = storage.clone().load_state(persistence_id).await;
                if let Some(json) = stored {
                    // Restore from storage - convert JSON to Value and emit to output
                    let restored_value = Value::from_json(
                        &json,
                        ConstructId::new("HOLD restored value"),
                        construct_context,
                        ValueIdempotencyKey::new(),
                        actor_context,
                    );
                    if let Some(output) = output_weak.upgrade() {
                        output.store_value_directly(restored_value);
                    }
                    // Don't save - we just loaded this value
                    return value;
                }
                // No stored state, this is truly the first value
                // Store to output and save
                if let Some(output) = output_weak.upgrade() {
                    output.store_value_directly(value.clone());
                }
                let json = value.to_json().await;
                storage.save_state(persistence_id, &json);
            } else {
                // Subsequent values (reset): send to state_receiver with backpressure
                if let Err(e) = state_sender.send(value.clone()).await {
                    zoon::println!("[HOLD] Failed to send state reset: {e}");
                }
                // Store value directly to output
                // Use weak reference to avoid circular reference
                if let Some(output) = output_weak.upgrade() {
                    output.store_value_directly(value.clone());
                }
                // Save reset state to storage
                let json = value.to_json().await;
                storage.save_state(persistence_id, &json);
            }
            // Always pass through as HOLD output
            value
        }
    }).boxed_local();

    // Modify state_update_stream to also store values directly to output
    // IMPORTANT: Use Weak<ValueActor> to avoid circular reference!
    let output_weak_for_update = Arc::downgrade(&output);
    let state_update_stream = state_update_stream.map(move |value| {
        // Use weak reference to avoid circular reference
        if let Some(output) = output_weak_for_update.upgrade() {
            output.store_value_directly(value.clone());
        }
        value
    }).boxed_local();

    // Combine: input stream sets/resets state, body updates state
    // Use select to merge both streams - any emission from input resets state
    let combined_stream = stream::select(
        initial_stream, // Any emission from input resets the state
        state_update_stream
    );

    // Create an actor loop to drive the combined stream (poll it so closures execute).
    // The output actor stays alive via its pending stream, and values are stored
    // directly via store_value_directly() in the stream closures above.
    // The driver loop is stored in driver_loop_holder so it's dropped when the output is dropped.
    let driver_loop = ActorLoop::new(async move {
        let mut stream = combined_stream;
        while stream.next().await.is_some() {
            // Values are already stored via store_value_directly in the map closures
        }
    });
    // Store driver loop in the OnceLock - it will be dropped when the output stream is dropped
    if driver_loop_holder.set(driver_loop).is_err() {
        zoon::eprintln!("[HOLD] Driver loop holder already set - this is a bug");
    }

    Ok(Some(output))
}

/// Build a TEXT { ... } literal actor with interpolation support.
fn build_text_literal_actor(
    parts: Vec<static_expression::TextPart>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    // Collect all parts - literals as constant streams, interpolations as variable lookups
    let mut part_actors: Vec<(bool, Arc<ValueActor>)> = Vec::new();
    // Collect actor loops to keep forwarding tasks alive
    let mut forwarding_loops: Vec<ActorLoop> = Vec::new();

    for part in &parts {
        match part {
            static_expression::TextPart::Text(text) => {
                // Literal text part - create a constant text value
                let text_string = text.to_string();
                let text_actor = Text::new_arc_value_actor(
                    ConstructInfo::new(
                        format!("TextLiteral part"),
                        None,
                        format!("{span}; TextLiteral text part"),
                    ),
                    ctx.construct_context.clone(),
                    persistence_id,
                    ctx.actor_context.clone(),
                    text_string,
                );
                part_actors.push((true, text_actor));
            }
            static_expression::TextPart::Interpolation { var, referenced_span } => {
                // Interpolation - look up the variable (supports field access paths like "item.text")
                let var_name = var.to_string();
                let parts: Vec<&str> = var_name.split('.').collect();
                let base_name = parts[0];
                let field_path: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

                // Look up the base variable
                let base_actor = if let Some(var_actor) = ctx.actor_context.parameters.get(base_name) {
                    Some(var_actor.clone())
                } else if let Some(ref_span) = referenced_span {
                    // Use reference_connector for outer scope
                    let ref_connector = ctx.try_reference_connector()
                        .ok_or_else(|| "ReferenceConnector dropped - program shutting down".to_string())?;
                    let ref_span_copy = *ref_span;

                    // Create forwarding actor for the base variable
                    let (base_ref_actor, base_sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("TextInterpolation:{}:base", var_name),
                            None,
                            format!("{span}; TextInterpolation base for '{}'", base_name),
                        ),
                        ctx.actor_context.clone(),
                        PersistenceId::new(),
                    );

                    let actor_loop = ActorLoop::new(async move {
                        let actor = ref_connector.referenceable(ref_span_copy).await;
                        let mut subscription = actor.stream();
                        while let Some(value) = subscription.next().await {
                            if base_sender.send(value).await.is_err() {
                                break;
                            }
                        }
                    });
                    forwarding_loops.push(actor_loop);
                    Some(base_ref_actor)
                } else {
                    None
                };

                if let Some(base_actor) = base_actor {
                    if field_path.is_empty() {
                        // Simple variable, no field access
                        part_actors.push((false, base_actor));
                    } else {
                        // Field access path - create forwarding actor that subscribes to the final field
                        let (field_actor, field_sender) = ValueActor::new_arc_forwarding(
                            ConstructInfo::new(
                                format!("TextInterpolation:{}", var_name),
                                None,
                                format!("{span}; TextInterpolation for '{}'", var_name),
                            ),
                            ctx.actor_context.clone(),
                            PersistenceId::new(),
                        );

                        let actor_loop = ActorLoop::new(async move {
                            // First, wait for base actor to have a value so we can navigate to the field
                            let base_value = {
                                let mut sub = base_actor.stream();
                                sub.next().await
                            };

                            let Some(base_value) = base_value else {
                                return; // Base actor closed without emitting
                            };

                            // Navigate through the field path to find the final value actor
                            let mut current_value_actor: Option<Arc<ValueActor>> = None;

                            // For intermediate fields, we need to resolve them
                            let mut current_obj_value = base_value;
                            for (i, field_name) in field_path.iter().enumerate() {
                                let is_last = i == field_path.len() - 1;

                                match &current_obj_value {
                                    Value::Object(obj, _) => {
                                        if let Some(var) = obj.variable(field_name) {
                                            if is_last {
                                                // Last field - we want to subscribe to this
                                                current_value_actor = Some(var.value_actor().clone());
                                            } else {
                                                // Intermediate field - get its stored value to navigate further
                                                if let Ok(val) = var.value_actor().current_value().await {
                                                    current_obj_value = val;
                                                } else {
                                                    return; // Can't resolve path
                                                }
                                            }
                                        } else {
                                            return; // Field not found
                                        }
                                    }
                                    Value::TaggedObject(tagged, _) => {
                                        if let Some(var) = tagged.variable(field_name) {
                                            if is_last {
                                                current_value_actor = Some(var.value_actor().clone());
                                            } else {
                                                if let Ok(val) = var.value_actor().current_value().await {
                                                    current_obj_value = val;
                                                } else {
                                                    return;
                                                }
                                            }
                                        } else {
                                            return;
                                        }
                                    }
                                    _ => return, // Not an object, can't access fields
                                }
                            }

                            // Now subscribe to the final field's value actor
                            if let Some(final_actor) = current_value_actor {
                                let mut subscription = final_actor.stream();
                                while let Some(value) = subscription.next().await {
                                    if field_sender.send(value).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        });
                        forwarding_loops.push(actor_loop);

                        part_actors.push((false, field_actor));
                    }
                } else {
                    return Err(format!("Variable '{}' not found for text interpolation. Available: {:?}",
                        var_name, ctx.actor_context.parameters.keys().collect::<Vec<_>>()));
                }
            }
        }
    }

    if part_actors.is_empty() {
        // Empty text literal
        Ok(Text::new_arc_value_actor(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; TextLiteral empty"),
            ),
            ctx.construct_context,
            persistence_id,
            ctx.actor_context,
            String::new(),
        ))
    } else if part_actors.len() == 1 && part_actors[0].0 {
        // Single literal text part - return as-is
        Ok(part_actors.into_iter().next().unwrap().1)
    } else {
        // Multiple parts or interpolations - combine with combineLatest-like behavior
        let construct_context_for_combine = ctx.construct_context.clone();
        let span_for_combine = span;

        // Create combined stream using select_all on all part streams
        // Each time any part emits, we need to recombine
        let part_subscriptions: Vec<_> = part_actors
            .iter()
            .map(|(_, actor)| actor.clone().stream())
            .collect();

        // For simplicity, use select_all and latest values approach.
        // Sort by Lamport timestamp to restore happened-before ordering.
        let merged = stream::select_all(part_subscriptions.into_iter().enumerate().map(|(idx, s)| {
            s.map(move |v| (idx, v))
        }))
        .ready_chunks(8)
        .flat_map(|mut chunk| {
            chunk.sort_by_key(|(_, value)| value.lamport_time());
            stream::iter(chunk)
        });

        let part_count = part_actors.len();
        // Move forwarding_loops into scan state to keep them alive
        let combined_stream = merged.scan(
            (vec![None; part_count], forwarding_loops),
            move |(latest_values, _forwarding_loops), (idx, value)| {
                latest_values[idx] = Some(value);

                // Check if all parts have values
                if latest_values.iter().all(|v| v.is_some()) {
                    // Combine all text parts
                    let combined: String = latest_values
                        .iter()
                        .filter_map(|v| {
                            v.as_ref().and_then(|val| {
                                match val {
                                    Value::Text(text, _) => Some(text.text().to_string()),
                                    Value::Number(num, _) => Some(num.number().to_string()),
                                    Value::Tag(tag, _) => Some(tag.tag().to_string()),
                                    _ => None,
                                }
                            })
                        })
                        .collect();

                    std::future::ready(Some(Some(combined)))
                } else {
                    std::future::ready(Some(None))
                }
            },
        )
        .filter_map(|opt| async move { opt });

        // Map combined strings directly to Text Values.
        // IMPORTANT: Do NOT use flat_map with Text actors here!
        // flat_map waits for inner streams to complete, but constant Text actor
        // subscriptions never complete - they emit once then return Pending forever.
        // This would cause flat_map to wait forever after the first value.
        let text_value_stream = combined_stream.map(move |combined_text| {
            Value::Text(
                Arc::new(Text::new(
                    ConstructInfo::new(
                        format!("TextLiteral combined"),
                        None,
                        format!("{span_for_combine}; TextLiteral combined"),
                    ),
                    construct_context_for_combine.clone(),
                    combined_text,
                )),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            )
        });

        Ok(ValueActor::new_arc(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; TextLiteral {{..}}"),
            ),
            ctx.actor_context,
            TypedStream::infinite(text_value_stream),
            persistence_id,
        ))
    }
}

/// Build a link setter actor for expressions like `foo.bar`.
fn build_link_setter_actor(
    alias: static_expression::Spanned<static_expression::Alias>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    // Link setter creates an actor that subscribes to the aliased value
    // and emits Link values that can be used to set up connections

    // First resolve the alias
    let alias_actor = evaluate_alias_immediate(
        alias.node,
        span,
        persistence.clone(),
        persistence_id,
        ctx.clone(),
    )?;

    // Create a stream that forwards the alias values through a link setter
    let stream = alias_actor.stream().map(move |value| {
        // Forward the value through the link connector
        value
    });

    Ok(ValueActor::new_arc(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; LinkSetter"),
        ),
        ctx.actor_context,
        TypedStream::infinite(stream),
        persistence_id,
    ))
}

/// Traverse an alias path and return the LINK sender at the end of the path.
/// This is used by piped LinkSetter (`element |> LINK { path.to.link }`) to send
/// the piped value to the target LINK variable.
async fn get_link_sender_from_alias(
    alias: static_expression::Alias,
    ctx: EvaluationContext,
) -> Option<NamedChannel<Value>> {
    match alias {
        static_expression::Alias::WithPassed { extra_parts } => {
            // Start from PASSED value and traverse extra_parts
            let passed = ctx.actor_context.passed.as_ref()?;

            // Wait for the first value from PASSED using value()
            // This fixes the race condition where store object isn't assigned to PASSED yet
            // when element tries to bind. value() returns immediately if available, else waits.
            let mut current_value = passed.clone().value().await.ok()?;

            // Traverse the path
            for (i, part) in extra_parts.iter().enumerate() {
                let part_str = part.to_string();
                let is_last = i == extra_parts.len() - 1;

                // Get the variable from the current object
                let variable = match &current_value {
                    Value::Object(obj, _) => obj.variable(&part_str),
                    Value::TaggedObject(tagged, _) => tagged.variable(&part_str),
                    _ => {
                        zoon::eprintln!("[get_link_sender_from_alias] Expected object at '{}', got {}",
                            part_str, current_value.construct_info());
                        return None;
                    }
                }?;

                if is_last {
                    // At the end of the path, this should be a LINK variable
                    return variable.link_value_sender();
                } else {
                    // Not at the end yet, wait for next value
                    // value() returns immediately if available, else waits
                    current_value = variable.value_actor().value().await.ok()?;
                }
            }

            None
        }
        static_expression::Alias::WithoutPassed { parts, referenced_span } => {
            // For non-PASSED aliases, use the reference connector to get the root variable
            if parts.is_empty() {
                return None;
            }

            // Get the root variable from object_locals, parameters, or reference connector
            // Note: object_locals contains function argument actors (arg_locals) which are needed
            // to correctly resolve LINK targets inside function calls. Without this, multiple
            // calls to the same function would overwrite each other's LINK registrations.
            let first_part = parts.first()?.to_string();

            let root_actor = if let Some(param_actor) = ctx.actor_context.parameters.get(&first_part).cloned() {
                param_actor
            } else if let Some(ref_span) = referenced_span {
                // First check object_locals (which includes arg_locals for function arguments)
                // This ensures each function call instance gets its own unique actor
                if let Some(local_actor) = ctx.actor_context.object_locals.get(&ref_span).cloned() {
                    local_actor
                } else {
                    // Fall back to global reference connector
                    let ref_connector = ctx.try_reference_connector()?;
                    ref_connector.referenceable(ref_span).await
                }
            } else {
                zoon::eprintln!("[get_link_sender_from_alias] Cannot resolve root variable '{}'", first_part);
                return None;
            };

            if parts.len() == 1 {
                // Single-part alias pointing to a LINK variable directly is not supported
                // without object navigation context. LINK senders are obtained from Variables,
                // and for single-part aliases we only have a ValueActor.
                // Multi-part paths like `obj.link_field` navigate to the Variable and work.
                zoon::eprintln!("[get_link_sender_from_alias] Single-part LINK alias '{}' not supported without object context", first_part);
                return None;
            }

            // Wait for the first value (handles race conditions)
            // value() returns immediately if available, else waits
            let mut current_value = root_actor.value().await.ok()?;

            // Traverse the remaining path (skip first part since we already resolved it)
            for (i, part) in parts.iter().skip(1).enumerate() {
                let part_str = part.to_string();
                let is_last = i == parts.len() - 2; // -2 because we skipped first

                // Get the variable from the current object
                let variable = match &current_value {
                    Value::Object(obj, _) => obj.variable(&part_str),
                    Value::TaggedObject(tagged, _) => tagged.variable(&part_str),
                    _ => {
                        zoon::eprintln!("[get_link_sender_from_alias] Expected object at '{}', got {}",
                            part_str, current_value.construct_info());
                        return None;
                    }
                }?;

                if is_last {
                    // At the end of the path, this should be a LINK variable
                    let sender = variable.link_value_sender();
                    if sender.is_none() {
                        zoon::eprintln!("[get_link_sender_from_alias] Final variable '{}' is not a LINK", part_str);
                    }
                    return sender;
                } else {
                    // Not at the end yet, wait for next value
                    current_value = variable.value_actor().value().await.ok()?;
                }
            }

            None
        }
    }
}

/// Build a List binding function (map, retain, every, any, sort_by).
/// These functions take an unevaluated expression that gets evaluated per-item.
fn build_list_binding_function(
    path_strs: &[String],
    arguments: Vec<static_expression::Spanned<static_expression::Argument>>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Option<Arc<ValueActor>>, String> {
    let operation = match path_strs[1].as_str() {
        "map" => ListBindingOperation::Map,
        "retain" => ListBindingOperation::Retain,
        "remove" => ListBindingOperation::Remove,
        "every" => ListBindingOperation::Every,
        "any" => ListBindingOperation::Any,
        "sort_by" => ListBindingOperation::SortBy,
        _ => return Err(format!("Unknown List binding function: {}", path_strs[1])),
    };

    // For List binding functions:
    // - First arg: binding name (e.g., "old", "item"), value is the list (or passed via piped)
    // - Second arg: transform/predicate expression (e.g., "new: expr", "if: expr")
    if arguments.len() < 2 {
        return Err(format!("List/{} requires 2 arguments", path_strs[1]));
    }

    // Get binding name from first argument
    let binding_name = arguments[0].node.name.clone();

    // Get the list - either from first argument's value or from piped
    let list_actor = if let Some(ref list_value) = arguments[0].node.value {
        // Evaluate list expression using stack-safe evaluator
        match evaluate_expression(list_value.clone(), ctx.clone())? {
            Some(actor) => actor,
            None => {
                // List is SKIP - result is SKIP
                return Ok(None);
            }
        }
    } else if let Some(ref piped) = ctx.actor_context.piped {
        piped.clone()
    } else {
        return Err(format!("List/{} requires a list argument", path_strs[1]));
    };

    // Get transform/predicate expression from second argument (NOT evaluated)
    let transform_expr = arguments[1].node.value.clone()
        .ok_or_else(|| format!("List/{} requires a transform expression", path_strs[1]))?;

    let reference_connector = ctx.try_reference_connector()
        .ok_or_else(|| "ReferenceConnector dropped - program shutting down".to_string())?;
    let link_connector = ctx.try_link_connector()
        .ok_or_else(|| "LinkConnector dropped - program shutting down".to_string())?;
    let pass_through_connector = ctx.try_pass_through_connector()
        .ok_or_else(|| "PassThroughConnector dropped - program shutting down".to_string())?;
    let config = ListBindingConfig {
        binding_name,
        transform_expr,
        operation,
        reference_connector,
        link_connector,
        pass_through_connector,
        source_code: ctx.source_code.clone(),
        function_registry_snapshot: ctx.function_registry_snapshot.clone(),
    };

    // Pass persistence_id for List/remove so it can persist its removed set
    // Other operations don't need persistence - complex objects with LINKs don't survive JSON serialization
    let pid_for_operation = match operation {
        ListBindingOperation::Remove => Some(persistence_id),
        _ => None,
    };

    let result = ListBindingFunction::new_arc_value_actor(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; List/{}(..)", path_strs[1]),
        ),
        ctx.construct_context,
        ctx.actor_context,
        list_actor,
        config,
        pid_for_operation,
    );
    Ok(Some(result))
}

/// Build List/append with call recording for persistence.
/// This enables capturing function calls that produce list items (e.g., new_todo())
/// so they can be replayed on restoration.
fn build_list_append_with_recording(
    arguments: Vec<static_expression::Spanned<static_expression::Argument>>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Option<Arc<ValueActor>>, String> {
    zoon::println!("[DEBUG] build_list_append_with_recording called");
    zoon::println!("[DEBUG] persistence: {:?}, persistence_id: {}", persistence.is_some(), persistence_id);

    // For List/append(item: expr):
    // - First arg "item:" has the expression to evaluate
    // - List comes from piped value
    if arguments.is_empty() {
        return Err("List/append requires an item argument".to_string());
    }

    // Get the list from piped value
    let list_actor = if let Some(ref piped) = ctx.actor_context.piped {
        piped.clone()
    } else {
        return Err("List/append requires a list (piped)".to_string());
    };

    // Get the item expression
    let item_arg = &arguments[0];
    let item_expr = item_arg.node.value.clone()
        .ok_or_else(|| "List/append requires an item expression".to_string())?;

    // Create persisting child scope for item evaluation
    // This enables call recording for function calls within the item expression
    // Use span (source position) for stable key across page reloads, not persistence_id (which includes timestamps)
    let scope_id = format!("list_append_{}", span);
    // Storage key must be defined first to pass to with_persisting_child_scope
    let storage_key = format!("list_calls:{}", scope_id);
    let (child_ctx, call_receiver) = ctx.actor_context.with_persisting_child_scope(&scope_id, storage_key.clone());
    zoon::println!("[DEBUG] Created persisting scope: {}, call_recorder is Some: {}", scope_id, child_ctx.call_recorder.is_some());

    // Create new evaluation context with the persisting scope
    let item_eval_ctx = EvaluationContext {
        actor_context: child_ctx.clone(),
        ..ctx.clone()
    };

    // Evaluate the item expression in the persisting scope
    // Recording happens automatically during evaluation (in call_function when piped values flow)
    let item_actor = match evaluate_expression(item_expr, item_eval_ctx)? {
        Some(actor) => actor,
        None => {
            // Item is SKIP - just forward the list unchanged
            return Ok(Some(list_actor));
        }
    };

    // Load any existing recorded calls for restoration
    // These are replayed to recreate list items from previous sessions
    let stored_calls: Vec<RecordedCall> = {
        use zoon::{local_storage, WebStorage};
        match local_storage().get::<Vec<RecordedCall>>(&storage_key) {
            None => Vec::new(),
            Some(Ok(calls)) => calls,
            Some(Err(error)) => {
                zoon::eprintln!("[DEBUG] Failed to load stored calls for restoration: {:#}", error);
                Vec::new()
            }
        }
    };

    // Update recording_counter to start after existing stored calls
    // This ensures new items get unique call_ids that don't conflict with restored items
    if let Some(counter) = &child_ctx.recording_counter {
        counter.store(stored_calls.len(), std::sync::atomic::Ordering::SeqCst);
    }

    // Replay stored calls to restore list items
    // Each call creates an item by evaluating the function with stored inputs
    let mut restored_items: Vec<Arc<ValueActor>> = Vec::new();
    if !stored_calls.is_empty() {
        zoon::println!("[DEBUG] Restoring {} items from stored calls", stored_calls.len());
        for (index, recorded_call) in stored_calls.iter().enumerate() {
            zoon::println!("[DEBUG] Restoring item {}: {:?}", index, recorded_call);

            // 1. Convert CapturedValue back to a Value actor
            let Some(input_value) = recorded_call.inputs.restore_with_context(
                ConstructInfo::new(
                    format!("restored_input_value_{}", index),
                    None,
                    format!("Restored input value for item {}", index),
                ),
                ctx.construct_context.clone(),
            ) else {
                zoon::eprintln!("[DEBUG] Failed to restore input for item {}", index);
                continue;
            };
            let input_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("restored_input_{}", index),
                    None,
                    format!("Restored input for item {}", index),
                ),
                ctx.actor_context.clone(),
                constant(input_value),
                PersistenceId::new(),
            );

            // 2. Look up the function in registry
            let function_path = recorded_call.path.join("/");
            let Some(func_def) = ctx.function_registry_snapshot.as_ref()
                .and_then(|registry| registry.get(&function_path)) else {
                zoon::eprintln!("[DEBUG] Function '{}' not found for restoration", function_path);
                continue;
            };

            // 3. Bind the input to the function's first parameter (like call_function does with piped)
            // For `title_to_add |> new_todo()`, the piped value becomes the first argument (title)
            let mut parameters = ctx.actor_context.parameters.clone();
            if let Some(first_param) = func_def.parameters.first() {
                zoon::println!("[DEBUG] Binding restored input to parameter '{}' for function '{}'", first_param, function_path);
                parameters.insert(first_param.clone(), input_actor.clone());
            } else {
                zoon::eprintln!("[DEBUG] Function '{}' has no parameters to bind input to", function_path);
            }

            // 4. Create restoring context with parameters bound (prevents re-recording)
            // Use the same scope format as during initial creation (call_id)
            // This ensures HOLDs inside the function can find their persisted state
            let body_ctx = EvaluationContext {
                actor_context: ActorContext {
                    parameters,
                    is_restoring: true,
                    ..child_ctx.with_restoring_child_scope(&recorded_call.id)
                },
                ..ctx.clone()
            };

            // 5. Evaluate function body
            match evaluate_expression(func_def.body.clone(), body_ctx) {
                Ok(Some(item_actor)) => {
                    zoon::println!("[DEBUG] Restored item {} successfully", index);
                    // Wrap the item with origin for removal tracking
                    let origin = ListItemOrigin {
                        source_storage_key: storage_key.clone(),
                        call_id: recorded_call.id.clone(),
                    };
                    let wrapped_item = ValueActor::new_arc_with_origin_boxed(
                        ConstructInfo::new(
                            format!("restored_item_wrapper_{}", index),
                            None,
                            format!("Restored item wrapper with origin for item {}", index),
                        ),
                        ctx.actor_context.clone(),
                        Box::pin(item_actor.stream()),
                        PersistenceId::new(),
                        origin,
                    );
                    restored_items.push(wrapped_item);
                }
                Ok(None) => {
                    zoon::println!("[DEBUG] Restored item {} was SKIP", index);
                }
                Err(e) => {
                    zoon::eprintln!("[DEBUG] Failed to restore item {}: {}", index, e);
                }
            }
        }
    }
    let storage_handle = spawn_recorded_calls_storage_actor(storage_key.clone(), call_receiver);
    zoon::println!("[DEBUG] Spawned storage actor for key: {}", storage_key);

    // Build a custom change stream that includes restored items
    // This replicates function_list_append logic but injects restored items after the first Replace

    let function_call_id = ConstructId::new(format!("List/append:{}", persistence_id));
    let function_call_id_for_append = function_call_id.clone();
    let actor_context_for_append = ctx.actor_context.clone();
    let storage_key_for_append = storage_key.clone();
    // Counter for generating call_ids - starts at stored_calls.len() so new items get unique IDs
    let appending_counter = Arc::new(std::sync::atomic::AtomicUsize::new(stored_calls.len()));

    // Tag changes with their source so we can ensure proper ordering
    enum TaggedChange {
        FromList(ListChange),
        FromAppend(ListChange),
        FromRestored(ListChange),
    }

    // Source list changes
    let list_actor_for_stream = list_actor.clone();
    let list_changes = list_actor_for_stream.stream().filter_map(|value| {
        future::ready(match value {
            Value::List(list, _) => Some(list),
            _ => None,
        })
    }).flat_map(|list| list.stream()).map(TaggedChange::FromList);

    // New item changes (from item_actor stream)
    let item_actor_for_stream = item_actor.clone();
    let append_changes = item_actor_for_stream.stream().map(move |value| {
        // Generate call_id that matches the one used during recording
        let call_id = format!("call_{}", appending_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let origin = ListItemOrigin {
            source_storage_key: storage_key_for_append.clone(),
            call_id,
        };
        let new_item_actor = ValueActor::new_arc_with_origin(
            ConstructInfo::new(
                function_call_id_for_append.with_child_id("appended_item"),
                None,
                "List/append appended item",
            ),
            actor_context_for_append.clone(),
            constant(value),
            PersistenceId::new(),
            origin,
        );
        TaggedChange::FromAppend(ListChange::Push { item: new_item_actor })
    });

    // Restored item changes (one-time stream of Push changes for each restored item)
    // Track if we have stored calls - determines whether to override initial items
    let had_stored_calls = !restored_items.is_empty();

    let restored_changes = stream::iter(
        restored_items.into_iter().map(|item| {
            zoon::println!("[DEBUG] Emitting restored item to change stream");
            TaggedChange::FromRestored(ListChange::Push { item })
        })
    );

    // Merge all change streams, then use scan to ensure proper ordering:
    // 1. First list Replace must come first
    // 2. Then restored items are pushed
    // 3. Then new appended items
    let change_stream = stream::select(
        stream::select(list_changes, append_changes),
        restored_changes,
    )
    .scan(
        (false, Vec::<ListChange>::new(), false, had_stored_calls), // (has_received_first_list_change, buffered_appends, restored_emitted, had_stored_calls)
        |state, tagged_change| {
            let (has_received_first, buffered, restored_emitted, had_stored_calls) = state;

            let changes_to_emit = match tagged_change {
                TaggedChange::FromList(change) => {
                    if !*has_received_first {
                        *has_received_first = true;
                        // Always include FromList items (default items from LIST literal)
                        // plus restored items (dynamically added and recorded via List/append).
                        // FromList items are fresh evaluations, not JSON-restored, so they're valid.
                        // Buffered includes restored items and any early appends.
                        let mut all = vec![change];
                        all.append(buffered);
                        all
                    } else {
                        // Subsequent list change - emit directly
                        vec![change]
                    }
                }
                TaggedChange::FromAppend(change) => {
                    if *has_received_first {
                        // Already received first list change - emit directly
                        vec![change]
                    } else {
                        // Buffer until first list change arrives
                        buffered.push(change);
                        vec![]
                    }
                }
                TaggedChange::FromRestored(change) => {
                    if *has_received_first {
                        // Already received first list change - emit restored item directly
                        *restored_emitted = true;
                        vec![change]
                    } else {
                        // Buffer until first list change arrives (treat like append)
                        buffered.push(change);
                        vec![]
                    }
                }
            };

            future::ready(Some(changes_to_emit))
        }
    )
    .flat_map(|changes| stream::iter(changes));

    // Create the result list with the combined change stream
    let list = List::new_with_change_stream(
        ConstructInfo::new(
            function_call_id.with_child_id(ulid::Ulid::new().to_string()),
            None,
            "List/append with restoration result",
        ),
        ctx.actor_context.clone(),
        change_stream,
        (list_actor, item_actor, storage_handle),  // Keep these alive
    );

    let result_stream = constant(Value::List(
        Arc::new(list),
        ValueMetadata::new(ValueIdempotencyKey::new()),
    ));

    let result_actor = ValueActor::new_arc(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; List/append(..) with recording"),
        ),
        ctx.actor_context.clone(),
        result_stream,
        persistence_id,
    );

    Ok(Some(result_actor))
}

/// Call a function with stack-safe evaluation.
/// Supports both user-defined functions and builtin functions.
/// Returns `Ok(None)` if the function body is SKIP.
fn call_function(
    path: Vec<String>,
    args: Vec<(String, Arc<ValueActor>)>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    use_piped_for_builtin: bool,
    function_registry: &HashMap<String, StaticFunctionDefinition>,
) -> Result<Option<Arc<ValueActor>>, String> {
    let full_path = path.join("/");

    // Convert args to a map (for user-defined functions)
    let mut arg_map: HashMap<String, Arc<ValueActor>> = HashMap::new();
    // Also keep positional list (for builtin functions)
    let mut positional_args: Vec<Arc<ValueActor>> = Vec::new();
    for (name, actor) in args {
        positional_args.push(actor.clone());
        arg_map.insert(name, actor);
    }

    // Check user-defined functions first
    // For nested evaluations (closures), use snapshot from context.
    // For main evaluation, use the passed registry.
    let func_def_opt = ctx.function_registry_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.get(&full_path).cloned())
        .or_else(|| function_registry.get(&full_path).cloned());

    if let Some(func_def) = func_def_opt {
        // Create parameters from arguments
        let mut parameters = ctx.actor_context.parameters.clone();
        for (param_name, arg_actor) in arg_map {
            parameters.insert(param_name, arg_actor);
        }

        // Check if piped value should be bound to an unbound function parameter.
        // For `position |> fibonacci()`, bind `position` to the first parameter of `fibonacci`.
        let piped_param_name = if let Some(_piped) = &ctx.actor_context.piped {
            let mut found_param = None;
            for param_name in &func_def.parameters {
                if !parameters.contains_key(param_name) {
                    found_param = Some(param_name.clone());
                    break; // Only bind to the first unbound parameter
                }
            }
            found_param
        } else {
            None
        };

        // If there's a piped value bound to a parameter, wrap the function call in a reactive actor.
        // This ensures that if piped is SKIP (never produces values), the function call is also SKIP.
        // Without this, the function body would execute immediately with the piped actor as a reference,
        // producing a value even if the piped actor never produces values.
        if let (Some(piped), Some(param_name)) = (&ctx.actor_context.piped, &piped_param_name) {
            // Clone everything needed for the async closure
            let piped_for_closure = piped.clone();
            let parameters_for_closure = parameters.clone();
            let param_name_for_closure = param_name.clone();
            let func_body = func_def.body.clone();
            let ctx_for_closure = ctx.clone();
            let persistence_for_construct = persistence.clone();
            let span_for_construct = span;

            // Clone path for recording
            let path_for_recording = path.clone();

            // Create a stream that:
            // 1. Subscribes to the piped input
            // 2. For each value from piped, evaluates the function body with that value
            // 3. If piped never produces values (SKIP), this stream also never produces values
            let result_stream = piped_for_closure.stream().flat_map(move |piped_value| {
                // Generate unique invocation_id for this call (used for both recording and scope)
                // This ensures each invocation of the function gets its own scope for internal HOLDs
                let invocation_id = ctx_for_closure.actor_context.recording_counter
                    .as_ref()
                    .map(|counter| format!("call_{}", counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)))
                    .unwrap_or_else(|| format!("call_{}", ulid::Ulid::new()));

                // Record the call if we're in a persisting scope
                if let Some(call_recorder) = &ctx_for_closure.actor_context.call_recorder {
                    let captured_input = CapturedValue::capture(&piped_value);
                    zoon::println!("[DEBUG] Recording call: {:?} with id: {} and input: {:?}", path_for_recording, invocation_id, captured_input);
                    let recorded_call = RecordedCall {
                        id: invocation_id.clone(),
                        path: path_for_recording.clone(),
                        inputs: captured_input,
                    };
                    call_recorder.send_or_drop(recorded_call);
                }

                // Create a constant actor for this specific piped value
                let value_actor = ValueActor::new_arc(
                    ConstructInfo::new(
                        "piped function input".to_string(),
                        None,
                        format!("piped value for user function param: {}", param_name_for_closure),
                    ),
                    ctx_for_closure.actor_context.clone(),
                    constant(piped_value),
                    PersistenceId::new(),
                );

                // Bind the constant value actor to the parameter
                let mut params = parameters_for_closure.clone();
                params.insert(param_name_for_closure.clone(), value_actor);

                // Create a nested scope using the invocation_id (call_id like "call_0").
                // This ensures each invocation of the function gets a UNIQUE scope,
                // so HOLDs inside don't overwrite each other across invocations.
                // Note: We use just invocation_id (not persistence_id:invocation_id) to match
                // the scope format used during restoration, which uses recorded_call.id.
                let call_scope = match &ctx_for_closure.actor_context.scope {
                    Scope::Root => Scope::Nested(invocation_id.clone()),
                    Scope::Nested(existing) => {
                        Scope::Nested(format!("{}:{}", existing, invocation_id))
                    }
                };

                let new_actor_context = ActorContext {
                    output_valve_signal: ctx_for_closure.actor_context.output_valve_signal.clone(),
                    piped: None, // Clear piped - we've consumed it
                    passed: ctx_for_closure.actor_context.passed.clone(),
                    parameters: params,
                    sequential_processing: ctx_for_closure.actor_context.sequential_processing,
                    backpressure_permit: ctx_for_closure.actor_context.backpressure_permit.clone(),
                    hold_state_update_callback: None,
                    use_lazy_actors: ctx_for_closure.actor_context.use_lazy_actors,
                    // Don't inherit snapshot mode - function body evaluates in normal streaming context
                    is_snapshot_context: false,
                    // Clear object_locals - function body is a new scope
                    object_locals: HashMap::new(),
                    scope: call_scope,
                    subscription_scope: ctx_for_closure.actor_context.subscription_scope.clone(),
                    call_recorder: ctx_for_closure.actor_context.call_recorder.clone(),
                    is_restoring: ctx_for_closure.actor_context.is_restoring,
                    list_append_storage_key: ctx_for_closure.actor_context.list_append_storage_key.clone(),
                    recording_counter: ctx_for_closure.actor_context.recording_counter.clone(),
                    // Inherit subscription_time - function bodies should respect caller's filtering
                    subscription_time: ctx_for_closure.actor_context.subscription_time,
                };

                let new_ctx = EvaluationContext {
                    construct_context: ctx_for_closure.construct_context.clone(),
                    actor_context: new_actor_context,
                    reference_connector: ctx_for_closure.reference_connector.clone(),
                    link_connector: ctx_for_closure.link_connector.clone(),
                    pass_through_connector: ctx_for_closure.pass_through_connector.clone(),
                    module_loader: ctx_for_closure.module_loader.clone(),
                    source_code: ctx_for_closure.source_code.clone(),
                    function_registry_snapshot: ctx_for_closure.function_registry_snapshot.clone(),
                };

                // Evaluate the function body with this piped value
                match evaluate_expression(func_body.clone(), new_ctx) {
                    Ok(Some(result_actor)) => {
                        // Use value() for type-safe single-value semantics (like THEN does)
                        let result_stream: Pin<Box<dyn Stream<Item = Value>>> =
                            Box::pin(stream::once(result_actor.value()).filter_map(|v| async { v.ok() }));
                        result_stream
                    }
                    Ok(None) => {
                        // Function body returned SKIP - produce empty stream
                        Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
                    }
                    Err(_) => {
                        // Error - produce empty stream
                        Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
                    }
                }
            });

            // Create the wrapper actor
            let wrapper_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence_for_construct,
                    format!("{span_for_construct}; piped user function call: {full_path}"),
                ),
                ctx.actor_context.clone(),
                TypedStream::infinite(result_stream),
                persistence_id,
            );

            return Ok(Some(wrapper_actor));
        }

        // No piped value or no parameter to bind - evaluate immediately (original behavior)
        // Collect argument actors to keep them alive for the duration of the function result
        // Note: we collect from parameters which now contains the arg_map values
        let arg_actors: Vec<Arc<ValueActor>> = parameters.values().cloned().collect();

        // Create a nested scope using the function call's persistence_id.
        // This ensures that HOLDs inside the function body get unique persistence IDs
        // for each call site (e.g., each call to new_todo() gets its own scope).
        let call_scope = match &ctx.actor_context.scope {
            Scope::Root => Scope::Nested(persistence_id.to_string()),
            Scope::Nested(existing) => {
                Scope::Nested(format!("{}:{}", existing, persistence_id))
            }
        };

        let new_actor_context = ActorContext {
            output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
            piped: ctx.actor_context.piped.clone(),
            passed: ctx.actor_context.passed.clone(),
            parameters,
            sequential_processing: ctx.actor_context.sequential_processing,
            backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
            // Don't propagate HOLD callback into user-defined functions - they have their own scope
            hold_state_update_callback: None,
            use_lazy_actors: ctx.actor_context.use_lazy_actors,
            // Don't inherit snapshot mode - function body evaluates in normal streaming context
            is_snapshot_context: false,
            // Clear object_locals - function body is a new scope
            object_locals: HashMap::new(),
            scope: call_scope,
            subscription_scope: ctx.actor_context.subscription_scope.clone(),
            call_recorder: ctx.actor_context.call_recorder.clone(),
            is_restoring: ctx.actor_context.is_restoring,
            list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
            recording_counter: ctx.actor_context.recording_counter.clone(),
            // Inherit subscription_time - function bodies should respect caller's filtering
            subscription_time: ctx.actor_context.subscription_time,
        };

        let new_ctx = EvaluationContext {
            construct_context: ctx.construct_context.clone(),
            actor_context: new_actor_context,
            reference_connector: ctx.reference_connector,
            link_connector: ctx.link_connector,
            pass_through_connector: ctx.pass_through_connector,
            function_registry_snapshot: ctx.function_registry_snapshot,
            module_loader: ctx.module_loader,
            source_code: ctx.source_code,
        };

        let result = evaluate_expression(func_def.body, new_ctx);

        // If we have argument actors, wrap the result to keep them alive
        match result {
            Ok(Some(result_actor)) if !arg_actors.is_empty() => {
                // Create a wrapper actor that keeps argument actors alive
                let wrapper = ValueActor::new_arc_with_inputs(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; {}(..) (with args)", full_path),
                    ),
                    ctx.actor_context,
                    TypedStream::infinite(result_actor.stream()),
                    persistence_id,
                    arg_actors,  // Keep argument actors alive
                );
                return Ok(Some(wrapper));
            }
            _ => return result,
        }
    }

    // Try builtin functions
    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    match static_function_call_path_to_definition(&path_strs, span) {
        Ok(definition) => {
            // Call the builtin function using FunctionCall
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; {}(..)", full_path),
            );

            // For builtin functions, only prepend piped value if use_piped_for_builtin is true.
            // This flag is only true when the function is the direct target of a pipe (`|>`).
            let mut builtin_args = Vec::new();
            if use_piped_for_builtin {
                if let Some(piped) = &ctx.actor_context.piped {
                    builtin_args.push(piped.clone());
                }
            }
            builtin_args.extend(positional_args);

            let call_actor_context = ActorContext {
                output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
                piped: None, // Clear piped
                passed: ctx.actor_context.passed.clone(),
                parameters: ctx.actor_context.parameters.clone(),
                sequential_processing: ctx.actor_context.sequential_processing,
                backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
                // Don't propagate HOLD callback into builtin function calls
                hold_state_update_callback: None,
                use_lazy_actors: ctx.actor_context.use_lazy_actors,
                // Don't inherit snapshot mode - builtin functions evaluate in normal streaming context
                is_snapshot_context: false,
                // Clear object_locals - function call is a new scope
                object_locals: HashMap::new(),
                scope: ctx.actor_context.scope.clone(),
                subscription_scope: ctx.actor_context.subscription_scope.clone(),
                call_recorder: ctx.actor_context.call_recorder.clone(),
                is_restoring: ctx.actor_context.is_restoring,
                list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
                recording_counter: ctx.actor_context.recording_counter.clone(),
                // Inherit subscription_time - builtin function calls should respect caller's filtering
                subscription_time: ctx.actor_context.subscription_time,
            };

            Ok(Some(FunctionCall::new_arc_value_actor(
                construct_info,
                ctx.construct_context,
                call_actor_context,
                definition,
                builtin_args,
            )))
        }
        Err(_) => {
            Err(format!("Function '{}' not found", full_path))
        }
    }
}

/// Pattern matching that properly extracts bindings from Objects.
/// This can handle reactive Object values by awaiting subscriptions to get current field values.
async fn match_pattern(
    pattern: &static_expression::Pattern,
    value: &Value,
) -> Option<HashMap<String, Value>> {
    use zoon::futures_util::StreamExt;

    let mut bindings = HashMap::new();

    match pattern {
        static_expression::Pattern::WildCard => Some(bindings),

        static_expression::Pattern::Alias { name } => {
            bindings.insert(name.to_string(), value.clone());
            Some(bindings)
        }

        static_expression::Pattern::Literal(lit) => {
            match (lit, value) {
                (static_expression::Literal::Number(n), Value::Number(v, _)) => {
                    if (*n - v.number()).abs() < f64::EPSILON {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                (static_expression::Literal::Tag(t), Value::Tag(v, _)) => {
                    if t.as_str() == v.tag() {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                (static_expression::Literal::Text(t), Value::Text(v, _)) => {
                    if t.as_str() == v.text() {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }

        static_expression::Pattern::TaggedObject { tag, variables } => {
            if let Value::TaggedObject(to, _) = value {
                if to.tag() == tag.as_str() {
                    // Extract field values from the tagged object
                    for pattern_var in variables {
                        let var_name = pattern_var.name.as_str();
                        // Find the variable in the tagged object by name
                        if let Some(variable) = to.variables().iter().find(|v| v.name() == var_name) {
                            // Await the current value from the reactive actor
                            if let Some(field_value) = variable.value_actor().current_value().await.ok() {
                                // Handle nested patterns if present
                                if let Some(ref nested_pattern) = pattern_var.value {
                                    // Recursively match nested pattern
                                    if let Some(nested_bindings) = Box::pin(match_pattern(nested_pattern, &field_value)).await {
                                        bindings.extend(nested_bindings);
                                    } else {
                                        return None; // Nested pattern didn't match
                                    }
                                } else {
                                    // Simple binding - just bind the value to the name
                                    bindings.insert(var_name.to_string(), field_value);
                                }
                            } else {
                                return None; // Field value not available
                            }
                        } else {
                            return None; // Field not found in object
                        }
                    }
                    Some(bindings)
                } else {
                    None
                }
            } else {
                None
            }
        }

        static_expression::Pattern::Object { variables } => {
            // Helper to extract bindings from object variables
            async fn extract_object_bindings(
                variables: &[Arc<Variable>],
                pattern_vars: &[static_expression::PatternVariable],
                bindings: &mut HashMap<String, Value>,
            ) -> bool {
                for pattern_var in pattern_vars {
                    let var_name = pattern_var.name.as_str();
                    // Find the variable in the object by name
                    if let Some(variable) = variables.iter().find(|v| v.name() == var_name) {
                        // Await the current value from the reactive actor
                        if let Some(field_value) = variable.value_actor().current_value().await.ok() {
                            // Handle nested patterns if present
                            if let Some(ref nested_pattern) = pattern_var.value {
                                // Recursively match nested pattern
                                if let Some(nested_bindings) = Box::pin(match_pattern(nested_pattern, &field_value)).await {
                                    bindings.extend(nested_bindings);
                                } else {
                                    return false; // Nested pattern didn't match
                                }
                            } else {
                                // Simple binding - just bind the value to the name
                                bindings.insert(var_name.to_string(), field_value);
                            }
                        } else {
                            return false; // Field value not available
                        }
                    } else {
                        return false; // Field not found in object
                    }
                }
                true
            }

            if let Value::Object(obj, _) = value {
                if extract_object_bindings(obj.variables(), variables, &mut bindings).await {
                    Some(bindings)
                } else {
                    None
                }
            } else if let Value::TaggedObject(to, _) = value {
                if extract_object_bindings(to.variables(), variables, &mut bindings).await {
                    Some(bindings)
                } else {
                    None
                }
            } else {
                None
            }
        }

        static_expression::Pattern::List { items } => {
            if let Value::List(list, _) = value {
                // Get the current snapshot of list items
                let snapshot = list.snapshot().await;

                // Require exact length match (no REST patterns supported yet)
                if items.len() != snapshot.len() {
                    return None;
                }

                // Match each pattern against corresponding list item
                for (item_pattern, (_item_id, item_actor)) in items.iter().zip(snapshot.iter()) {
                    // Get the current value from the item actor
                    if let Some(item_value) = item_actor.clone().current_value().await.ok() {
                        // Recursively match the pattern
                        if let Some(nested_bindings) = Box::pin(match_pattern(item_pattern, &item_value)).await {
                            bindings.extend(nested_bindings);
                        } else {
                            return None; // Pattern didn't match
                        }
                    } else {
                        return None; // Couldn't get item value
                    }
                }
                Some(bindings)
            } else {
                None
            }
        }

        static_expression::Pattern::Map { entries: _ } => {
            // Map type not implemented in Value enum yet.
            // Return None (no match) rather than silently succeeding with empty bindings.
            None
        }
    }
}

// =============================================================================
// END STACK-SAFE EVALUATION FUNCTIONS
// =============================================================================

/// Type alias for function registry - just a simple HashMap.
/// No actor, no ArcSwap, no locks. Owned by EvaluationState during evaluation,
/// then returned to caller for potential reuse across files.
pub type FunctionRegistry = HashMap<String, StaticFunctionDefinition>;

/// A user-defined function definition using static expressions.
#[derive(Clone)]
pub struct StaticFunctionDefinition {
    pub parameters: Vec<String>,
    pub body: static_expression::Spanned<static_expression::Expression>,
}

/// Cached module data - contains functions and variables from a parsed module file.
#[derive(Clone)]
pub struct ModuleData {
    /// Functions defined in this module (name -> definition)
    pub functions: HashMap<String, StaticFunctionDefinition>,
    /// Variables defined in this module (name -> value expression)
    pub variables: HashMap<String, static_expression::Spanned<static_expression::Expression>>,
}

/// Request types for the module loader actor.
enum ModuleLoaderRequest {
    SetBaseDir(String),
    GetBaseDir { reply: oneshot::Sender<String> },
    GetCached { name: String, reply: oneshot::Sender<Option<ModuleData>> },
    Cache { name: String, data: ModuleData },
}

/// Module loader with caching for loading and parsing Boon modules.
/// Resolves module paths like "Theme" to file paths and caches parsed modules.
/// Uses actor model with channels - no locks, no RefCell.
#[derive(Clone)]
pub struct ModuleLoader {
    request_sender: NamedChannel<ModuleLoaderRequest>,
    _actor_loop: Arc<ActorLoop>,
}

impl Default for ModuleLoader {
    fn default() -> Self {
        Self::new("")
    }
}

impl ModuleLoader {
    pub fn new(base_dir: impl Into<String>) -> Self {
        let (tx, mut rx) = NamedChannel::new("module_loader.requests", 16);
        let initial_base_dir = base_dir.into();

        let actor_loop = ActorLoop::new(async move {
            let mut cache: HashMap<String, ModuleData> = HashMap::new();
            let mut base_dir = initial_base_dir;

            while let Some(request) = rx.next().await {
                match request {
                    ModuleLoaderRequest::SetBaseDir(dir) => {
                        base_dir = dir;
                    }
                    ModuleLoaderRequest::GetBaseDir { reply } => {
                        if reply.send(base_dir.clone()).is_err() {
                            zoon::println!("[MODULE_LOADER] GetBaseDir reply receiver dropped");
                        }
                    }
                    ModuleLoaderRequest::GetCached { name, reply } => {
                        if reply.send(cache.get(&name).cloned()).is_err() {
                            zoon::println!("[MODULE_LOADER] GetCached reply receiver dropped for {}", name);
                        }
                    }
                    ModuleLoaderRequest::Cache { name, data } => {
                        cache.insert(name, data);
                    }
                }
            }
        });

        Self {
            request_sender: tx,
            _actor_loop: Arc::new(actor_loop),
        }
    }

    /// Set the base directory for module resolution (fire-and-forget).
    pub fn set_base_dir(&self, dir: impl Into<String>) {
        if let Err(e) = self.request_sender.try_send(ModuleLoaderRequest::SetBaseDir(dir.into())) {
            zoon::eprintln!("[MODULE_LOADER] Failed to send SetBaseDir: {e}");
        }
    }

    /// Get the base directory (async).
    pub async fn base_dir(&self) -> String {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.request_sender.try_send(ModuleLoaderRequest::GetBaseDir { reply: tx }) {
            zoon::eprintln!("[MODULE_LOADER] Failed to send GetBaseDir: {e}");
            return String::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Get a cached module (async).
    async fn get_cached(&self, name: &str) -> Option<ModuleData> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.request_sender.try_send(ModuleLoaderRequest::GetCached {
            name: name.to_string(),
            reply: tx,
        }) {
            zoon::println!("[MODULE_LOADER] Failed to send GetCached for {}: {e}", name);
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Cache a module (fire-and-forget).
    fn cache(&self, name: String, data: ModuleData) {
        if let Err(e) = self.request_sender.try_send(ModuleLoaderRequest::Cache { name: name.clone(), data }) {
            zoon::eprintln!("[MODULE_LOADER] Failed to cache module {}: {e}", name);
        }
    }

    /// Load a module by name (e.g., "Theme", "Professional", "Assets")
    /// Tries multiple resolution paths:
    /// 1. {base_dir}/{module_name}.bn
    /// 2. {base_dir}/{module_name}/{module_name}.bn
    /// 3. {base_dir}/Generated/{module_name}.bn (for generated files)
    pub async fn load_module(
        &self,
        module_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<ModuleData> {
        // Check cache first
        if let Some(cached) = self.get_cached(module_name).await {
            return Some(cached);
        }

        let base_dir_owned = self.base_dir().await;
        let base = current_dir.unwrap_or(&base_dir_owned);

        // Helper to create path, avoiding leading slash when base is empty
        let make_path = |base: &str, rest: &str| {
            if base.is_empty() {
                rest.to_string()
            } else {
                format!("{}/{}", base, rest)
            }
        };

        // Try different resolution paths
        let paths_to_try = vec![
            make_path(base, &format!("{}.bn", module_name)),
            make_path(base, &format!("{}/{}.bn", module_name, module_name)),
            make_path(base, &format!("Generated/{}.bn", module_name)),
            // Also try from the module loader's base directory if current_dir is different
            make_path(&base_dir_owned, &format!("{}.bn", module_name)),
            make_path(&base_dir_owned, &format!("{}/{}.bn", module_name, module_name)),
            make_path(&base_dir_owned, &format!("Generated/{}.bn", module_name)),
        ];

        for path in paths_to_try {
            if let Some(source_code) = virtual_fs.read_text(&path).await {
                zoon::println!("[ModuleLoader] Loading module '{}' from '{}'", module_name, path);
                if let Some(module_data) = parse_module(&path, &source_code) {
                    // Cache the module
                    self.cache(module_name.to_string(), module_data.clone());
                    return Some(module_data);
                }
            }
        }

        zoon::eprintln!("[ModuleLoader] Could not find module '{}' (tried from base '{}')", module_name, base);
        None
    }

    /// Get a function from a module
    pub async fn get_function(
        &self,
        module_name: &str,
        function_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<StaticFunctionDefinition> {
        let module = self.load_module(module_name, virtual_fs, current_dir).await?;
        module.functions.get(function_name).cloned()
    }

    /// Get a variable from a module
    pub async fn get_variable(
        &self,
        module_name: &str,
        variable_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<static_expression::Spanned<static_expression::Expression>> {
        let module = self.load_module(module_name, virtual_fs, current_dir).await?;
        module.variables.get(variable_name).cloned()
    }
}

/// Parse module source code into ModuleData (free function, no state needed).
fn parse_module(filename: &str, source_code: &str) -> Option<ModuleData> {
    // Lexer
    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        zoon::eprintln!("[ModuleLoader] Lex errors in '{}': {:?}", filename, errors.len());
        return None;
    }
    let mut tokens = tokens?;
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    // Parser
    let (ast, errors) = parser()
        .parse(ChumskyStream::from_iter(tokens).map(
            span_at(source_code.len()),
            |Spanned { node, span, persistence: _ }| (node, span),
        ))
        .into_output_errors();
    if !errors.is_empty() {
        zoon::eprintln!("[ModuleLoader] Parse errors in '{}': {:?}", filename, errors.len());
        return None;
    }
    let ast = ast?;

    // Reference resolution
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            zoon::eprintln!("[ModuleLoader] Reference errors in '{}': {:?}", filename, errors.len());
            return None;
        }
    };

    // Convert to static expressions
    let source_code_arc = SourceCode::new(source_code.to_string());
    let static_ast = static_expression::convert_expressions(source_code_arc, ast);

    // Extract functions and variables
    let mut functions = HashMap::new();
    let mut variables = HashMap::new();

    for expr in static_ast {
        match expr.node.clone() {
            static_expression::Expression::Variable(variable) => {
                let name = variable.name.to_string();
                let value_expr = variable.value;
                variables.insert(name, value_expr);
            }
            static_expression::Expression::Function { name, parameters, body } => {
                functions.insert(
                    name.to_string(),
                    StaticFunctionDefinition {
                        parameters: parameters.into_iter().map(|p| p.node.to_string()).collect(),
                        body: *body,
                    },
                );
            }
            _ => {}
        }
    }

    Some(ModuleData { functions, variables })
}

/// Main evaluation function - takes static expressions (owned, 'static, no lifetimes).
pub fn evaluate(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
) -> Result<(Arc<Object>, ConstructContext), String> {
    let function_registry = FunctionRegistry::new();
    let module_loader = ModuleLoader::default();
    let (obj, ctx, _, _, _, _, _) = evaluate_with_registry(
        source_code,
        expressions,
        states_local_storage_key,
        virtual_fs,
        function_registry,
        module_loader,
    )?;
    Ok((obj, ctx))
}

/// Evaluation function that accepts and returns a function registry and module loader.
/// This enables sharing function definitions across multiple files.
///
/// Returns a tuple containing:
/// - `Arc<Object>`: The root object containing all top-level variables
/// - `ConstructContext`: Context for construct storage and virtual filesystem
/// - `FunctionRegistry`: Registry of function definitions (HashMap)
/// - `ModuleLoader`: Module loader for imports
/// - `Arc<ReferenceConnector>`: Connector for variable references (MUST be dropped when done!)
/// - `Arc<LinkConnector>`: Connector for LINK variables (MUST be dropped when done!)
/// - `Arc<PassThroughConnector>`: Connector for LINK pass-throughs (MUST be dropped when done!)
///
/// IMPORTANT: The ReferenceConnector, LinkConnector, and PassThroughConnector
/// MUST be dropped when the program is finished (e.g., when switching examples) to allow actors
/// to be cleaned up. These connectors hold strong references to all top-level actors.
pub fn evaluate_with_registry(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    mut function_registry: FunctionRegistry,
    module_loader: ModuleLoader,
) -> Result<(Arc<Object>, ConstructContext, FunctionRegistry, ModuleLoader, Arc<ReferenceConnector>, Arc<LinkConnector>, Arc<PassThroughConnector>), String> {
    let construct_context = ConstructContext {
        construct_storage: Arc::new(ConstructStorage::new(states_local_storage_key)),
        virtual_fs,
    };
    let actor_context = ActorContext::default();
    let reference_connector = Arc::new(ReferenceConnector::new());
    let link_connector = Arc::new(LinkConnector::new());
    let pass_through_connector = Arc::new(PassThroughConnector::new());

    // First pass: collect function definitions and variables
    let mut variables = Vec::new();
    for expr in expressions {
        let static_expression::Spanned {
            span,
            node: expression,
            persistence,
        } = expr;
        match expression {
            static_expression::Expression::Variable(variable) => {
                variables.push(static_expression::Spanned {
                    span,
                    node: *variable,
                    persistence,
                });
            }
            static_expression::Expression::Function {
                name,
                parameters,
                body,
            } => {
                // Store function definition in registry - direct insert, no locks
                function_registry.insert(
                    name.to_string(),
                    StaticFunctionDefinition {
                        parameters: parameters.into_iter().map(|p| p.node.to_string()).collect(),
                        body: *body,
                    },
                );
            }
            _ => {
                return Err(format!(
                    "Only variables or functions expected at top level (span: {span})"
                ));
            }
        }
    }

    // Second pass: evaluate variables
    let evaluated_variables: Result<Vec<_>, _> = variables
        .into_iter()
        .map(|variable| {
            static_spanned_variable_into_variable(
                variable,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                pass_through_connector.clone(),
                &function_registry,
                module_loader.clone(),
                source_code.clone(),
            )
        })
        .collect();

    let root_object = Object::new_arc(
        ConstructInfo::new("root", None, "root"),
        construct_context.clone(),
        evaluated_variables?,
    );
    Ok((root_object, construct_context, function_registry, module_loader, reference_connector, link_connector, pass_through_connector))
}

/// Evaluates a static variable into a Variable.
fn static_spanned_variable_into_variable(
    variable: static_expression::Spanned<static_expression::Variable>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    pass_through_connector: Arc<PassThroughConnector>,
    function_registry: &FunctionRegistry,
    module_loader: ModuleLoader,
    source_code: SourceCode,
) -> Result<Arc<Variable>, String> {
    let static_expression::Spanned {
        span,
        node: variable,
        persistence,
    } = variable;
    let static_expression::Variable {
        name,
        value,
        is_referenced,
    } = variable;

    let persistence_id = persistence.clone().ok_or("Failed to get Persistence")?.id;
    let name_string = name.to_string();

    let construct_info = ConstructInfo::new(
        format!("PersistenceId: {persistence_id}"),
        persistence,
        format!("{span}; {name_string}"),
    );

    let is_link = matches!(&value.node, static_expression::Expression::Link);

    // Save scope before creating variable (actor_context may be moved)
    let scope_for_link = actor_context.scope.clone();

    let variable = if is_link {
        Variable::new_link_arc(construct_info, construct_context, name_string, actor_context, persistence_id)
    } else {
        Variable::new_arc(
            construct_info,
            construct_context.clone(),
            name_string,
            static_spanned_expression_into_value_actor(
                value,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                pass_through_connector.clone(),
                function_registry,
                module_loader,
                source_code,
            )?,
            persistence_id,
            actor_context.scope,
        )
    };
    // FIX: Always register top-level variables to ensure they're available for
    // nested function argument references. The is_referenced flag from scope
    // resolution doesn't correctly track references inside nested function calls.
    reference_connector.register_referenceable(span, variable.value_actor());
    // Register LINK variable senders with LinkConnector
    // IMPORTANT: Include scope to ensure LINK bindings inside functions
    // (like new_todo() in List/map) get unique identities per list item
    if is_link {
        if let Some(sender) = variable.link_value_sender() {
            link_connector.register_link(span, scope_for_link, sender);
        }
    }
    Ok(variable)
}

/// Evaluates a static expression, returning a ValueActor.
///
/// This is used by ListBindingFunction to evaluate transform expressions
/// for each list item. The binding variable is passed via `actor_context.parameters`.
/// Flattens a pipe chain like `Pipe(a, Pipe(b, Pipe(c, d)))` into `[a, b, c, d]`.
/// This allows iterative processing instead of recursive, reducing call stack depth.
fn flatten_pipe_chain(
    expr: static_expression::Spanned<static_expression::Expression>,
) -> Vec<static_expression::Spanned<static_expression::Expression>> {
    let mut chain = Vec::new();
    let mut current = expr;

    loop {
        match current.node {
            static_expression::Expression::Pipe { from, to } => {
                chain.push(*from);
                current = *to;
            }
            _ => {
                // Last element in chain (not a Pipe)
                chain.push(current);
                break;
            }
        }
    }

    chain
}

/// Evaluates a static expression with optional function registry.
/// If `function_registry_snapshot` is provided, user-defined functions are available.
/// Otherwise, only built-in functions and operators work.
pub fn evaluate_static_expression(
    static_expr: &static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    pass_through_connector: Arc<PassThroughConnector>,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    evaluate_static_expression_with_registry(
        static_expr,
        construct_context,
        actor_context,
        reference_connector,
        link_connector,
        pass_through_connector,
        source_code,
        None,
    )
}

/// Evaluates a static expression with an optional function registry snapshot.
/// When registry is provided, user-defined function calls work.
pub fn evaluate_static_expression_with_registry(
    static_expr: &static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    pass_through_connector: Arc<PassThroughConnector>,
    source_code: SourceCode,
    function_registry_snapshot: Option<Arc<FunctionRegistry>>,
) -> Result<Arc<ValueActor>, String> {
    // Use the provided registry or create an empty one
    let registry = function_registry_snapshot
        .map(|snap| (*snap).clone())
        .unwrap_or_else(FunctionRegistry::new);
    static_spanned_expression_into_value_actor(
        static_expr.clone(),
        construct_context,
        actor_context,
        reference_connector,
        link_connector,
        pass_through_connector,
        &registry,
        ModuleLoader::default(),
        source_code,
    )
}

/// Evaluates a static expression directly (no to_borrowed conversion).
/// This is the core static evaluator used for List binding functions.
///
/// NOTE: This now delegates to the stack-safe evaluator to avoid WASM stack overflow.
fn static_spanned_expression_into_value_actor(
    expression: static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    pass_through_connector: Arc<PassThroughConnector>,
    function_registry: &FunctionRegistry,
    module_loader: ModuleLoader,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    // Create snapshot from function registry for nested evaluations
    let snapshot = if function_registry.is_empty() {
        None
    } else {
        Some(Arc::new(function_registry.clone()))
    };

    // Create EvaluationContext from the parameters
    let ctx = EvaluationContext {
        construct_context,
        actor_context,
        reference_connector: Arc::downgrade(&reference_connector),
        link_connector: Arc::downgrade(&link_connector),
        pass_through_connector: Arc::downgrade(&pass_through_connector),
        module_loader,
        source_code,
        function_registry_snapshot: snapshot,
    };

    // Delegate to the stack-safe evaluator
    evaluate_expression(expression, ctx)?
        .ok_or_else(|| "Top-level expression cannot be SKIP".to_string())
}


/// Get function definition for static function calls.
fn static_function_call_path_to_definition(
    path: &[&str],
    span: Span,
) -> Result<
    impl Fn(
        Arc<Vec<Arc<ValueActor>>>,
        ConstructId,
        PersistenceId,
        ConstructContext,
        ActorContext,
    ) -> Pin<Box<dyn Stream<Item = Value>>>
    + 'static,
    String,
> {
    let definition = match path {
        ["Math", "sum"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_math_sum(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "space"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_space(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "trim"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_trim(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "is_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_is_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "is_not_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_is_not_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "not"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_not(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "toggle"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_toggle(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "or"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_or(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "count"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_count(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "append"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_append(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "clear"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_clear(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "latest"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_latest(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "is_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_is_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "is_not_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_is_not_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Router", "route"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_router_route(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Router", "go_to"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_router_go_to(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Ulid", "generate"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_ulid_generate(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Document", "new"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_document_new(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "stripe"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_stripe(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "container"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_container(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "stack"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_stack(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "button"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_button(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "text_input"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_text_input(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "checkbox"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_checkbox(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "label"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_label(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "paragraph"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_paragraph(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "link"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_link(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Timer", "interval"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_timer_interval(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Log", "info"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_log_info(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Log", "error"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_log_error(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Build", "succeed"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_build_succeed(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Build", "fail"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_build_fail(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Scene", "new"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_scene_new(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Theme", "background_color"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_theme_background_color(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Theme", "text_color"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_theme_text_color(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Theme", "accent_color"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_theme_accent_color(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["File", "read_text"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_file_read_text(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["File", "write_text"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_file_write_text(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Directory", "entries"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_directory_entries(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Stream", "skip"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_skip(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Stream", "take"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_take(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Stream", "distinct"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_distinct(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Stream", "pulses"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_pulses(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Stream", "debounce"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_debounce(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        _ => return Err(format!("Unknown function '{}(..)' in static context", path.join("/"))),
    };
    Ok(definition)
}

/// Match result containing bindings if match succeeded
type PatternBindings = HashMap<String, Arc<ValueActor>>;

/// Spawn an actor that stores recorded calls to localStorage.
/// Each recorded call represents an item added to a list (e.g., new_todo() call).
/// Calls are stored in order so they can be replayed on restoration.
fn spawn_recorded_calls_storage_actor(
    storage_key: String,
    mut call_receiver: mpsc::Receiver<RecordedCall>,
) -> TaskHandle {
    use zoon::futures_util::StreamExt;
    use zoon::{local_storage, WebStorage};

    Task::start_droppable(async move {
        // Load existing recorded calls from storage (if any)
        let mut recorded_calls: Vec<RecordedCall> = match local_storage().get::<Vec<RecordedCall>>(&storage_key) {
            None => Vec::new(),
            Some(Ok(calls)) => calls,
            Some(Err(error)) => {
                zoon::eprintln!("[DEBUG] Failed to deserialize recorded calls for {}: {:#}", storage_key, error);
                Vec::new()
            }
        };
        zoon::println!("[DEBUG] Storage actor loaded {} existing calls for {}", recorded_calls.len(), storage_key);

        // Process incoming recorded calls
        while let Some(call) = call_receiver.next().await {
            zoon::println!("[DEBUG] Storage actor received call: {:?}", call);
            recorded_calls.push(call);

            // Save to localStorage after each call
            if let Err(error) = local_storage().insert(&storage_key, &recorded_calls) {
                zoon::eprintln!("[DEBUG] Failed to save recorded calls for {}: {:#}", storage_key, error);
            } else {
                zoon::println!("[DEBUG] Storage actor saved {} calls to {}", recorded_calls.len(), storage_key);
            }
        }

        zoon::println!("[DEBUG] Storage actor for {} shutting down", storage_key);
    })
}


