use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use chumsky::Parser as ChumskyParser;
use chumsky::input::{Input as ChumskyInput, Stream as ChumskyStream};
use ulid::Ulid;
use zoon::futures_util::stream;
use zoon::futures_util::FutureExt;
use zoon::{Stream, StreamExt, println, eprintln};

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
#[derive(Clone)]
pub struct EvaluationContext {
    pub construct_context: ConstructContext,
    pub actor_context: ActorContext,
    pub reference_connector: Arc<ReferenceConnector>,
    pub link_connector: Arc<LinkConnector>,
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
            reference_connector,
            link_connector,
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn evaluate_expression_stacksafe(
    expression: static_expression::Spanned<static_expression::Expression>,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    let mut state = EvaluationState::new();
    let final_slot = state.alloc_slot();

    // Debug: log the expression type
    zoon::println!("[DEBUG] evaluate_expression_stacksafe: {:?}", std::mem::discriminant(&expression.node));

    // Schedule the root expression
    schedule_expression(&mut state, expression, ctx, final_slot)?;

    // Debug: log state after scheduling
    zoon::println!("[DEBUG] After scheduling: queue_len={}, results_len={}, next_slot={}",
        state.work_queue.len(), state.results.len(), state.next_slot);

    // Process work items until the queue is empty
    while let Some(item) = state.pop() {
        zoon::println!("[DEBUG] Processing work item: {}", item.debug_name());
        process_work_item(&mut state, item)?;
        zoon::println!("[DEBUG] After processing: results_len={}", state.results.len());
    }

    // Get the final result (top-level expression cannot be SKIP)
    zoon::println!("[DEBUG] Getting final result from slot {}", final_slot);
    state.get(final_slot).ok_or_else(|| "Top-level expression evaluated to SKIP".to_string())
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

                variable_data.push(ObjectVariableData {
                    name,
                    value_slot: var_slot,
                    is_link,
                    is_referenced,
                    span: var_span,
                    persistence: var_persistence.clone(),
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

                variable_data.push(ObjectVariableData {
                    name,
                    value_slot: var_slot,
                    is_link,
                    is_referenced,
                    span: var_span,
                    persistence: var_persistence.clone(),
                });

                // Collect for later scheduling (skip Link)
                if !is_link {
                    vars_to_schedule.push((var.node.value, var_slot));
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

            // Schedule variable expressions last (will be processed first due to LIFO)
            for (var_expr, var_slot) in vars_to_schedule {
                schedule_expression(state, var_expr, ctx.clone(), var_slot)?;
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

                variable_data.push(ObjectVariableData {
                    name,
                    value_slot: var_slot,
                    is_link,
                    is_referenced,
                    span: var_span,
                    persistence: var_persistence.clone(),
                });

                // Collect for later scheduling (skip Link)
                if !is_link {
                    vars_to_schedule.push((var.node.value, var_slot));
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

            // Schedule variable expressions last (will be processed first due to LIFO)
            for (var_expr, var_slot) in vars_to_schedule {
                schedule_expression(state, var_expr, ctx.clone(), var_slot)?;
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
                    let actor = build_list_binding_function(
                        &path_strs,
                        arguments,
                        span,
                        persistence,
                        persistence_id,
                        ctx,
                    )?;
                    state.store(result_slot, actor);
                }
                _ => {
                    // Normal function call - pre-evaluate all arguments
                    // First pass: collect argument data and allocate slots (don't schedule yet)
                    let mut arg_slots = Vec::new();
                    let mut args_to_schedule = Vec::new();
                    let mut passed_slot = None;
                    let mut passed_context: Option<SlotId> = None;

                    // Note: piped value is handled in call_function_stacksafe for BUILTIN functions only
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
        // PULSES (iteration)
        // ============================================================

        static_expression::Expression::Pulses { count } => {
            let actor = build_pulses_actor(
                *count,
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

            // Build variables and register referenced ones with the reference connector
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

                // Register with reference connector if this variable is referenced elsewhere
                if vd.is_referenced {
                    ctx.reference_connector.register_referenceable(vd.span, variable.value_actor());
                }

                // Register LINK variable senders with LinkConnector
                if vd.is_link {
                    if let Some(sender) = variable.link_value_sender() {
                        ctx.link_connector.register_link(vd.span, sender);
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

            // Build variables and register referenced ones with the reference connector
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

                // Register with reference connector if this variable is referenced elsewhere
                if vd.is_referenced {
                    ctx.reference_connector.register_referenceable(vd.span, variable.value_actor());
                }

                // Register LINK variable senders with LinkConnector
                if vd.is_link {
                    if let Some(sender) = variable.link_value_sender() {
                        ctx.link_connector.register_link(vd.span, sender);
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
            let Some(initial_actor) = state.get(initial_slot) else { return Ok(()); };
            let persistence_id = persistence.as_ref().map(|p| p.id).unwrap_or_default();
            let actor = build_hold_actor(initial_actor, state_param, *body, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
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
            let Some(prev_actor) = state.get(prev_slot) else { return Ok(()); };
            let new_ctx = ctx.with_piped(prev_actor);

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
            let actor = call_function_stacksafe(
                path,
                args,
                span,
                persistence,
                persistence_id,
                ctx,
                use_piped_for_builtin,
            )?;
            state.store(result_slot, actor);
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
                Box::pin(ctx.reference_connector.clone().referenceable(*ref_span))
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
    let should_materialize = backpressure_permit.is_some();

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

    let eval_body = move |value: Value| {
        let actor_context_clone = actor_context_for_then.clone();
        let construct_context_clone = construct_context_for_then.clone();
        let reference_connector_clone = reference_connector_for_then.clone();
        let link_connector_clone = link_connector_for_then.clone();
        let function_registry_clone = function_registry_for_then.clone();
        let module_loader_clone = module_loader_for_then.clone();
        let source_code_clone = source_code_for_then.clone();
        let persistence_clone = persistence_for_then.clone();
        let body_clone = body.clone();

        async move {
            // SKIP body means "no value for this input" - return None immediately
            // to avoid hanging on the subscription.next().await (SKIP never emits)
            if matches!(body_clone.node, static_expression::Expression::Skip) {
                return None;
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

            let new_actor_context = ActorContext {
                output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                piped: Some(value_actor.clone()),
                passed: actor_context_clone.passed.clone(),
                parameters: actor_context_clone.parameters.clone(),
                sequential_processing: actor_context_clone.sequential_processing,
                backpressure_permit: actor_context_clone.backpressure_permit.clone(),
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

            match evaluate_expression_stacksafe(body_expr, new_ctx) {
                Ok(result_actor) => {
                    println!("[DEBUG] THEN eval_body: body evaluated, result_actor version={}",
                        result_actor.version());
                    let mut subscription = result_actor.subscribe();
                    let _keep_alive = value_actor;
                    println!("[DEBUG] THEN eval_body: awaiting subscription.next()...");
                    if let Some(mut result_value) = subscription.next().await {
                        println!("[DEBUG] THEN eval_body: got result_value!");
                        result_value.set_idempotency_key(ValueIdempotencyKey::new());

                        if should_materialize {
                            result_value = materialize_value(
                                result_value,
                                construct_context_clone,
                                new_actor_context.clone(),
                            ).await;
                        }

                        println!("[DEBUG] THEN eval_body: returning Some(result_value)");
                        Some(result_value)
                    } else {
                        println!("[DEBUG] THEN eval_body: got None from subscription");
                        None
                    }
                }
                Err(e) => {
                    println!("[DEBUG] THEN eval_body: error: {e}");
                    None
                },
            }
        }
    };

    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
        let stream = piped.clone().subscribe()
            .then(eval_body)
            .filter_map(|opt| async { opt });
        Box::pin(stream)
    } else {
        let stream = piped.clone().subscribe().filter_map(eval_body);
        Box::pin(stream)
    };

    // Keep the piped actor alive by including it in inputs
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

    let eval_body = move |value: Value| {
        let actor_context_clone = actor_context_for_when.clone();
        let construct_context_clone = construct_context_for_when.clone();
        let reference_connector_clone = reference_connector_for_when.clone();
        let link_connector_clone = link_connector_for_when.clone();
        let function_registry_clone = function_registry_for_when.clone();
        let module_loader_clone = module_loader_for_when.clone();
        let source_code_clone = source_code_for_when.clone();
        let persistence_clone = persistence_for_when.clone();
        let arms_clone = arms.clone();

        async move {
            zoon::println!("[DEBUG] WHEN eval_body: starting");
            // Try to match against each arm
            for arm in &arms_clone {
                if let Some(bindings) = match_pattern_simple(&arm.pattern, &value) {
                    zoon::println!("[DEBUG] WHEN eval_body: pattern matched, num_bindings={}", bindings.len());
                    // SKIP body means "no value for this input" - return None immediately
                    // to avoid hanging on the subscription.next().await (SKIP never emits)
                    if matches!(arm.body, static_expression::Expression::Skip) {
                        zoon::println!("[DEBUG] WHEN eval_body: body is SKIP, returning None");
                        return None;
                    }

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

                    // Create parameter actors for the bindings
                    let mut parameters = actor_context_clone.parameters.clone();
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

                    zoon::println!("[DEBUG] WHEN eval_body: about to evaluate body expression, persistence_clone.is_some()={}", persistence_clone.is_some());
                    match evaluate_expression_stacksafe(body_expr, new_ctx) {
                        Ok(result_actor) => {
                            zoon::println!("[DEBUG] WHEN eval_body: body eval OK, subscribing to result");
                            let mut subscription = result_actor.subscribe();
                            let _keep_alive = value_actor;
                            zoon::println!("[DEBUG] WHEN eval_body: waiting for subscription.next()");
                            if let Some(mut result_value) = subscription.next().await {
                                zoon::println!("[DEBUG] WHEN eval_body: got result_value, returning Some");
                                result_value.set_idempotency_key(ValueIdempotencyKey::new());

                                if should_materialize {
                                    result_value = materialize_value(
                                        result_value,
                                        construct_context_clone,
                                        new_actor_context.clone(),
                                    ).await;
                                }

                                return Some(result_value);
                            } else {
                                zoon::println!("[DEBUG] WHEN eval_body: subscription.next() returned None!");
                            }
                        }
                        Err(e) => {
                            zoon::println!("[DEBUG] WHEN eval_body: body eval ERROR: {}", e);
                        }
                    }
                }
            }
            None
        }
    };

    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
        let stream = piped.clone().subscribe()
            .then(eval_body)
            .filter_map(|opt| async { opt });
        Box::pin(stream)
    } else {
        let stream = piped.clone().subscribe().filter_map(eval_body);
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

    let stream = piped.clone().subscribe().flat_map(move |value| {
        let actor_context_clone = actor_context_for_while.clone();
        let construct_context_clone = construct_context_for_while.clone();
        let reference_connector_clone = reference_connector_for_while.clone();
        let link_connector_clone = link_connector_for_while.clone();
        let function_registry_clone = function_registry_for_while.clone();
        let module_loader_clone = module_loader_for_while.clone();
        let source_code_clone = source_code_for_while.clone();
        let persistence_clone = persistence_for_while.clone();
        let arms_clone = arms.clone();

        // Find matching arm
        let matched_arm = arms_clone.iter().find(|arm| {
            match_pattern_simple(&arm.pattern, &value).is_some()
        });

        if let Some(arm) = matched_arm {
            // SKIP body means "no value for this input" - return empty stream immediately
            // to avoid subscribing to SKIP (which never emits)
            if matches!(arm.body, static_expression::Expression::Skip) {
                return Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>;
            }

            let bindings = match_pattern_simple(&arm.pattern, &value).unwrap_or_default();

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

            match evaluate_expression_stacksafe(body_expr, new_ctx) {
                Ok(result_actor) => {
                    let stream: Pin<Box<dyn Stream<Item = Value>>> = Box::pin(result_actor.subscribe());
                    stream
                }
                Err(_) => {
                    Box::pin(stream::empty())
                }
            }
        } else {
            Box::pin(stream::empty())
        }
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
) -> Result<Arc<ValueActor>, String> {
    println!("[DEBUG] build_hold_actor: state_param={state_param}, span={span:?}");
    // Use a channel to hold current state value and broadcast updates
    let (state_sender, state_receiver) = zoon::futures_channel::mpsc::unbounded::<Value>();
    let state_sender = Rc::new(RefCell::new(state_sender));
    let state_sender_for_update = state_sender.clone();

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

    println!("[DEBUG] build_hold_actor: initial_actor version={}",
        initial_actor.version());

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
    body_parameters.insert(state_param.clone(), state_actor);

    // Create backpressure permit for synchronizing THEN with state updates.
    // Initial count = 1 allows first body evaluation to start.
    // HOLD releases permit after each state update, allowing next body to run.
    let backpressure_permit = BackpressurePermit::new(1);
    let permit_for_state_update = backpressure_permit.clone();

    let body_actor_context = ActorContext {
        output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
        piped: None, // Clear piped - the body shouldn't re-use it
        passed: ctx.actor_context.passed.clone(),
        parameters: body_parameters,
        // Force sequential processing in HOLD body to ensure state consistency.
        // Without this, THEN/WHEN would spawn parallel body evaluations that all
        // read stale state (e.g., PULSES {3} |> THEN { counter + 1 } would read counter=0 three times).
        sequential_processing: true,
        // Pass permit to body - THEN will acquire before each evaluation
        backpressure_permit: Some(backpressure_permit),
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
    let body_result = evaluate_expression_stacksafe(body, body_ctx)?;

    // When body produces new values, update the state
    // Note: We avoid self-reactivity by not triggering body re-evaluation
    // from state changes. Body only evaluates when its event sources fire.
    let body_subscription = body_result.subscribe();
    println!("[DEBUG] HOLD: created body_subscription");
    let state_update_stream = body_subscription.map(move |new_value| {
        println!("[DEBUG] HOLD state_update_stream: received new_value from body!");
        // Update current state
        *current_state_for_update.borrow_mut() = Some(new_value.clone());
        // Send to state channel so body can see it on next event
        let _ = state_sender_for_update.borrow().unbounded_send(new_value.clone());
        // Release permit to allow THEN to process next input.
        // This guarantees state is updated before next body evaluation starts.
        println!("[DEBUG] HOLD state_update_stream: releasing permit");
        permit_for_state_update.release();
        println!("[DEBUG] HOLD state_update_stream: permit released");
        new_value
    });

    // When initial value emits, set up initial state
    // Note: We only update current_state here, NOT send to state_receiver.
    // The state_actor already gets the initial value via take(1) at line 2045.
    // Sending to state_receiver would cause state_actor to emit the initial value twice!
    let initial_stream = initial_actor.subscribe().map(move |initial| {
        // Set current state (for body to read synchronously if needed)
        *current_state_for_body.borrow_mut() = Some(initial.clone());
        // Do NOT send to state channel - take(1) already handles initial value
        initial
    });

    // Combine: input stream sets/resets state, body updates state
    // Use select to merge both streams - any emission from input resets state
    let combined_stream = stream::select(
        initial_stream, // Any emission from input resets the state
        state_update_stream
    );

    Ok(ValueActor::new_arc(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; HOLD {state_param} {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(combined_stream),
        Some(persistence_id),
    ))
}

/// Build a PULSES actor for iteration.
/// PULSES { count } emits count values (0 to count-1).
/// Can be used with THEN for iteration: `PULSES { 10 } |> THEN { ... }`
fn build_pulses_actor(
    count_expr: static_expression::Spanned<static_expression::Expression>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Arc<ValueActor>, String> {
    println!("[DEBUG] build_pulses_actor: span={span:?}, has_backpressure_permit={}", ctx.actor_context.backpressure_permit.is_some());
    // Evaluate the count expression
    let count_actor = evaluate_expression_stacksafe(count_expr, ctx.clone())?;

    // Debug: check if count_actor has an immediate value
    println!("[DEBUG] PULSES: count_actor version={}",
        count_actor.version());

    let construct_context_for_pulses = ctx.construct_context.clone();

    // Get backpressure permit from HOLD context if available.
    // When inside HOLD, PULSES will acquire permit before each emission,
    // ensuring consumer (THEN) processes each value before next is emitted.
    let backpressure_permit = ctx.actor_context.backpressure_permit.clone();

    // When count changes, emit that many pulses
    // Use stream::unfold instead of stream::iter to yield between emissions,
    // ensuring downstream subscribers have a chance to process each pulse
    println!("[DEBUG] PULSES: setting up stream");
    let pulses_stream = count_actor.clone().subscribe()
        .inspect(|v| {
            let info = match v {
                Value::Number(n, _) => format!("Number({})", n.number()),
                _ => "other".to_string(),
            };
            println!("[DEBUG] PULSES: count_actor emitted value: {}", info);
        })
        .flat_map(move |count_value| {
        let n = match &count_value {
            Value::Number(num, _) => num.number() as i64,
            _ => 0,
        };

        let construct_context_inner = construct_context_for_pulses.clone();
        let permit_for_iteration = backpressure_permit.clone();

        // Use unfold to emit pulses one at a time with async yield points.
        // Boon uses 1-based indexing, so PULSES { 5 } emits 1, 2, 3, 4, 5.
        stream::unfold(1i64, move |i| {
            let construct_context_for_iter = construct_context_inner.clone();
            let permit = permit_for_iteration.clone();
            async move {
                if i > n.max(0) {
                    println!("[DEBUG] PULSES: done emitting (i={i} > n={n})");
                    return None;
                }

                println!("[DEBUG] PULSES: about to emit i={i}, n={n}, has_permit={}", permit.is_some());

                // Yield/wait before emitting to allow downstream to process
                if let Some(ref permit) = permit {
                    println!("[DEBUG] PULSES: acquiring permit for i={i}...");
                    permit.acquire().await;
                    println!("[DEBUG] PULSES: acquired permit for i={i}");
                } else {
                    yield_once().await;
                }

                println!("[DEBUG] PULSES: emitting value {i}");
                let value = Value::Number(
                    Arc::new(Number::new(
                        ConstructInfo::new(
                            format!("PULSES iteration {i}"),
                            None,
                            format!("PULSES iteration {i}"),
                        ),
                        construct_context_for_iter,
                        i as f64,
                    )),
                    ValueMetadata {
                        idempotency_key: Ulid::new(),
                    },
                );
                Some((value, i + 1))
            }
        })
    });

    // Keep count_actor alive by passing it as an input dependency
    Ok(ValueActor::new_arc_with_inputs(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; PULSES {{..}}"),
        ),
        ctx.actor_context,
        TypedStream::infinite(pulses_stream),
        Some(persistence_id),
        vec![count_actor],
    ))
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
                    // Create a wrapper actor that resolves the reference asynchronously
                    let ref_connector = ctx.reference_connector.clone();
                    let ref_span_copy = *ref_span;
                    let value_stream = stream::once(ref_connector.referenceable(ref_span_copy))
                        .flat_map(|actor| actor.subscribe())
                        .boxed_local();
                    let ref_actor = Arc::new(ValueActor::new(
                        ConstructInfo::new(
                            format!("TextInterpolation:{}", var_name),
                            None,
                            format!("{span}; TextInterpolation for '{}'", var_name),
                        ).complete(ConstructType::ValueActor),
                        ctx.actor_context.clone(),
                        TypedStream::infinite(value_stream),
                        None,
                    ));
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
        let actor_context_for_combine = ctx.actor_context.clone();
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
        let combined_stream = merged.scan(
            vec![None; part_count],
            move |latest_values, (idx, value)| {
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

        // Create a value actor for the combined text
        // We'll use flat_map to create each combined text value
        let flattened = combined_stream.flat_map(move |combined_text| {
            let text_actor = Text::new_arc_value_actor(
                ConstructInfo::new(
                    format!("TextLiteral combined"),
                    None,
                    format!("{span_for_combine}; TextLiteral combined"),
                ),
                construct_context_for_combine.clone(),
                Ulid::new(),
                actor_context_for_combine.clone(),
                combined_text,
            );
            text_actor.subscribe()
        });

        Ok(ValueActor::new_arc(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; TextLiteral {{..}}"),
            ),
            ctx.actor_context,
            TypedStream::infinite(flattened),
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
) -> Result<Arc<ValueActor>, String> {
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
        evaluate_expression_stacksafe(list_value.clone(), ctx.clone())?
    } else if let Some(ref piped) = ctx.actor_context.piped {
        piped.clone()
    } else {
        return Err(format!("List/{} requires a list argument", path_strs[1]));
    };

    // Get transform/predicate expression from second argument (NOT evaluated)
    let transform_expr = arguments[1].node.value.clone()
        .ok_or_else(|| format!("List/{} requires a transform expression", path_strs[1]))?;

    let config = ListBindingConfig {
        binding_name,
        transform_expr,
        operation,
        reference_connector: ctx.reference_connector.clone(),
        link_connector: ctx.link_connector.clone(),
        source_code: ctx.source_code.clone(),
    };

    Ok(ListBindingFunction::new_arc_value_actor(
        ConstructInfo::new(
            format!("PersistenceId: {persistence_id}"),
            persistence,
            format!("{span}; List/{}(..)", path_strs[1]),
        ),
        ctx.construct_context,
        ctx.actor_context,
        list_actor,
        config,
    ))
}

/// Call a function with stack-safe evaluation.
/// Supports both user-defined functions and builtin functions.
fn call_function_stacksafe(
    path: Vec<String>,
    args: Vec<(String, Arc<ValueActor>)>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    use_piped_for_builtin: bool,
) -> Result<Arc<ValueActor>, String> {
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

        // Bind piped value to unbound function parameters.
        // For `position |> fibonacci()`, bind `position` to the first parameter of `fibonacci`.
        if let Some(piped) = &ctx.actor_context.piped {
            for param_name in &func_def.parameters {
                if !parameters.contains_key(param_name) {
                    parameters.insert(param_name.clone(), piped.clone());
                    break; // Only bind to the first unbound parameter
                }
            }
        }

        let new_actor_context = ActorContext {
            output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
            piped: ctx.actor_context.piped.clone(),
            passed: ctx.actor_context.passed.clone(),
            parameters,
            sequential_processing: ctx.actor_context.sequential_processing,
            backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
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

        return evaluate_expression_stacksafe(func_def.body, new_ctx);
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
            };

            Ok(FunctionCall::new_arc_value_actor(
                construct_info,
                ctx.construct_context,
                call_actor_context,
                definition,
                builtin_args,
            ))
        }
        Err(_) => {
            Err(format!("Function '{}' not found", full_path))
        }
    }
}

/// Pattern matching helper for stack-safe evaluator - returns bindings if pattern matches.
fn match_pattern_simple(
    pattern: &static_expression::Pattern,
    value: &Value,
) -> Option<HashMap<String, Value>> {
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
                    // Match variables against the tagged object's fields
                    // For now simplified - we'd need async handling for full implementation
                    let _ = variables;
                    Some(bindings)
                } else {
                    None
                }
            } else {
                None
            }
        }

        static_expression::Pattern::Object { variables } => {
            if let Value::Object(obj, _) = value {
                // Match variables against the object's fields
                // For now simplified - we'd need async handling for full implementation
                let _ = (variables, obj);
                Some(bindings)
            } else if let Value::TaggedObject(to, _) = value {
                let _ = (variables, to);
                Some(bindings)
            } else {
                None
            }
        }

        static_expression::Pattern::List { items } => {
            // List pattern matching - simplified for now
            let _ = items;
            Some(bindings)
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
    let (obj, ctx, _, _) = evaluate_with_registry(
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
pub fn evaluate_with_registry(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
) -> Result<(Arc<Object>, ConstructContext, StaticFunctionRegistry, ModuleLoader), String> {
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
    Ok((root_object, construct_context, function_registry, module_loader))
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
        reference_connector,
        link_connector,
        function_registry,
        module_loader,
        source_code,
    };

    // Delegate to the stack-safe evaluator
    evaluate_expression_stacksafe(expression, ctx)
}

/// OLD RECURSIVE IMPLEMENTATION - kept for reference during migration
/// This function has been replaced by evaluate_expression_stacksafe above.
#[allow(dead_code)]
fn static_spanned_expression_into_value_actor_OLD(
    expression: static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    let static_expression::Spanned {
        span,
        node: expression,
        persistence,
    } = expression;

    let persistence_info = persistence.clone().ok_or("Failed to get Persistence")?;
    let persistence_id = persistence_info.id;
    let idempotency_key = persistence_id;

    // NOTE: Actor reuse is disabled because it creates broken subscription graphs.
    // Reused actors keep OLD subscriptions to OLD actors, which fail when other
    // parts of the graph are recreated. The proper solution is STATE persistence
    // (saving/restoring values to localStorage), not actor instance reuse.
    //
    // TODO: Implement proper state persistence for stateful constructs like:
    // - LATEST with state (the accumulated value)
    // - Math/sum (the running total)
    // - User-defined stateful functions
    //
    // if persistence_info.status == PersistenceStatus::Unchanged {
    //     if let Some(existing_actor) = construct_context.previous_actors.get_actor(persistence_id) {
    //         return Ok(existing_actor);
    //     }
    // }

    let actor = match expression {
        static_expression::Expression::Variable(_) => {
            return Err("Failed to evaluate the variable in this context.".to_string());
        }
        static_expression::Expression::Literal(literal) => match literal {
            static_expression::Literal::Number(number) => Number::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Number {number}"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
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
                    construct_context,
                    idempotency_key,
                    actor_context,
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
                    construct_context,
                    idempotency_key,
                    actor_context,
                    text,
                )
            }
        },
        static_expression::Expression::List { items } => {
            let evaluated_items: Result<Vec<_>, _> = items
                .into_iter()
                .map(|item| {
                    static_spanned_expression_into_value_actor(
                        item,
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
            List::new_arc_value_actor_with_persistence(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; LIST {{..}}"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                evaluated_items?,
            )
        }
        static_expression::Expression::Alias(alias) => {
            type BoxedFuture = Pin<Box<dyn std::future::Future<Output = Arc<ValueActor>>>>;

            let root_value_actor: BoxedFuture = match &alias {
                static_expression::Alias::WithPassed { extra_parts: _ } => {
                    match &actor_context.passed {
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
                    if let Some(param_actor) = actor_context.parameters.get(&first_part) {
                        let param_actor = param_actor.clone();
                        Box::pin(async move { param_actor })
                    } else if let Some(ref_span) = referenced_span {
                        Box::pin(reference_connector.referenceable(*ref_span))
                    } else if parts.len() >= 2 {
                        // Try module variable access: e.g., Assets/icon.checkbox_active
                        // parts[0] = module name (Assets), parts[1] = variable name (icon)
                        let module_name = &parts[0];
                        let var_name = parts[1].to_string();

                        if let Some(module_data) = module_loader.load_module(module_name, &construct_context.virtual_fs, None) {
                            if let Some(var_expr) = module_data.variables.get(&var_name).cloned() {
                                println!("[ModuleLoader] Found variable '{}' in module '{}'", var_name, module_name);

                                // Evaluate the module's variable expression
                                let var_actor = static_spanned_expression_into_value_actor(
                                    var_expr,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                    reference_connector.clone(),
                                    link_connector.clone(),
                                    function_registry.clone(),
                                    module_loader.clone(),
                                    source_code.clone(),
                                )?;
                                Box::pin(async move { var_actor })
                            } else {
                                return Err(format!("Variable '{}' not found in module '{}'", var_name, module_name));
                            }
                        } else {
                            return Err(format!("Module '{}' not found for variable access", module_name));
                        }
                    } else {
                        return Err(format!("Failed to get aliased variable '{}'", first_part));
                    }
                }
            };

            VariableOrArgumentReference::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; (alias)"),
                ),
                construct_context,
                actor_context,
                alias,
                root_value_actor,
            )
        }
        static_expression::Expression::ArithmeticOperator(op) => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; ArithmeticOperator"),
            );
            match op {
                static_expression::ArithmeticOperator::Add { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_add(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Subtract { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_subtract(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Multiply { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_multiply(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Divide { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_divide(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Negate { operand } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    let neg_one = Number::new_arc_value_actor(
                        ConstructInfo::new("neg_one", None, "-1 constant"),
                        construct_context.clone(),
                        idempotency_key,
                        actor_context.clone(),
                        -1.0,
                    );
                    ArithmeticCombinator::new_multiply(
                        construct_info,
                        construct_context,
                        actor_context,
                        neg_one,
                        a,
                    )
                }
            }
        }
        static_expression::Expression::Comparator(cmp) => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; Comparator"),
            );
            match cmp {
                static_expression::Comparator::Equal { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::NotEqual { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_not_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::Greater { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_greater(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::GreaterOrEqual { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_greater_or_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::Less { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_less(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::LessOrEqual { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_less_or_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
            }
        }
        static_expression::Expression::FunctionCall { path, arguments } => {
            // Handle built-in function calls
            let path_strs: Vec<&str> = path.iter().map(|s| &**s).collect();

            // Special handling for List binding functions (map, retain, every, any, sort_by)
            // These need the unevaluated expression to evaluate per-item with bindings
            match path_strs.as_slice() {
                ["List", "map"] | ["List", "retain"] | ["List", "every"] | ["List", "any"] | ["List", "sort_by"] => {
                    let operation = match path_strs[1] {
                        "map" => ListBindingOperation::Map,
                        "retain" => ListBindingOperation::Retain,
                        "every" => ListBindingOperation::Every,
                        "any" => ListBindingOperation::Any,
                        "sort_by" => ListBindingOperation::SortBy,
                        _ => unreachable!(),
                    };

                    // For List binding functions:
                    // - First arg: binding name (e.g., "old", "item"), value is the list (passed)
                    // - Second arg: transform/predicate expression (e.g., "new: expr", "if: expr")
                    if arguments.len() < 2 {
                        return Err(format!("List/{} requires 2 arguments", path_strs[1]));
                    }

                    // Get binding name from first argument
                    let binding_name = arguments[0].node.name.clone();

                    // Get the list - either from first argument's value or from PASSED
                    let list_actor = if let Some(ref list_value) = arguments[0].node.value {
                        static_spanned_expression_into_value_actor(
                            list_value.clone(),
                            construct_context.clone(),
                            actor_context.clone(),
                            reference_connector.clone(),
                            link_connector.clone(),
                            function_registry.clone(),
                            module_loader.clone(),
                            source_code.clone(),
                        )?
                    } else if let Some(ref piped) = actor_context.piped {
                        piped.clone()
                    } else {
                        return Err(format!("List/{} requires a list argument", path_strs[1]));
                    };

                    // Get transform/predicate expression from second argument (NOT evaluated)
                    let transform_expr = arguments[1].node.value.clone()
                        .ok_or_else(|| format!("List/{} requires a transform expression", path_strs[1]))?;

                    let config = ListBindingConfig {
                        binding_name,
                        transform_expr,
                        operation,
                        reference_connector: reference_connector.clone(),
                        link_connector: link_connector.clone(),
                        source_code: source_code.clone(),
                    };

                    ListBindingFunction::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; List/{}(..)", path_strs[1]),
                        ),
                        construct_context,
                        actor_context,
                        list_actor,
                        config,
                    )
                }
                _ => {
                    // Check for user-defined function first (single-element path like "new_todo")
                    if path.len() == 1 {
                        let func_name = path[0].as_str();
                        let maybe_user_func = function_registry.functions.borrow().get(func_name).cloned();

                        if let Some(user_func) = maybe_user_func {
                            // User-defined function call
                            // Evaluate arguments and bind to parameters
                            let mut param_bindings: HashMap<String, Arc<ValueActor>> = HashMap::new();
                            let mut passed_context: Option<Arc<ValueActor>> = actor_context.passed.clone();

                            // If there's a piped value, bind it to the first parameter
                            if let Some(piped) = &actor_context.piped {
                                if let Some(first_param) = user_func.parameters.first() {
                                    param_bindings.insert(first_param.clone(), piped.clone());
                                }
                            }

                            // Process named arguments
                            for arg in &arguments {
                                // Check for PASS: argument
                                if arg.node.name.as_str() == "PASS" {
                                    if let Some(value) = &arg.node.value {
                                        let pass_actor = static_spanned_expression_into_value_actor(
                                            value.clone(),
                                            construct_context.clone(),
                                            actor_context.clone(),
                                            reference_connector.clone(),
                                            link_connector.clone(),
                                            function_registry.clone(),
                                            module_loader.clone(),
                                            source_code.clone(),
                                        )?;
                                        passed_context = Some(pass_actor);
                                    }
                                    continue;
                                }

                                // Bind named argument to parameter
                                let param_name = arg.node.name.to_string();
                                if let Some(value) = &arg.node.value {
                                    let actor = static_spanned_expression_into_value_actor(
                                        value.clone(),
                                        construct_context.clone(),
                                        actor_context.clone(),
                                        reference_connector.clone(),
                                        link_connector.clone(),
                                        function_registry.clone(),
                                        module_loader.clone(),
                                        source_code.clone(),
                                    )?;
                                    param_bindings.insert(param_name, actor);
                                }
                            }

                            // Create actor context with parameter bindings for the function body
                            let func_actor_context = ActorContext {
                                output_valve_signal: actor_context.output_valve_signal.clone(),
                                piped: None, // Clear piped - it was bound to first param
                                passed: passed_context,
                                parameters: param_bindings,
                                sequential_processing: actor_context.sequential_processing,
                                backpressure_permit: actor_context.backpressure_permit.clone(),
                            };

                            // Evaluate the function body with the new context
                            return static_spanned_expression_into_value_actor(
                                user_func.body,
                                construct_context,
                                func_actor_context,
                                reference_connector,
                                link_connector,
                                function_registry,
                                module_loader,
                                source_code,
                            );
                        }
                    }

                    // Check for module function call (path.len() >= 2, e.g., Theme/material)
                    // Built-in modules: Math, Text, List, Bool, Logic, Storage, Time, Object, Browser, Ui, Css, Selector
                    let builtin_modules = ["Math", "Text", "List", "Bool", "Logic", "Storage", "Time", "Object", "Browser", "Ui", "Css", "Selector", "Color", "Spring", "Page", "Attr", "Router", "Ulid", "Document", "Element", "Timer", "Log", "Build", "Scene", "Theme", "File", "Directory"];
                    if path.len() >= 2 && !builtin_modules.contains(&path[0].as_str()) {
                        let module_name = &path[0];
                        let func_name = &path[1];

                        // Try to load module and get the function
                        if let Some(module_data) = module_loader.load_module(module_name, &construct_context.virtual_fs, None) {
                            if let Some(user_func) = module_data.functions.get(func_name.as_str()) {
                                println!("[ModuleLoader] Found function '{}' in module '{}'", func_name, module_name);

                                // User-defined function from module - evaluate arguments and bind to parameters
                                let mut param_bindings: HashMap<String, Arc<ValueActor>> = HashMap::new();
                                let mut passed_context: Option<Arc<ValueActor>> = actor_context.passed.clone();

                                // If there's a piped value, bind it to the first parameter
                                if let Some(piped) = &actor_context.piped {
                                    if let Some(first_param) = user_func.parameters.first() {
                                        param_bindings.insert(first_param.clone(), piped.clone());
                                    }
                                }

                                // Process named arguments
                                for arg in &arguments {
                                    // Check for PASS: argument
                                    if arg.node.name.as_str() == "PASS" {
                                        if let Some(value) = &arg.node.value {
                                            let pass_actor = static_spanned_expression_into_value_actor(
                                                value.clone(),
                                                construct_context.clone(),
                                                actor_context.clone(),
                                                reference_connector.clone(),
                                                link_connector.clone(),
                                                function_registry.clone(),
                                                module_loader.clone(),
                                                source_code.clone(),
                                            )?;
                                            passed_context = Some(pass_actor);
                                        }
                                        continue;
                                    }

                                    // Bind named argument to parameter
                                    let param_name = arg.node.name.to_string();
                                    if let Some(value) = &arg.node.value {
                                        let actor = static_spanned_expression_into_value_actor(
                                            value.clone(),
                                            construct_context.clone(),
                                            actor_context.clone(),
                                            reference_connector.clone(),
                                            link_connector.clone(),
                                            function_registry.clone(),
                                            module_loader.clone(),
                                            source_code.clone(),
                                        )?;
                                        param_bindings.insert(param_name, actor);
                                    }
                                }

                                // Create actor context with parameter bindings for the function body
                                let func_actor_context = ActorContext {
                                    output_valve_signal: actor_context.output_valve_signal.clone(),
                                    piped: None, // Clear piped - it was bound to first param
                                    passed: passed_context,
                                    parameters: param_bindings,
                                    sequential_processing: actor_context.sequential_processing,
                                    backpressure_permit: actor_context.backpressure_permit.clone(),
                                };

                                // Evaluate the function body with the new context
                                return static_spanned_expression_into_value_actor(
                                    user_func.body.clone(),
                                    construct_context,
                                    func_actor_context,
                                    reference_connector,
                                    link_connector,
                                    function_registry,
                                    module_loader,
                                    source_code,
                                );
                            }
                        }
                    }

                    // Built-in function call - evaluate all arguments
                    let mut evaluated_args: Vec<Arc<ValueActor>> = Vec::new();
                    let mut passed_context: Option<Arc<ValueActor>> = actor_context.passed.clone();

                    // If there's a piped value, add it as the first argument
                    if let Some(piped) = &actor_context.piped {
                        evaluated_args.push(piped.clone());
                    }

                    for arg in &arguments {
                        // Check for PASS: argument - sets implicit context for nested calls
                        if arg.node.name.as_str() == "PASS" {
                            if let Some(value) = &arg.node.value {
                                let pass_actor = static_spanned_expression_into_value_actor(
                                    value.clone(),
                                    construct_context.clone(),
                                    actor_context.clone(),
                                    reference_connector.clone(),
                                    link_connector.clone(),
                                    function_registry.clone(),
                                    module_loader.clone(),
                                    source_code.clone(),
                                )?;
                                passed_context = Some(pass_actor);
                            }
                            continue; // Don't add PASS to positional arguments
                        }

                        if let Some(value) = &arg.node.value {
                            let actor = static_spanned_expression_into_value_actor(
                                value.clone(),
                                construct_context.clone(),
                                actor_context.clone(),
                                reference_connector.clone(),
                                link_connector.clone(),
                                function_registry.clone(),
                                module_loader.clone(),
                                source_code.clone(),
                            )?;
                            evaluated_args.push(actor);
                        }
                    }

                    // Create actor context with PASS context for the function call
                    let call_actor_context = ActorContext {
                        output_valve_signal: actor_context.output_valve_signal.clone(),
                        piped: None, // Clear piped - it was already added as first arg
                        passed: passed_context,
                        parameters: actor_context.parameters.clone(),
                        sequential_processing: actor_context.sequential_processing,
                        backpressure_permit: actor_context.backpressure_permit.clone(),
                    };

                    // Get function definition
                    let borrowed_path: Vec<&str> = path.iter().map(|s| &**s).collect();
                    let definition = static_function_call_path_to_definition(&borrowed_path, span)?;

                    FunctionCall::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; {}(..)", path_strs.join("/")),
                        ),
                        construct_context,
                        call_actor_context,
                        definition,
                        evaluated_args,
                    )
                }
            }
        }
        static_expression::Expression::Skip => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; SKIP"),
            );
            ValueActor::new_arc(
                construct_info,
                actor_context,
                TypedStream::infinite(stream::empty()),
                Some(persistence_id),
            )
        }
        // Object expressions - [key: value, ...]
        static_expression::Expression::Object(object) => {
            let evaluated_variables: Result<Vec<Arc<Variable>>, String> = object.variables
                .into_iter()
                .map(|var| {
                    let var_name = var.node.name.to_string();
                    let var_span = var.span.clone();
                    let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);

                    let variable = if is_link {
                        Variable::new_link_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; Object variable {var_name} (LINK)"),
                            ),
                            construct_context.clone(),
                            var_name,
                            actor_context.clone(),
                            None,
                        )
                    } else {
                        let value_actor = static_spanned_expression_into_value_actor(
                            var.node.value,
                            construct_context.clone(),
                            actor_context.clone(),
                            reference_connector.clone(),
                            link_connector.clone(),
                            function_registry.clone(),
                            module_loader.clone(),
                            source_code.clone(),
                        )?;
                        Variable::new_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; Object variable {var_name}"),
                            ),
                            construct_context.clone(),
                            var_name,
                            value_actor,
                            None,
                        )
                    };

                    // Register LINK variable senders with LinkConnector
                    if is_link {
                        if let Some(sender) = variable.link_value_sender() {
                            link_connector.register_link(var_span, sender);
                        }
                    }

                    Ok(variable)
                })
                .collect();
            Object::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Object [..]"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                evaluated_variables?,
            )
        }
        // TaggedObject expressions - Tag[key: value, ...]
        static_expression::Expression::TaggedObject { tag, object } => {
            let tag_string = tag.to_string();
            let evaluated_variables: Result<Vec<Arc<Variable>>, String> = object.variables
                .into_iter()
                .map(|var| {
                    let var_name = var.node.name.to_string();
                    let var_span = var.span.clone();
                    let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);

                    let variable = if is_link {
                        Variable::new_link_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; TaggedObject {tag_string} variable {var_name} (LINK)"),
                            ),
                            construct_context.clone(),
                            var_name,
                            actor_context.clone(),
                            None,
                        )
                    } else {
                        let value_actor = static_spanned_expression_into_value_actor(
                            var.node.value,
                            construct_context.clone(),
                            actor_context.clone(),
                            reference_connector.clone(),
                            link_connector.clone(),
                            function_registry.clone(),
                            module_loader.clone(),
                            source_code.clone(),
                        )?;
                        Variable::new_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; TaggedObject {tag_string} variable {var_name}"),
                            ),
                            construct_context.clone(),
                            var_name,
                            value_actor,
                            None,
                        )
                    };

                    // Register LINK variable senders with LinkConnector
                    if is_link {
                        if let Some(sender) = variable.link_value_sender() {
                            link_connector.register_link(var_span, sender);
                        }
                    }

                    Ok(variable)
                })
                .collect();
            TaggedObject::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; {tag_string}[..]"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                tag_string,
                evaluated_variables?,
            )
        }
        static_expression::Expression::Map { .. } => {
            return Err("Map expressions not yet supported in static context".to_string());
        }
        static_expression::Expression::Function { .. } => {
            return Err("Function definitions not supported in static context".to_string());
        }
        static_expression::Expression::LinkSetter { alias } => {
            // LinkSetter: `|> LINK { alias }` - sends piped value to the LINK variable
            // Get the referenced_span from the alias to look up the link sender
            let referenced_span = match &alias.node {
                static_expression::Alias::WithoutPassed { referenced_span, .. } => {
                    referenced_span.ok_or("LinkSetter alias has no referenced_span")?
                }
                static_expression::Alias::WithPassed { .. } => {
                    return Err("LinkSetter does not support PASSED alias".to_string());
                }
            };

            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let link_connector_for_setter = link_connector.clone();

                    // Subscribe to the piped stream and send each value to the link sender
                    let sending_stream = piped.clone().subscribe().then(move |value| {
                        let link_connector_clone = link_connector_for_setter.clone();
                        async move {
                            // Get the link sender from the connector
                            let sender = link_connector_clone.link_sender(referenced_span).await;
                            // Send the value to the LINK variable
                            if sender.unbounded_send(value.clone()).is_err() {
                                eprintln!("Failed to send value to LINK variable");
                            }
                            value
                        }
                    });

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; LinkSetter"),
                        ),
                        actor_context,
                        TypedStream::infinite(sending_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("LinkSetter requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::Link => {
            return Err("Link not yet supported in static context".to_string());
        }
        static_expression::Expression::Latest { inputs } => {
            // LATEST merges multiple streams and emits whenever any stream produces a value
            // Returns the most recent value from any of the input streams
            let evaluated_inputs: Result<Vec<Arc<ValueActor>>, String> = inputs
                .into_iter()
                .map(|input| {
                    static_spanned_expression_into_value_actor(
                        input,
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
            let inputs = evaluated_inputs?;

            // Merge all input streams using select_all
            // subscribe() returns a Subscription that keeps the actor alive
            let merged_stream = stream::select_all(
                inputs.iter().map(|input| input.clone().subscribe())
            );

            ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; LATEST {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(merged_stream),
                Some(persistence_id),
            )
        }
        static_expression::Expression::Then { body } => {
            // THEN transforms the piped value using the body expression
            // It evaluates the body with piped set to each incoming value
            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let construct_context_for_then = construct_context.clone();
                    let actor_context_for_then = actor_context.clone();
                    let reference_connector_for_then = reference_connector.clone();
                    let link_connector_for_then = link_connector.clone();
                    let function_registry_for_then = function_registry.clone();
                    let module_loader_for_then = module_loader.clone();
                    let source_code_for_then = source_code.clone();
                    let persistence_for_then = persistence.clone();
                    let span_for_then = span;

                    // Subscribe to the piped stream and for each value, evaluate the body
                    let body_for_closure = body;

                    // Check if we need sequential processing (inside HOLD).
                    // When sequential_processing is true, we use .then() which processes
                    // one value at a time, waiting for the body to complete before processing next.
                    // This prevents race conditions where parallel body evaluations all read stale state.
                    let sequential = actor_context_for_then.sequential_processing;

                    // Extract backpressure permit BEFORE creating eval_body closure
                    // (closure moves actor_context_for_then, so we must extract first)
                    let backpressure_permit = actor_context_for_then.backpressure_permit.clone();

                    // When inside HOLD (has backpressure), we need to materialize Object values
                    // to prevent circular dependencies from lazy ValueActors referencing state
                    let should_materialize = backpressure_permit.is_some();

                    // Helper closure for evaluating a single input value
                    // Returns Option<Value> - the body result (or None on error)
                    let eval_body = move |value: Value| {
                        let actor_context_clone = actor_context_for_then.clone();
                        let construct_context_clone = construct_context_for_then.clone();
                        let reference_connector_clone = reference_connector_for_then.clone();
                        let link_connector_clone = link_connector_for_then.clone();
                        let function_registry_clone = function_registry_for_then.clone();
                        let module_loader_clone = module_loader_for_then.clone();
                        let source_code_clone = source_code_for_then.clone();
                        let persistence_clone = persistence_for_then.clone();
                        let body_clone = body_for_closure.clone();

                        async move {
                            // Create a new value actor for this specific value
                            // Use constant() to keep the actor alive (emits once then stays pending)
                            // rather than stream::once() which ends after emitting.
                            let value_actor = ValueActor::new_arc(
                                ConstructInfo::new(
                                    format!("THEN input value"),
                                    None,
                                    format!("{span_for_then}; THEN input"),
                                ),
                                actor_context_clone.clone(),
                                constant(value),
                                None,
                            );

                            // Evaluate the body with PASSED set to this value
                            // Clone value_actor - we need one for the actor context and one to keep alive
                            let new_actor_context = ActorContext {
                                output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                                piped: Some(value_actor.clone()),
                                passed: actor_context_clone.passed.clone(),
                                parameters: actor_context_clone.parameters.clone(),
                                // Propagate sequential_processing to nested THEN/WHEN inside body
                                sequential_processing: actor_context_clone.sequential_processing,
                                // Propagate backpressure_permit to nested constructs
                                backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                            };

                            let body_expr = static_expression::Spanned {
                                span: body_clone.span,
                                persistence: persistence_clone,
                                node: body_clone.node.clone(),
                            };

                            // Clone construct_context for potential materialization use
                            let construct_context_for_materialize = construct_context_clone.clone();

                            match static_spanned_expression_into_value_actor(
                                body_expr,
                                construct_context_clone,
                                new_actor_context.clone(),
                                reference_connector_clone,
                                link_connector_clone,
                                function_registry_clone,
                                module_loader_clone,
                                source_code_clone,
                            ) {
                                Ok(result_actor) => {
                                    // Subscribe and get first value from body
                                    // Keep value_actor alive while we wait for the result
                                    let mut subscription = result_actor.subscribe();
                                    let _keep_alive = value_actor;
                                    if let Some(mut result_value) = subscription.next().await {
                                        // THEN SEMANTICS: Each input pulse produces a conceptually "new" value,
                                        // even if the body evaluates to the same content (e.g., constant `1`).
                                        // We assign fresh idempotency keys so downstream consumers (like Math/sum)
                                        // treat each pulse's output as unique rather than skipping "duplicates".
                                        result_value.set_idempotency_key(ValueIdempotencyKey::new());

                                        // When inside HOLD, materialize Object values to break circular
                                        // dependencies from lazy ValueActors that reference state
                                        if should_materialize {
                                            result_value = materialize_value(
                                                result_value,
                                                construct_context_for_materialize,
                                                new_actor_context.clone(),
                                            ).await;
                                        }

                                        Some(result_value)
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None,
                            }
                        }
                    };

                    // Create the flattened stream using either sequential or parallel processing
                    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
                        // Backpressure or sequential mode: process one at a time.
                        // When backpressure permit exists (inside HOLD), PULSES has already
                        // acquired the permit before emitting each value. THEN just processes
                        // sequentially, and HOLD releases permit after state update.
                        // This guarantees state is updated before next pulse arrives.
                        let stream = piped.clone().subscribe()
                            .then(eval_body)
                            .filter_map(|opt| async { opt });
                        Box::pin(stream)
                    } else {
                        // Parallel mode (default): use filter_map which spawns concurrent tasks.
                        // Each input value's body evaluation runs in parallel, which is faster
                        // but can cause race conditions when reading shared state (like HOLD).
                        //
                        // The original two-step approach (filter_map + flat_map) is kept for
                        // compatibility, even though eval_body now does both steps.
                        let stream = piped.clone().subscribe().filter_map(eval_body);
                        Box::pin(stream)
                    };

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; THEN {{..}}"),
                        ),
                        actor_context,
                        TypedStream::infinite(flattened_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("THEN requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::When { arms } => {
            // WHEN performs pattern matching on the piped value
            // It tries each arm in order and evaluates the first matching arm's body
            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let construct_context_for_when = construct_context.clone();
                    let actor_context_for_when = actor_context.clone();
                    let reference_connector_for_when = reference_connector.clone();
                    let link_connector_for_when = link_connector.clone();
                    let function_registry_for_when = function_registry.clone();
                    let module_loader_for_when = module_loader.clone();
                    let source_code_for_when = source_code.clone();
                    let persistence_for_when = persistence.clone();
                    let span_for_when = span;
                    let arms_for_closure = arms.clone();

                    // Extract these BEFORE creating eval_body closure
                    // (closure moves actor_context_for_when, so we must extract first)
                    let sequential = actor_context_for_when.sequential_processing;
                    let backpressure_permit = actor_context_for_when.backpressure_permit.clone();

                    // Helper closure for evaluating a single input value
                    // Returns Option<Value> - the body result (or None on error or no match)
                    let eval_body = move |value: Value| {
                        let actor_context_clone = actor_context_for_when.clone();
                        let construct_context_clone = construct_context_for_when.clone();
                        let reference_connector_clone = reference_connector_for_when.clone();
                        let link_connector_clone = link_connector_for_when.clone();
                        let function_registry_clone = function_registry_for_when.clone();
                        let module_loader_clone = module_loader_for_when.clone();
                        let source_code_clone = source_code_for_when.clone();
                        let persistence_clone = persistence_for_when.clone();
                        let arms_clone = arms_for_closure.clone();

                        async move {
                            // Try each arm in order
                            for arm in &arms_clone {
                                if let Some(bindings) = match_pattern(
                                    &arm.pattern,
                                    &value,
                                    &construct_context_clone,
                                    &actor_context_clone,
                                ) {
                                    // Pattern matched! Evaluate the body with bindings
                                    let mut params = actor_context_clone.parameters.clone();
                                    for (name, actor) in bindings {
                                        params.insert(name, actor);
                                    }

                                    let new_actor_context = ActorContext {
                                        output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                                        piped: actor_context_clone.piped.clone(),
                                        passed: actor_context_clone.passed.clone(),
                                        parameters: params,
                                        // Propagate sequential_processing to nested THEN/WHEN inside body
                                        sequential_processing: actor_context_clone.sequential_processing,
                                        backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                                    };

                                    // Create a spanned expression from the body
                                    let body_expr = static_expression::Spanned {
                                        span: span_for_when,
                                        persistence: persistence_clone,
                                        node: arm.body.clone(),
                                    };

                                    match static_spanned_expression_into_value_actor(
                                        body_expr,
                                        construct_context_clone,
                                        new_actor_context,
                                        reference_connector_clone,
                                        link_connector_clone,
                                        function_registry_clone,
                                        module_loader_clone,
                                        source_code_clone,
                                    ) {
                                        Ok(result_actor) => {
                                            // Get first value from body before returning
                                            let mut subscription = result_actor.subscribe();
                                            if let Some(mut result_value) = subscription.next().await {
                                                // WHEN SEMANTICS: Like THEN, each input pulse produces a "new" value.
                                                result_value.set_idempotency_key(ValueIdempotencyKey::new());
                                                return Some(result_value);
                                            }
                                            return None;
                                        }
                                        Err(_) => return None,
                                    }
                                }
                            }
                            // No arm matched
                            None
                        }
                    };

                    // Create the flattened stream using either sequential or parallel processing
                    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if let Some(permit) = backpressure_permit {
                        // Backpressure mode: HOLD controls pacing via permit.
                        // WHEN acquires permit before each body evaluation.
                        // HOLD releases permit after updating state.
                        // This guarantees state is updated before next body starts.
                        let stream = piped.clone().subscribe()
                            .then(move |value| {
                                let permit = permit.clone();
                                let eval = eval_body.clone();
                                async move {
                                    // Wait for permit - HOLD releases after state update
                                    permit.acquire().await;
                                    eval(value).await
                                }
                            })
                            .filter_map(|opt| async { opt });
                        Box::pin(stream)
                    } else if sequential {
                        // Sequential mode without backpressure (fallback).
                        // Uses .then() to process one at a time, but no synchronization with HOLD.
                        let stream = piped.clone().subscribe().then(eval_body).filter_map(|opt| async { opt });
                        Box::pin(stream)
                    } else {
                        // Parallel mode (default): use filter_map which spawns concurrent tasks.
                        let stream = piped.clone().subscribe().filter_map(eval_body);
                        Box::pin(stream)
                    };

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; WHEN {{..}}"),
                        ),
                        actor_context,
                        TypedStream::infinite(flattened_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("WHEN requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::While { arms } => {
            // WHILE is similar to WHEN but used for conditional UI rendering
            // It performs pattern matching and evaluates the matching arm's body
            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let construct_context_for_while = construct_context.clone();
                    let actor_context_for_while = actor_context.clone();
                    let reference_connector_for_while = reference_connector.clone();
                    let link_connector_for_while = link_connector.clone();
                    let function_registry_for_while = function_registry.clone();
                    let source_code_for_while = source_code.clone();
                    let module_loader_for_while = module_loader.clone();
                    let persistence_for_while = persistence.clone();
                    let span_for_while = span;
                    let arms_for_closure = arms.clone();

                    // For each value, try to match against arms and evaluate matching body
                    let mapped_stream = piped.clone().subscribe().filter_map(move |value| {
                        let actor_context_clone = actor_context_for_while.clone();
                        let construct_context_clone = construct_context_for_while.clone();
                        let reference_connector_clone = reference_connector_for_while.clone();
                        let link_connector_clone = link_connector_for_while.clone();
                        let function_registry_clone = function_registry_for_while.clone();
                        let source_code_clone = source_code_for_while.clone();
                        let module_loader_clone = module_loader_for_while.clone();
                        let persistence_clone = persistence_for_while.clone();
                        let arms_clone = arms_for_closure.clone();

                        async move {
                            // Try each arm in order
                            for arm in &arms_clone {
                                if let Some(bindings) = match_pattern(
                                    &arm.pattern,
                                    &value,
                                    &construct_context_clone,
                                    &actor_context_clone,
                                ) {
                                    // Pattern matched! Evaluate the body with bindings
                                    let mut params = actor_context_clone.parameters.clone();
                                    for (name, actor) in bindings {
                                        params.insert(name, actor);
                                    }

                                    let new_actor_context = ActorContext {
                                        output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                                        piped: actor_context_clone.piped.clone(),
                                        passed: actor_context_clone.passed.clone(),
                                        parameters: params,
                                        // Propagate sequential_processing to nested constructs
                                        sequential_processing: actor_context_clone.sequential_processing,
                                        backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                                    };

                                    // Create a spanned expression from the body
                                    let body_expr = static_expression::Spanned {
                                        span: span_for_while,
                                        persistence: persistence_clone,
                                        node: arm.body.clone(),
                                    };

                                    match static_spanned_expression_into_value_actor(
                                        body_expr,
                                        construct_context_clone,
                                        new_actor_context,
                                        reference_connector_clone,
                                        link_connector_clone,
                                        function_registry_clone,
                                        module_loader_clone,
                                        source_code_clone,
                                    ) {
                                        Ok(result_actor) => return Some(result_actor),
                                        Err(_) => return None,
                                    }
                                }
                            }
                            // No arm matched
                            None
                        }
                    });

                    // Flatten the stream of actors into a stream of values
                    // subscribe() returns Subscription which keeps the actor alive
                    // IMPORTANT: Use .take(1) because body results use constant() streams
                    // which never complete. Without take(1), flat_map blocks waiting for
                    // the inner stream to finish, preventing subsequent input values from
                    // being processed (causing interval/counter to only process first tick).
                    //
                    // WHILE: Like THEN/WHEN, each matching input produces a body evaluation.
                    // While WHILE has "let everything flow" semantics at a conceptual level,
                    // the current implementation evaluates the body per input pulse, so we
                    // need fresh idempotency keys to prevent downstream duplicate skipping.
                    let flattened_stream = mapped_stream.flat_map(|actor| {
                        actor.subscribe().take(1).map(|mut value| {
                            value.set_idempotency_key(ValueIdempotencyKey::new());
                            value
                        })
                    });

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; WHILE {{..}}"),
                        ),
                        actor_context,
                        TypedStream::infinite(flattened_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("WHILE requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::Hold { state_param, body } => {
            // TODO: Add compiler check to reject expensive-to-copy types (LIST, MAP, BYTES, MEMORY) in HOLD.
            // HOLD replaces entire value on each update - variable-size types are a performance trap.
            // See docs/language/HOLD.md "Supported Types".

            // HOLD: `input |> HOLD state_param { body }`
            // The piped value sets/resets the state (not just initial - any emission).
            // The body can reference `state_param` to get the current state.
            // The body expression's result becomes the new state value.
            // CRITICAL: The state is NOT self-reactive - changes to state don't
            // trigger re-evaluation of body. Only external events trigger updates.
            //
            // Example with reset:
            // ```boon
            // counter: LATEST { 0, reset } |> HOLD counter {
            //     increment |> THEN { counter + 1 }
            // }
            // ```
            // Here, `counter` starts at 0. When `increment` fires, `counter + 1`
            // becomes new state. When `reset` emits, state resets to that value.

            let initial_actor = actor_context.piped.clone()
                .ok_or("HOLD requires a piped initial value")?;

            let state_param_string = state_param.to_string();
            let construct_context_for_state = construct_context.clone();
            let actor_context_for_state = actor_context.clone();
            let persistence_for_state = persistence.clone();
            let span_for_state = span;

            // Use a channel to hold current state value and broadcast updates
            let (state_sender, state_receiver) = zoon::futures_channel::mpsc::unbounded::<Value>();
            let state_sender = Rc::new(RefCell::new(state_sender));
            let state_sender_for_body = state_sender.clone();
            let state_sender_for_update = state_sender.clone();

            // Current state holder (starts with None, will be set when initial emits)
            let current_state: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
            let current_state_for_body = current_state.clone();
            let current_state_for_update = current_state.clone();

            // Create a ValueActor that provides the current state to the body
            // This is what the state_param references
            //
            // CRITICAL: state_actor's stream MUST first get the initial value directly
            // from initial_actor (using take(1)), then listen to state_receiver for updates.
            // This ensures the initial value is available BEFORE body evaluation starts.
            // Without this, there's a race condition:
            // 1. Body evaluates, creating PULSES/THEN actors
            // 2. PULSES emits, THEN evaluates, needs state.field
            // 3. But state_actor hasn't received initial value yet (it comes from combined_stream)
            // 4. Deadlock: body waits for state, state waits for combined_stream to run
            //
            // The fix: state_actor first subscribes to initial_actor directly, getting
            // the initial value immediately. Then it chains with state_receiver for:
            // - Body updates (from state_update_stream → state_sender_for_update)
            // - Reset values (from initial_stream → state_sender_for_body)
            let state_stream = initial_actor.clone().subscribe()
                .take(1)  // Get the first initial value directly
                .chain(state_receiver);  // Then listen for updates and resets
            let state_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("Hold state actor for {state_param_string}"),
                    None,
                    format!("{span}; HOLD state parameter"),
                ),
                actor_context.clone(),
                TypedStream::infinite(state_stream),
                None,
            );

            // Bind the state parameter in the context so body can reference it
            let mut body_parameters = actor_context.parameters.clone();
            body_parameters.insert(state_param_string.clone(), state_actor);

            // Create backpressure permit for synchronizing THEN with state updates.
            // Initial count = 1 allows first body evaluation to start.
            // HOLD releases permit after each state update, allowing next body to run.
            let backpressure_permit = BackpressurePermit::new(1);
            let permit_for_state_update = backpressure_permit.clone();

            let body_actor_context = ActorContext {
                output_valve_signal: actor_context.output_valve_signal.clone(),
                piped: None, // Clear piped - the body shouldn't re-use it
                passed: actor_context.passed.clone(),
                parameters: body_parameters,
                // Force sequential processing in HOLD body to ensure state consistency.
                // Without this, THEN/WHEN would spawn parallel body evaluations that all
                // read stale state (e.g., PULSES {3} |> THEN { counter + 1 } would read counter=0 three times).
                sequential_processing: true,
                // Pass permit to body - THEN will acquire before each evaluation
                backpressure_permit: Some(backpressure_permit),
            };

            // Evaluate the body with state parameter bound
            let body_result = static_spanned_expression_into_value_actor(
                *body,
                construct_context.clone(),
                body_actor_context,
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            // When body produces new values, update the state
            // Note: We avoid self-reactivity by not triggering body re-evaluation
            // from state changes. Body only evaluates when its event sources fire.
            let body_subscription = body_result.subscribe();
            let state_update_stream = body_subscription.map(move |new_value| {
                // Update current state
                *current_state_for_update.borrow_mut() = Some(new_value.clone());
                // Send to state channel so body can see it on next event
                let _ = state_sender_for_update.borrow().unbounded_send(new_value.clone());
                // Release permit to allow THEN to process next input.
                // This guarantees state is updated before next body evaluation starts.
                permit_for_state_update.release();
                new_value
            });

            // When initial value emits, set up initial state
            let initial_stream = initial_actor.subscribe().map(move |initial| {
                // Set current state
                *current_state_for_body.borrow_mut() = Some(initial.clone());
                // Send initial state to the state channel
                let _ = state_sender_for_body.borrow().unbounded_send(initial.clone());
                initial
            });

            // Combine: input stream sets/resets state, body updates state
            // Use select to merge both streams - any emission from input resets state
            let combined_stream = stream::select(
                initial_stream, // Any emission from input resets the state
                state_update_stream
            );

            ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; HOLD {state_param_string} {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(combined_stream),
                Some(persistence_id),
            )
        }
        static_expression::Expression::Flush { value } => {
            // FLUSH for fail-fast error handling
            // `FLUSH { error_value }` creates a FLUSHED[value] wrapper that propagates transparently
            // The wrapper bypasses function processing and unwraps at boundaries
            // (variable bindings, function returns, BLOCK returns)
            //
            // From FLUSH.md:
            // - FLUSHED[value] propagates transparently through pipelines
            // - Functions check if input is FLUSHED, if so bypass processing
            // - Unwraps at boundaries (assignment, function return, BLOCK return)

            let error_actor = static_spanned_expression_into_value_actor(
                *value,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            // Wrap each emitted value in Value::Flushed
            let flushed_stream = error_actor.subscribe().map(|value| {
                value.into_flushed()
            });

            ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; FLUSH {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(flushed_stream),
                Some(persistence_id),
            )
        }
        static_expression::Expression::Pulses { count } => {
            // PULSES for iteration: `PULSES { count }` emits count values (0 to count-1)
            // Can be used with THEN for iteration: `PULSES { 10 } |> THEN { ... }`

            let count_actor = static_spanned_expression_into_value_actor(
                *count,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            let construct_context_for_pulses = construct_context.clone();

            // Get backpressure permit from HOLD context if available.
            // When inside HOLD, PULSES will acquire permit before each emission,
            // ensuring consumer (THEN) processes each value before next is emitted.
            let backpressure_permit = actor_context.backpressure_permit.clone();

            // When count changes, emit that many pulses
            // Clone count_actor before moving into closure - we need to keep it alive
            // Use stream::unfold instead of stream::iter to yield between emissions,
            // ensuring downstream subscribers have a chance to process each pulse
            let pulses_stream = count_actor.clone().subscribe().flat_map(move |count_value| {
                let n = match &count_value {
                    Value::Number(num, _) => num.number() as i64,
                    _ => 0,
                };

                let construct_context_inner = construct_context_for_pulses.clone();
                let permit_for_iteration = backpressure_permit.clone();

                // Use unfold to emit pulses one at a time with async yield points
                // Boon uses 1-based indexing, so PULSES { 5 } emits 1, 2, 3, 4, 5.
                // When backpressure permit exists (inside HOLD), acquire it before
                // each emission to ensure THEN processes the value before next pulse.
                stream::unfold(1i64, move |i| {
                    let construct_context_for_iter = construct_context_inner.clone();
                    let permit = permit_for_iteration.clone();
                    async move {
                        if i > n.max(0) {
                            return None;
                        }

                        // If backpressure permit exists, acquire before emitting.
                        // This ensures previous value was processed by consumer (THEN/HOLD).
                        // HOLD releases permit after state update, so next pulse can emit.
                        if let Some(ref permit) = permit {
                            permit.acquire().await;
                        } else {
                            // No backpressure: yield to allow downstream to process
                            yield_once().await;
                        }

                        let value = Value::Number(
                            Arc::new(Number::new(
                                ConstructInfo::new(
                                    format!("PULSES iteration {i}"),
                                    None,
                                    format!("PULSES iteration {i}"),
                                ),
                                construct_context_for_iter,
                                i as f64,
                            )),
                            ValueMetadata {
                                idempotency_key: Ulid::new(),
                            },
                        );
                        Some((value, i + 1))
                    }
                })
            });

            // Keep count_actor alive by passing it as an input dependency
            ValueActor::new_arc_with_inputs(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; PULSES {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(pulses_stream),
                Some(persistence_id),
                vec![count_actor],
            )
        }
        static_expression::Expression::Spread { value } => {
            // Spread operator: `...expression` - spreads object fields
            // Used in object literals: `[...base, override: new_value]`
            // For now, just evaluate the expression and return it
            // The actual spreading happens at object construction time

            static_spanned_expression_into_value_actor(
                *value,
                construct_context,
                actor_context,
                reference_connector,
                link_connector,
                function_registry,
                module_loader,
                source_code,
            )?
        }
        static_expression::Expression::Pipe { from, to } => {
            // Flatten the pipe chain to process iteratively instead of recursively.
            // This reduces call stack depth from O(chain_length) to O(1).
            //
            // Example: `a |> b |> c |> d` is stored as `Pipe(a, Pipe(b, Pipe(c, d)))`
            // Flattening gives us `[a, b, c, d]` which we process in a loop.

            // Reconstruct the full Pipe expression to pass to flatten_pipe_chain
            let full_pipe = static_expression::Spanned {
                span,
                persistence: persistence.clone(),
                node: static_expression::Expression::Pipe { from, to },
            };
            let chain = flatten_pipe_chain(full_pipe);

            // Process the chain iteratively
            let mut current_actor_context = actor_context.clone();

            for step in chain.into_iter() {
                let step_actor = static_spanned_expression_into_value_actor(
                    step,
                    construct_context.clone(),
                    current_actor_context.clone(),
                    reference_connector.clone(),
                    link_connector.clone(),
                    function_registry.clone(),
                    module_loader.clone(),
                    source_code.clone(),
                )?;

                // Update context for next step - the result becomes the piped value
                current_actor_context = ActorContext {
                    output_valve_signal: current_actor_context.output_valve_signal.clone(),
                    piped: Some(step_actor.clone()),
                    passed: current_actor_context.passed.clone(),
                    parameters: current_actor_context.parameters.clone(),
                    sequential_processing: current_actor_context.sequential_processing,
                    backpressure_permit: current_actor_context.backpressure_permit.clone(),
                };
            }

            // The last step's result is in current_actor_context.piped
            current_actor_context.piped.unwrap()
        }
        static_expression::Expression::Block { variables, output } => {
            // BLOCK creates a scope with local variables
            // Variables are evaluated in order and added to parameters
            // The output expression is then evaluated with access to those variables

            // Start with current parameters
            let mut local_parameters = actor_context.parameters.clone();

            // Evaluate each variable and add to local scope
            for var in variables {
                let var_name = var.node.name.to_string();
                let value_actor = static_spanned_expression_into_value_actor(
                    var.node.value,
                    construct_context.clone(),
                    ActorContext {
                        output_valve_signal: actor_context.output_valve_signal.clone(),
                        piped: actor_context.piped.clone(),
                        passed: actor_context.passed.clone(),
                        parameters: local_parameters.clone(),
                        sequential_processing: actor_context.sequential_processing,
                        backpressure_permit: actor_context.backpressure_permit.clone(),
                    },
                    reference_connector.clone(),
                    link_connector.clone(),
                    function_registry.clone(),
                    module_loader.clone(),
                    source_code.clone(),
                )?;
                local_parameters.insert(var_name, value_actor);
            }

            // Evaluate the output expression with local variables in scope
            return static_spanned_expression_into_value_actor(
                *output,
                construct_context,
                ActorContext {
                    output_valve_signal: actor_context.output_valve_signal.clone(),
                    piped: actor_context.piped.clone(),
                    passed: actor_context.passed.clone(),
                    parameters: local_parameters,
                    sequential_processing: actor_context.sequential_processing,
                    backpressure_permit: actor_context.backpressure_permit.clone(),
                },
                reference_connector,
                link_connector,
                function_registry,
                module_loader,
                source_code,
            );
        }
        static_expression::Expression::TextLiteral { parts } => {
            // TextLiteral combines literal text with interpolated variables
            // e.g., TEXT { {count} item{maybe_s} left }

            // Collect all parts - literals as constant streams, interpolations as variable lookups
            let mut part_actors: Vec<(bool, Arc<ValueActor>)> = Vec::new();

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
                            construct_context.clone(),
                            idempotency_key,
                            actor_context.clone(),
                            text_string,
                        );
                        part_actors.push((true, text_actor));
                    }
                    static_expression::TextPart::Interpolation { var, referenced_span } => {
                        // Interpolation - look up the variable
                        let var_name = var.to_string();
                        if let Some(var_actor) = actor_context.parameters.get(&var_name) {
                            part_actors.push((false, var_actor.clone()));
                        } else if let Some(ref_span) = referenced_span {
                            // Use reference_connector to get the variable from outer scope
                            // Create a wrapper actor that resolves the reference asynchronously
                            let ref_connector = reference_connector.clone();
                            let ref_span_copy = *ref_span;
                            let value_stream = stream::once(ref_connector.referenceable(ref_span_copy))
                                .flat_map(|actor| actor.subscribe())
                                .boxed_local();
                            let ref_actor = Arc::new(ValueActor::new(
                                ConstructInfo::new(
                                    format!("TextInterpolation:{}", var_name),
                                    None,
                                    format!("{span}; TextInterpolation for '{}'", var_name),
                                ).complete(ConstructType::ValueActor),
                                actor_context.clone(),
                                TypedStream::infinite(value_stream),
                                None,
                            ));
                            part_actors.push((false, ref_actor));
                        } else {
                            return Err(format!("Variable '{}' not found for text interpolation", var_name));
                        }
                    }
                }
            }

            if part_actors.is_empty() {
                // Empty text literal
                Text::new_arc_value_actor(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; TextLiteral empty"),
                    ),
                    construct_context,
                    idempotency_key,
                    actor_context,
                    String::new(),
                )
            } else if part_actors.len() == 1 && part_actors[0].0 {
                // Single literal text part - return as-is
                part_actors.into_iter().next().unwrap().1
            } else {
                // Multiple parts or interpolations - combine with combineLatest-like behavior
                let actor_context_for_combine = actor_context.clone();
                let construct_context_for_combine = construct_context.clone();
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
                let combined_stream = merged.scan(
                    vec![None; part_count],
                    move |latest_values, (idx, value)| {
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

                // Create a value actor for the combined text
                // We'll use flat_map to create each combined text value
                let flattened = combined_stream.flat_map(move |combined_text| {
                    let text_actor = Text::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("TextLiteral combined"),
                            None,
                            format!("{span_for_combine}; TextLiteral combined"),
                        ),
                        construct_context_for_combine.clone(),
                        Ulid::new(),
                        actor_context_for_combine.clone(),
                        combined_text,
                    );
                    text_actor.subscribe()
                });

                ValueActor::new_arc(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; TextLiteral {{..}}"),
                    ),
                    actor_context,
                    TypedStream::infinite(flattened),
                    Some(persistence_id),
                )
            }
        }
        // Hardware types (parse-only for now - return error if used)
        static_expression::Expression::Bits { .. }
        | static_expression::Expression::Memory { .. }
        | static_expression::Expression::Bytes { .. } => {
            return Err("Hardware types (BITS, MEMORY, BYTES) are parse-only and cannot be evaluated yet".to_string());
        }
    };
    Ok(actor)
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
        _ => return Err(format!("Unknown function '{}(..)' in static context", path.join("/"))),
    };
    Ok(definition)
}

/// Match result containing bindings if match succeeded
type PatternBindings = HashMap<String, Arc<ValueActor>>;

/// Try to match a Value against a Pattern.
/// Returns Some(bindings) if match succeeds, None otherwise.
fn match_pattern(
    pattern: &static_expression::Pattern,
    value: &Value,
    _construct_context: &ConstructContext,
    actor_context: &ActorContext,
) -> Option<PatternBindings> {
    let mut bindings = HashMap::new();

    match pattern {
        static_expression::Pattern::WildCard => {
            // Wildcard matches everything
            Some(bindings)
        }
        static_expression::Pattern::Alias { name } => {
            // Bind the value to a new name
            let name_string = name.to_string();
            let value_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("pattern_binding_{name_string}"),
                    None,
                    format!("Pattern binding {name_string}"),
                ),
                actor_context.clone(),
                constant(value.clone()),
                None,
            );
            bindings.insert(name_string, value_actor);
            Some(bindings)
        }
        static_expression::Pattern::Literal(lit) => {
            // Match literal values
            match (lit, value) {
                (static_expression::Literal::Number(pattern_num), Value::Number(num, _)) => {
                    if (num.number() - pattern_num).abs() < f64::EPSILON {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                (static_expression::Literal::Tag(pattern_tag), Value::Tag(tag, _)) => {
                    if tag.tag() == pattern_tag.as_ref() {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                (static_expression::Literal::Text(pattern_text), Value::Text(text, _)) => {
                    if text.text() == pattern_text.as_ref() {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        static_expression::Pattern::TaggedObject { tag: pattern_tag, variables: pattern_vars } => {
            // Match tagged objects
            if let Value::TaggedObject(tagged_obj, _) = value {
                if tagged_obj.tag() == pattern_tag.as_ref() {
                    // Match each pattern variable against object variables
                    for pattern_var in pattern_vars {
                        let var_name = pattern_var.name.to_string();
                        // Find the variable in the object
                        if let Some(obj_var) = tagged_obj.variables().iter().find(|v| v.name() == var_name) {
                            if let Some(sub_pattern) = &pattern_var.value {
                                // TODO: Would need to get a value from obj_var.value_actor() to match
                                // For now, just bind the variable
                                bindings.insert(var_name, obj_var.value_actor());
                            } else {
                                // No sub-pattern, just bind the variable
                                bindings.insert(var_name, obj_var.value_actor());
                            }
                        } else {
                            return None; // Required variable not found
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
        static_expression::Pattern::Object { variables: pattern_vars } => {
            // Match objects
            if let Value::Object(obj, _) = value {
                for pattern_var in pattern_vars {
                    let var_name = pattern_var.name.to_string();
                    if let Some(obj_var) = obj.variables().iter().find(|v| v.name() == var_name) {
                        bindings.insert(var_name, obj_var.value_actor());
                    } else {
                        return None;
                    }
                }
                Some(bindings)
            } else {
                None
            }
        }
        static_expression::Pattern::List { items: _ } => {
            // TODO: Implement list pattern matching
            None
        }
        static_expression::Pattern::Map { entries: _ } => {
            // TODO: Implement map pattern matching
            None
        }
    }
}

