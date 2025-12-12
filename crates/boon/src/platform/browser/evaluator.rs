use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Weak};
use std::task::Poll;

use chumsky::Parser as ChumskyParser;
use chumsky::input::{Input as ChumskyInput, Stream as ChumskyStream};
use ulid::Ulid;
use zoon::futures_util::stream;
use zoon::{Stream, StreamExt, println, eprintln, Task, TaskHandle, mpsc};

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
    Persistence, PersistenceId, SourceCode, Span, span_at, static_expression, lexer, parser, resolve_references, Token, Spanned,
};
use super::api;
use super::engine::*;

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
    pub function_registry: StaticFunctionRegistry,
    pub module_loader: ModuleLoader,
    pub source_code: SourceCode,
}

impl EvaluationContext {
    /// Create a new EvaluationContext with all parameters.
    pub fn new(
        construct_context: ConstructContext,
        actor_context: ActorContext,
        reference_connector: Arc<ReferenceConnector>,
        link_connector: Arc<LinkConnector>,
        function_registry: StaticFunctionRegistry,
        module_loader: ModuleLoader,
        source_code: SourceCode,
    ) -> Self {
        Self {
            construct_context,
            actor_context,
            reference_connector: Arc::downgrade(&reference_connector),
            link_connector: Arc::downgrade(&link_connector),
            function_registry,
            module_loader,
            source_code,
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

    /// Create a derived context with sequential processing enabled.
    pub fn with_sequential_processing(&self, sequential: bool) -> Self {
        self.with_actor_context(ActorContext {
            sequential_processing: sequential,
            ..self.actor_context.clone()
        })
    }

    /// Create a derived context with backpressure permit.
    pub fn with_backpressure_permit(&self, permit: Option<BackpressurePermit>) -> Self {
        self.with_actor_context(ActorContext {
            backpressure_permit: permit,
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
    /// The variable_slots contain all the block's variable actors which must be kept alive.
    BuildBlock {
        variable_slots: Vec<SlotId>,
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
    pub forwarding_actor: Option<(Arc<ValueActor>, mpsc::UnboundedSender<Value>)>,
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
}

impl EvaluationState {
    /// Create a new empty evaluation state.
    pub fn new() -> Self {
        Self {
            work_queue: Vec::new(),
            results: HashMap::new(),
            next_slot: 0,
        }
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
            let fresh_id = Ulid::new();
            (None, fresh_id)
        }
    };
    let idempotency_key = persistence_id;

    // Debug: log every expression type being scheduled

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

            // Schedule each item (pushed last = processed first)
            for (item, slot) in items.into_iter().zip(item_slots.into_iter()) {
                schedule_expression(state, item, ctx.clone(), slot)?;
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
            let actor = build_then_actor(
                *body,
                span,
                persistence,
                persistence_id,
                ctx,
            )?;
            state.store(result_slot, actor);
        }

        static_expression::Expression::When { arms } => {
            let actor = build_when_actor(
                arms,
                span,
                persistence,
                persistence_id,
                ctx,
            )?;
            state.store(result_slot, actor);
        }

        static_expression::Expression::While { arms } => {
            let actor = build_while_actor(
                arms,
                span,
                persistence,
                persistence_id,
                ctx,
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
            let mut variable_slots = Vec::new();

            for var in variables {
                let var_slot = state.alloc_slot();
                let name = var.node.name.to_string();
                let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
                let is_referenced = var.node.is_referenced;
                let var_span = var.span;
                let var_persistence = var.persistence.clone();

                // For referenced fields, create a forwarding actor BEFORE scheduling expressions
                // This allows sibling fields to look up this field's actor immediately
                let forwarding_actor = if is_referenced && !is_link {
                    let var_persistence_id = var_persistence.as_ref().map(|p| p.id);
                    let (actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            var_persistence.clone(),
                            format!("{}: (forwarding field)", name),
                        ),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );
                    // Register with ReferenceConnector immediately
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

                // Collect all variable slots (for keeping them alive)
                variable_slots.push(var_slot);

                // Collect for later scheduling (skip Link)
                if !is_link {
                    vars_to_schedule.push((var.node.value, var_slot));
                }
            }

            // Push BuildBlock first (will be processed last due to LIFO)
            // BuildBlock takes the output expression result and keeps variables alive
            state.push(WorkItem::BuildBlock {
                variable_slots,
                output_slot: output_expr_slot,
                result_slot,
            });

            // Schedule output expression second - these work items will be processed AFTER BuildObject
            // This is important because the output may reference block variables (like `state.iteration`)
            // which need to be registered with the reference_connector by BuildObject first
            schedule_expression(state, *output, ctx.clone(), output_expr_slot)?;

            // Push BuildObject third - will be processed AFTER variable expressions but BEFORE output
            // This registers variables with reference_connector so output can resolve them
            state.push(WorkItem::BuildObject {
                variable_data,
                span,
                persistence,
                ctx: ctx.clone(),
                result_slot: object_slot,
            });

            // Schedule variable expressions last (will be processed first due to LIFO)
            for (var_expr, var_slot) in vars_to_schedule {
                schedule_expression(state, var_expr, ctx.clone(), var_slot)?;
            }
        }

        // ============================================================
        // OBJECTS (schedule values first, then build)
        // ============================================================

        static_expression::Expression::Object(object) => {
            // First pass: collect variable data and allocate slots (don't schedule yet)
            let mut variable_data = Vec::new();
            let mut vars_to_schedule = Vec::new();

            for var in object.variables {
                let var_slot = state.alloc_slot();
                let name = var.node.name.to_string();
                let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
                let is_referenced = var.node.is_referenced;
                let var_span = var.span;
                let var_persistence = var.persistence.clone();

                // For referenced fields, create a forwarding actor BEFORE scheduling expressions
                // This allows sibling fields to look up this field's actor immediately
                let forwarding_actor = if is_referenced && !is_link {
                    let var_persistence_id = var_persistence.as_ref().map(|p| p.id);
                    let (actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            var_persistence.clone(),
                            format!("{}: (forwarding field)", name),
                        ),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );
                    // Register with ReferenceConnector immediately
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

            // Push BuildObject first (will be processed last due to LIFO)
            state.push(WorkItem::BuildObject {
                variable_data,
                span,
                persistence,
                ctx: ctx.clone(),
                result_slot,
            });

            // Schedule variable expressions: referenced fields LAST so they're processed FIRST (LIFO).
            // This ensures that when `count: prev + 1` is evaluated, the `prev` field's forwarding
            // actor already has its value, because `prev: state.count` was processed first.

            // First: schedule NON-referenced fields (processed last due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule.iter() {
                if !*is_referenced {
                    schedule_expression(state, var_expr.clone(), ctx.clone(), *var_slot)?;
                }
            }
            // Last: schedule referenced fields (processed first due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule {
                if is_referenced {
                    schedule_expression(state, var_expr, ctx.clone(), var_slot)?;
                }
            }
        }

        static_expression::Expression::TaggedObject { tag, object } => {
            let tag_str = tag.to_string();
            // First pass: collect variable data and allocate slots (don't schedule yet)
            let mut variable_data = Vec::new();
            let mut vars_to_schedule = Vec::new();

            for var in object.variables {
                let var_slot = state.alloc_slot();
                let name = var.node.name.to_string();
                let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
                let is_referenced = var.node.is_referenced;
                let var_span = var.span;
                let var_persistence = var.persistence.clone();

                // For referenced fields, create a forwarding actor BEFORE scheduling expressions
                // This allows sibling fields to look up this field's actor immediately
                let forwarding_actor = if is_referenced && !is_link {
                    let var_persistence_id = var_persistence.as_ref().map(|p| p.id);
                    let (actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            var_persistence.clone(),
                            format!("{}: (forwarding field)", name),
                        ),
                        ctx.actor_context.clone(),
                        var_persistence_id,
                    );
                    // Register with ReferenceConnector immediately
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

            // Push BuildTaggedObject first (will be processed last due to LIFO)
            state.push(WorkItem::BuildTaggedObject {
                tag: tag_str,
                variable_data,
                span,
                persistence,
                ctx: ctx.clone(),
                result_slot,
            });

            // Schedule variable expressions: referenced fields LAST so they're processed FIRST (LIFO).
            // This ensures that when `count: prev + 1` is evaluated, the `prev` field's forwarding
            // actor already has its value, because `prev: state.count` was processed first.

            // First: schedule NON-referenced fields (processed last due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule.iter() {
                if !*is_referenced {
                    schedule_expression(state, var_expr.clone(), ctx.clone(), *var_slot)?;
                }
            }
            // Last: schedule referenced fields (processed first due to LIFO)
            for (var_expr, var_slot, is_referenced) in vars_to_schedule {
                if is_referenced {
                    schedule_expression(state, var_expr, ctx.clone(), var_slot)?;
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

            // Special handling for List binding functions (map, retain, every, any, sort_by)
            // These need the unevaluated expression to evaluate per-item with bindings
            match path_strs_ref.as_slice() {
                ["List", "map"] | ["List", "retain"] | ["List", "every"] | ["List", "any"] | ["List", "sort_by"] => {
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
                _ => {
                    // Normal function call - pre-evaluate all arguments
                    // First pass: collect argument data and allocate slots (don't schedule yet)
                    let mut arg_slots = Vec::new();
                    let mut args_to_schedule = Vec::new();
                    let mut passed_slot = None;
                    let mut passed_context: Option<SlotId> = None;

                    // Note: piped value is handled in call_function for BUILTIN functions only
                    // User-defined functions don't receive piped as positional arg

                    for arg in arguments {
                        let arg_slot = state.alloc_slot();
                        let arg_name = arg.node.name.to_string();

                        // Handle PASS argument - sets implicit context for nested calls
                        if arg_name == "PASS" {
                            if let Some(value) = arg.node.value {
                                args_to_schedule.push((value, arg_slot));
                                passed_context = Some(arg_slot);
                            }
                            continue; // Don't add PASS to positional arguments
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

                    // Schedule argument expressions last (will be processed first due to LIFO)
                    for (arg_expr, arg_slot) in args_to_schedule {
                        schedule_expression(state, arg_expr, ctx.clone(), arg_slot)?;
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
            // Register the function in the function registry
            let func_name = name.to_string();
            let param_names: Vec<String> = parameters.iter().map(|p| p.node.to_string()).collect();

            let func_def = StaticFunctionDefinition {
                parameters: param_names,
                body: *body,
            };

            ctx.function_registry.functions.borrow_mut().insert(func_name.clone(), func_def);

            // Function definitions don't produce a value, return SKIP
            let actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Function '{}'", func_name),
                ),
                ctx.actor_context,
                TypedStream::infinite(stream::pending::<Value>()),
                Some(persistence_id),
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
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
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
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();

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
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();

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
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();

            // Build variables
            let mut variables = Vec::new();
            for vd in variable_data.iter() {
                let var_persistence_id = vd.persistence.as_ref().map(|p| p.id);
                let variable = if vd.is_link {
                    // LINK variables don't have pre-evaluated values
                    Variable::new_link_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
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
                    // Spawn a task to forward values from the expression's actor to the channel
                    let Some(source_actor) = state.get(vd.value_slot) else { continue; };
                    let sender = sender.clone();

                    // CRITICAL: Send initial value synchronously BEFORE starting the async task.
                    // This ensures intra-object field references (e.g., `[a: 1, b: a + 1]`) work
                    // correctly, because the forwarding actor needs to have a value before
                    // other field expressions try to subscribe to it.
                    if let Some(initial_value) = source_actor.stored_value() {
                        let _ = sender.unbounded_send(initial_value);
                    }

                    let source_actor_clone = source_actor.clone();
                    let forwarding_task = Task::start_droppable(async move {
                        let mut subscription = source_actor_clone.subscribe();
                        while let Some(value) = subscription.next().await {
                            if sender.unbounded_send(value).is_err() {
                                break;
                            }
                        }
                    });
                    Variable::new_arc_with_forwarding_task(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        forwarding_actor.clone(),
                        var_persistence_id,
                        forwarding_task,
                    )
                } else {
                    // If value slot is empty, skip this variable
                    let Some(value_actor) = state.get(vd.value_slot) else { continue; };
                    Variable::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        value_actor,
                        var_persistence_id,
                    )
                };

                // Note: For referenced fields with forwarding actors, registration
                // already happened in schedule_expression, so we skip it here
                if vd.is_referenced && vd.forwarding_actor.is_none() {
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(vd.span, variable.value_actor());
                    }
                }

                // Register LINK variable senders with LinkConnector
                if vd.is_link {
                    if let Some(sender) = variable.link_value_sender() {
                        if let Some(link_connector) = ctx.try_link_connector() {
                            link_connector.register_link(vd.span, sender);
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
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();

            // Build variables
            let mut variables = Vec::new();
            for vd in variable_data.iter() {
                let var_persistence_id = vd.persistence.as_ref().map(|p| p.id);
                let variable = if vd.is_link {
                    Variable::new_link_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
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
                    // Spawn a task to forward values from the expression's actor to the channel
                    let Some(source_actor) = state.get(vd.value_slot) else { continue; };
                    let sender = sender.clone();

                    // CRITICAL: Send initial value synchronously BEFORE starting the async task.
                    // This ensures intra-object field references (e.g., `[a: 1, b: a + 1]`) work
                    // correctly, because the forwarding actor needs to have a value before
                    // other field expressions try to subscribe to it.
                    if let Some(initial_value) = source_actor.stored_value() {
                        let _ = sender.unbounded_send(initial_value);
                    }

                    let source_actor_clone = source_actor.clone();
                    let forwarding_task = Task::start_droppable(async move {
                        let mut subscription = source_actor_clone.subscribe();
                        while let Some(value) = subscription.next().await {
                            if sender.unbounded_send(value).is_err() {
                                break;
                            }
                        }
                    });
                    Variable::new_arc_with_forwarding_task(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        forwarding_actor.clone(),
                        var_persistence_id,
                        forwarding_task,
                    )
                } else {
                    // If value slot is empty, skip this variable
                    let Some(value_actor) = state.get(vd.value_slot) else { continue; };
                    Variable::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {:?}", var_persistence_id),
                            vd.persistence.clone(),
                            format!("{}: (variable)", vd.name),
                        ),
                        ctx.construct_context.clone(),
                        vd.name.clone(),
                        value_actor,
                        var_persistence_id,
                    )
                };

                // Note: For referenced fields with forwarding actors, registration
                // already happened in schedule_expression, so we skip it here
                if vd.is_referenced && vd.forwarding_actor.is_none() {
                    if let Some(ref_connector) = ctx.try_reference_connector() {
                        ref_connector.register_referenceable(vd.span, variable.value_actor());
                    }
                }

                // Register LINK variable senders with LinkConnector
                if vd.is_link {
                    if let Some(sender) = variable.link_value_sender() {
                        if let Some(link_connector) = ctx.try_link_connector() {
                            link_connector.register_link(vd.span, sender);
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
            let _persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();

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
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
            let actor = build_then_actor(*body, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        WorkItem::BuildWhen { piped_slot: _, arms, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
            let actor = build_when_actor(arms, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        WorkItem::BuildWhile { piped_slot: _, arms, span, persistence, ctx, result_slot } => {
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
            let actor = build_while_actor(arms, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        WorkItem::BuildHold { initial_slot, state_param, body, span, persistence, ctx, result_slot } => {
            // If initial value slot is empty, produce nothing
            let Some(initial_actor) = state.get(initial_slot) else {
                return Ok(());
            };
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
            if let Some(actor) = build_hold_actor(initial_actor, state_param, *body, span, persistence, persistence_id, ctx)? {
                state.store(result_slot, actor);
            }
        }

        WorkItem::BuildBlock { variable_slots, output_slot, result_slot } => {
            // If output slot is empty, this block produces nothing
            let Some(output_actor) = state.get(output_slot) else { return Ok(()); };

            // Collect variable actors to keep them alive (empty slots are ignored)
            let variable_actors: Vec<Arc<ValueActor>> = variable_slots
                .iter()
                .filter_map(|slot| state.get(*slot))
                .collect();

            // If there are variables, create a wrapper actor that keeps them alive
            // by capturing them in the stream closure
            if variable_actors.is_empty() {
                state.store(result_slot, output_actor);
            } else {
                // Create a wrapper actor that subscribes to output and holds variable actors
                // The stream::unfold keeps variable_actors alive in its closure
                let value_stream = stream::unfold(
                    (output_actor.subscribe(), variable_actors),
                    |(mut subscription, vars)| async move {
                        subscription.next().await.map(|value| (value, (subscription, vars)))
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
                    None,
                ));
                state.store(result_slot, wrapper);
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
                    ["List", "map"] | ["List", "retain"] | ["List", "every"] | ["List", "any"] | ["List", "sort_by"] => {
                        // Handle List binding functions specially - they have their own handling
                        // These use the piped value from the context
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
            } else if let static_expression::Expression::LinkSetter { alias: _ } = &expr.node {
                // Handle LinkSetter specially when piped: LINK is pass-through.
                // When you do `element |> LINK { store.path }`, the element flows through
                // unchanged. The LINK binding (connecting element to store.path) is handled
                // by the LinkConnector which matches up LINK declarations with LinkSetters
                // based on span information resolved during parsing.
                //
                // The key insight: the piped element IS the value that should flow to
                // whatever references `store.path`. We just pass it through here.
                state.store(result_slot, prev_actor);
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
                        },
                        reference_connector: ctx.reference_connector,
                        link_connector: ctx.link_connector,
                        function_registry: ctx.function_registry,
                        module_loader: ctx.module_loader,
                        source_code: ctx.source_code,
                    };
                }
            }

            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
            let actor_opt = call_function(
                path,
                args,
                span,
                persistence,
                persistence_id,
                ctx,
                use_piped_for_builtin,
            )?;
            // If function returns SKIP (None), don't store anything
            if let Some(actor) = actor_opt {
                state.store(result_slot, actor);
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
                // Use async lookup via ReferenceConnector
                let ref_connector = ctx.try_reference_connector()
                    .ok_or_else(|| "ReferenceConnector dropped - program shutting down".to_string())?;
                Box::pin(ref_connector.referenceable(*ref_span))
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
    let function_registry_for_then = ctx.function_registry.clone();
    let module_loader_for_then = ctx.module_loader.clone();
    let source_code_for_then = ctx.source_code.clone();
    let persistence_for_then = persistence.clone();
    let span_for_then = span;

    // Clone backpressure_permit for the closure
    let backpressure_permit_for_then = backpressure_permit.clone();

    // Clone HOLD callback for synchronous state updates during eager polling
    let hold_callback_for_then = actor_context_for_then.hold_state_update_callback.clone();

    // Clone body and callback for synchronous processing before eval_body closure captures them
    let body_for_sync = body.clone();
    let hold_callback_for_sync = hold_callback_for_then.clone();

    // eval_body now returns a Stream instead of Option<Value>
    // This avoids blocking on .next().await which would hang if body returns SKIP
    let eval_body = move |value: Value| -> Pin<Box<dyn Future<Output = Pin<Box<dyn Stream<Item = Value>>>>>> {
        let actor_context_clone = actor_context_for_then.clone();
        let construct_context_clone = construct_context_for_then.clone();
        let reference_connector_clone = reference_connector_for_then.clone();
        let link_connector_clone = link_connector_for_then.clone();
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
                None,
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
            let frozen_parameters: HashMap<String, Arc<ValueActor>> = actor_context_clone.parameters
                .iter()
                .filter_map(|(name, actor)| {
                    // Create a constant actor from the current stored value
                    if let Some(current_value) = actor.stored_value() {
                        let frozen_actor = ValueActor::new_arc(
                            ConstructInfo::new(
                                format!("frozen param: {name}"),
                                None,
                                format!("frozen parameter {name}"),
                            ),
                            actor_context_clone.clone(),
                            constant(current_value),
                            None,
                        );
                        Some((name.clone(), frozen_actor))
                    } else {
                        // No value yet, keep original actor
                        Some((name.clone(), actor.clone()))
                    }
                })
                .collect();

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
            };

            let new_ctx = EvaluationContext {
                construct_context: construct_context_clone.clone(),
                actor_context: new_actor_context.clone(),
                reference_connector: reference_connector_clone,
                link_connector: link_connector_clone,
                function_registry: function_registry_clone,
                module_loader: module_loader_clone,
                source_code: source_code_clone,
            };

            let body_expr = static_expression::Spanned {
                span: body_clone.span,
                persistence: persistence_clone,
                node: body_clone.node.clone(),
            };

            match evaluate_expression(body_expr, new_ctx) {
                Ok(Some(result_actor)) => {
                    // IMPORTANT: Use .take(1) because the body may be a reactive expression
                    // (e.g., `counter + 1`) that re-emits whenever its dependencies change.
                    // Without take(1), when used inside HOLD, the body would re-emit on each
                    // state update, causing infinite loops. Each THEN body evaluation should
                    // produce exactly ONE value per input.
                    let result_actor_keepalive = result_actor.clone();
                    let hold_callback_for_map = hold_callback_clone.clone();
                    let result_stream = result_actor.subscribe().take(1).map(move |mut result_value| {
                        // Keep value_actor and result_actor alive while stream is consumed
                        let _ = &value_actor;
                        let _ = &result_actor_keepalive;
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
    // SYNCHRONOUS INITIAL PROCESSING
    // =========================================================================
    // Process piped's historical values synchronously during construction.
    // This is critical for HOLD + Stream/pulses patterns where:
    // 1. Stream/pulses emits N pulses synchronously (via stream::iter())
    // 2. Those pulses are stored in piped's ValueActor (due to eager polling)
    // 3. THEN must process those pulses synchronously so HOLD sees all updates
    // 4. Document/new then sees the final computed value, not initial value
    //
    // Without this fix, .then(eval_body) makes processing async because
    // eval_body awaits permit.acquire(), causing Poll::Pending on first poll.

    let initial_version = piped.version();
    let (initial_piped_values, _oldest) = piped.get_values_since(0);

    // Evaluate body for each initial piped value synchronously
    let mut initial_results: Vec<Value> = Vec::new();
    for (idx, input_value) in initial_piped_values.into_iter().enumerate() {

        // Create frozen parameters for SNAPSHOT semantics
        let frozen_parameters: HashMap<String, Arc<ValueActor>> = ctx.actor_context.parameters
            .iter()
            .filter_map(|(name, actor)| {
                if let Some(current_value) = actor.stored_value() {
                    let frozen_actor = ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("frozen param: {name} (sync)"),
                            None,
                            format!("frozen parameter {name} (sync)"),
                        ),
                        ctx.actor_context.clone(),
                        constant(current_value),
                        None,
                    );
                    Some((name.clone(), frozen_actor))
                } else {
                    Some((name.clone(), actor.clone()))
                }
            })
            .collect();

        // Create piped value actor for body evaluation
        let value_actor = ValueActor::new_arc(
            ConstructInfo::new(
                "THEN input value (sync)".to_string(),
                None,
                format!("{span_for_then}; THEN input (sync)"),
            ),
            ctx.actor_context.clone(),
            constant(input_value),
            None,
        );

        let sync_actor_context = ActorContext {
            output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
            piped: Some(value_actor.clone()),
            passed: ctx.actor_context.passed.clone(),
            parameters: frozen_parameters,
            sequential_processing: ctx.actor_context.sequential_processing,
            backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
            hold_state_update_callback: None,
            use_lazy_actors: ctx.actor_context.use_lazy_actors,
        };

        let sync_ctx = EvaluationContext {
            construct_context: ctx.construct_context.clone(),
            actor_context: sync_actor_context,
            reference_connector: ctx.reference_connector.clone(),
            link_connector: ctx.link_connector.clone(),
            function_registry: ctx.function_registry.clone(),
            module_loader: ctx.module_loader.clone(),
            source_code: ctx.source_code.clone(),
        };

        let body_expr = static_expression::Spanned {
            span: body_for_sync.span,
            persistence: persistence.clone(),
            node: body_for_sync.node.clone(),
        };

        // Evaluate body synchronously
        match evaluate_expression(body_expr, sync_ctx) {
            Ok(Some(result_actor)) => {
                // Due to ValueActor's eager polling, result_actor should have stored value
                if let Some(mut result_value) = result_actor.stored_value() {
                    result_value.set_idempotency_key(ValueIdempotencyKey::new());

                    // Call HOLD's callback if present (for state updates)
                    if let Some(ref callback) = hold_callback_for_sync {
                        callback(result_value.clone());
                    }

                    initial_results.push(result_value);
                } else {
                }
            }
            Ok(None) => {
            }
            Err(e) => {
            }
        }
    }


    // =========================================================================
    // REACTIVE SUBSCRIPTION (for future values)
    // =========================================================================
    // Use then + flatten_unordered instead of then + filter_map
    // flatten_unordered processes inner streams concurrently, so even if one stream
    // never emits (SKIP case), others can still produce values

    // Clone piped for the filter closure
    let piped_for_filter = piped.clone();

    // When use_lazy_actors is true and piped has a lazy delegate, use subscribe_boxed()
    // to get lazy subscription (pull-based). Otherwise use regular subscribe() (eager).
    let use_lazy_subscription = ctx.actor_context.use_lazy_actors && piped.has_lazy_delegate();

    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
        // For sequential mode, use regular flatten (processes one stream at a time)
        // Skip values we already processed synchronously
        if use_lazy_subscription {
            // For LAZY subscription: NO filter needed!
            // LazyValueActor doesn't eagerly buffer values, so there are no "initial values"
            // to skip. The filter `version > initial_version` would ALWAYS fail because
            // LazyValueActor's shell ValueActor has version=0 (never updated).
            // All values will be pulled on demand.
            let stream = piped.clone().subscribe_boxed()
                .then(eval_body)
                .flatten();
            Box::pin(stream)
        } else {
            let stream = piped.clone().subscribe()
                .filter(move |_| zoon::future::ready(piped_for_filter.version() > initial_version))
                .then(eval_body)
                .flatten();
            // Chain initial synchronous results with reactive subscription
            Box::pin(stream::iter(initial_results).chain(stream))
        }
    } else {
        // Clone piped_for_filter for the concurrent branch
        let piped_for_filter_concurrent = piped.clone();
        // For non-sequential mode, use flatten_unordered for concurrent processing
        if use_lazy_subscription {
            // For LAZY subscription: NO filter needed (see comment above)
            let stream = piped.clone().subscribe_boxed()
                .then(eval_body)
                .flatten_unordered(None);
            Box::pin(stream)
        } else {
            let stream = piped.clone().subscribe()
                .filter(move |_| zoon::future::ready(piped_for_filter_concurrent.version() > initial_version))
                .then(eval_body)
                .flatten_unordered(None);
            // Chain initial synchronous results with reactive subscription
            Box::pin(stream::iter(initial_results).chain(stream))
        }
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
            Some(persistence_id),
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
            Some(persistence_id),
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
    let function_registry_for_when = ctx.function_registry.clone();
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
        let function_registry_clone = function_registry_for_when.clone();
        let module_loader_clone = module_loader_for_when.clone();
        let source_code_clone = source_code_for_when.clone();
        let persistence_clone = persistence_for_when.clone();
        let arms_clone = arms.clone();

        Box::pin(async move {
            // Try to match against each arm
            for arm in &arms_clone {
                // Use async pattern matching to properly extract bindings from Objects
                if let Some(bindings) = match_pattern(&arm.pattern, &value).await {
                    let value_actor = ValueActor::new_arc(
                        ConstructInfo::new(
                            "WHEN input value".to_string(),
                            None,
                            format!("{span_for_when}; WHEN input"),
                        ),
                        actor_context_clone.clone(),
                        constant(value.clone()),
                        None,
                    );

                    // CRITICAL FIX: Freeze parameters for SNAPSHOT semantics (same as THEN).
                    // When WHEN body references `state` (from HOLD), we want the CURRENT value at the
                    // time of body evaluation, not all historical values from the reactive subscription.
                    let frozen_parameters: HashMap<String, Arc<ValueActor>> = actor_context_clone.parameters
                        .iter()
                        .filter_map(|(name, actor)| {
                            if let Some(current_value) = actor.stored_value() {
                                let frozen_actor = ValueActor::new_arc(
                                    ConstructInfo::new(
                                        format!("frozen param: {name}"),
                                        None,
                                        format!("frozen parameter {name}"),
                                    ),
                                    actor_context_clone.clone(),
                                    constant(current_value),
                                    None,
                                );
                                Some((name.clone(), frozen_actor))
                            } else {
                                Some((name.clone(), actor.clone()))
                            }
                        })
                        .collect();

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
                            None,
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
                    };

                    let new_ctx = EvaluationContext {
                        construct_context: construct_context_clone.clone(),
                        actor_context: new_actor_context.clone(),
                        reference_connector: reference_connector_clone.clone(),
                        link_connector: link_connector_clone.clone(),
                        function_registry: function_registry_clone.clone(),
                        module_loader: module_loader_clone.clone(),
                        source_code: source_code_clone.clone(),
                    };

                    // arm.body is Expression, not Spanned<Expression>
                    // We wrap it with the parent's span/persistence
                    let body_expr = static_expression::Spanned {
                        span: span.clone(),
                        persistence: persistence_clone,
                        node: arm.body.clone(),
                    };

                    match evaluate_expression(body_expr, new_ctx) {
                        Ok(Some(result_actor)) => {
                            // IMPORTANT: Use .take(1) because the body may be a reactive expression
                            // (e.g., `counter + 1`) that re-emits whenever its dependencies change.
                            // Without take(1), when used inside HOLD, the body would re-emit on each
                            // state update, causing infinite loops. Each WHEN body evaluation should
                            // produce exactly ONE value per input.
                            let result_actor_keepalive = result_actor.clone();
                            let result_stream = result_actor.subscribe().take(1).map(move |mut result_value| {
                                // Keep value_actor and result_actor alive while stream is consumed
                                let _ = &value_actor;
                                let _ = &result_actor_keepalive;
                                result_value.set_idempotency_key(ValueIdempotencyKey::new());
                                result_value
                            });
                            return Box::pin(result_stream) as Pin<Box<dyn Stream<Item = Value>>>;
                        }
                        Ok(None) => {
                            // SKIP - return finite empty stream (flatten_unordered removes it cleanly)
                            return Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>;
                        }
                        Err(_e) => {
                            return Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>;
                        }
                    }
                }
            }
            Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
        })
    };

    // Use then + flatten_unordered instead of then + filter_map
    // flatten_unordered processes inner streams concurrently, so even if one stream
    // never emits (SKIP case), others can still produce values
    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
        // For sequential mode, use regular flatten (processes one stream at a time)
        let stream = piped.clone().subscribe()
            .then(eval_body)
            .flatten();
        Box::pin(stream)
    } else {
        // For non-sequential mode, use flatten_unordered for concurrent processing
        // None = unlimited concurrency
        let stream = piped.clone().subscribe()
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
        Some(persistence_id),
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
) -> Result<Arc<ValueActor>, String> {
    let piped = ctx.actor_context.piped.clone()
        .ok_or("WHILE requires a piped value")?;

    let construct_context_for_while = ctx.construct_context.clone();
    let actor_context_for_while = ctx.actor_context.clone();
    let reference_connector_for_while = ctx.reference_connector.clone();
    let link_connector_for_while = ctx.link_connector.clone();
    let function_registry_for_while = ctx.function_registry.clone();
    let module_loader_for_while = ctx.module_loader.clone();
    let source_code_for_while = ctx.source_code.clone();
    let persistence_for_while = persistence.clone();
    let span_for_while = span;

    // Use then().flatten() pattern to allow async pattern matching
    let stream = piped.clone().subscribe().then(move |value| {
        let actor_context_clone = actor_context_for_while.clone();
        let construct_context_clone = construct_context_for_while.clone();
        let reference_connector_clone = reference_connector_for_while.clone();
        let link_connector_clone = link_connector_for_while.clone();
        let function_registry_clone = function_registry_for_while.clone();
        let module_loader_clone = module_loader_for_while.clone();
        let source_code_clone = source_code_for_while.clone();
        let persistence_clone = persistence_for_while.clone();
        let arms_clone = arms.clone();

        async move {
            // Find matching arm using async pattern matching
            let mut matched_arm_with_bindings: Option<(&static_expression::Arm, HashMap<String, Value>)> = None;
            for arm in &arms_clone {
                if let Some(bindings) = match_pattern(&arm.pattern, &value).await {
                    matched_arm_with_bindings = Some((arm, bindings));
                    break;
                }
            }

            if let Some((arm, bindings)) = matched_arm_with_bindings {
                let value_actor = ValueActor::new_arc(
                    ConstructInfo::new(
                        "WHILE input value".to_string(),
                        None,
                        format!("{span_for_while}; WHILE input"),
                    ),
                    actor_context_clone.clone(),
                    constant(value),
                    None,
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
                        None,
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
                };

                let new_ctx = EvaluationContext {
                    construct_context: construct_context_clone,
                    actor_context: new_actor_context,
                    reference_connector: reference_connector_clone,
                    link_connector: link_connector_clone,
                    function_registry: function_registry_clone,
                    module_loader: module_loader_clone,
                    source_code: source_code_clone,
                };

                // arm.body is Expression, not Spanned<Expression>
                // We wrap it with the parent's span/persistence
                let body_expr = static_expression::Spanned {
                    span: span.clone(),
                    persistence: persistence_clone,
                    node: arm.body.clone(),
                };

                match evaluate_expression(body_expr, new_ctx) {
                    Ok(Some(result_actor)) => {
                        let stream: Pin<Box<dyn Stream<Item = Value>>> = Box::pin(result_actor.subscribe());
                        stream
                    }
                    Ok(None) => {
                        // SKIP - return finite empty stream (flatten removes it cleanly)
                        Box::pin(stream::empty())
                    }
                    Err(_) => {
                        Box::pin(stream::empty())
                    }
                }
            } else {
                Box::pin(stream::empty())
            }
        }
    }).flatten();

    // Keep the piped actor alive by including it in inputs
    Ok(ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; WHILE {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(stream),
        Some(persistence_id),
        vec![piped],  // Keep piped actor alive
    ))
}

/// Synchronously extract a field value from a Value following a path of field names.
/// Returns None if the path cannot be fully resolved (e.g., non-object value, missing field).
fn extract_field_path(value: &Value, path: &[String]) -> Option<Value> {
    let mut current = value.clone();
    for field_name in path {
        match &current {
            Value::Object(object, _) => {
                let variable_actor = object.expect_variable(field_name).value_actor();
                if let Some(val) = variable_actor.stored_value() {
                    current = val;
                } else {
                    // Field actor doesn't have a stored value yet
                    return None;
                }
            }
            Value::TaggedObject(tagged_object, _) => {
                let variable_actor = tagged_object.expect_variable(field_name).value_actor();
                if let Some(val) = variable_actor.stored_value() {
                    current = val;
                } else {
                    // Field actor doesn't have a stored value yet
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
fn build_field_access_actor(
    path: Vec<String>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    let piped = ctx.actor_context.piped.clone()
        .ok_or("FieldAccess requires a piped value")?;

    // Keep path for display in ConstructInfo
    let path_display = path.join(".");
    let path_strings = path.clone();

    // CRITICAL: Emit initial value synchronously if piped has a stored value.
    // This ensures intra-object field references work correctly because the
    // FieldAccess result is available immediately during synchronous evaluation.
    let initial_version = piped.version();
    let initial_value: Option<Value> = if let Some(source_value) = piped.stored_value() {
        // Synchronously extract the field value from the stored source
        extract_field_path(&source_value, &path)
    } else {
        None
    };

    // Build the subscription stream that handles future updates (with field extraction)
    let piped_for_filter = piped.clone();
    let mut subscription_stream: Pin<Box<dyn Stream<Item = Value>>> = Box::pin(
        piped.clone().subscribe()
            .filter(move |_| {
                // Skip versions we already processed synchronously
                zoon::future::ready(piped_for_filter.version() > initial_version)
            })
    );

    // For each field in the path, transform the subscription stream to extract that field
    // IMPORTANT: Use map + flatten_unordered instead of flat_map!
    // flat_map uses flatten which waits for each inner stream to complete before processing the next.
    // Since Object field streams are infinite (they use pending()), flatten would block.
    // flatten_unordered processes inner streams concurrently.
    for field_name in path_strings {
        subscription_stream = Box::pin(
            subscription_stream
                .map(move |value| {
                    let field_name = field_name.clone();
                    // Get the field Variable and subscribe to all its values
                    match value {
                        Value::Object(object, _) => {
                            let variable = object.expect_variable(&field_name);
                            let is_link = variable.link_value_sender().is_some();
                            let variable_actor = variable.value_actor();

                            if is_link {
                                // LINK field: stay subscribed to receive multiple events
                                stream::unfold(
                                    (variable_actor.subscribe(), object, variable),
                                    move |(mut subscription, object, variable)| async move {
                                        subscription.next().await.map(|value| (value, (subscription, object, variable)))
                                    }
                                ).boxed_local()
                            } else {
                                // Non-LINK field: emit once and complete
                                stream::once(async move {
                                    let mut subscription = variable_actor.subscribe();
                                    let result = subscription.next().await;
                                    let _ = (&object, &variable);
                                    result
                                }).filter_map(|opt| async move { opt }).boxed_local()
                            }
                        }
                        Value::TaggedObject(tagged_object, _) => {
                            let variable = tagged_object.expect_variable(&field_name);
                            let is_link = variable.link_value_sender().is_some();
                            let variable_actor = variable.value_actor();

                            if is_link {
                                // LINK field: stay subscribed to receive multiple events
                                stream::unfold(
                                    (variable_actor.subscribe(), tagged_object, variable),
                                    move |(mut subscription, tagged_object, variable)| async move {
                                        subscription.next().await.map(|value| (value, (subscription, tagged_object, variable)))
                                    }
                                ).boxed_local()
                            } else {
                                // Non-LINK field: emit once and complete
                                stream::once(async move {
                                    let mut subscription = variable_actor.subscribe();
                                    let result = subscription.next().await;
                                    let _ = (&tagged_object, &variable);
                                    result
                                }).filter_map(|opt| async move { opt }).boxed_local()
                            }
                        }
                        other => {
                            // Not an object - log error and return empty stream
                            zoon::println!(
                                "FieldAccess: Cannot access field '{}' on non-object value: {}",
                                field_name, other.construct_info()
                            );
                            stream::empty().boxed_local()
                        }
                    }
                })
                // With Some(1), non-LINK streams complete after emitting, allowing subsequent LINK events
                .flatten_unordered(Some(1))
        );
    }

    // Combine: emit initial value synchronously first, then follow with subscription stream
    let combined_stream: Pin<Box<dyn Stream<Item = Value>>> = Box::pin(
        stream::iter(initial_value.into_iter()).chain(subscription_stream)
    );

    // Keep the piped actor alive by including it in inputs
    Ok(ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; .{path_display}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(combined_stream),
        Some(persistence_id),
        vec![piped],  // Keep piped actor alive
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
    // Use a channel to hold current state value and broadcast updates
    let (state_sender, state_receiver) = zoon::futures_channel::mpsc::unbounded::<Value>();
    let state_sender = Rc::new(RefCell::new(state_sender));
    let state_sender_for_update = state_sender.clone();
    let state_sender_for_reset = state_sender.clone();

    // Current state holder (starts with None, will be set when initial emits)
    let current_state: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
    let current_state_for_body = current_state.clone();
    let current_state_for_update = current_state.clone();

    // Create a ValueActor that provides the current state to the body
    // This is what the state_param references
    //
    // State stream: first value from initial_actor, then updates from state_receiver
    let state_stream = initial_actor.clone().subscribe()
        .take(1)  // Get the first initial value
        .chain(state_receiver);  // Then listen for updates and resets

    // Create state actor - initial value will come through the stream asynchronously
    let state_actor = ValueActor::new_arc(
        ConstructInfo::new(
            format!("Hold state actor for {state_param}"),
            None,
            format!("{span}; HOLD state parameter"),
        ),
        ctx.actor_context.clone(),
        TypedStream::infinite(state_stream),
        None,
    );

    // Bind the state parameter in the context so body can reference it
    let mut body_parameters = ctx.actor_context.parameters.clone();
    body_parameters.insert(state_param.clone(), state_actor.clone());

    // Clone state_actor for use in state_update_stream to directly update its stored value
    let state_actor_for_update = state_actor.clone();
    // Clone for the synchronous callback that THEN will use
    let state_actor_for_callback = state_actor;

    // Create backpressure permit for synchronizing THEN with state updates.
    // Initial count = 1 allows first body evaluation to start.
    // HOLD's callback releases permit after each state update, allowing next body to run.
    let backpressure_permit = BackpressurePermit::new(1);
    let permit_for_callback = backpressure_permit.clone();

    // Create callback for THEN to update HOLD's state synchronously.
    // This ensures the next body evaluation sees the updated state.
    // NOTE: We do NOT store to output here - state_update_stream handles that.
    // Storing in both places would cause duplicate emissions.
    let hold_state_update_callback: Rc<dyn Fn(Value)> = Rc::new(move |new_value: Value| {
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
    };

    // Create new context for body evaluation
    let body_ctx = EvaluationContext {
        construct_context: ctx.construct_context.clone(),
        actor_context: body_actor_context,
        reference_connector: ctx.reference_connector.clone(),
        link_connector: ctx.link_connector.clone(),
        function_registry: ctx.function_registry.clone(),
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
    // Use subscribe_boxed() to get lazy subscription if body has lazy_delegate.
    // This enables demand-driven evaluation where HOLD pulls values one at a time
    // and updates state between each pull (sequential state updates).
    let body_subscription = body_result.clone().subscribe_boxed();
    let state_update_stream = body_subscription.map(move |new_value| {
        // Update current state
        *current_state_for_update.borrow_mut() = Some(new_value.clone());
        // Send to state channel so body can see it on next event
        let _ = state_sender_for_update.borrow().unbounded_send(new_value.clone());
        // DIRECTLY update state_actor's stored value - bypass async channel delay.
        // This ensures the next THEN body evaluation reads the fresh state value.
        // NOTE: The callback already updates state_actor, but we update here too
        // for cases where the value flows through without callback (e.g., non-THEN body).
        state_actor_for_update.store_value_directly(new_value.clone());
        // NOTE: Do NOT release permit here! The hold_state_update_callback already
        // releases it after THEN's body evaluation. Releasing twice would cause
        // permit count to grow, defeating backpressure and allowing parallel processing.
        new_value
    });

    // Create output actor FIRST with a pending stream (stays alive, no async stream processing).
    // Values will be stored directly via store_value_directly() from the stream closures below.
    // This ensures values are available in history immediately when Stream/skip subscribes.
    //
    // The task_handle_cell holds the TaskHandle for the driver task. When the output actor
    // is dropped, the stream is dropped, which drops the Rc, which drops the TaskHandle,
    // which cancels the driver task. This ensures Timer/interval stops when switching examples.
    let task_handle_cell: Rc<RefCell<Option<TaskHandle>>> = Rc::new(RefCell::new(None));
    let task_handle_cell_for_stream = task_handle_cell.clone();
    let output_stream = stream::poll_fn(move |_cx| {
        // Keep the RefCell alive - this holds the TaskHandle when set
        let _keep_alive = task_handle_cell_for_stream.borrow();
        Poll::Pending::<Option<Value>>
    });
    let output = ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; HOLD {state_param} {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(output_stream),
        Some(persistence_id),
        vec![body_result.clone(), initial_actor.clone()],
    );

    // CRITICAL: Store initial value SYNCHRONOUSLY before returning.
    // This ensures downstream subscribers (like Stream/skip) see the initial value immediately,
    // even before the Task that drives combined_stream runs.
    // Without this, Stream/skip's task may run before HOLD's task, seeing no values.
    if let Some(initial_value) = initial_actor.stored_value() {
        output.store_value_directly(initial_value);
    }

    // NOTE: We do NOT copy body_result's history here anymore.
    // The state_update_stream (below) will emit those values when polled, and it calls
    // store_value_directly() on output. Copying here would cause duplicate emissions.

    // Reset/passthrough behavior: ALL emissions from input pass through as HOLD output.
    // First emission: state_actor gets it via take(1), so we don't send to state_receiver.
    // Subsequent emissions: send to state_receiver so body sees the reset value.
    // NOTE: First value is already stored synchronously above, so we skip storing it here.
    //
    // IMPORTANT: Use Weak<ValueActor> instead of Arc to avoid circular reference!
    // The output actor holds (via Rc chain) the driver task, which holds combined_stream,
    // which holds these closures. Using Arc would create a cycle preventing cleanup.
    let is_first_input = Rc::new(RefCell::new(true));
    let output_weak_for_initial = Arc::downgrade(&output);
    let initial_stream = initial_actor.clone().subscribe().map(move |value| {
        let is_first = *is_first_input.borrow();
        if is_first {
            *is_first_input.borrow_mut() = false;
            // First value: just update current_state, don't send to state_receiver
            // (take(1) in state_stream already handles state_actor for first value)
            // NOTE: Don't store to output here - it was already stored synchronously above.
            *current_state_for_body.borrow_mut() = Some(value.clone());
        } else {
            // Subsequent values (reset): update current_state AND send to state_receiver
            *current_state_for_body.borrow_mut() = Some(value.clone());
            let _ = state_sender_for_reset.borrow().unbounded_send(value.clone());
            // Store value directly to output - only for reset values, not initial
            // Use weak reference to avoid circular reference
            if let Some(output) = output_weak_for_initial.upgrade() {
                output.store_value_directly(value.clone());
            }
        }
        // Always pass through as HOLD output
        value
    });

    // Modify state_update_stream to also store values directly to output
    // IMPORTANT: Use Weak<ValueActor> to avoid circular reference!
    let output_weak_for_update = Arc::downgrade(&output);
    let state_update_stream = state_update_stream.map(move |value| {
        // Use weak reference to avoid circular reference
        if let Some(output) = output_weak_for_update.upgrade() {
            output.store_value_directly(value.clone());
        }
        value
    });

    // Combine: input stream sets/resets state, body updates state
    // Use select to merge both streams - any emission from input resets state
    let combined_stream = stream::select(
        initial_stream, // Any emission from input resets the state
        state_update_stream
    );

    // Start a droppable task to drive the combined stream (poll it so closures execute).
    // The output actor stays alive via its pending stream, and values are stored
    // directly via store_value_directly() in the stream closures above.
    // The TaskHandle is stored in task_handle_cell so it's dropped when the output is dropped.
    let task_handle = Task::start_droppable(async move {
        let mut stream = combined_stream;
        while stream.next().await.is_some() {
            // Values are already stored via store_value_directly in the map closures
        }
    });
    *task_handle_cell.borrow_mut() = Some(task_handle);

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
    // Collect task handles to keep forwarding tasks alive
    let mut forwarding_tasks: Vec<TaskHandle> = Vec::new();

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
                // Interpolation - look up the variable
                let var_name = var.to_string();
                if let Some(var_actor) = ctx.actor_context.parameters.get(&var_name) {
                    part_actors.push((false, var_actor.clone()));
                } else if let Some(ref_span) = referenced_span {
                    // Use reference_connector to get the variable from outer scope
                    // Create a forwarding actor that immediately starts forwarding values
                    // from the referenced actor via a dedicated task.
                    // This avoids timing issues with the lazy stream approach.
                    let ref_connector = ctx.try_reference_connector()
                        .ok_or_else(|| "ReferenceConnector dropped - program shutting down".to_string())?;
                    let ref_span_copy = *ref_span;

                    // Create forwarding actor with unbounded channel
                    let (ref_actor, sender) = ValueActor::new_arc_forwarding(
                        ConstructInfo::new(
                            format!("TextInterpolation:{}", var_name),
                            None,
                            format!("{span}; TextInterpolation for '{}'", var_name),
                        ),
                        ctx.actor_context.clone(),
                        None,
                    );

                    // Spawn task to resolve reference and forward values
                    let task_handle = Task::start_droppable(async move {
                        let actor = ref_connector.referenceable(ref_span_copy).await;
                        let mut subscription = actor.subscribe();
                        while let Some(value) = subscription.next().await {
                            if sender.unbounded_send(value).is_err() {
                                // Receiver dropped, stop forwarding
                                break;
                            }
                        }
                    });
                    forwarding_tasks.push(task_handle);

                    part_actors.push((false, ref_actor));
                } else {
                    return Err(format!("Variable '{}' not found for text interpolation", var_name));
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
            .map(|(_, actor)| actor.clone().subscribe())
            .collect();

        // For simplicity, use select_all and latest values approach
        let merged = stream::select_all(part_subscriptions.into_iter().enumerate().map(|(idx, s)| {
            s.map(move |v| (idx, v))
        }));

        let part_count = part_actors.len();
        // Move forwarding_tasks into scan state to keep them alive
        let combined_stream = merged.scan(
            (vec![None; part_count], forwarding_tasks),
            move |(latest_values, _forwarding_tasks), (idx, value)| {
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
                ValueMetadata {
                    idempotency_key: ValueIdempotencyKey::new(),
                },
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
            Some(persistence_id),
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
    let stream = alias_actor.subscribe().map(move |value| {
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
        Some(persistence_id),
    ))
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
    let config = ListBindingConfig {
        binding_name,
        transform_expr,
        operation,
        reference_connector,
        link_connector,
        source_code: ctx.source_code.clone(),
    };

    Ok(Some(ListBindingFunction::new_arc_value_actor(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; List/{}(..)", path_strs[1]),
        ),
        ctx.construct_context,
        ctx.actor_context,
        list_actor,
        config,
    )))
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
    let func_def_opt = ctx.function_registry.functions.borrow().get(&full_path).cloned();

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

            // Create a stream that:
            // 1. Subscribes to the piped input
            // 2. For each value from piped, evaluates the function body with that value
            // 3. If piped never produces values (SKIP), this stream also never produces values
            let result_stream = piped_for_closure.subscribe().flat_map(move |piped_value| {
                // Create a constant actor for this specific piped value
                let value_actor = ValueActor::new_arc(
                    ConstructInfo::new(
                        "piped function input".to_string(),
                        None,
                        format!("piped value for user function param: {}", param_name_for_closure),
                    ),
                    ctx_for_closure.actor_context.clone(),
                    constant(piped_value),
                    None,
                );

                // Bind the constant value actor to the parameter
                let mut params = parameters_for_closure.clone();
                params.insert(param_name_for_closure.clone(), value_actor);

                let new_actor_context = ActorContext {
                    output_valve_signal: ctx_for_closure.actor_context.output_valve_signal.clone(),
                    piped: None, // Clear piped - we've consumed it
                    passed: ctx_for_closure.actor_context.passed.clone(),
                    parameters: params,
                    sequential_processing: ctx_for_closure.actor_context.sequential_processing,
                    backpressure_permit: ctx_for_closure.actor_context.backpressure_permit.clone(),
                    hold_state_update_callback: None,
                    use_lazy_actors: ctx_for_closure.actor_context.use_lazy_actors,
                };

                let new_ctx = EvaluationContext {
                    construct_context: ctx_for_closure.construct_context.clone(),
                    actor_context: new_actor_context,
                    reference_connector: ctx_for_closure.reference_connector.clone(),
                    link_connector: ctx_for_closure.link_connector.clone(),
                    function_registry: ctx_for_closure.function_registry.clone(),
                    module_loader: ctx_for_closure.module_loader.clone(),
                    source_code: ctx_for_closure.source_code.clone(),
                };

                // Evaluate the function body with this piped value
                match evaluate_expression(func_body.clone(), new_ctx) {
                    Ok(Some(result_actor)) => {
                        // Take only the first value from the result (like THEN does)
                        let result_stream: Pin<Box<dyn Stream<Item = Value>>> =
                            Box::pin(result_actor.subscribe().take(1));
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
                None,
            );

            return Ok(Some(wrapper_actor));
        }

        // No piped value or no parameter to bind - evaluate immediately (original behavior)
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
        };

        let new_ctx = EvaluationContext {
            construct_context: ctx.construct_context,
            actor_context: new_actor_context,
            reference_connector: ctx.reference_connector,
            link_connector: ctx.link_connector,
            function_registry: ctx.function_registry,
            module_loader: ctx.module_loader,
            source_code: ctx.source_code,
        };

        let result = evaluate_expression(func_def.body, new_ctx);
        // Propagate None (SKIP) from function body
        return result;
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

    // Debug: log pattern and value types
    let pattern_type = match pattern {
        static_expression::Pattern::WildCard => "WildCard".to_string(),
        static_expression::Pattern::Alias { name } => format!("Alias({})", name.as_str()),
        static_expression::Pattern::Literal(lit) => match lit {
            static_expression::Literal::Number(n) => format!("Literal::Number({})", n),
            static_expression::Literal::Tag(t) => format!("Literal::Tag({})", t.as_str()),
            static_expression::Literal::Text(t) => format!("Literal::Text({})", t.as_str()),
        },
        static_expression::Pattern::TaggedObject { tag, .. } => format!("TaggedObject({})", tag.as_str()),
        static_expression::Pattern::Object { .. } => "Object".to_string(),
        static_expression::Pattern::List { .. } => "List".to_string(),
        static_expression::Pattern::Map { .. } => "Map".to_string(),
    };
    let value_type = match value {
        Value::Number(n, _) => format!("Number({})", n.number()),
        Value::Text(t, _) => format!("Text({})", t.text()),
        Value::Tag(t, _) => format!("Tag({})", t.tag()),
        Value::TaggedObject(to, _) => format!("TaggedObject({})", to.tag()),
        Value::Object(_, _) => "Object".to_string(),
        Value::List(_, _) => "List".to_string(),
        Value::Flushed(_, _) => "Flushed".to_string(),
    };

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
                            if let Some(field_value) = variable.value_actor().subscribe().next().await {
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
                        if let Some(field_value) = variable.value_actor().subscribe().next().await {
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
            // TODO: List pattern matching needs special handling since List is a complex
            // reactive type with diff-based updates. For now, we don't support extracting
            // individual items from List patterns.
            // SLEEPING BOMB: This will silently match Lists without extracting bindings!
            if let Value::List(_list, _) = value {
                let _ = items;
                Some(bindings)
            } else {
                None
            }
        }

        static_expression::Pattern::Map { entries } => {
            // Map pattern matching - simplified for now
            let _ = entries;
            Some(bindings)
        }
    }
}

// =============================================================================
// END STACK-SAFE EVALUATION FUNCTIONS
// =============================================================================

/// Registry for user-defined functions using static expressions.
/// No lifetime parameter - can be stored and used anywhere.
#[derive(Clone, Default)]
pub struct StaticFunctionRegistry {
    pub functions: Rc<RefCell<HashMap<String, StaticFunctionDefinition>>>,
}

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

/// Module loader with caching for loading and parsing Boon modules.
/// Resolves module paths like "Theme" to file paths and caches parsed modules.
#[derive(Clone, Default)]
pub struct ModuleLoader {
    /// Cache of loaded modules (module_path -> ModuleData)
    cache: Rc<RefCell<HashMap<String, ModuleData>>>,
    /// Base directory for module resolution (e.g., the directory containing RUN.bn)
    base_dir: Rc<RefCell<String>>,
}

impl ModuleLoader {
    pub fn new(base_dir: impl Into<String>) -> Self {
        Self {
            cache: Rc::new(RefCell::new(HashMap::new())),
            base_dir: Rc::new(RefCell::new(base_dir.into())),
        }
    }

    /// Set the base directory for module resolution
    pub fn set_base_dir(&self, dir: impl Into<String>) {
        *self.base_dir.borrow_mut() = dir.into();
    }

    /// Get the base directory
    pub fn base_dir(&self) -> String {
        self.base_dir.borrow().clone()
    }

    /// Load a module by name (e.g., "Theme", "Professional", "Assets")
    /// Tries multiple resolution paths:
    /// 1. {base_dir}/{module_name}.bn
    /// 2. {base_dir}/{module_name}/{module_name}.bn
    /// 3. {base_dir}/Generated/{module_name}.bn (for generated files)
    pub fn load_module(
        &self,
        module_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<ModuleData> {
        // Check cache first
        if let Some(cached) = self.cache.borrow().get(module_name) {
            return Some(cached.clone());
        }

        let base_dir_binding = self.base_dir.borrow();
        let base = current_dir.unwrap_or(&base_dir_binding);

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
            make_path(&base_dir_binding, &format!("{}.bn", module_name)),
            make_path(&base_dir_binding, &format!("{}/{}.bn", module_name, module_name)),
            make_path(&base_dir_binding, &format!("Generated/{}.bn", module_name)),
        ];

        for path in paths_to_try {
            if let Some(source_code) = virtual_fs.read_text(&path) {
                println!("[ModuleLoader] Loading module '{}' from '{}'", module_name, path);
                if let Some(module_data) = self.parse_module(&path, &source_code) {
                    // Cache the module
                    self.cache.borrow_mut().insert(module_name.to_string(), module_data.clone());
                    return Some(module_data);
                }
            }
        }

        eprintln!("[ModuleLoader] Could not find module '{}' (tried from base '{}')", module_name, base);
        None
    }

    /// Parse module source code into ModuleData
    fn parse_module(&self, filename: &str, source_code: &str) -> Option<ModuleData> {
        // Lexer
        let (tokens, errors) = lexer().parse(source_code).into_output_errors();
        if !errors.is_empty() {
            eprintln!("[ModuleLoader] Lex errors in '{}': {:?}", filename, errors.len());
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
            eprintln!("[ModuleLoader] Parse errors in '{}': {:?}", filename, errors.len());
            return None;
        }
        let ast = ast?;

        // Reference resolution
        let ast = match resolve_references(ast) {
            Ok(ast) => ast,
            Err(errors) => {
                eprintln!("[ModuleLoader] Reference errors in '{}': {:?}", filename, errors.len());
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

    /// Get a function from a module
    pub fn get_function(
        &self,
        module_name: &str,
        function_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<StaticFunctionDefinition> {
        let module = self.load_module(module_name, virtual_fs, current_dir)?;
        module.functions.get(function_name).cloned()
    }

    /// Get a variable from a module
    pub fn get_variable(
        &self,
        module_name: &str,
        variable_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<static_expression::Spanned<static_expression::Expression>> {
        let module = self.load_module(module_name, virtual_fs, current_dir)?;
        module.variables.get(variable_name).cloned()
    }
}

/// Main evaluation function - takes static expressions (owned, 'static, no lifetimes).
pub fn evaluate(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
) -> Result<(Arc<Object>, ConstructContext), String> {
    let function_registry = StaticFunctionRegistry::default();
    let module_loader = ModuleLoader::default();
    let (obj, ctx, _, _, _, _) = evaluate_with_registry(
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
/// - `StaticFunctionRegistry`: Registry of function definitions
/// - `ModuleLoader`: Module loader for imports
/// - `Arc<ReferenceConnector>`: Connector for variable references (MUST be dropped when done!)
/// - `Arc<LinkConnector>`: Connector for LINK variables (MUST be dropped when done!)
///
/// IMPORTANT: The ReferenceConnector and LinkConnector MUST be dropped when the program
/// is finished (e.g., when switching examples) to allow actors to be cleaned up.
/// These connectors hold strong references to all top-level actors.
pub fn evaluate_with_registry(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
) -> Result<(Arc<Object>, ConstructContext, StaticFunctionRegistry, ModuleLoader, Arc<ReferenceConnector>, Arc<LinkConnector>), String> {
    let construct_context = ConstructContext {
        construct_storage: Arc::new(ConstructStorage::new(states_local_storage_key)),
        virtual_fs,
    };
    let actor_context = ActorContext::default();
    let reference_connector = Arc::new(ReferenceConnector::new());
    let link_connector = Arc::new(LinkConnector::new());

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
                // Store function definition in registry
                function_registry.functions.borrow_mut().insert(
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
                function_registry.clone(),
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
    Ok((root_object, construct_context, function_registry, module_loader, reference_connector, link_connector))
}

/// Evaluates a static variable into a Variable.
fn static_spanned_variable_into_variable(
    variable: static_expression::Spanned<static_expression::Variable>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    function_registry: StaticFunctionRegistry,
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

    let variable = if is_link {
        Variable::new_link_arc(construct_info, construct_context, name_string, actor_context, Some(persistence_id))
    } else {
        Variable::new_arc(
            construct_info,
            construct_context.clone(),
            name_string,
            static_spanned_expression_into_value_actor(
                value,
                construct_context,
                actor_context,
                reference_connector.clone(),
                link_connector.clone(),
                function_registry,
                module_loader,
                source_code,
            )?,
            Some(persistence_id),
        )
    };
    if is_referenced {
        reference_connector.register_referenceable(span, variable.value_actor());
    }
    // Register LINK variable senders with LinkConnector
    if is_link {
        if let Some(sender) = variable.link_value_sender() {
            link_connector.register_link(span, sender);
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

///
/// Note: User-defined function calls inside the expression will not work
/// (the function registry is empty). Built-in functions and operators work fine.
pub fn evaluate_static_expression(
    static_expr: &static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    static_spanned_expression_into_value_actor(
        static_expr.clone(),
        construct_context,
        actor_context,
        reference_connector,
        link_connector,
        StaticFunctionRegistry::default(),
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
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    // Create EvaluationContext from the parameters
    let ctx = EvaluationContext {
        construct_context,
        actor_context,
        reference_connector: Arc::downgrade(&reference_connector),
        link_connector: Arc::downgrade(&link_connector),
        function_registry,
        module_loader,
        source_code,
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
        ["List", "latest"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_latest(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "not_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_not_empty(arguments, id, persistence_id, construct_context, actor_context)
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


