use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use chumsky::Parser as ChumskyParser;
use chumsky::input::{Input as ChumskyInput, Stream as ChumskyStream};
use ulid::Ulid;
use zoon::futures_channel::oneshot;
use zoon::futures_util::future;
use zoon::futures_util::stream::{self, LocalBoxStream};
use zoon::{SinkExt, Stream, StreamExt, Task, TaskHandle, mpsc};

use super::engine::*;
use crate::api;
use boon::parser::{
    Persistence, PersistenceId, PersistenceStatus, Scope, SourceCode, Span, Spanned, Token, lexer,
    parser, resolve_persistence, resolve_references, span_at, static_expression,
};

async fn take_first_seeded_actor_value_and_future_stream(
    actor: ActorHandle,
) -> Option<(Value, LocalBoxStream<'static, Value>)> {
    let mut source_subscription = actor.current_or_future_stream();
    let first_value = source_subscription.next().await?;
    Some((first_value, source_subscription))
}

fn hold_body_current_and_future_values(body_result: ActorHandle) -> LocalBoxStream<'static, Value> {
    body_result.current_or_future_stream()
}

fn restored_list_append_item_current_and_future_values(
    item_actor: ActorHandle,
) -> LocalBoxStream<'static, Value> {
    item_actor.current_or_future_stream()
}

async fn actor_current_value_or_wait(actor: &ActorHandle) -> Option<Value> {
    match actor.current_value() {
        Ok(value) => Some(value),
        Err(CurrentValueError::NoValueYet) => actor.value().await.ok(),
        Err(CurrentValueError::ActorDropped) => None,
    }
}

async fn actor_field_actor_from_current_or_wait(
    base_actor: &ActorHandle,
    field_path: &[String],
) -> Option<ActorHandle> {
    let mut current_obj_value = actor_current_value_or_wait(base_actor).await?;

    for (i, field_name) in field_path.iter().enumerate() {
        let is_last = i == field_path.len() - 1;

        match &current_obj_value {
            Value::Object(obj, _) => {
                let var = obj.variable(field_name)?;
                if is_last {
                    return Some(var.value_actor().clone());
                }
                current_obj_value = actor_current_value_or_wait(&var.value_actor()).await?;
            }
            Value::TaggedObject(tagged, _) => {
                let var = tagged.variable(field_name)?;
                if is_last {
                    return Some(var.value_actor().clone());
                }
                current_obj_value = actor_current_value_or_wait(&var.value_actor()).await?;
            }
            _ => return None,
        }
    }

    None
}

fn wrap_restored_list_append_item_with_origin(
    item_actor: ActorHandle,
    origin: ListItemOrigin,
    scope_id: ScopeId,
) -> ActorHandle {
    create_actor_with_origin(
        Box::pin(restored_list_append_item_current_and_future_values(
            item_actor,
        )),
        PersistenceId::new(),
        origin,
        scope_id,
    )
}

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
    source_actor: ActorHandle,
    storage: Arc<ConstructStorage>,
    persistence_id: PersistenceId,
    scope: Scope,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    value_changed: bool,
) -> impl Stream<Item = Value> {
    let scoped_id = persistence_id.in_scope(&scope);

    // Skip restoration in two cases:
    // 1. If value expression changed in code - ensures `width: 300` → `width: 400` uses the new value
    // 2. Inside HOLD body (sequential_processing) - HOLD handles state persistence;
    //    variables inside HOLD body are recreated each iteration with fresh values
    if value_changed || actor_context.sequential_processing {
        return source_actor
            .current_or_future_stream()
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
    stream::unfold(PersistenceState::LoadingStored, move |state| {
        let storage = storage.clone();
        let source_actor = source_actor.clone();
        let construct_context = construct_context.clone();
        let actor_context = actor_context.clone();

        async move {
            match state {
                PersistenceState::LoadingStored => {
                    // Try to load stored value
                    let loaded: Option<zoon::serde_json::Value> = storage.load_state_now(scoped_id);

                    let restored_value = loaded.and_then(|json| {
                        match &json {
                            zoon::serde_json::Value::String(_)
                            | zoon::serde_json::Value::Number(_)
                            | zoon::serde_json::Value::Bool(_)
                            | zoon::serde_json::Value::Null => Some(Value::from_json(
                                &json,
                                ConstructId::new("variable restored from storage"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                actor_context.clone(),
                            )),
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

                    let mut source_subscription = source_actor.current_or_future_stream();

                    if let Some(value) = restored_value {
                        // Emit restored value, then skip first source emission
                        Some((
                            Some(value),
                            PersistenceState::SkipFirstSource {
                                source_subscription,
                                storage,
                            },
                        ))
                    } else {
                        // No stored value, use the current source value if ready,
                        // then continue with future-only updates.
                        let first_value = source_subscription.next().await?;
                        save_value_if_applicable(&first_value, scoped_id, &storage).await;
                        Some((
                            Some(first_value),
                            PersistenceState::ForwardingSource {
                                source_subscription,
                                storage,
                            },
                        ))
                    }
                }
                PersistenceState::SkipFirstSource {
                    mut source_subscription,
                    storage,
                } => {
                    // Skip first emission from source (it's the initial empty state)
                    // Return None to skip this iteration, but continue with ForwardingSource
                    let _ = source_subscription.next().await?;
                    Some((
                        None,
                        PersistenceState::ForwardingSource {
                            source_subscription,
                            storage,
                        },
                    ))
                }
                PersistenceState::ForwardingSource {
                    mut source_subscription,
                    storage,
                } => {
                    let value = source_subscription.next().await?;
                    save_value_if_applicable(&value, scoped_id, &storage).await;
                    Some((
                        Some(value),
                        PersistenceState::ForwardingSource {
                            source_subscription,
                            storage,
                        },
                    ))
                }
            }
        }
    })
    .filter_map(future::ready)
    .right_stream()
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

async fn save_value_if_applicable(
    value: &Value,
    scoped_id: PersistenceId,
    storage: &ConstructStorage,
) {
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
    /// Current module context for resolving intra-module function calls.
    /// When evaluating `Theme/material()` body, this is `Some("Theme")`.
    /// Unqualified calls like `get()` will try `Theme/get` as fallback.
    pub current_module: Option<String>,
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
            current_module: None,
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
    pub fn with_piped(&self, piped: ActorHandle) -> Self {
        self.with_actor_context(ActorContext {
            piped: Some(piped),
            ..self.actor_context.clone()
        })
    }

    /// Create a derived context with passed value set.
    pub fn with_passed(&self, passed: ActorHandle) -> Self {
        self.with_actor_context(ActorContext {
            passed: Some(passed),
            ..self.actor_context.clone()
        })
    }

    /// Create a derived context with additional parameters.
    pub fn with_parameters(&self, params: HashMap<String, ActorHandle>) -> Self {
        let mut new_params = (*self.actor_context.parameters).clone();
        new_params.extend(params);
        self.with_actor_context(ActorContext {
            parameters: Arc::new(new_params),
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
        scope_id: ScopeId,
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
        connections: Vec<(SlotId, ActorHandle)>,
    },

    /// Wrap an evaluated slot with variable persistence.
    /// Used for function arguments that have persistence IDs.
    WrapWithPersistence {
        source_slot: SlotId,
        persistence_id: PersistenceId,
        ctx: EvaluationContext,
        result_slot: SlotId,
        /// True if the expression's value changed in code (NewOrChanged status).
        value_changed: bool,
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
    /// For referenced fields, holds a pre-created forwarding actor.
    /// This allows the actor to be registered with ReferenceConnector before
    /// the field expression is evaluated, fixing forward reference race conditions.
    pub forwarding_actor: Option<ActorHandle>,
    /// True if the value expression has changed since last run.
    /// Used to skip restoration from storage when code has changed.
    pub value_changed: bool,
}

type ScheduledStructField = (
    static_expression::Spanned<static_expression::Expression>,
    SlotId,
    bool,
);

struct PreparedStructBuild {
    variable_data: Vec<ObjectVariableData>,
    vars_to_schedule: Vec<ScheduledStructField>,
    ctx_with_locals: EvaluationContext,
}

struct ResolvedStructVariables {
    variables: Vec<Arc<Variable>>,
    spread_actors: Vec<ActorHandle>,
}

fn prepare_struct_build(
    state: &mut EvaluationState,
    variables: Vec<static_expression::Spanned<static_expression::Variable>>,
    ctx: &EvaluationContext,
    force_reactive_locals: bool,
) -> PreparedStructBuild {
    let mut variable_data = Vec::new();
    let mut vars_to_schedule = Vec::new();
    let mut object_locals = (*ctx.actor_context.object_locals).clone();

    for var in variables {
        let var_slot = state.alloc_slot();
        let name = var.node.name.to_string();

        if name.is_empty() {
            variable_data.push(ObjectVariableData {
                name,
                value_slot: var_slot,
                is_link: false,
                is_referenced: false,
                span: var.span,
                persistence: var.persistence.clone(),
                forwarding_actor: None,
                value_changed: false,
            });
            vars_to_schedule.push((var.node.value, var_slot, false));
            continue;
        }

        let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);
        let is_referenced = var.node.is_referenced;
        let var_span = var.span;
        let var_persistence = var.persistence.clone();

        let forwarding_actor = if is_referenced {
            let var_persistence_id = var_persistence
                .as_ref()
                .expect("variable persistence should be set by resolver")
                .id;
            let actor = create_actor_forwarding(var_persistence_id, ctx.actor_context.scope_id());
            object_locals.insert(var_span, actor.clone());
            if let Some(ref_connector) = ctx.try_reference_connector() {
                ref_connector.register_referenceable(var_span, actor.clone());
            }
            Some(actor)
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
            value_changed: var.node.value_changed,
        });

        if !is_link {
            vars_to_schedule.push((var.node.value, var_slot, is_referenced));
        }
    }

    let ctx_with_locals = EvaluationContext {
        actor_context: ActorContext {
            object_locals: Arc::new(object_locals),
            is_snapshot_context: if force_reactive_locals {
                false
            } else {
                ctx.actor_context.is_snapshot_context
            },
            ..ctx.actor_context.clone()
        },
        current_module: ctx.current_module.clone(),
        ..ctx.clone()
    };

    PreparedStructBuild {
        variable_data,
        vars_to_schedule,
        ctx_with_locals,
    }
}

fn schedule_struct_fields(
    state: &mut EvaluationState,
    vars_to_schedule: Vec<ScheduledStructField>,
    ctx: EvaluationContext,
) -> Result<(), String> {
    for (var_expr, var_slot, is_referenced) in vars_to_schedule.iter() {
        if !*is_referenced {
            schedule_expression(state, var_expr.clone(), ctx.clone(), *var_slot)?;
        }
    }
    for (var_expr, var_slot, is_referenced) in vars_to_schedule {
        if is_referenced {
            schedule_expression(state, var_expr, ctx.clone(), var_slot)?;
        }
    }
    Ok(())
}

fn resolve_struct_variables(
    variable_data: &[ObjectVariableData],
    state: &EvaluationState,
    ctx: &EvaluationContext,
) -> ResolvedStructVariables {
    let mut variables = Vec::new();
    let mut spread_actors = Vec::new();

    for vd in variable_data {
        if vd.name.is_empty() {
            if let Some(actor) = state.get(vd.value_slot) {
                spread_actors.push(actor);
            }
            continue;
        }

        let Some(variable) = build_struct_variable(vd, state, ctx) else {
            continue;
        };
        register_struct_variable(vd, &variable, ctx);
        variables.push(variable);
    }

    ResolvedStructVariables {
        variables,
        spread_actors,
    }
}

fn extend_variables_from_spread_value(value: Value, all_variables: &mut Vec<Arc<Variable>>) {
    match value {
        Value::Object(obj, _) => {
            all_variables.extend(obj.variables().iter().cloned());
        }
        Value::TaggedObject(obj, _) => {
            all_variables.extend(obj.variables().iter().cloned());
        }
        _ => {}
    }
}

fn try_resolve_spread_struct_variables_now(
    spread_actors: &[ActorHandle],
    variables: Vec<Arc<Variable>>,
) -> Option<Vec<Arc<Variable>>> {
    let mut all_variables = Vec::new();

    for spread_actor in spread_actors {
        let value = spread_actor.current_value().ok()?;
        extend_variables_from_spread_value(value, &mut all_variables);
    }

    all_variables.extend(variables);
    Some(all_variables)
}

async fn resolve_spread_struct_variables(
    spread_actors: Vec<ActorHandle>,
    variables: Vec<Arc<Variable>>,
) -> Vec<Arc<Variable>> {
    let mut all_variables = Vec::new();

    for spread_actor in &spread_actors {
        if let Some(value) = actor_current_value_or_wait(spread_actor).await {
            extend_variables_from_spread_value(value, &mut all_variables);
        }
    }

    all_variables.extend(variables);
    all_variables
}

fn build_struct_variable(
    vd: &ObjectVariableData,
    state: &EvaluationState,
    ctx: &EvaluationContext,
) -> Option<Arc<Variable>> {
    let var_persistence_id = vd
        .persistence
        .as_ref()
        .expect("persistence should be set by resolver")
        .id;

    let variable = if vd.is_link && vd.forwarding_actor.is_some() {
        let forwarding_actor = vd.forwarding_actor.as_ref().unwrap();

        let temp_link = Variable::new_link_arc(
            ConstructInfo::new(
                format!("PersistenceId: {}", var_persistence_id),
                vd.persistence.clone(),
                format!("{}: (link variable internal)", vd.name),
            ),
            vd.name.clone(),
            ctx.actor_context.clone(),
            var_persistence_id,
        );

        let link_value_actor = temp_link.value_actor();
        let link_value_sender = temp_link.expect_link_value_sender();
        connect_forwarding_current_and_future(forwarding_actor.clone(), link_value_actor);

        Variable::new_link_arc_with_forwarding_actor(
            ConstructInfo::new(
                format!("PersistenceId: {}", var_persistence_id),
                vd.persistence.clone(),
                format!("{}: (link variable with forwarding)", vd.name),
            ),
            vd.name.clone(),
            var_persistence_id,
            ctx.actor_context.scope.clone(),
            forwarding_actor.clone(),
            link_value_sender,
        )
    } else if vd.is_link {
        Variable::new_link_arc(
            ConstructInfo::new(
                format!("PersistenceId: {}", var_persistence_id),
                vd.persistence.clone(),
                format!("{}: (link variable)", vd.name),
            ),
            vd.name.clone(),
            ctx.actor_context.clone(),
            var_persistence_id,
        )
    } else if let Some(forwarding_actor) = &vd.forwarding_actor {
        let source_actor = state.get(vd.value_slot)?;
        connect_forwarding_current_and_future(forwarding_actor.clone(), source_actor);
        Variable::new_arc(
            ConstructInfo::new(
                format!("PersistenceId: {}", var_persistence_id),
                vd.persistence.clone(),
                format!("{}: (variable)", vd.name),
            ),
            vd.name.clone(),
            forwarding_actor.clone(),
            var_persistence_id,
            ctx.actor_context.scope.clone(),
        )
    } else {
        let value_actor = state.get(vd.value_slot)?;

        let effective_actor = if ctx.construct_context.construct_storage.is_disabled() {
            value_actor
        } else {
            let persistence_stream = create_variable_persistence_stream(
                value_actor.clone(),
                ctx.construct_context.construct_storage.clone(),
                var_persistence_id,
                ctx.actor_context.scope.clone(),
                ctx.construct_context.clone(),
                ctx.actor_context.clone(),
                vd.value_changed,
            );

            create_actor(
                persistence_stream,
                var_persistence_id,
                ctx.actor_context.scope_id(),
            )
        };

        Variable::new_arc(
            ConstructInfo::new(
                format!("PersistenceId: {}", var_persistence_id),
                vd.persistence.clone(),
                format!("{}: (variable)", vd.name),
            ),
            vd.name.clone(),
            effective_actor,
            var_persistence_id,
            ctx.actor_context.scope.clone(),
        )
    };

    Some(variable)
}

fn register_struct_variable(
    vd: &ObjectVariableData,
    variable: &Arc<Variable>,
    ctx: &EvaluationContext,
) {
    if vd.forwarding_actor.is_none() {
        if let Some(ref_connector) = ctx.try_reference_connector() {
            ref_connector.register_referenceable(vd.span, variable.value_actor());
        }
    }

    if vd.is_link {
        if let Some(sender) = variable.link_value_sender() {
            if let Some(link_connector) = ctx.try_link_connector() {
                link_connector.register_link(vd.span, ctx.actor_context.scope.clone(), sender);
            }
        }
    }
}

/// Holds the state of an ongoing work queue evaluation.
pub struct EvaluationState {
    /// Work queue (LIFO for depth-first evaluation).
    work_queue: Vec<WorkItem>,

    /// Results storage - indexed by SlotId for O(1) access.
    /// None means SKIP (no value). Vec grows as slots are allocated.
    results: Vec<Option<ActorHandle>>,

    /// Next available slot ID.
    next_slot: SlotId,

    /// Function registry - stores user-defined functions during evaluation.
    /// Owned by the evaluation state, no sharing or interior mutability needed.
    function_registry: HashMap<String, StaticFunctionDefinition>,
}

impl EvaluationState {
    /// Create a new empty evaluation state.
    pub fn new() -> Self {
        Self {
            work_queue: Vec::new(),
            results: Vec::new(),
            next_slot: 0,
            function_registry: HashMap::new(),
        }
    }

    /// Create evaluation state with pre-populated function registry.
    pub fn with_functions(functions: HashMap<String, StaticFunctionDefinition>) -> Self {
        Self {
            work_queue: Vec::new(),
            results: Vec::new(),
            next_slot: 0,
            function_registry: functions,
        }
    }

    /// Register a function in the registry.
    pub fn register_function(&mut self, name: String, def: StaticFunctionDefinition) {
        self.function_registry.insert(name, def);
    }

    /// Look up a function by name.
    pub fn get_function(&self, name: &str) -> Option<&StaticFunctionDefinition> {
        self.function_registry.get(name)
    }

    /// Create a merged function registry snapshot for sub-evaluations (THEN/WHEN/WHILE).
    /// Avoids cloning the parent snapshot when there are no local functions to merge.
    pub fn merged_registry_snapshot(
        &self,
        ctx: &EvaluationContext,
    ) -> Arc<HashMap<String, StaticFunctionDefinition>> {
        inc_metric!(REGISTRY_CLONES);
        if self.function_registry.is_empty() {
            // Common case: no local functions, just reuse the parent snapshot
            if let Some(ref snapshot) = ctx.function_registry_snapshot {
                inc_metric!(REGISTRY_CLONE_ENTRIES, snapshot.len() as u64);
                return Arc::clone(snapshot);
            }
            return Arc::new(HashMap::new());
        }
        // Merge: clone parent + insert local overrides
        let mut merged = ctx
            .function_registry_snapshot
            .as_ref()
            .map(|s| (**s).clone())
            .unwrap_or_default();
        for (name, def) in &self.function_registry {
            merged.insert(name.clone(), def.clone());
        }
        inc_metric!(REGISTRY_CLONE_ENTRIES, merged.len() as u64);
        Arc::new(merged)
    }

    /// Allocate a new result slot.
    pub fn alloc_slot(&mut self) -> SlotId {
        inc_metric!(SLOTS_ALLOCATED);
        let slot = self.next_slot;
        self.next_slot += 1;
        self.results.push(None);
        slot
    }

    /// Store a result in a slot.
    pub fn store(&mut self, slot: SlotId, actor: ActorHandle) {
        let idx = usize::try_from(slot).expect("SlotId should fit in usize");
        self.results[idx] = Some(actor);
    }

    /// Get a result from a slot. Returns None if the slot was SKIP (not stored).
    pub fn get(&self, slot: SlotId) -> Option<ActorHandle> {
        let idx = usize::try_from(slot).ok()?;
        self.results.get(idx).and_then(|opt| opt.clone())
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
) -> Result<Option<ActorHandle>, String> {
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
                    current_module: ctx.current_module.clone(),
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
            let actor = evaluate_alias_immediate(alias, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // CONTROL FLOW (THEN, WHEN, WHILE, HOLD)
        // These are special because their bodies are evaluated at runtime
        // ============================================================
        static_expression::Expression::Then { body } => {
            // THEN creates an actor that evaluates body at runtime for each piped value
            // We can build it immediately since the body is evaluated lazily
            let registry_snapshot = state.merged_registry_snapshot(&ctx);
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
            let registry_snapshot = state.merged_registry_snapshot(&ctx);
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
            let registry_snapshot = state.merged_registry_snapshot(&ctx);
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
            let piped = ctx
                .actor_context
                .piped
                .clone()
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
                let step_slot = if is_last {
                    result_slot
                } else {
                    state.alloc_slot()
                };

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
            let PreparedStructBuild {
                variable_data,
                vars_to_schedule,
                ctx_with_locals,
            } = prepare_struct_build(state, variables, &ctx, true);

            // Push BuildBlock first (will be processed last due to LIFO)
            // BuildBlock takes the output expression result and keeps the Object alive
            // (the Object contains the Variables which must not be dropped)
            state.push(WorkItem::BuildBlock {
                object_slot,
                output_slot: output_expr_slot,
                result_slot,
                scope_id: ctx.actor_context.scope_id(),
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

            // Schedule variable expressions: referenced fields LAST so they're processed FIRST
            // (LIFO), matching object-field behavior. This is required when a sibling block
            // variable is consumed through nested function-call arguments, e.g.
            // `display_value: compute_value(formula_text: formula_text)`.
            schedule_struct_fields(state, vars_to_schedule, ctx_with_locals)?;
        }

        // ============================================================
        // OBJECTS (schedule values first, then build)
        // ============================================================
        static_expression::Expression::Object(object) => {
            let PreparedStructBuild {
                variable_data,
                vars_to_schedule,
                ctx_with_locals,
            } = prepare_struct_build(state, object.variables, &ctx, false);

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

            schedule_struct_fields(state, vars_to_schedule, ctx_with_locals)?;
        }

        static_expression::Expression::TaggedObject { tag, object } => {
            let tag_str = tag.to_string();
            let PreparedStructBuild {
                variable_data,
                vars_to_schedule,
                ctx_with_locals,
            } = prepare_struct_build(state, object.variables, &ctx, false);

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

            schedule_struct_fields(state, vars_to_schedule, ctx_with_locals)?;
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
                ["List", "map"]
                | ["List", "retain"]
                | ["List", "remove"]
                | ["List", "every"]
                | ["List", "any"]
                | ["List", "sort_by"] => {
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
                    let mut forwarding_connections: Vec<(SlotId, ActorHandle)> = Vec::new();

                    // Build arg_locals map from forwarding actors
                    // This allows subsequent arguments to resolve references locally
                    // instead of relying on the global ReferenceConnector (which can be overwritten
                    // when the same function is called multiple times)
                    let mut arg_locals = (*ctx.actor_context.object_locals).clone();

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
                            let forwarding_actor = create_actor_forwarding(
                                PersistenceId::new(),
                                ctx.actor_context.scope_id(),
                            );
                            // Store in arg_locals for local resolution
                            // This prevents overwrites when same function is called multiple times
                            arg_locals.insert(arg_span, forwarding_actor.clone());
                            // Also register with ReferenceConnector for backward compatibility
                            if let Some(ref_connector) = ctx.try_reference_connector() {
                                ref_connector
                                    .register_referenceable(arg_span, forwarding_actor.clone());
                            }
                            forwarding_connections.push((arg_slot, forwarding_actor.clone()));
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
                                return Err(format!(
                                    "PASS argument requires piped value at {:?} (arg '{}', function path: {:?})",
                                    span, arg_name, path
                                ));
                            }
                        }
                    }

                    // Create context with arg_locals for argument expression evaluation.
                    // Snapshot context propagates naturally - function arguments capture values
                    // at trigger time when called inside THEN/WHEN bodies.
                    let ctx_with_arg_locals = EvaluationContext {
                        actor_context: ActorContext {
                            object_locals: Arc::new(arg_locals),
                            ..ctx.actor_context.clone()
                        },
                        current_module: ctx.current_module.clone(),
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
                    let latest_args_with_persistence: Vec<_> = args_to_schedule
                        .iter()
                        .filter_map(|(expr, slot)| {
                            // Only wrap LATEST expressions with value persistence
                            if matches!(expr.node, static_expression::Expression::Latest { .. }) {
                                expr.persistence.as_ref().map(|p| {
                                    let value_changed = p.status == PersistenceStatus::NewOrChanged;
                                    (*slot, p.id, value_changed)
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    // DEBUG: Log LATEST args with persistence
                    if LOG_DEBUG && !latest_args_with_persistence.is_empty() {
                        zoon::println!(
                            "[DEBUG] FunctionCall has {} LATEST args with persistence",
                            latest_args_with_persistence.len()
                        );
                    }

                    // Push persistence wrappers for LATEST args
                    for (arg_slot, persistence_id, value_changed) in latest_args_with_persistence {
                        if LOG_DEBUG {
                            zoon::println!(
                                "[DEBUG] Pushing WrapWithPersistence for LATEST slot {:?} with id {}",
                                arg_slot,
                                persistence_id
                            );
                        }
                        state.push(WorkItem::WrapWithPersistence {
                            source_slot: arg_slot,
                            persistence_id,
                            ctx: ctx_with_arg_locals.clone(),
                            result_slot: arg_slot, // wrap in place
                            value_changed,
                        });
                    }

                    // Schedule argument expressions last (will be processed first due to LIFO)
                    // Use ctx_with_arg_locals so subsequent args can resolve references to earlier args
                    for (arg_expr, arg_slot) in args_to_schedule {
                        schedule_expression(
                            state,
                            arg_expr,
                            ctx_with_arg_locals.clone(),
                            arg_slot,
                        )?;
                    }
                }
            }
        }

        // ============================================================
        // TEXT LITERAL (text with interpolations)
        // ============================================================
        static_expression::Expression::TextLiteral { parts, .. } => {
            let actor = build_text_literal_actor(parts, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // FUNCTION DEFINITION (registers function for later use)
        // ============================================================
        static_expression::Expression::Function {
            name,
            parameters,
            body,
        } => {
            // Register the function in the evaluation state's registry
            let func_name = name.to_string();
            let param_names: Vec<String> = parameters.iter().map(|p| p.node.to_string()).collect();

            let func_def = StaticFunctionDefinition {
                parameters: param_names,
                body: *body,
                module_name: None,
            };

            state.register_function(func_name.clone(), func_def);

            // Function definitions only register into the evaluation state.
            // They intentionally produce no runtime value.
        }

        // ============================================================
        // LINK SETTER (sets a link on an object)
        // ============================================================
        static_expression::Expression::LinkSetter { alias } => {
            let actor = build_link_setter_actor(alias, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // FIELD ACCESS (.field.subfield at pipe position)
        // Equivalent to WHILE { value => value.field.subfield }
        // ============================================================
        static_expression::Expression::FieldAccess { path } => {
            // Convert StrSlice path to Vec<String> for the function
            let path_strings: Vec<String> = path.iter().map(|s| s.to_string()).collect();
            let actor =
                build_field_access_actor(path_strings, span, persistence, persistence_id, ctx)?;
            state.store(result_slot, actor);
        }

        // ============================================================
        // POSTFIX FIELD ACCESS (expr.field)
        // e.g., Theme/material(of: Danger).color
        // Desugars to: expr |> .field (pipe + field access)
        // ============================================================
        static_expression::Expression::PostfixFieldAccess { expr, field } => {
            // Desugar: schedule inner expression, then pipe its result through field access
            let inner_slot = state.alloc_slot();

            // Push the EvaluateWithPiped work item FIRST (LIFO - processed second)
            let field_access_expr = static_expression::Spanned {
                span,
                node: static_expression::Expression::FieldAccess { path: vec![field] },
                persistence,
            };
            state.push(WorkItem::EvaluateWithPiped {
                expr: field_access_expr,
                prev_slot: inner_slot,
                ctx: ctx.clone(),
                result_slot,
            });

            // Schedule inner expression LAST (LIFO - processed first)
            schedule_expression(state, *expr, ctx, inner_slot)?;
        }

        // ============================================================
        // TODO: More expression types to be added
        // ============================================================
        _ => {
            // For now, return an error for unsupported expressions
            // In the final version, all expression types will be handled
            return Err(format!(
                "Expression type not yet supported in stack-safe evaluator: {:?}",
                span
            ));
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
        static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Add,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Subtract,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Multiply,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Divide,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::ArithmeticOperator::Negate { operand } => {
            // Negate is implemented as multiply by -1
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
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
        static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Equal,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::NotEqual,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::Comparator::Greater {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Greater,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::GreaterOrEqual,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::Comparator::Less {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::Less,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
        static_expression::Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => schedule_binary_op(
            state,
            BinaryOpKind::LessOrEqual,
            *operand_a,
            *operand_b,
            span,
            persistence,
            ctx,
            result_slot,
        ),
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
fn process_work_item(state: &mut EvaluationState, item: WorkItem) -> Result<(), String> {
    match item {
        WorkItem::Evaluate {
            expr,
            ctx,
            result_slot,
        } => {
            // This delegates back to schedule_expression
            schedule_expression(state, expr, ctx, result_slot)?;
        }

        WorkItem::BinaryOp {
            op,
            operand_a_slot,
            operand_b_slot,
            span: _,
            persistence: _,
            ctx,
            result_slot,
        } => {
            // If either operand slot is empty, produce nothing
            let Some(a) = state.get(operand_a_slot) else {
                return Ok(());
            };
            let Some(b) = state.get(operand_b_slot) else {
                return Ok(());
            };
            let actor = create_binary_op_actor(op, ctx.construct_context, ctx.actor_context, a, b);
            state.store(result_slot, actor);
        }

        WorkItem::BuildList {
            item_slots,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            // Collect items that have values (empty slots are ignored)
            let items: Vec<_> = item_slots
                .iter()
                .filter_map(|slot| state.get(*slot))
                .collect();
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;

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

        WorkItem::BuildObject {
            variable_data,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
            let ResolvedStructVariables {
                variables,
                spread_actors,
            } = resolve_struct_variables(&variable_data, state, &ctx);

            if spread_actors.is_empty() {
                // No spreads — use existing sync path
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
            } else {
                // Has spreads — create async stream that awaits spread values
                // and merges their variables with the explicit ones.
                // Spread variables come first so explicit fields override via rposition lookup.
                let object_construct_info = ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}; spread-merged Object"),
                    persistence.clone(),
                    format!("{span}; Object {{..}} (spread)"),
                );
                if let Some(merged_variables) =
                    try_resolve_spread_struct_variables_now(&spread_actors, variables.clone())
                {
                    let actor = Object::new_arc_value_actor(
                        object_construct_info,
                        ctx.construct_context,
                        persistence_id,
                        ctx.actor_context,
                        merged_variables,
                    );
                    state.store(result_slot, actor);
                } else {
                    let construct_context = ctx.construct_context.clone();
                    let scope_id = ctx.actor_context.scope_id();

                    let merge_future = async move {
                        Object::new_value(
                            object_construct_info,
                            construct_context,
                            persistence_id,
                            resolve_spread_struct_variables(spread_actors, variables).await,
                        )
                    };

                    let actor = create_actor_from_future(merge_future, persistence_id, scope_id);
                    state.store(result_slot, actor);
                }
            }
        }

        WorkItem::BuildTaggedObject {
            tag,
            variable_data,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
            let ResolvedStructVariables {
                variables,
                spread_actors,
            } = resolve_struct_variables(&variable_data, state, &ctx);

            if spread_actors.is_empty() {
                // No spreads — use existing sync path
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
            } else {
                // Has spreads — async merge (same as BuildObject spread path)
                let object_construct_info = ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}; spread-merged TaggedObject"),
                    persistence.clone(),
                    format!("{span}; {} {{..}} (spread)", tag),
                );
                if let Some(merged_variables) =
                    try_resolve_spread_struct_variables_now(&spread_actors, variables.clone())
                {
                    let actor = TaggedObject::new_arc_value_actor(
                        object_construct_info,
                        ctx.construct_context,
                        persistence_id,
                        ctx.actor_context,
                        tag,
                        merged_variables,
                    );
                    state.store(result_slot, actor);
                } else {
                    let construct_context = ctx.construct_context.clone();
                    let scope_id = ctx.actor_context.scope_id();

                    let merge_future = async move {
                        TaggedObject::new_value(
                            object_construct_info,
                            construct_context,
                            persistence_id,
                            tag,
                            resolve_spread_struct_variables(spread_actors, variables).await,
                        )
                    };

                    let actor = create_actor_from_future(merge_future, persistence_id, scope_id);
                    state.store(result_slot, actor);
                }
            }
        }

        WorkItem::BuildLatest {
            input_slots,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            // Collect inputs that have values (empty slots are ignored)
            let inputs: Vec<_> = input_slots
                .iter()
                .filter_map(|slot| state.get(*slot))
                .collect();
            let _persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;

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

        WorkItem::BuildThen {
            piped_slot: _,
            body,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
            let registry_snapshot = state.merged_registry_snapshot(&ctx);
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

        WorkItem::BuildWhen {
            piped_slot: _,
            arms,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
            let registry_snapshot = state.merged_registry_snapshot(&ctx);
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

        WorkItem::BuildWhile {
            piped_slot: _,
            arms,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
            let registry_snapshot = state.merged_registry_snapshot(&ctx);
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

        WorkItem::BuildHold {
            initial_slot,
            state_param,
            body,
            span,
            persistence,
            ctx,
            result_slot,
        } => {
            // If initial value slot is empty, produce nothing
            let Some(initial_actor) = state.get(initial_slot) else {
                return Ok(());
            };
            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
            if let Some(actor) = build_hold_actor(
                initial_actor,
                state_param,
                *body,
                span,
                persistence,
                persistence_id,
                ctx,
            )? {
                state.store(result_slot, actor);
            }
        }

        WorkItem::BuildBlock {
            object_slot,
            output_slot,
            result_slot,
            scope_id: _,
        } => {
            // If output slot is empty, this block produces nothing
            let Some(output_actor) = state.get(output_slot) else {
                return Ok(());
            };

            // Get the Object actor which contains the Variables
            // We need to keep the Object alive so Variables don't get dropped
            let object_actor = state.get(object_slot);

            if let Some(object_actor) = object_actor {
                retain_actor_handle(&output_actor, object_actor);
                state.store(result_slot, output_actor);
            } else {
                // No Object (block has no variables) - just use output directly
                state.store(result_slot, output_actor);
            }
        }

        WorkItem::EvaluateWithPiped {
            expr,
            prev_slot,
            ctx,
            result_slot,
        } => {
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
                    ["List", "map"]
                    | ["List", "retain"]
                    | ["List", "remove"]
                    | ["List", "every"]
                    | ["List", "any"]
                    | ["List", "sort_by"]
                    | ["List", "append"] => {
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
                                    return Err(format!(
                                        "PASS argument requires piped value at {:?} (arg '{}', function path: {:?})",
                                        span, arg_name, path_strs
                                    ));
                                }
                            }
                        }

                        // Push CallFunction with use_piped_for_builtin: true
                        state.push(WorkItem::CallFunction {
                            path: path_strs,
                            arg_slots,
                            passed_slot,
                            passed_context,
                            use_piped_for_builtin: true, // This is the key difference!
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
                // Handle `value |> LINK { target }` without blocking visible pass-through
                // on sender resolution. The source actor keeps the async link-hook
                // sidecar alive directly in its owned actor slot.
                let alias_for_stream = alias.node.clone();
                let ctx_for_stream = new_ctx.clone();
                let prev_actor_for_link_updates = prev_actor.clone();
                let subscription_scope_for_stream =
                    new_ctx.actor_context.subscription_scope.clone();
                start_retained_actor_task(&prev_actor, async move {
                    let Some(link_sender) =
                        get_link_sender_from_alias(alias_for_stream, ctx_for_stream).await
                    else {
                        return;
                    };

                    let mut updates = prev_actor_for_link_updates.current_or_future_stream();
                    while let Some(value) = updates.next().await {
                        let is_active = subscription_scope_for_stream
                            .as_ref()
                            .map_or(true, |scope| !scope.is_cancelled());
                        if !is_active {
                            break;
                        }
                        if link_sender.send(value).await.is_err() {
                            break;
                        }
                    }
                });
                state.store(result_slot, prev_actor);
            } else {
                // For non-FunctionCall expressions, just schedule normally
                schedule_expression(state, expr, new_ctx, result_slot)?;
            }
        }

        WorkItem::CallFunction {
            path,
            arg_slots,
            passed_slot: _,
            passed_context,
            use_piped_for_builtin,
            span,
            persistence,
            mut ctx,
            result_slot,
        } => {
            // Collect arguments that have values (empty slots are ignored)
            let args: Vec<(String, ActorHandle)> = arg_slots
                .iter()
                .filter_map(|(name, slot)| state.get(*slot).map(|actor| (name.clone(), actor)))
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
                            hold_state_update_callback: ctx
                                .actor_context
                                .hold_state_update_callback,
                            use_lazy_actors: ctx.actor_context.use_lazy_actors,
                            is_snapshot_context: ctx.actor_context.is_snapshot_context,
                            object_locals: ctx.actor_context.object_locals,
                            scope: ctx.actor_context.scope,
                            subscription_scope: ctx.actor_context.subscription_scope.clone(),
                            call_recorder: ctx.actor_context.call_recorder,
                            is_restoring: ctx.actor_context.is_restoring,
                            list_append_storage_key: ctx.actor_context.list_append_storage_key,
                            recording_counter: ctx.actor_context.recording_counter,
                            subscription_after_seq: ctx.actor_context.subscription_after_seq,
                            snapshot_emission_seq: ctx.actor_context.snapshot_emission_seq,
                            registry_scope_id: ctx.actor_context.registry_scope_id,
                        },
                        reference_connector: ctx.reference_connector,
                        link_connector: ctx.link_connector,
                        pass_through_connector: ctx.pass_through_connector,
                        function_registry_snapshot: ctx.function_registry_snapshot,
                        current_module: ctx.current_module.clone(),
                        module_loader: ctx.module_loader,
                        source_code: ctx.source_code,
                    };
                }
            }

            let persistence_id = persistence
                .as_ref()
                .expect("persistence should be set by resolver")
                .id;
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

            // If function returns SKIP (None), don't store anything
            if let Some(actor) = actor_opt {
                state.store(result_slot, actor);
            }
        }

        WorkItem::ConnectForwardingActors { connections } => {
            // Connect forwarding actors to their evaluated argument results.
            for (slot, forwarding_actor) in connections {
                if let Some(source_actor) = state.get(slot) {
                    // Retain the forwarding task on the forwarding actor itself so
                    // the function result does not need an extra wrapper actor only
                    // to keep this sidecar task alive.
                    connect_forwarding_replay_all(forwarding_actor.clone(), source_actor.clone());
                }
            }
        }

        WorkItem::WrapWithPersistence {
            source_slot,
            persistence_id,
            ctx,
            result_slot,
            value_changed,
        } => {
            // Wrap an evaluated function argument with persistence.
            // This enables persistence for LATEST and other constructs used as function arguments.
            if LOG_DEBUG {
                zoon::println!(
                    "[DEBUG] Processing WrapWithPersistence for slot {:?} with id {}",
                    source_slot,
                    persistence_id
                );
            }
            if let Some(source_actor) = state.get(source_slot) {
                if LOG_DEBUG {
                    zoon::println!("[DEBUG] Found source actor, wrapping with persistence");
                }
                let persistence_stream = create_variable_persistence_stream(
                    source_actor.clone(),
                    ctx.construct_context.construct_storage.clone(),
                    persistence_id,
                    ctx.actor_context.scope.clone(),
                    ctx.construct_context.clone(),
                    ctx.actor_context.clone(),
                    value_changed,
                );

                let persisted_actor = create_actor(
                    persistence_stream,
                    persistence_id,
                    ctx.actor_context.scope_id(),
                );

                state.store(result_slot, persisted_actor);
            }
        }
    }

    Ok(())
}

/// Create a binary operation actor (arithmetic or comparison).
fn create_binary_op_actor(
    op: BinaryOpKind,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    a: ActorHandle,
    b: ActorHandle,
) -> ActorHandle {
    match op {
        BinaryOpKind::Add => ArithmeticCombinator::new_add(construct_context, actor_context, a, b),
        BinaryOpKind::Subtract => {
            ArithmeticCombinator::new_subtract(construct_context, actor_context, a, b)
        }
        BinaryOpKind::Multiply => {
            ArithmeticCombinator::new_multiply(construct_context, actor_context, a, b)
        }
        BinaryOpKind::Divide => {
            ArithmeticCombinator::new_divide(construct_context, actor_context, a, b)
        }
        BinaryOpKind::Equal => {
            ComparatorCombinator::new_equal(construct_context, actor_context, a, b)
        }
        BinaryOpKind::NotEqual => {
            ComparatorCombinator::new_not_equal(construct_context, actor_context, a, b)
        }
        BinaryOpKind::Greater => {
            ComparatorCombinator::new_greater(construct_context, actor_context, a, b)
        }
        BinaryOpKind::GreaterOrEqual => {
            ComparatorCombinator::new_greater_or_equal(construct_context, actor_context, a, b)
        }
        BinaryOpKind::Less => {
            ComparatorCombinator::new_less(construct_context, actor_context, a, b)
        }
        BinaryOpKind::LessOrEqual => {
            ComparatorCombinator::new_less_or_equal(construct_context, actor_context, a, b)
        }
    }
}

/// Evaluate an Alias expression immediately (for simple cases).
fn evaluate_alias_immediate(
    alias: static_expression::Alias,
    _span: Span,
    _persistence: Option<Persistence>,
    _persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<ActorHandle, String> {
    type BoxedFuture = Pin<Box<dyn std::future::Future<Output = ActorHandle>>>;

    let (root_value_actor, root_should_use_subscription_after_seq): (BoxedFuture, bool) =
        match alias.clone() {
            static_expression::Alias::WithPassed { extra_parts: _ } => {
                match &ctx.actor_context.passed {
                    Some(passed) => {
                        let passed = passed.clone();
                        // PASSED is a stable root value supplied by the caller, not an arm-local
                        // event stream. Filtering it by the caller's subscription_after_seq can drop the
                        // entire already-constructed state/object graph for late-mounted dynamic UI,
                        // for example `PASSED.store.todos` inside TodoMVC helper functions.
                        (Box::pin(async move { passed }), false)
                    }
                    None => {
                        return Err("PASSED is not available in this context".to_string());
                    }
                }
            }
            static_expression::Alias::WithoutPassed {
                parts,
                referenced_span,
            } => {
                let first_part = parts.first().map(|s| s.to_string()).unwrap_or_default();
                if let Some(param_actor) = ctx.actor_context.parameters.get(&first_part).cloned() {
                    // For simple parameter references (no field accesses), return directly
                    if parts.len() == 1 {
                        return Ok(param_actor);
                    }
                    // For multi-part aliases (e.g., state.current), wrap in async Future
                    (Box::pin(async move { param_actor }), false)
                } else if let Some(ref_span) = referenced_span {
                    // First check object_locals for instance-specific resolution
                    // This prevents span-based overwrites when multiple Objects are created
                    // from the same function definition (Bug 7.2 fix)
                    if let Some(local_actor) =
                        ctx.actor_context.object_locals.get(&ref_span).cloned()
                    {
                        // Object-local roots participate in the same live dataflow as parameter
                        // and referenced roots. Filtering them by subscription_after_seq can starve
                        // late-linked structural values like `todo_elements.todo_checkbox`,
                        // where the object shell exists first and the linked element arrives
                        // shortly afterwards through `value |> LINK { ... }`.
                        if ctx.actor_context.is_snapshot_context {
                            let construct_context = ctx.construct_context.clone();
                            let actor_context = ctx.actor_context.clone();
                            let label = format!("snapshot alias root {first_part}");
                            (
                                Box::pin(async move {
                                    freeze_snapshot_root_actor(
                                        local_actor,
                                        construct_context,
                                        actor_context,
                                        label,
                                    )
                                    .await
                                }),
                                false,
                            )
                        } else {
                            (Box::pin(async move { local_actor }), false)
                        }
                    } else {
                        // Fall back to async lookup via ReferenceConnector
                        let ref_connector = ctx.try_reference_connector().ok_or_else(|| {
                            "ReferenceConnector dropped - program shutting down".to_string()
                        })?;
                        if ctx.actor_context.is_snapshot_context {
                            let construct_context = ctx.construct_context.clone();
                            let actor_context = ctx.actor_context.clone();
                            let label = format!("snapshot alias root {first_part}");
                            (
                                Box::pin(async move {
                                    let actor = ref_connector.referenceable(ref_span).await;
                                    freeze_snapshot_root_actor(
                                        actor,
                                        construct_context,
                                        actor_context,
                                        label,
                                    )
                                    .await
                                }),
                                false,
                            )
                        } else {
                            (Box::pin(ref_connector.referenceable(ref_span)), false)
                        }
                    }
                } else if parts.len() >= 2 {
                    // Module variable access - for now fall back to returning an error
                    return Err(format!(
                        "Module variable access '{}' not yet supported in stack-safe evaluator",
                        first_part
                    ));
                } else {
                    return Err(format!("Failed to get aliased variable '{}'", first_part));
                }
            }
        };

    let mut actor_context = ctx.actor_context;
    if !root_should_use_subscription_after_seq {
        actor_context.subscription_after_seq = None;
    }

    Ok(VariableOrArgumentReference::new_arc_value_actor(
        actor_context,
        alias,
        root_value_actor,
    ))
}

async fn freeze_snapshot_root_actor(
    actor: ActorHandle,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    _label: String,
) -> ActorHandle {
    if let Some(frozen_actor) =
        try_freeze_snapshot_actor_now(&actor, construct_context.clone(), actor_context.clone())
    {
        return frozen_actor;
    }

    let snapshot_value = snapshot_current_value(&actor, &actor_context);

    match snapshot_value {
        Ok(current_value) => {
            let frozen_value =
                materialize_snapshot_value(current_value, construct_context, actor_context.clone())
                    .await;
            create_constant_actor(PersistenceId::new(), frozen_value, actor_context.scope_id())
        }
        Err(_) => actor,
    }
}

fn snapshot_current_value(
    actor: &ActorHandle,
    actor_context: &ActorContext,
) -> Result<Value, CurrentValueError> {
    if let Some(emission_seq) = actor_context.snapshot_emission_seq {
        actor.current_value_before_emission(emission_seq)
    } else {
        actor.current_value()
    }
}

fn try_freeze_snapshot_actor_now(
    actor: &ActorHandle,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Option<ActorHandle> {
    let current_value = snapshot_current_value(actor, &actor_context).ok()?;
    let frozen_value = try_materialize_snapshot_value_now(
        current_value,
        construct_context,
        actor_context.clone(),
    )?;
    Some(create_constant_actor(
        PersistenceId::new(),
        frozen_value,
        actor_context.scope_id(),
    ))
}

fn one_shot_actor_value_stream<K: 'static>(
    result_actor: ActorHandle,
    keepalive: K,
) -> LocalBoxStream<'static, Value> {
    match result_actor.current_value() {
        Ok(value) => stream::once(future::ready((value, keepalive)))
            .map(|(value, _keepalive)| value)
            .boxed_local(),
        Err(CurrentValueError::ActorDropped) => stream::empty().boxed_local(),
        Err(CurrentValueError::NoValueYet) => stream::once(async move {
            let _keepalive = &keepalive;
            result_actor.value().await.ok()
        })
        .filter_map(future::ready)
        .boxed_local(),
    }
}

/// Build a THEN actor (runtime evaluation of body for each piped value).
fn build_then_actor(
    body: static_expression::Spanned<static_expression::Expression>,
    _span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    function_registry_snapshot: Arc<HashMap<String, StaticFunctionDefinition>>,
) -> Result<ActorHandle, String> {
    let piped = ctx
        .actor_context
        .piped
        .clone()
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
    let current_module_for_then = ctx.current_module.clone();
    let persistence_for_then = persistence.clone();
    // Clone backpressure_permit for the closure
    let backpressure_permit_for_then = backpressure_permit.clone();

    // Clone HOLD callback for state updates
    let hold_callback_for_then = actor_context_for_then.hold_state_update_callback.clone();

    // eval_body now returns a Stream instead of Option<Value>
    // This avoids blocking on .next().await which would hang if body returns SKIP
    let eval_body =
        move |value: Value| -> Pin<Box<dyn Future<Output = Pin<Box<dyn Stream<Item = Value>>>>>> {
            let actor_context_clone = actor_context_for_then.clone();
            let construct_context_clone = construct_context_for_then.clone();
            let reference_connector_clone = reference_connector_for_then.clone();
            let link_connector_clone = link_connector_for_then.clone();
            let pass_through_connector_clone = pass_through_connector_for_then.clone();
            let function_registry_clone = function_registry_for_then.clone();
            let module_loader_clone = module_loader_for_then.clone();
            let source_code_clone = source_code_for_then.clone();
            let current_module_clone = current_module_for_then.clone();
            let persistence_clone = persistence_for_then.clone();
            let body_clone = body.clone();
            let permit_clone = backpressure_permit_for_then.clone();
            let hold_callback_clone = hold_callback_for_then.clone();
            let should_materialize = permit_clone.is_some();
            let source_emission = branch_condition_emission_seq(&value);
            Box::pin(async move {
                // Acquire permit BEFORE body evaluation - this ensures HOLD's state update
                // completes before we read state for the next iteration. Without this,
                // all pulses would run in parallel reading the same initial state.
                if let Some(ref permit) = permit_clone {
                    permit.acquire().await;
                }

                let value_actor: ActorHandle = create_constant_actor(
                    PersistenceId::new(),
                    value,
                    actor_context_clone.scope_id(),
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
                let mut frozen_parameters: HashMap<String, ActorHandle> = HashMap::new();
                for (name, actor) in actor_context_clone.parameters.iter() {
                    if let Some(frozen_actor) = try_freeze_snapshot_actor_now(
                        actor,
                        construct_context_clone.clone(),
                        actor_context_clone.clone(),
                    ) {
                        frozen_parameters.insert(name.clone(), frozen_actor);
                    } else if let Ok(current_value) =
                        snapshot_current_value(actor, &actor_context_clone)
                    {
                        let frozen_value = materialize_snapshot_value(
                            current_value,
                            construct_context_clone.clone(),
                            actor_context_clone.clone(),
                        )
                        .await;
                        let frozen_actor = create_constant_actor(
                            PersistenceId::new(),
                            frozen_value,
                            actor_context_clone.scope_id(),
                        );
                        frozen_parameters.insert(name.clone(), frozen_actor);
                    } else {
                        // No value yet, keep original actor
                        frozen_parameters.insert(name.clone(), actor.clone());
                    }
                }

                let frozen_passed = if let Some(passed_actor) = actor_context_clone.passed.clone() {
                    if let Some(frozen_actor) = try_freeze_snapshot_actor_now(
                        &passed_actor,
                        construct_context_clone.clone(),
                        actor_context_clone.clone(),
                    ) {
                        Some(frozen_actor)
                    } else if let Ok(current_value) =
                        snapshot_current_value(&passed_actor, &actor_context_clone)
                    {
                        let frozen_value = materialize_snapshot_value(
                            current_value,
                            construct_context_clone.clone(),
                            actor_context_clone.clone(),
                        )
                        .await;
                        Some(create_constant_actor(
                            PersistenceId::new(),
                            frozen_value,
                            actor_context_clone.scope_id(),
                        ))
                    } else {
                        Some(passed_actor)
                    }
                } else {
                    None
                };

                let mut frozen_object_locals: HashMap<Span, ActorHandle> = HashMap::new();
                for (span, actor) in actor_context_clone.object_locals.iter() {
                    if let Some(frozen_actor) = try_freeze_snapshot_actor_now(
                        actor,
                        construct_context_clone.clone(),
                        actor_context_clone.clone(),
                    ) {
                        frozen_object_locals.insert(span.clone(), frozen_actor);
                    } else if let Ok(current_value) =
                        snapshot_current_value(actor, &actor_context_clone)
                    {
                        let frozen_value = materialize_snapshot_value(
                            current_value,
                            construct_context_clone.clone(),
                            actor_context_clone.clone(),
                        )
                        .await;
                        let frozen_actor = create_constant_actor(
                            PersistenceId::new(),
                            frozen_value,
                            actor_context_clone.scope_id(),
                        );
                        frozen_object_locals.insert(span.clone(), frozen_actor);
                    } else {
                        frozen_object_locals.insert(span.clone(), actor.clone());
                    }
                }

                let new_actor_context = ActorContext {
                    output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                    piped: Some(value_actor.clone()),
                    passed: frozen_passed,
                    parameters: Arc::new(frozen_parameters),
                    sequential_processing: actor_context_clone.sequential_processing,
                    backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                    // Don't propagate callback to body - body evaluation is internal
                    hold_state_update_callback: None,
                    use_lazy_actors: actor_context_clone.use_lazy_actors,
                    // THEN body needs snapshot semantics - read current variable values, not history
                    is_snapshot_context: true,
                    // Snapshot object_locals too, so THEN bodies don't observe sibling/object
                    // fields changing underneath a shared event as other subscribers update.
                    object_locals: Arc::new(frozen_object_locals),
                    scope: actor_context_clone.scope.clone(),
                    subscription_scope: actor_context_clone.subscription_scope.clone(),
                    call_recorder: actor_context_clone.call_recorder.clone(),
                    is_restoring: actor_context_clone.is_restoring,
                    list_append_storage_key: actor_context_clone.list_append_storage_key.clone(),
                    recording_counter: actor_context_clone.recording_counter.clone(),
                    // THEN body uses snapshot semantics for variables - don't filter stale values
                    // The filtering should only happen on the piped stream, not all variable refs
                    subscription_after_seq: None,
                    snapshot_emission_seq: Some(source_emission),
                    registry_scope_id: actor_context_clone.registry_scope_id,
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
                    current_module: current_module_clone,
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
                        let hold_callback_for_map = hold_callback_clone.clone();
                        let construct_context_for_map = construct_context_clone.clone();
                        let actor_context_for_map = new_actor_context.clone();
                        let result_stream = one_shot_actor_value_stream(
                            result_actor.clone(),
                            (result_actor, value_actor.clone(), new_actor_context.clone()),
                        )
                        .then(move |mut result_value| {
                            let _value_actor = &value_actor;
                            let hold_callback_for_map = hold_callback_for_map.clone();
                            let construct_context_for_map = construct_context_for_map.clone();
                            let actor_context_for_map = actor_context_for_map.clone();
                            let source_emission = source_emission;
                            async move {
                                preserve_emission_seq(&mut result_value, source_emission);
                                if should_materialize {
                                    result_value = if let Some(materialized_now) =
                                        try_materialize_value_now(
                                            result_value.clone(),
                                            construct_context_for_map.clone(),
                                            actor_context_for_map.clone(),
                                        ) {
                                        materialized_now
                                    } else {
                                        materialize_value(
                                            result_value,
                                            construct_context_for_map,
                                            actor_context_for_map,
                                        )
                                        .await
                                    };
                                    preserve_emission_seq(&mut result_value, source_emission);
                                }
                                // CRITICAL: Call HOLD's callback synchronously if present.
                                // This updates state_actor and releases the permit BEFORE this stream yields,
                                // enabling the next pulse to be processed synchronously during eager polling.
                                if let Some(ref callback) = hold_callback_for_map {
                                    callback(result_value.clone());
                                }
                                result_value
                            }
                        });
                        Box::pin(result_stream) as Pin<Box<dyn Stream<Item = Value>>>
                    }
                    Ok(None) => {
                        // SKIP - return finite empty stream (flatten_unordered removes it cleanly)
                        Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>
                    }
                    Err(_e) => Box::pin(stream::empty()) as Pin<Box<dyn Stream<Item = Value>>>,
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
    // EMISSION FILTERING: Record the current emission-sequence floor when this THEN is created.
    // Any values from the piped stream that were already emitted at or before that
    // sequence are stale
    // (e.g., old click events) and should not trigger body evaluation.
    // This fixes the Toggle All bug where new todos receive old toggle events.
    let subscription_after_seq = current_emission_seq();
    let filtered_piped = piped.clone().stream().filter(move |value| {
        let should_pass = value.is_emitted_after(subscription_after_seq);
        future::ready(should_pass)
    });

    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> =
        if backpressure_permit.is_some() || sequential {
            // For sequential mode, use regular flatten (processes one stream at a time)
            let stream = filtered_piped.then(eval_body).flatten();
            Box::pin(stream)
        } else {
            // For non-sequential mode, use flatten_unordered for concurrent processing
            let stream = filtered_piped.then(eval_body).flatten_unordered(None);
            Box::pin(stream)
        };

    // Use lazy actor construction when in HOLD body context for sequential state updates
    if ctx.actor_context.use_lazy_actors {
        let scope_id = ctx.actor_context.scope_id();
        Ok(create_actor_lazy(
            flattened_stream,
            persistence_id,
            scope_id,
        ))
    } else {
        let scope_id = ctx.actor_context.scope_id();
        Ok(create_actor(flattened_stream, persistence_id, scope_id))
    }
}

/// Build a WHEN actor (pattern matching on piped values).
#[derive(Clone, PartialEq, Eq)]
enum BranchConditionDedupKey {
    Text(String),
    Tag(String),
    Number(u64),
}

#[derive(Clone, PartialEq, Eq)]
enum BranchConditionEmissionDedupKey {
    Text(String, EmissionSeq),
    Tag(String, EmissionSeq),
    Number(u64, EmissionSeq),
}

fn branch_condition_emission_seq(value: &Value) -> EmissionSeq {
    value.emission_seq()
}

fn preserve_emission_seq(value: &mut Value, emission_seq: EmissionSeq) {
    value.set_emission_seq(emission_seq);
}

fn branch_condition_dedup_key(value: &Value) -> Option<BranchConditionDedupKey> {
    match value {
        Value::Text(text, _) => Some(BranchConditionDedupKey::Text(text.text().to_string())),
        Value::Tag(tag, _) => Some(BranchConditionDedupKey::Tag(tag.tag().to_string())),
        Value::Number(number, _) => {
            Some(BranchConditionDedupKey::Number(number.number().to_bits()))
        }
        Value::Flushed(inner, _) => branch_condition_dedup_key(inner),
        Value::Object(_, _) | Value::TaggedObject(_, _) | Value::List(_, _) => None,
    }
}

fn branch_condition_emission_dedup_key(value: &Value) -> Option<BranchConditionEmissionDedupKey> {
    let emission_seq = branch_condition_emission_seq(value);
    match value {
        Value::Text(text, _) => Some(BranchConditionEmissionDedupKey::Text(
            text.text().to_string(),
            emission_seq,
        )),
        Value::Tag(tag, _) => Some(BranchConditionEmissionDedupKey::Tag(
            tag.tag().to_string(),
            emission_seq,
        )),
        Value::Number(number, _) => Some(BranchConditionEmissionDedupKey::Number(
            number.number().to_bits(),
            emission_seq,
        )),
        Value::Flushed(inner, _) => branch_condition_emission_dedup_key(inner),
        Value::Object(_, _) | Value::TaggedObject(_, _) | Value::List(_, _) => None,
    }
}

fn build_when_actor(
    arms: Vec<static_expression::Arm>,
    _span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    function_registry_snapshot: Arc<HashMap<String, StaticFunctionDefinition>>,
) -> Result<ActorHandle, String> {
    let piped = ctx
        .actor_context
        .piped
        .clone()
        .ok_or("WHEN requires a piped value")?;

    let should_materialize = ctx.actor_context.backpressure_permit.is_some();

    let construct_context_for_when = ctx.construct_context.clone();
    let actor_context_for_when = ctx.actor_context.clone();
    let reference_connector_for_when = ctx.reference_connector.clone();
    let link_connector_for_when = ctx.link_connector.clone();
    let pass_through_connector_for_when = ctx.pass_through_connector.clone();
    let function_registry_for_when = function_registry_snapshot;
    let module_loader_for_when = ctx.module_loader.clone();
    let source_code_for_when = ctx.source_code.clone();
    let current_module_for_when = ctx.current_module.clone();
    let persistence_for_when = persistence.clone();
    // eval_body returns a Stream instead of Option<Value>
    // This allows nested WHENs that return SKIP to work correctly:
    // - SKIP returns empty stream (no blocking)
    // - Regular values return stream with one item
    // - flat_map naturally handles empty streams
    let eval_body =
        move |value: Value| -> Pin<Box<dyn Future<Output = Pin<Box<dyn Stream<Item = Value>>>>>> {
            let actor_context_clone = actor_context_for_when.clone();
            let construct_context_clone = construct_context_for_when.clone();
            let reference_connector_clone = reference_connector_for_when.clone();
            let link_connector_clone = link_connector_for_when.clone();
            let pass_through_connector_clone = pass_through_connector_for_when.clone();
            let function_registry_clone = function_registry_for_when.clone();
            let module_loader_clone = module_loader_for_when.clone();
            let source_code_clone = source_code_for_when.clone();
            let current_module_clone = current_module_for_when.clone();
            let _persistence_clone = persistence_for_when.clone();
            let arms_clone = arms.clone();
            let source_emission = branch_condition_emission_seq(&value);

            Box::pin(async move {
                // Debug: log what WHEN receives
                let value_desc = match &value {
                    Value::Tag(tag, _) => format!("Tag({})", tag.tag()),
                    Value::Object(_, _) => "Object".to_string(),
                    Value::Text(t, _) => format!("Text({})", t.text()),
                    Value::List(_, _) => "List".to_string(),
                    _ => "Other".to_string(),
                };
                if LOG_DEBUG {
                    zoon::println!("[WHEN] Received value: {}", value_desc);
                }
                // Pre-resolve ValueComparison patterns from scope
                let comparison_values = resolve_comparison_values(
                    &arms_clone,
                    &actor_context_clone,
                    &reference_connector_clone,
                )
                .await;

                // Try to match against each arm
                for (arm_idx, arm) in arms_clone.iter().enumerate() {
                    // Use async pattern matching to properly extract bindings from Objects
                    if let Some(bindings) =
                        match_pattern(&arm.pattern, &value, &comparison_values).await
                    {
                        if LOG_DEBUG {
                            zoon::println!("[WHEN] Pattern MATCHED: {:?}", arm.pattern);
                        }
                        let arm_scope = Arc::new(SubscriptionScope::new());
                        let scope_guard = ScopeGuard::new(arm_scope.clone());
                        let arm_registry_scope =
                            actor_context_clone.registry_scope_id.map(|parent_scope| {
                                let scope_id = create_registry_scope(Some(parent_scope));
                                (scope_id, ScopeDestroyGuard::new(scope_id))
                            });
                        let value_actor: ActorHandle = create_constant_actor(
                            PersistenceId::new(),
                            value.clone(),
                            actor_context_clone.scope_id(),
                        );

                        // CRITICAL FIX: Freeze parameters for SNAPSHOT semantics (same as THEN).
                        // When WHEN body references `state` (from HOLD), we want the CURRENT value at the
                        // time of body evaluation, not all historical values from the reactive subscription.
                        let mut frozen_parameters: HashMap<String, ActorHandle> = HashMap::new();
                        for (name, actor) in actor_context_clone.parameters.iter() {
                            if let Some(frozen_actor) = try_freeze_snapshot_actor_now(
                                actor,
                                construct_context_clone.clone(),
                                actor_context_clone.clone(),
                            ) {
                                frozen_parameters.insert(name.clone(), frozen_actor);
                            } else if let Ok(current_value) =
                                snapshot_current_value(actor, &actor_context_clone)
                            {
                                let frozen_value = materialize_snapshot_value(
                                    current_value,
                                    construct_context_clone.clone(),
                                    actor_context_clone.clone(),
                                )
                                .await;
                                let frozen_actor = create_constant_actor(
                                    PersistenceId::new(),
                                    frozen_value,
                                    actor_context_clone.scope_id(),
                                );
                                frozen_parameters.insert(name.clone(), frozen_actor);
                            } else {
                                frozen_parameters.insert(name.clone(), actor.clone());
                            }
                        }

                        // Create parameter actors for the pattern bindings
                        let mut parameters = frozen_parameters;
                        for (name, bound_value) in bindings {
                            let bound_actor = create_constant_actor(
                                PersistenceId::new(),
                                bound_value,
                                actor_context_clone.scope_id(),
                            );
                            parameters.insert(name, bound_actor);
                        }

                        let frozen_passed =
                            if let Some(passed_actor) = actor_context_clone.passed.clone() {
                                if let Some(frozen_actor) = try_freeze_snapshot_actor_now(
                                    &passed_actor,
                                    construct_context_clone.clone(),
                                    actor_context_clone.clone(),
                                ) {
                                    Some(frozen_actor)
                                } else if let Ok(current_value) =
                                    snapshot_current_value(&passed_actor, &actor_context_clone)
                                {
                                    let frozen_value = materialize_snapshot_value(
                                        current_value,
                                        construct_context_clone.clone(),
                                        actor_context_clone.clone(),
                                    )
                                    .await;
                                    Some(create_constant_actor(
                                        PersistenceId::new(),
                                        frozen_value,
                                        actor_context_clone.scope_id(),
                                    ))
                                } else {
                                    Some(passed_actor)
                                }
                            } else {
                                None
                            };

                        let mut frozen_object_locals: HashMap<Span, ActorHandle> = HashMap::new();
                        for (span, actor) in actor_context_clone.object_locals.iter() {
                            if let Some(frozen_actor) = try_freeze_snapshot_actor_now(
                                actor,
                                construct_context_clone.clone(),
                                actor_context_clone.clone(),
                            ) {
                                frozen_object_locals.insert(span.clone(), frozen_actor);
                            } else if let Ok(current_value) =
                                snapshot_current_value(actor, &actor_context_clone)
                            {
                                let frozen_value = materialize_snapshot_value(
                                    current_value,
                                    construct_context_clone.clone(),
                                    actor_context_clone.clone(),
                                )
                                .await;
                                let frozen_actor = create_constant_actor(
                                    PersistenceId::new(),
                                    frozen_value,
                                    actor_context_clone.scope_id(),
                                );
                                frozen_object_locals.insert(span.clone(), frozen_actor);
                            } else {
                                frozen_object_locals.insert(span.clone(), actor.clone());
                            }
                        }

                        let new_actor_context = ActorContext {
                            output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                            piped: Some(value_actor.clone()),
                            passed: frozen_passed,
                            parameters: Arc::new(parameters),
                            sequential_processing: actor_context_clone.sequential_processing,
                            backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                            // Don't propagate HOLD callback into WHEN arms - each arm is a separate evaluation
                            hold_state_update_callback: None,
                            use_lazy_actors: actor_context_clone.use_lazy_actors,
                            // Freeze parameters/PASSED/object locals above, but keep the body itself
                            // in normal streaming mode. Otherwise nested WHEN/WHILE inside a data-style
                            // WHEN arm inherit snapshot mode and collapse to a one-shot value, which
                            // freezes expressions like `has_comma |> WHEN { __ => has_colon |> WHEN {
                            // True => List/range(..) |> List/map(..) |> List/sum() } }`.
                            is_snapshot_context: false,
                            // Snapshot object_locals too, so each arm sees a stable view of
                            // sibling/object fields for the triggering input.
                            object_locals: Arc::new(frozen_object_locals),
                            scope: {
                                use boon::parser::Scope;
                                let scope_id = format!("when_arm_{}", arm_idx);
                                match &actor_context_clone.scope {
                                    Scope::Root => Scope::Nested(scope_id),
                                    Scope::Nested(existing) => {
                                        Scope::Nested(format!("{}:{}", existing, scope_id))
                                    }
                                }
                            },
                            subscription_scope: Some(arm_scope.clone()),
                            call_recorder: actor_context_clone.call_recorder.clone(),
                            is_restoring: actor_context_clone.is_restoring,
                            list_append_storage_key: actor_context_clone
                                .list_append_storage_key
                                .clone(),
                            recording_counter: actor_context_clone.recording_counter.clone(),
                            // WHEN body uses snapshot semantics for variables - don't filter stale values
                            // The filtering should only happen on the piped stream, not all variable refs
                            subscription_after_seq: None,
                            snapshot_emission_seq: Some(source_emission),
                            registry_scope_id: arm_registry_scope
                                .as_ref()
                                .map(|(id, _)| *id)
                                .or(actor_context_clone.registry_scope_id),
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
                            current_module: current_module_clone.clone(),
                        };

                        match evaluate_expression(arm.body.clone(), new_ctx) {
                            Ok(Some(result_actor)) => {
                                let body_stream = if actor_context_clone.is_snapshot_context {
                                    // Snapshot contexts (for example THEN/HOLD one-shot bodies)
                                    // need a single resolved branch value, not an ongoing reactive
                                    // matched-arm stream. Keeping the arm live here can recursively
                                    // spawn work while a one-shot `.value()` caller is still waiting.
                                    let construct_context_for_map = construct_context_clone.clone();
                                    let actor_context_for_map = new_actor_context.clone();
                                    one_shot_actor_value_stream(result_actor.clone(), result_actor)
                                        .then(move |mut result_value| {
                                            let construct_context_for_map =
                                                construct_context_for_map.clone();
                                            let actor_context_for_map =
                                                actor_context_for_map.clone();
                                            let source_emission = source_emission;
                                            async move {
                                                preserve_emission_seq(
                                                    &mut result_value,
                                                    source_emission,
                                                );
                                                if should_materialize {
                                                    result_value = if let Some(materialized_now) =
                                                        try_materialize_value_now(
                                                            result_value.clone(),
                                                            construct_context_for_map.clone(),
                                                            actor_context_for_map.clone(),
                                                        ) {
                                                        materialized_now
                                                    } else {
                                                        materialize_value(
                                                            result_value,
                                                            construct_context_for_map,
                                                            actor_context_for_map,
                                                        )
                                                        .await
                                                    };
                                                    preserve_emission_seq(
                                                        &mut result_value,
                                                        source_emission,
                                                    );
                                                }
                                                result_value
                                            }
                                        })
                                        .boxed_local()
                                } else {
                                    // Keep streaming the matched arm while it stays selected.
                                    // This is required for data-style WHENs like:
                                    // `has_comma |> WHEN { True => compute_value(cell_formula(...)) }`
                                    // where the branch condition stays matched but the body should
                                    // still react to downstream changes (for example A1 -> B1/C1 in Cells).
                                    //
                                    // Snapshot semantics for event-style WHENs are still preserved by
                                    // freezing parameters / PASSED above at match time.
                                    let construct_context_for_map = construct_context_clone.clone();
                                    let actor_context_for_map = new_actor_context.clone();
                                    result_actor
                                        .stream()
                                        .then(move |mut result_value| {
                                            let construct_context_for_map =
                                                construct_context_for_map.clone();
                                            let actor_context_for_map =
                                                actor_context_for_map.clone();
                                            async move {
                                                let emission = result_value.emission_seq();
                                                if should_materialize {
                                                    result_value = if let Some(materialized_now) =
                                                        try_materialize_value_now(
                                                            result_value.clone(),
                                                            construct_context_for_map.clone(),
                                                            actor_context_for_map.clone(),
                                                        ) {
                                                        materialized_now
                                                    } else {
                                                        materialize_value(
                                                            result_value,
                                                            construct_context_for_map,
                                                            actor_context_for_map,
                                                        )
                                                        .await
                                                    };
                                                    preserve_emission_seq(
                                                        &mut result_value,
                                                        emission,
                                                    );
                                                }
                                                result_value
                                            }
                                        })
                                        .scan(None::<EmissionSeq>, |last_emission, result_value| {
                                            let current_emission = result_value.emission_seq();
                                            let should_emit =
                                                Some(current_emission) != *last_emission;
                                            *last_emission = Some(current_emission);
                                            future::ready(Some(if should_emit {
                                                Some(result_value)
                                            } else {
                                                None
                                            }))
                                        })
                                        .filter_map(future::ready)
                                        .boxed_local()
                                };
                                return stream::unfold(
                                    (body_stream, Some(scope_guard), arm_registry_scope),
                                    |(mut s, guard, reg_scope)| async move {
                                        s.next().await.map(|v| (v, (s, guard, reg_scope)))
                                    },
                                )
                                .boxed_local();
                            }
                            Ok(None) => {
                                // SKIP - return finite empty stream (flatten_unordered removes it cleanly)
                                drop(scope_guard);
                                drop(arm_registry_scope);
                                return Box::pin(stream::empty())
                                    as Pin<Box<dyn Stream<Item = Value>>>;
                            }
                            Err(_e) => {
                                drop(scope_guard);
                                drop(arm_registry_scope);
                                return Box::pin(stream::empty())
                                    as Pin<Box<dyn Stream<Item = Value>>>;
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
    // WHEN should keep the currently matched arm live, but cancel the old arm when
    // the piped input changes and a different arm becomes active.
    //
    // This gives the expected data semantics:
    // - `count |> WHEN { 1 => "1 item", n => "{n} items" }` updates when count changes
    // - `has_comma |> WHEN { True => expensive_branch(...) }` keeps the branch reactive
    //   while `has_comma` stays True
    // - once the match flips, the old branch stops contributing values
    //
    // That last point is critical for Cells editing state, where stale `True` branches
    // must not keep an input mounted after Escape switches the condition back to False.
    // Helper-produced boolean/tag conditions can emit the same scalar value repeatedly during
    // startup. If WHEN keeps restarting before the matched branch yields its first value, nested
    // helpers feeding WHILE/element paths can disappear entirely. Suppress only consecutive
    // duplicate scalar inputs here; complex values still pass through unchanged.
    let seeded_condition_stream = piped.current_or_future_stream();

    let deduped_condition_stream = seeded_condition_stream
        .scan(
            None::<BranchConditionEmissionDedupKey>,
            move |last_key, value| {
                let current_key = branch_condition_emission_dedup_key(&value);
                let should_emit = current_key.as_ref() != last_key.as_ref();
                *last_key = current_key;
                future::ready(Some(if should_emit { Some(value) } else { None }))
            },
        )
        .filter_map(future::ready);

    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> =
        Box::pin(switch_map(deduped_condition_stream, move |value| {
            stream::once(eval_body(value)).flatten()
        }));

    let scope_id = ctx.actor_context.scope_id();
    Ok(create_actor(flattened_stream, persistence_id, scope_id))
}

/// Build a WHILE actor (continuous processing while pattern matches).
fn build_while_actor(
    arms: Vec<static_expression::Arm>,
    _span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    function_registry_snapshot: Arc<HashMap<String, StaticFunctionDefinition>>,
) -> Result<ActorHandle, String> {
    let piped = ctx
        .actor_context
        .piped
        .clone()
        .ok_or("WHILE requires a piped value")?;
    let should_materialize = ctx.actor_context.backpressure_permit.is_some();

    let construct_context_for_while = ctx.construct_context.clone();
    let actor_context_for_while = Arc::new(ctx.actor_context.clone());
    let reference_connector_for_while = ctx.reference_connector.clone();
    let link_connector_for_while = ctx.link_connector.clone();
    let pass_through_connector_for_while = ctx.pass_through_connector.clone();
    let function_registry_for_while = function_registry_snapshot;
    let module_loader_for_while = ctx.module_loader.clone();
    let source_code_for_while = ctx.source_code.clone();
    let current_module_for_while = ctx.current_module.clone();
    let persistence_for_while = persistence.clone();
    let arms_for_while = Arc::new(arms);

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
    // Helper-produced boolean/tag conditions can emit the same scalar value more than once
    // during startup. If WHILE restarts the arm before the branch yields its first element,
    // the visible output goes blank and the page churns. Suppress only consecutive duplicate
    // scalar conditions here; complex values still pass through unchanged.
    let seeded_condition_stream = piped.current_or_future_stream();

    let deduped_condition_stream = seeded_condition_stream
        .scan(None::<BranchConditionDedupKey>, move |last_key, value| {
            let current_key = branch_condition_dedup_key(&value);
            let should_emit = current_key.as_ref() != last_key.as_ref();
            *last_key = current_key;
            future::ready(Some(if should_emit { Some(value) } else { None }))
        })
        .filter_map(future::ready);

    let stream = switch_map(deduped_condition_stream, move |value| {
        let actor_context_clone = Arc::clone(&actor_context_for_while);
        let construct_context_clone = construct_context_for_while.clone();
        let reference_connector_clone = reference_connector_for_while.clone();
        let link_connector_clone = link_connector_for_while.clone();
        let pass_through_connector_clone = pass_through_connector_for_while.clone();
        let function_registry_clone = function_registry_for_while.clone();
        let module_loader_clone = module_loader_for_while.clone();
        let source_code_clone = source_code_for_while.clone();
        let current_module_clone = current_module_for_while.clone();
        let _persistence_clone = persistence_for_while.clone();
        let arms_clone = Arc::clone(&arms_for_while);

        // Wrap async pattern matching in stream::once().flatten() so switch_map can work with it.
        // When a new input arrives, switch_map will drop this whole inner stream (cancelling
        // any async work and the forwarded body stream) and start a new one.
        stream::once(async move {
            // Pre-resolve ValueComparison patterns from scope
            let comparison_values = resolve_comparison_values(
                &arms_clone,
                &actor_context_clone,
                &reference_connector_clone,
            )
            .await;

            // Find matching arm using async pattern matching
            let mut matched_arm_with_bindings: Option<(usize, HashMap<String, Value>)> = None;
            for (arm_idx, arm) in arms_clone.iter().enumerate() {
                if let Some(bindings) =
                    match_pattern(&arm.pattern, &value, &comparison_values).await
                {
                    matched_arm_with_bindings = Some((arm_idx, bindings));
                    break;
                }
            }

            if let Some((arm_idx, bindings)) = matched_arm_with_bindings {
                let arm = &arms_clone[arm_idx];
                let source_emission = value.emission_seq();
                // Create a new subscription scope for this arm
                // The ScopeGuard will cancel the scope when dropped (when switch_map drops the inner stream)
                let arm_scope = Arc::new(SubscriptionScope::new());
                let scope_guard = ScopeGuard::new(arm_scope.clone());

                // Create a registry scope for this arm - all actors created within
                // will be destroyed when the arm switches (ScopeDestroyGuard dropped)
                let arm_registry_scope =
                    actor_context_clone.registry_scope_id.map(|parent_scope| {
                        let scope_id = create_registry_scope(Some(parent_scope));
                        (scope_id, ScopeDestroyGuard::new(scope_id))
                    });

                let value_actor: ActorHandle = create_constant_actor(
                    PersistenceId::new(),
                    value,
                    actor_context_clone.scope_id(),
                );

                let parameters = if bindings.is_empty() {
                    actor_context_clone.parameters.clone()
                } else {
                    let mut parameters = (*actor_context_clone.parameters).clone();
                    for (name, bound_value) in bindings {
                        let bound_actor = create_constant_actor(
                            PersistenceId::new(),
                            bound_value,
                            actor_context_clone.scope_id(),
                        );
                        parameters.insert(name, bound_actor);
                    }
                    Arc::new(parameters)
                };

                let new_actor_context = ActorContext {
                    output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                    piped: Some(value_actor),
                    passed: actor_context_clone.passed.clone(),
                    parameters,
                    sequential_processing: actor_context_clone.sequential_processing,
                    backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                    // Propagate HOLD callback through WHILE arms - body might need it
                    hold_state_update_callback: actor_context_clone
                        .hold_state_update_callback
                        .clone(),
                    use_lazy_actors: actor_context_clone.use_lazy_actors,
                    // WHILE is continuous, not snapshot - variables should stream normally
                    is_snapshot_context: false,
                    // Inherit object_locals - WHILE body may reference Object sibling fields
                    object_locals: actor_context_clone.object_locals.clone(),
                    // Each WHILE arm evaluation gets a fresh nested scope identity.
                    // Reusing the same scope across arm re-evaluations keeps stale local
                    // event variables alive under the same alias path, which can replay old
                    // UI events and drive reopen loops on examples like Cells.
                    scope: {
                        use boon::parser::Scope;
                        let scope_id = format!("while_arm_{}_{}", arm_idx, source_emission);
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
                    // Freshly recreated arm-local variables should ignore stale values from
                    // earlier incarnations of the same arm. Parameter/global references opt
                    // out in evaluate_alias_immediate, but object-local roots keep this filter.
                    subscription_after_seq: Some(source_emission),
                    snapshot_emission_seq: Some(source_emission),
                    // Use the arm's registry scope (if available) so actors are owned by this arm
                    registry_scope_id: arm_registry_scope
                        .as_ref()
                        .map(|(id, _)| *id)
                        .or(actor_context_clone.registry_scope_id),
                };

                let construct_context_for_body_stream = construct_context_clone.clone();
                let actor_context_for_body_stream = new_actor_context.clone();
                let new_ctx = EvaluationContext {
                    construct_context: construct_context_clone,
                    actor_context: new_actor_context,
                    reference_connector: reference_connector_clone,
                    link_connector: link_connector_clone,
                    pass_through_connector: pass_through_connector_clone,
                    module_loader: module_loader_clone,
                    source_code: source_code_clone,
                    function_registry_snapshot: Some(function_registry_clone),
                    current_module: current_module_clone,
                };

                match evaluate_expression(arm.body.clone(), new_ctx) {
                    Ok(Some(result_actor)) => {
                        let construct_context_for_map = construct_context_for_body_stream.clone();
                        let actor_context_for_map = actor_context_for_body_stream.clone();
                        let body_stream = result_actor
                            .stream()
                            .then(move |mut result_value| {
                                let construct_context_for_map = construct_context_for_map.clone();
                                let actor_context_for_map = actor_context_for_map.clone();
                                async move {
                                    let emission = result_value.emission_seq();
                                    if should_materialize {
                                        result_value = if let Some(materialized_now) =
                                            try_materialize_value_now(
                                                result_value.clone(),
                                                construct_context_for_map.clone(),
                                                actor_context_for_map.clone(),
                                            ) {
                                            materialized_now
                                        } else {
                                            materialize_value(
                                                result_value,
                                                construct_context_for_map,
                                                actor_context_for_map,
                                            )
                                            .await
                                        };
                                        preserve_emission_seq(&mut result_value, emission);
                                    }
                                    result_value
                                }
                            })
                            .boxed_local();
                        let body_stream = stream::unfold(
                            (body_stream, None::<EmissionSeq>, None::<Value>),
                            |(mut upstream, mut last_emission, mut last_value)| async move {
                                while let Some(result_value) = upstream.next().await {
                                    let current_emission = result_value.emission_seq();
                                    let same_emission = Some(current_emission) == last_emission;
                                    let structurally_equal = if same_emission {
                                        false
                                    } else if let Some(previous) = last_value.as_ref() {
                                        crate::engine::values_equal_async(previous, &result_value)
                                            .await
                                    } else {
                                        false
                                    };

                                    last_emission = Some(current_emission);
                                    if same_emission || structurally_equal {
                                        continue;
                                    }

                                    last_value = Some(result_value.clone());
                                    return Some((
                                        result_value,
                                        (upstream, last_emission, last_value),
                                    ));
                                }

                                None
                            },
                        )
                        .boxed_local();

                        // Wrap the stream to keep scope_guard and scope_destroy_guard alive -
                        // when this stream is dropped (by switch_map switching to new arm),
                        // both guards are dropped: scope_guard cancels subscriptions,
                        // scope_destroy_guard destroys registry scope and all its actors
                        stream::unfold(
                            (body_stream, Some(scope_guard), arm_registry_scope),
                            |(mut s, guard, reg_scope)| async move {
                                s.next().await.map(|v| (v, (s, guard, reg_scope)))
                            },
                        )
                        .boxed_local()
                    }
                    Ok(None) => {
                        // SKIP - scope_guard dropped here, scope cancelled immediately
                        // arm_registry_scope guard also dropped, destroying the scope
                        drop(scope_guard);
                        drop(arm_registry_scope);
                        stream::empty().boxed_local()
                    }
                    Err(e) => {
                        // Error evaluating body - scope_guard dropped here, scope cancelled
                        drop(scope_guard);
                        zoon::eprintln!("Error evaluating WHILE body: {e}");
                        stream::empty().boxed_local()
                    }
                }
            } else {
                stream::empty().boxed_local()
            }
        })
        .flatten()
    });

    let scope_id = ctx.actor_context.scope_id();
    Ok(create_actor(stream, persistence_id, scope_id))
}

/// Asynchronously extract a field value from a Value following a path of field names.
/// Returns None if the path cannot be fully resolved (e.g., non-object value, missing field).
async fn extract_field_path(value: &Value, path: &[String]) -> Option<Value> {
    let mut current = value.clone();
    for field_name in path {
        match &current {
            Value::Object(object, _) => {
                let variable_actor = object.expect_variable(field_name).value_actor();
                if let Some(val) = actor_current_value_or_wait(&variable_actor).await {
                    current = val;
                } else {
                    // Field actor dropped
                    return None;
                }
            }
            Value::TaggedObject(tagged_object, _) => {
                let variable_actor = tagged_object.expect_variable(field_name).value_actor();
                if let Some(val) = actor_current_value_or_wait(&variable_actor).await {
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
    _span: Span,
    _persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<ActorHandle, String> {
    let piped = ctx
        .actor_context
        .piped
        .clone()
        .ok_or("FieldAccess requires a piped value")?;

    let path_display = path.join(".");

    // Seed from the current piped value first, then continue with future updates.
    // Mapped helper calls often receive a fully-constructed object before any live
    // updates occur; using stream() alone can miss that initial object and blank the
    // whole derived subtree.
    let mut value_stream: LocalBoxStream<'static, Value> = piped.current_or_future_stream();

    // Chain switch_map for EACH field - THE KEY FIX!
    // When ANY field emits a new value, downstream switch_maps automatically
    // cancel old subscriptions and re-subscribe to the new value.
    // This prevents stale subscriptions when intermediate elements are recreated.
    for (idx, field_name) in path.iter().enumerate() {
        let field_name = field_name.clone();
        let path_display_for_log = if LOG_DEBUG {
            Some(path_display.clone())
        } else {
            None
        };
        let field_idx = idx;

        value_stream = switch_map(value_stream, move |value| {
            let field_name = field_name.clone();

            if LOG_DEBUG {
                let path_display = path_display_for_log.as_deref().unwrap_or("");
                let value_type = match &value {
                    Value::Object(_, _) => "Object",
                    Value::TaggedObject(tagged, _) => tagged.tag(),
                    Value::Tag(tag, _) => tag.tag(),
                    Value::Text(_, _) => "Text",
                    Value::Number(_, _) => "Number",
                    _ => "Other",
                };
                zoon::println!(
                    "[FIELD_ACCESS] .{} step {}: received {} for field '{}'",
                    path_display,
                    field_idx,
                    value_type,
                    field_name
                );
            }

            let variable = match &value {
                Value::Object(object, _) => object.variable(&field_name),
                Value::TaggedObject(tagged, _) => tagged.variable(&field_name),
                _ => None,
            };

            if let Some(var) = variable {
                let value_actor = var.value_actor();
                if LOG_DEBUG {
                    let path_display = path_display_for_log.as_deref().unwrap_or("");
                    zoon::println!(
                        "[FIELD_ACCESS] .{} step {}: found field '{}', subscribing to actor",
                        path_display,
                        field_idx,
                        field_name
                    );
                }
                value_actor.current_or_future_stream()
            } else {
                if LOG_DEBUG {
                    let path_display = path_display_for_log.as_deref().unwrap_or("");
                    let value_type = match &value {
                        Value::Object(_, _) => "Object",
                        Value::TaggedObject(tagged, _) => tagged.tag(),
                        Value::Tag(tag, _) => tag.tag(),
                        Value::Text(_, _) => "Text",
                        Value::Number(_, _) => "Number",
                        _ => "Other",
                    };
                    zoon::println!(
                        "[FIELD_ACCESS] .{} step {}: field '{}' NOT FOUND in {}",
                        path_display,
                        field_idx,
                        field_name,
                        value_type
                    );
                }
                // Field not found - emit empty stream (switch_map handles this gracefully)
                stream::empty().boxed_local()
            }
        });
    }

    let scope_id = ctx.actor_context.scope_id();
    Ok(create_actor(value_stream, persistence_id, scope_id))
}

/// Build a HOLD actor (stateful accumulator).
/// HOLD: `input |> HOLD state_param { body }`
/// The piped value sets/resets the state (not just initial - any emission).
/// The body can reference `state_param` to get the current state.
/// The body expression's result becomes the new state value.
/// CRITICAL: The state is NOT self-reactive - changes to state don't
/// trigger re-evaluation of body. Only external events trigger updates.
fn build_hold_actor(
    initial_actor: ActorHandle,
    state_param: String,
    body: static_expression::Spanned<static_expression::Expression>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Option<ActorHandle>, String> {
    // Apply scope to persistence_id so HOLDs inside user-defined functions
    // get unique storage keys for each call site
    let persistence_id = persistence_id.in_scope(&ctx.actor_context.scope);

    if !expression_references_parameter(&body, &state_param) {
        return build_stateless_hold_actor(
            initial_actor,
            state_param,
            body,
            span,
            persistence,
            persistence_id,
            ctx,
        );
    }

    // Use a bounded channel to hold current state value and broadcast updates
    // Note: Sender::try_send takes &self, so we can just clone the sender
    let (state_sender, state_receiver) = zoon::futures_channel::mpsc::channel::<Value>(16);

    // Get storage for persistence
    let storage = ctx.construct_context.construct_storage.clone();
    let storage_for_state_load = storage.clone();
    let storage_for_initial_save = storage.clone();
    let construct_context_for_state_load = ctx.construct_context.clone();
    let actor_context_for_state_load = ctx.actor_context.clone();

    // Create a ValueActor that provides the current state to the body
    // This is what the state_param references
    //
    // State stream: load stored value first (if exists), else initial_actor, then state_receiver
    let initial_actor_for_state = initial_actor.clone();
    let state_stream = stream::unfold(
        (true, state_receiver), // (is_first, receiver)
        move |(is_first, mut receiver)| {
            let storage = storage_for_state_load.clone();
            let initial_actor = initial_actor_for_state.clone();
            let construct_context = construct_context_for_state_load.clone();
            let actor_context = actor_context_for_state_load.clone();
            async move {
                if is_first {
                    // Try storage first - load persisted state
                    let loaded: Option<zoon::serde_json::Value> =
                        storage.load_state_now(persistence_id);
                    if let Some(json) = loaded {
                        // Deserialize stored value
                        let value = Value::from_json(
                            &json,
                            ConstructId::new("HOLD state restored from storage"),
                            construct_context,
                            ValueIdempotencyKey::new(),
                            actor_context,
                        );
                        return Some((value, (false, receiver)));
                    }
                    // No stored state - fall back to the current initial actor value
                    let initial = actor_current_value_or_wait(&initial_actor).await?;
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
    let state_actor: ActorHandle = create_actor(
        state_stream,
        PersistenceId::new(),
        ctx.actor_context.scope_id(),
    );

    // Bind the state parameter in the context so body can reference it
    let mut body_parameters = (*ctx.actor_context.parameters).clone();
    body_parameters.insert(state_param.clone(), state_actor.clone());

    // Clone state_actor for use in state_update_stream to directly update its stored value
    let state_actor_for_update = state_actor.clone();
    // Clone for the synchronous callback that THEN will use
    let state_actor_for_callback = state_actor;
    let _state_param_for_callback = state_param.clone();

    // Create backpressure coordinator for synchronizing THEN with state updates.
    // The coordinator uses message-based coordination (no shared atomics/Mutex).
    // HOLD's callback releases permit after each state update, allowing next body to run.
    let backpressure_permit = BackpressureCoordinator::new();
    let permit_for_callback = backpressure_permit.clone();

    // Create callback for THEN to update HOLD's state synchronously.
    // This ensures the next body evaluation sees the updated state.
    // NOTE: We do NOT store to output here - state_update_stream handles that.
    // Storing in both places would cause duplicate emissions.
    let hold_state_update_callback: Arc<dyn Fn(Value)> = Arc::new(move |new_value: Value| {
        // Update state_actor's stored value directly - THEN will read from here
        state_actor_for_callback.store_value_directly(new_value);
        // Release permit to allow THEN to process next input
        permit_for_callback.release();
    });

    let body_actor_context = ActorContext {
        output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
        piped: None, // Clear piped - the body shouldn't re-use it
        passed: ctx.actor_context.passed.clone(),
        parameters: Arc::new(body_parameters),
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
        // Inherit parent's subscription_after_seq - HOLD body is streaming context
        subscription_after_seq: ctx.actor_context.subscription_after_seq,
        snapshot_emission_seq: ctx.actor_context.snapshot_emission_seq,
        registry_scope_id: ctx.actor_context.registry_scope_id,
    };

    // Create new context for body evaluation
    let body_ctx = EvaluationContext {
        construct_context: ctx.construct_context.clone(),
        actor_context: body_actor_context,
        reference_connector: ctx.reference_connector.clone(),
        link_connector: ctx.link_connector.clone(),
        pass_through_connector: ctx.pass_through_connector.clone(),
        function_registry_snapshot: ctx.function_registry_snapshot.clone(),
        current_module: ctx.current_module.clone(),
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
    struct HoldOutputEvent {
        value: Value,
        persist: bool,
        reset_state: bool,
        update_state_actor: bool,
    }

    let body_subscription = hold_body_current_and_future_values(body_result.clone());
    let construct_context_for_initial = ctx.construct_context.clone();
    let actor_context_for_initial = ctx.actor_context.clone();
    let storage_for_output = storage.clone();
    let state_sender_for_output = state_sender.clone();
    let state_actor_for_output = state_actor_for_update.clone();
    let scope_id = ctx.actor_context.scope_id();

    enum HoldResetInputState {
        LoadingStored,
        ForwardingSource {
            source_subscription: LocalBoxStream<'static, Value>,
        },
    }

    let initial_actor_for_initial = initial_actor.clone();
    let initial_events = stream::unfold(HoldResetInputState::LoadingStored, move |state| {
        let initial_actor = initial_actor_for_initial.clone();
        let storage = storage_for_initial_save.clone();
        let construct_context = construct_context_for_initial.clone();
        let actor_context = actor_context_for_initial.clone();
        async move {
            match state {
                HoldResetInputState::LoadingStored => {
                    let stored: Option<zoon::serde_json::Value> =
                        storage.load_state_now(persistence_id);
                    if let Some(json) = stored {
                        let restored_value = Value::from_json(
                            &json,
                            ConstructId::new("HOLD restored value"),
                            construct_context,
                            ValueIdempotencyKey::new(),
                            actor_context,
                        );
                        return Some((
                            HoldOutputEvent {
                                value: restored_value,
                                persist: false,
                                reset_state: false,
                                update_state_actor: false,
                            },
                            HoldResetInputState::ForwardingSource {
                                source_subscription: initial_actor.stream_from_now(),
                            },
                        ));
                    }

                    let (first_value, source_subscription) =
                        take_first_seeded_actor_value_and_future_stream(initial_actor).await?;
                    Some((
                        HoldOutputEvent {
                            value: first_value,
                            persist: true,
                            reset_state: false,
                            update_state_actor: false,
                        },
                        HoldResetInputState::ForwardingSource {
                            source_subscription,
                        },
                    ))
                }
                HoldResetInputState::ForwardingSource {
                    mut source_subscription,
                } => {
                    let value = source_subscription.next().await?;
                    Some((
                        HoldOutputEvent {
                            value,
                            persist: true,
                            reset_state: true,
                            update_state_actor: false,
                        },
                        HoldResetInputState::ForwardingSource {
                            source_subscription,
                        },
                    ))
                }
            }
        }
    });

    let state_update_events = body_subscription.map(move |new_value| HoldOutputEvent {
        value: new_value,
        persist: true,
        reset_state: true,
        update_state_actor: true,
    });

    let output_stream = stream::select(initial_events, state_update_events)
        .scan(None::<Value>, move |last_emitted, event| {
            let is_duplicate = last_emitted
                .as_ref()
                .is_some_and(|current| crate::engine::values_equal(current, &event.value));
            let emitted_value = event.value.clone();
            if !is_duplicate {
                *last_emitted = Some(emitted_value.clone());
            }
            let mut state_sender = state_sender_for_output.clone();
            let state_actor = state_actor_for_output.clone();
            let storage = storage_for_output.clone();
            async move {
                if is_duplicate {
                    return Some(None);
                }

                if event.reset_state {
                    if let Err(e) = state_sender.send(event.value.clone()).await {
                        zoon::println!("[HOLD] Failed to send state update: {e}");
                    }
                }
                if event.update_state_actor {
                    state_actor.store_value_directly(event.value.clone());
                }
                if event.persist {
                    let value_json = event.value.to_json().await;
                    storage.save_state(persistence_id, &value_json);
                }

                Some(Some(emitted_value))
            }
        })
        .filter_map(future::ready);

    let output: ActorHandle = create_actor(output_stream, persistence_id, scope_id);

    Ok(Some(output))
}

fn build_stateless_hold_actor(
    initial_actor: ActorHandle,
    _state_param: String,
    body: static_expression::Spanned<static_expression::Expression>,
    _span: Span,
    _persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<Option<ActorHandle>, String> {
    struct HoldOutputEvent {
        value: Value,
        persist: bool,
    }

    let body_actor_context = ActorContext {
        output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
        piped: None,
        passed: ctx.actor_context.passed.clone(),
        parameters: ctx.actor_context.parameters.clone(),
        sequential_processing: ctx.actor_context.sequential_processing,
        backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
        hold_state_update_callback: None,
        use_lazy_actors: ctx.actor_context.use_lazy_actors,
        is_snapshot_context: false,
        object_locals: ctx.actor_context.object_locals.clone(),
        scope: ctx.actor_context.scope.clone(),
        subscription_scope: ctx.actor_context.subscription_scope.clone(),
        call_recorder: ctx.actor_context.call_recorder.clone(),
        is_restoring: ctx.actor_context.is_restoring,
        list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
        recording_counter: ctx.actor_context.recording_counter.clone(),
        subscription_after_seq: ctx.actor_context.subscription_after_seq,
        snapshot_emission_seq: ctx.actor_context.snapshot_emission_seq,
        registry_scope_id: ctx.actor_context.registry_scope_id,
    };

    let body_ctx = EvaluationContext {
        construct_context: ctx.construct_context.clone(),
        actor_context: body_actor_context,
        reference_connector: ctx.reference_connector.clone(),
        link_connector: ctx.link_connector.clone(),
        pass_through_connector: ctx.pass_through_connector.clone(),
        function_registry_snapshot: ctx.function_registry_snapshot.clone(),
        current_module: ctx.current_module.clone(),
        module_loader: ctx.module_loader.clone(),
        source_code: ctx.source_code.clone(),
    };

    let body_result = match evaluate_expression(body, body_ctx)? {
        Some(actor) => actor,
        None => return Ok(None),
    };

    let body_subscription = hold_body_current_and_future_values(body_result);
    let construct_context_for_initial = ctx.construct_context.clone();
    let actor_context_for_initial = ctx.actor_context.clone();
    let storage_for_initial_save = ctx.construct_context.construct_storage.clone();
    let storage_for_output = ctx.construct_context.construct_storage.clone();
    let scope_id = ctx.actor_context.scope_id();

    enum HoldResetInputState {
        LoadingStored,
        ForwardingSource {
            source_subscription: LocalBoxStream<'static, Value>,
        },
    }

    let initial_actor_for_initial = initial_actor.clone();
    let initial_events = stream::unfold(HoldResetInputState::LoadingStored, move |state| {
        let initial_actor = initial_actor_for_initial.clone();
        let storage = storage_for_initial_save.clone();
        let construct_context = construct_context_for_initial.clone();
        let actor_context = actor_context_for_initial.clone();
        async move {
            match state {
                HoldResetInputState::LoadingStored => {
                    let stored: Option<zoon::serde_json::Value> =
                        storage.load_state_now(persistence_id);
                    if let Some(json) = stored {
                        let restored_value = Value::from_json(
                            &json,
                            ConstructId::new("HOLD restored value"),
                            construct_context,
                            ValueIdempotencyKey::new(),
                            actor_context,
                        );
                        return Some((
                            HoldOutputEvent {
                                value: restored_value,
                                persist: false,
                            },
                            HoldResetInputState::ForwardingSource {
                                source_subscription: initial_actor.stream_from_now(),
                            },
                        ));
                    }

                    let (first_value, source_subscription) =
                        take_first_seeded_actor_value_and_future_stream(initial_actor).await?;
                    Some((
                        HoldOutputEvent {
                            value: first_value,
                            persist: true,
                        },
                        HoldResetInputState::ForwardingSource {
                            source_subscription,
                        },
                    ))
                }
                HoldResetInputState::ForwardingSource {
                    mut source_subscription,
                } => {
                    let value = source_subscription.next().await?;
                    Some((
                        HoldOutputEvent {
                            value,
                            persist: true,
                        },
                        HoldResetInputState::ForwardingSource {
                            source_subscription,
                        },
                    ))
                }
            }
        }
    });

    let state_update_events = body_subscription.map(move |new_value| HoldOutputEvent {
        value: new_value,
        persist: true,
    });

    let output_stream = stream::select(initial_events, state_update_events)
        .scan(None::<Value>, move |last_emitted, event| {
            let is_duplicate = last_emitted
                .as_ref()
                .is_some_and(|current| crate::engine::values_equal(current, &event.value));
            let emitted_value = event.value.clone();
            if !is_duplicate {
                *last_emitted = Some(emitted_value.clone());
            }
            let storage = storage_for_output.clone();
            async move {
                if is_duplicate {
                    return Some(None);
                }

                if event.persist {
                    let value_json = event.value.to_json().await;
                    storage.save_state(persistence_id, &value_json);
                }

                Some(Some(emitted_value))
            }
        })
        .filter_map(future::ready);

    let output: ActorHandle = create_actor(output_stream, persistence_id, scope_id);

    Ok(Some(output))
}

/// Build a TEXT { ... } literal actor with interpolation support.
fn build_text_literal_actor(
    parts: Vec<static_expression::TextPart>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<ActorHandle, String> {
    // Collect all parts - literals as constant streams, interpolations as variable lookups
    let mut part_actors: Vec<(bool, ActorHandle)> = Vec::new();

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
            static_expression::TextPart::Interpolation {
                var,
                referenced_span,
            } => {
                // Interpolation - look up the variable (supports field access paths like "item.text")
                let var_name = var.to_string();
                let parts: Vec<&str> = var_name.split('.').collect();
                let base_name = parts[0];
                let field_path: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

                // Look up the base variable
                let base_actor = if let Some(var_actor) =
                    ctx.actor_context.parameters.get(base_name)
                {
                    Some(var_actor.clone())
                } else if let Some(ref_span) = referenced_span {
                    // First check object_locals for instance-specific resolution
                    // This prevents span-based overwrites when multiple Objects are created
                    // from the same function definition (e.g., BLOCK inside List/map)
                    if let Some(local_actor) =
                        ctx.actor_context.object_locals.get(ref_span).cloned()
                    {
                        Some(local_actor)
                    } else {
                        // Fall back to async lookup via ReferenceConnector for outer scope
                        let ref_connector = ctx.try_reference_connector().ok_or_else(|| {
                            "ReferenceConnector dropped - program shutting down".to_string()
                        })?;
                        let ref_span_copy = *ref_span;
                        Some(create_actor_forwarding_from_future_source(
                            async move { Some(ref_connector.referenceable(ref_span_copy).await) },
                            PersistenceId::new(),
                            ctx.actor_context.scope_id(),
                        ))
                    }
                } else {
                    None
                };

                if let Some(base_actor) = base_actor {
                    if field_path.is_empty() {
                        // Simple variable, no field access
                        part_actors.push((false, base_actor));
                    } else {
                        let field_actor = create_actor_forwarding_from_future_source(
                            async move {
                                actor_field_actor_from_current_or_wait(&base_actor, &field_path)
                                    .await
                            },
                            PersistenceId::new(),
                            ctx.actor_context.scope_id(),
                        );

                        part_actors.push((false, field_actor));
                    }
                } else {
                    return Err(format!(
                        "Variable '{}' not found for text interpolation. Available: {:?}",
                        var_name,
                        ctx.actor_context.parameters.keys().collect::<Vec<_>>()
                    ));
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
        let combined_stream = combine_text_part_current_and_future_values(&part_actors);

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

        let scope_id = ctx.actor_context.scope_id();
        Ok(create_actor(text_value_stream, persistence_id, scope_id))
    }
}

fn combine_text_part_current_and_future_values(
    part_actors: &[(bool, ActorHandle)],
) -> LocalBoxStream<'static, String> {
    // Create combined stream using select_all on all part streams.
    // Each part only needs its current value plus future updates.
    let part_subscriptions: Vec<_> = part_actors
        .iter()
        .map(|(_, actor)| actor.current_or_future_stream())
        .collect();

    // Sort by emission sequence to restore source ordering within ready chunks.
    let merged = stream::select_all(
        part_subscriptions
            .into_iter()
            .enumerate()
            .map(|(idx, s)| s.map(move |v| (idx, v))),
    )
    .ready_chunks(8)
    .flat_map(|mut chunk| {
        chunk.sort_by_key(|(_, value)| value.emission_seq());
        stream::iter(chunk)
    });

    let part_count = part_actors.len();
    merged
        .scan(
            vec![None; part_count],
            move |latest_values, (idx, value)| {
                latest_values[idx] = Some(value);

                if latest_values.iter().all(|v| v.is_some()) {
                    let combined: String = latest_values
                        .iter()
                        .filter_map(|v| {
                            v.as_ref().and_then(|val| match val {
                                Value::Text(text, _) => Some(text.text().to_string()),
                                Value::Number(num, _) => Some(num.number().to_string()),
                                Value::Tag(tag, _) => Some(tag.tag().to_string()),
                                _ => None,
                            })
                        })
                        .collect();

                    std::future::ready(Some(Some(combined)))
                } else {
                    std::future::ready(Some(None))
                }
            },
        )
        .filter_map(|opt| async move { opt })
        .boxed_local()
}

/// Build a link setter actor for expressions like `foo.bar`.
fn build_link_setter_actor(
    alias: static_expression::Spanned<static_expression::Alias>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
) -> Result<ActorHandle, String> {
    // Non-piped LINK is just an alias lookup. The special sidecar behavior only exists
    // in the piped `value |> LINK { target }` path handled elsewhere.
    evaluate_alias_immediate(alias.node, span, persistence, persistence_id, ctx)
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

            // Prefer the already-materialized value when available. Nested objects that
            // contain LINK placeholders can have a structural current value even when
            // waiting for `value()` would hang on a child that never emits a normal value.
            let mut current_value = actor_current_value_or_wait(passed).await?;

            // Traverse the path
            for (i, part) in extra_parts.iter().enumerate() {
                let part_str = part.to_string();
                let is_last = i == extra_parts.len() - 1;

                // Get the variable from the current object
                let variable = match &current_value {
                    Value::Object(obj, _) => obj.variable(&part_str),
                    Value::TaggedObject(tagged, _) => tagged.variable(&part_str),
                    _ => {
                        zoon::eprintln!(
                            "[get_link_sender_from_alias] Expected object at '{}', got {}",
                            part_str,
                            current_value.construct_info()
                        );
                        return None;
                    }
                }?;

                if is_last {
                    // At the end of the path, this should be a LINK variable
                    return variable.link_value_sender();
                } else {
                    current_value = actor_current_value_or_wait(&variable.value_actor()).await?;
                }
            }

            None
        }
        static_expression::Alias::WithoutPassed {
            parts,
            referenced_span,
        } => {
            // For non-PASSED aliases, use the reference connector to get the root variable
            if parts.is_empty() {
                return None;
            }

            // Get the root variable from object_locals, parameters, or reference connector
            // Note: object_locals contains function argument actors (arg_locals) which are needed
            // to correctly resolve LINK targets inside function calls. Without this, multiple
            // calls to the same function would overwrite each other's LINK registrations.
            let first_part = parts.first()?.to_string();

            let root_actor = if let Some(param_actor) =
                ctx.actor_context.parameters.get(&first_part).cloned()
            {
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
                zoon::eprintln!(
                    "[get_link_sender_from_alias] Cannot resolve root variable '{}'",
                    first_part
                );
                return None;
            };

            if parts.len() == 1 {
                // Single-part alias pointing to a LINK variable directly is not supported
                // without object navigation context. LINK senders are obtained from Variables,
                // and for single-part aliases we only have a ValueActor.
                // Multi-part paths like `obj.link_field` navigate to the Variable and work.
                zoon::eprintln!(
                    "[get_link_sender_from_alias] Single-part LINK alias '{}' not supported without object context",
                    first_part
                );
                return None;
            }

            // Prefer current values during object traversal for the same reason as above.
            let mut current_value = actor_current_value_or_wait(&root_actor).await?;

            // Traverse the remaining path (skip first part since we already resolved it)
            for (i, part) in parts.iter().skip(1).enumerate() {
                let part_str = part.to_string();
                let is_last = i == parts.len() - 2; // -2 because we skipped first

                // Get the variable from the current object
                let variable = match &current_value {
                    Value::Object(obj, _) => obj.variable(&part_str),
                    Value::TaggedObject(tagged, _) => tagged.variable(&part_str),
                    _ => {
                        zoon::eprintln!(
                            "[get_link_sender_from_alias] Expected object at '{}', got {}",
                            part_str,
                            current_value.construct_info()
                        );
                        return None;
                    }
                }?;

                if is_last {
                    // At the end of the path, this should be a LINK variable
                    let sender = variable.link_value_sender();
                    if sender.is_none() {
                        zoon::eprintln!(
                            "[get_link_sender_from_alias] Final variable '{}' is not a LINK",
                            part_str
                        );
                    }
                    return sender;
                } else {
                    current_value = actor_current_value_or_wait(&variable.value_actor()).await?;
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
) -> Result<Option<ActorHandle>, String> {
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
    let transform_expr = arguments[1]
        .node
        .value
        .clone()
        .ok_or_else(|| format!("List/{} requires a transform expression", path_strs[1]))?;

    let reference_connector = ctx
        .try_reference_connector()
        .ok_or_else(|| "ReferenceConnector dropped - program shutting down".to_string())?;
    let link_connector = ctx
        .try_link_connector()
        .ok_or_else(|| "LinkConnector dropped - program shutting down".to_string())?;
    let pass_through_connector = ctx
        .try_pass_through_connector()
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
) -> Result<Option<ActorHandle>, String> {
    if LOG_DEBUG {
        zoon::println!("[DEBUG] build_list_append_with_recording called");
        zoon::println!(
            "[DEBUG] persistence: {:?}, persistence_id: {}",
            persistence.is_some(),
            persistence_id
        );
    }

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
    let item_expr = item_arg
        .node
        .value
        .clone()
        .ok_or_else(|| "List/append requires an item expression".to_string())?;

    // Only use the recording/persistence wrapper when the appended item expression can
    // actually create stateful values via function calls (for example `new_todo()`).
    // Plain appended objects like Cells overrides should stay on the simple append path.
    if !expression_contains_function_call(&item_expr) {
        let item_actor = match evaluate_expression(item_expr, ctx.clone())? {
            Some(actor) => actor,
            None => return Ok(Some(list_actor)),
        };

        // Inside HOLD-body materialization we need the appended list as an immediate concrete
        // value, not as a reactive list shell whose Push may arrive after the body snapshot.
        // Otherwise state updates like Cells overrides can materialize only the pre-append list.
        if ctx.actor_context.backpressure_permit.is_some() {
            let construct_context = ctx.construct_context.clone();
            let actor_context = ctx.actor_context.clone();
            if let Some(result_value) = try_build_list_append_hold_snapshot_now(
                &list_actor,
                &item_actor,
                construct_context.clone(),
                actor_context.clone(),
            ) {
                let result_actor = create_constant_actor(
                    PersistenceId::new(),
                    result_value,
                    actor_context.scope_id(),
                );
                return Ok(Some(result_actor));
            }

            let list_actor_for_snapshot = list_actor.clone();
            let item_actor_for_snapshot = item_actor.clone();
            let result_future = async move {
                build_list_append_hold_snapshot_value(
                    &list_actor_for_snapshot,
                    &item_actor_for_snapshot,
                    construct_context,
                    actor_context,
                )
                .await
            };

            let scope_id = ctx.actor_context.scope_id();
            let result_actor = if ctx.actor_context.use_lazy_actors {
                create_actor_lazy_from_future(result_future, PersistenceId::new(), scope_id)
            } else {
                create_actor_from_future(result_future, PersistenceId::new(), scope_id)
            };
            return Ok(Some(result_actor));
        }

        let call_actor_context = ActorContext {
            output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
            piped: None,
            passed: ctx.actor_context.passed.clone(),
            parameters: ctx.actor_context.parameters.clone(),
            sequential_processing: ctx.actor_context.sequential_processing,
            backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
            hold_state_update_callback: None,
            use_lazy_actors: ctx.actor_context.use_lazy_actors,
            is_snapshot_context: false,
            object_locals: Arc::new(HashMap::new()),
            scope: ctx.actor_context.scope.clone(),
            subscription_scope: ctx.actor_context.subscription_scope.clone(),
            call_recorder: ctx.actor_context.call_recorder.clone(),
            is_restoring: ctx.actor_context.is_restoring,
            list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
            recording_counter: ctx.actor_context.recording_counter.clone(),
            subscription_after_seq: ctx.actor_context.subscription_after_seq,
            snapshot_emission_seq: ctx.actor_context.snapshot_emission_seq,
            registry_scope_id: ctx.actor_context.registry_scope_id,
        };

        let definition = static_function_call_path_to_definition(&["List", "append"], span)?;
        let result_actor = FunctionCall::new_arc_value_actor(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; List/append(..)"),
            ),
            ctx.construct_context,
            call_actor_context,
            definition,
            vec![list_actor, item_actor],
        );
        return Ok(Some(result_actor));
    }

    // Create persisting child scope for item evaluation
    // This enables call recording for function calls within the item expression
    // Use span (source position) for stable key across page reloads, not persistence_id (which includes timestamps)
    let scope_id = format!("list_append_{}", span);
    // Storage key must be defined first to pass to with_persisting_child_scope
    let storage_key = format!("list_calls:{}", scope_id);
    let (child_ctx, call_receiver) = ctx
        .actor_context
        .with_persisting_child_scope(&scope_id, storage_key.clone());
    if LOG_DEBUG {
        zoon::println!(
            "[DEBUG] Created persisting scope: {}, call_recorder is Some: {}",
            scope_id,
            child_ctx.call_recorder.is_some()
        );
    }

    // Create new evaluation context with the persisting scope
    let item_eval_ctx = EvaluationContext {
        actor_context: child_ctx.clone(),
        current_module: ctx.current_module.clone(),
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
        use zoon::{WebStorage, local_storage};
        match local_storage().get::<Vec<RecordedCall>>(&storage_key) {
            None => Vec::new(),
            Some(Ok(calls)) => calls,
            Some(Err(error)) => {
                zoon::eprintln!(
                    "[DEBUG] Failed to load stored calls for restoration: {:#}",
                    error
                );
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
    let mut restored_items: Vec<ActorHandle> = Vec::new();
    if !stored_calls.is_empty() {
        if LOG_DEBUG {
            zoon::println!(
                "[DEBUG] Restoring {} items from stored calls",
                stored_calls.len()
            );
        }
        for (index, recorded_call) in stored_calls.iter().enumerate() {
            if LOG_DEBUG {
                zoon::println!("[DEBUG] Restoring item {}: {:?}", index, recorded_call);
            }

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
            let input_actor: ActorHandle = create_constant_actor(
                PersistenceId::new(),
                input_value,
                ctx.actor_context.scope_id(),
            );

            // 2. Look up the function in registry
            let function_path = recorded_call.path.join("/");
            let Some(func_def) = ctx
                .function_registry_snapshot
                .as_ref()
                .and_then(|registry| registry.get(&function_path))
            else {
                zoon::eprintln!(
                    "[DEBUG] Function '{}' not found for restoration",
                    function_path
                );
                continue;
            };

            // 3. Bind the input to the function's first parameter (like call_function does with piped)
            // For `title_to_add |> new_todo()`, the piped value becomes the first argument (title)
            let mut parameters = (*ctx.actor_context.parameters).clone();
            if let Some(first_param) = func_def.parameters.first() {
                if LOG_DEBUG {
                    zoon::println!(
                        "[DEBUG] Binding restored input to parameter '{}' for function '{}'",
                        first_param,
                        function_path
                    );
                }
                parameters.insert(first_param.clone(), input_actor.clone());
            } else {
                zoon::eprintln!(
                    "[DEBUG] Function '{}' has no parameters to bind input to",
                    function_path
                );
            }

            // 4. Create restoring context with parameters bound (prevents re-recording)
            // Use the same scope format as during initial creation (call_id)
            // This ensures HOLDs inside the function can find their persisted state
            let body_ctx = EvaluationContext {
                actor_context: ActorContext {
                    parameters: Arc::new(parameters),
                    is_restoring: true,
                    ..child_ctx.with_restoring_child_scope(&recorded_call.id)
                },
                current_module: ctx.current_module.clone(),
                ..ctx.clone()
            };

            // 5. Evaluate function body
            match evaluate_expression(func_def.body.clone(), body_ctx) {
                Ok(Some(item_actor)) => {
                    if LOG_DEBUG {
                        zoon::println!("[DEBUG] Restored item {} successfully", index);
                    }
                    // Wrap the item with origin for removal tracking
                    let origin = ListItemOrigin {
                        source_storage_key: storage_key.clone(),
                        call_id: recorded_call.id.clone(),
                    };
                    let wrapped_item = wrap_restored_list_append_item_with_origin(
                        item_actor,
                        origin,
                        ctx.actor_context.scope_id(),
                    );
                    restored_items.push(wrapped_item);
                }
                Ok(None) => {
                    if LOG_DEBUG {
                        zoon::println!("[DEBUG] Restored item {} was SKIP", index);
                    }
                }
                Err(e) => {
                    zoon::eprintln!("[DEBUG] Failed to restore item {}: {}", index, e);
                }
            }
        }
    }
    let storage_handle = spawn_recorded_calls_storage_actor(storage_key.clone(), call_receiver);
    if LOG_DEBUG {
        zoon::println!("[DEBUG] Spawned storage actor for key: {}", storage_key);
    }

    // Build a custom change stream that includes restored items
    // This replicates function_list_append logic but injects restored items after the first Replace

    let function_call_id = ConstructId::new(format!("List/append:{}", persistence_id));
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
    let list_changes = list_actor_for_stream
        .stream()
        .filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        })
        .flat_map(|list| list.stream())
        .map(TaggedChange::FromList);

    // New item changes (from item_actor stream)
    let item_actor_for_stream = item_actor.clone();
    let append_changes = item_actor_for_stream.stream().map(move |value| {
        // Generate call_id that matches the one used during recording
        let call_id = format!(
            "call_{}",
            appending_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        let origin = ListItemOrigin {
            source_storage_key: storage_key_for_append.clone(),
            call_id,
        };
        let new_item_actor = create_constant_actor_with_origin(
            PersistenceId::new(),
            value,
            origin,
            actor_context_for_append.scope_id(),
        );
        TaggedChange::FromAppend(ListChange::Push {
            item: new_item_actor,
        })
    });

    // Restored item changes (one-time stream of Push changes for each restored item)
    // Track if we have stored calls - determines whether to override initial items
    let had_stored_calls = !restored_items.is_empty();

    let restored_changes = stream::iter(restored_items.into_iter().map(|item| {
        if LOG_DEBUG {
            zoon::println!("[DEBUG] Emitting restored item to change stream");
        }
        TaggedChange::FromRestored(ListChange::Push { item })
    }));

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
            let (has_received_first, buffered, restored_emitted, _had_stored_calls) = state;

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
        },
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
        (list_actor, item_actor, storage_handle), // Keep these alive
    );

    let result_actor = create_constant_actor(
        persistence_id,
        Value::List(
            Arc::new(list),
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ),
        ctx.actor_context.scope_id(),
    );

    Ok(Some(result_actor))
}

fn try_build_list_append_hold_snapshot_now(
    list_actor: &ActorHandle,
    item_actor: &ActorHandle,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Option<Value> {
    let list_value = list_actor.current_value().ok();
    let item_value = item_actor.current_value().ok();

    match (list_value, item_value) {
        (Some(Value::List(list, metadata)), Some(item_value)) => {
            let mut items = list
                .snapshot_now()?
                .into_iter()
                .map(|(_item_id, item_actor)| item_actor)
                .collect::<Vec<_>>();

            let materialized_item = try_materialize_snapshot_value_now(
                item_value,
                construct_context.clone(),
                actor_context.clone(),
            )?;

            let appended_item = create_constant_actor(
                PersistenceId::new(),
                materialized_item,
                actor_context.scope_id(),
            );
            items.push(appended_item);

            Some(Value::List(
                List::new_arc(
                    ConstructInfo::new(
                        "list_append.hold_snapshot_result",
                        None,
                        "List/append HOLD snapshot result",
                    ),
                    construct_context,
                    actor_context,
                    items,
                ),
                metadata,
            ))
        }
        (Some(list_value), _) => Some(list_value),
        (None, Some(item_value)) => {
            let materialized_item = try_materialize_snapshot_value_now(
                item_value,
                construct_context.clone(),
                actor_context.clone(),
            )?;
            let appended_item = create_constant_actor(
                PersistenceId::new(),
                materialized_item,
                actor_context.scope_id(),
            );

            Some(Value::List(
                List::new_arc(
                    ConstructInfo::new(
                        "list_append.hold_snapshot_result",
                        None,
                        "List/append HOLD snapshot result",
                    ),
                    construct_context,
                    actor_context,
                    vec![appended_item],
                ),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            ))
        }
        (None, None) => Some(Value::List(
            List::new_arc(
                ConstructInfo::new(
                    "list_append.hold_snapshot_empty",
                    None,
                    "List/append HOLD snapshot empty",
                ),
                construct_context.clone(),
                actor_context.clone(),
                Vec::<ActorHandle>::new(),
            ),
            ValueMetadata::new(ValueIdempotencyKey::new()),
        )),
    }
}

async fn build_list_append_hold_snapshot_value(
    list_actor: &ActorHandle,
    item_actor: &ActorHandle,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> Value {
    let list_value = actor_current_value_or_wait(list_actor).await;
    let item_value = actor_current_value_or_wait(item_actor).await;

    match (list_value, item_value) {
        (Some(Value::List(list, metadata)), Some(item_value)) => {
            let mut items = list
                .snapshot()
                .await
                .into_iter()
                .map(|(_item_id, item_actor)| item_actor)
                .collect::<Vec<_>>();

            let materialized_item = materialize_snapshot_value(
                item_value,
                construct_context.clone(),
                actor_context.clone(),
            )
            .await;

            let appended_item = create_constant_actor(
                PersistenceId::new(),
                materialized_item,
                actor_context.scope_id(),
            );
            items.push(appended_item);

            Value::List(
                List::new_arc(
                    ConstructInfo::new(
                        "list_append.hold_snapshot_result",
                        None,
                        "List/append HOLD snapshot result",
                    ),
                    construct_context,
                    actor_context,
                    items,
                ),
                metadata,
            )
        }
        (Some(list_value), _) => list_value,
        (None, Some(item_value)) => {
            let materialized_item = materialize_snapshot_value(
                item_value,
                construct_context.clone(),
                actor_context.clone(),
            )
            .await;
            let appended_item = create_constant_actor(
                PersistenceId::new(),
                materialized_item,
                actor_context.scope_id(),
            );

            Value::List(
                List::new_arc(
                    ConstructInfo::new(
                        "list_append.hold_snapshot_result",
                        None,
                        "List/append HOLD snapshot result",
                    ),
                    construct_context,
                    actor_context,
                    vec![appended_item],
                ),
                ValueMetadata::new(ValueIdempotencyKey::new()),
            )
        }
        (None, None) => Value::List(
            List::new_arc(
                ConstructInfo::new(
                    "list_append.hold_snapshot_empty",
                    None,
                    "List/append HOLD snapshot empty",
                ),
                construct_context.clone(),
                actor_context.clone(),
                Vec::<ActorHandle>::new(),
            ),
            ValueMetadata::new(ValueIdempotencyKey::new()),
        ),
    }
}

fn expression_contains_function_call(
    expr: &static_expression::Spanned<static_expression::Expression>,
) -> bool {
    use static_expression::Expression;

    match &expr.node {
        Expression::FunctionCall { .. } => true,
        Expression::List { items } => items.iter().any(expression_contains_function_call),
        Expression::Object(object) => object
            .variables
            .iter()
            .any(|variable| expression_contains_function_call(&variable.node.value)),
        Expression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .any(|variable| expression_contains_function_call(&variable.node.value)),
        Expression::Map { entries } => entries
            .iter()
            .any(|entry| expression_contains_function_call(&entry.value)),
        Expression::Function { body, .. } => expression_contains_function_call(body),
        Expression::Latest { inputs } => inputs.iter().any(expression_contains_function_call),
        Expression::Hold { body, .. }
        | Expression::Then { body }
        | Expression::Flush { value: body }
        | Expression::Spread { value: body } => expression_contains_function_call(body),
        Expression::When { arms } | Expression::While { arms } => arms
            .iter()
            .any(|arm| expression_contains_function_call(&arm.body)),
        Expression::Pipe { from, to } => {
            expression_contains_function_call(from) || expression_contains_function_call(to)
        }
        Expression::Block { variables, output } => {
            variables
                .iter()
                .any(|variable| expression_contains_function_call(&variable.node.value))
                || expression_contains_function_call(output)
        }
        Expression::Comparator(comparator) => match comparator {
            static_expression::Comparator::Equal {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::Greater {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::Less {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                expression_contains_function_call(operand_a)
                    || expression_contains_function_call(operand_b)
            }
        },
        Expression::ArithmeticOperator(operator) => match operator {
            static_expression::ArithmeticOperator::Negate { operand } => {
                expression_contains_function_call(operand)
            }
            static_expression::ArithmeticOperator::Add {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                expression_contains_function_call(operand_a)
                    || expression_contains_function_call(operand_b)
            }
        },
        Expression::Bits { size } => expression_contains_function_call(size),
        Expression::Memory { address } => expression_contains_function_call(address),
        Expression::Bytes { data } => data.iter().any(expression_contains_function_call),
        Expression::PostfixFieldAccess { expr, .. } => expression_contains_function_call(expr),
        Expression::Variable(_)
        | Expression::Literal(_)
        | Expression::Alias(_)
        | Expression::LinkSetter { .. }
        | Expression::Link
        | Expression::Skip
        | Expression::TextLiteral { .. }
        | Expression::FieldAccess { .. } => false,
    }
}

fn expression_references_parameter(
    expr: &static_expression::Spanned<static_expression::Expression>,
    parameter: &str,
) -> bool {
    expression_references_parameter_with_bindings(expr, parameter, &mut Vec::new())
}

fn expression_references_parameter_with_bindings(
    expr: &static_expression::Spanned<static_expression::Expression>,
    parameter: &str,
    bindings: &mut Vec<String>,
) -> bool {
    use static_expression::Expression;

    match &expr.node {
        Expression::Alias(alias) => alias_references_parameter(alias, parameter, bindings),
        Expression::LinkSetter { alias } => {
            alias_references_parameter(&alias.node, parameter, bindings)
        }
        Expression::FunctionCall { arguments, .. } => arguments.iter().any(|argument| {
            argument.node.value.as_ref().is_some_and(|value| {
                expression_references_parameter_with_bindings(value, parameter, bindings)
            })
        }),
        Expression::List { items } => items
            .iter()
            .any(|item| expression_references_parameter_with_bindings(item, parameter, bindings)),
        Expression::Object(object) => {
            variables_reference_parameter(&object.variables, parameter, bindings)
        }
        Expression::TaggedObject { object, .. } => {
            variables_reference_parameter(&object.variables, parameter, bindings)
        }
        Expression::Map { entries } => entries.iter().any(|entry| {
            map_entry_key_references_parameter(&entry.key.node, parameter, bindings)
                || expression_references_parameter_with_bindings(&entry.value, parameter, bindings)
        }),
        Expression::Function {
            parameters, body, ..
        } => {
            let initial_len = bindings.len();
            bindings.extend(parameters.iter().map(|param| param.node.to_string()));
            let references =
                expression_references_parameter_with_bindings(body, parameter, bindings);
            bindings.truncate(initial_len);
            references
        }
        Expression::Latest { inputs } => inputs
            .iter()
            .any(|input| expression_references_parameter_with_bindings(input, parameter, bindings)),
        Expression::Hold { state_param, body } => {
            let initial_len = bindings.len();
            bindings.push(state_param.to_string());
            let references =
                expression_references_parameter_with_bindings(body, parameter, bindings);
            bindings.truncate(initial_len);
            references
        }
        Expression::Then { body }
        | Expression::Flush { value: body }
        | Expression::Spread { value: body } => {
            expression_references_parameter_with_bindings(body, parameter, bindings)
        }
        Expression::When { arms } | Expression::While { arms } => arms.iter().any(|arm| {
            expression_references_parameter_with_bindings(&arm.body, parameter, bindings)
        }),
        Expression::Pipe { from, to } => {
            expression_references_parameter_with_bindings(from, parameter, bindings)
                || expression_references_parameter_with_bindings(to, parameter, bindings)
        }
        Expression::Block { variables, output } => {
            let initial_len = bindings.len();
            for variable in variables {
                if expression_references_parameter_with_bindings(
                    &variable.node.value,
                    parameter,
                    bindings,
                ) {
                    bindings.truncate(initial_len);
                    return true;
                }
                bindings.push(variable.node.name.to_string());
            }
            let references =
                expression_references_parameter_with_bindings(output, parameter, bindings);
            bindings.truncate(initial_len);
            references
        }
        Expression::Comparator(comparator) => match comparator {
            static_expression::Comparator::Equal {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::NotEqual {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::Greater {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::Less {
                operand_a,
                operand_b,
            }
            | static_expression::Comparator::LessOrEqual {
                operand_a,
                operand_b,
            } => {
                expression_references_parameter_with_bindings(operand_a, parameter, bindings)
                    || expression_references_parameter_with_bindings(operand_b, parameter, bindings)
            }
        },
        Expression::ArithmeticOperator(operator) => match operator {
            static_expression::ArithmeticOperator::Negate { operand } => {
                expression_references_parameter_with_bindings(operand, parameter, bindings)
            }
            static_expression::ArithmeticOperator::Add {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            }
            | static_expression::ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                expression_references_parameter_with_bindings(operand_a, parameter, bindings)
                    || expression_references_parameter_with_bindings(operand_b, parameter, bindings)
            }
        },
        Expression::TextLiteral { parts, .. } => parts.iter().any(|part| match part {
            static_expression::TextPart::Text(_) => false,
            static_expression::TextPart::Interpolation { var, .. } => {
                !bindings.iter().any(|binding| binding == parameter)
                    && var
                        .as_str()
                        .split('.')
                        .next()
                        .is_some_and(|base| base == parameter)
            }
        }),
        Expression::Bits { size } => {
            expression_references_parameter_with_bindings(size, parameter, bindings)
        }
        Expression::Memory { address } => {
            expression_references_parameter_with_bindings(address, parameter, bindings)
        }
        Expression::Bytes { data } => data
            .iter()
            .any(|item| expression_references_parameter_with_bindings(item, parameter, bindings)),
        Expression::PostfixFieldAccess { expr, .. } => {
            expression_references_parameter_with_bindings(expr, parameter, bindings)
        }
        Expression::Variable(_)
        | Expression::Literal(_)
        | Expression::Link
        | Expression::Skip
        | Expression::FieldAccess { .. } => false,
    }
}

fn variables_reference_parameter(
    variables: &[static_expression::Spanned<static_expression::Variable>],
    parameter: &str,
    bindings: &mut Vec<String>,
) -> bool {
    let initial_len = bindings.len();
    for variable in variables {
        if expression_references_parameter_with_bindings(&variable.node.value, parameter, bindings)
        {
            bindings.truncate(initial_len);
            return true;
        }
        bindings.push(variable.node.name.to_string());
    }
    bindings.truncate(initial_len);
    false
}

fn alias_references_parameter(
    alias: &static_expression::Alias,
    parameter: &str,
    bindings: &[String],
) -> bool {
    match alias {
        static_expression::Alias::WithoutPassed { parts, .. } => {
            !bindings.iter().any(|binding| binding == parameter)
                && parts.first().is_some_and(|part| part.as_str() == parameter)
        }
        static_expression::Alias::WithPassed { .. } => false,
    }
}

fn map_entry_key_references_parameter(
    key: &static_expression::MapEntryKey,
    parameter: &str,
    bindings: &[String],
) -> bool {
    match key {
        static_expression::MapEntryKey::Literal(_) => false,
        static_expression::MapEntryKey::Alias(alias) => {
            alias_references_parameter(alias, parameter, bindings)
        }
    }
}

/// Call a function with stack-safe evaluation.
/// Supports both user-defined functions and builtin functions.
/// Returns `Ok(None)` if the function body is SKIP.
fn call_function(
    path: Vec<String>,
    args: Vec<(String, ActorHandle)>,
    span: Span,
    persistence: Option<Persistence>,
    persistence_id: PersistenceId,
    ctx: EvaluationContext,
    use_piped_for_builtin: bool,
    function_registry: &HashMap<String, StaticFunctionDefinition>,
) -> Result<Option<ActorHandle>, String> {
    let full_path = path.join("/");

    // Convert args to a map (for user-defined functions)
    let mut arg_map: HashMap<String, ActorHandle> = HashMap::new();
    // Also keep positional list (for builtin functions)
    let mut positional_args: Vec<ActorHandle> = Vec::new();
    for (name, actor) in args {
        positional_args.push(actor.clone());
        arg_map.insert(name, actor);
    }

    // Check user-defined functions first
    // For nested evaluations (closures), use snapshot from context.
    // For main evaluation, use the passed registry.
    let func_def_opt = ctx
        .function_registry_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.get(&full_path).cloned())
        .or_else(|| function_registry.get(&full_path).cloned())
        // Intra-module fallback: if unqualified lookup fails and we're inside a module,
        // try qualified name. E.g., `get()` inside Theme module → try `Theme/get`.
        .or_else(|| {
            if !full_path.contains('/') {
                if let Some(module) = &ctx.current_module {
                    let qualified = format!("{}/{}", module, full_path);
                    ctx.function_registry_snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.get(&qualified).cloned())
                        .or_else(|| function_registry.get(&qualified).cloned())
                } else {
                    None
                }
            } else {
                None
            }
        });

    if let Some(func_def) = func_def_opt {
        // User-defined functions don't capture caller parameter maps.
        // Carry only the explicit call arguments into the callee.
        let parameters = arg_map;

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
            let func_module_name = func_def.module_name.clone();
            let ctx_for_closure = ctx.clone();

            // Clone path for recording
            let path_for_recording = path.clone();

            // Create a stream that:
            // 1. Subscribes to the piped input
            // 2. For each value from piped, evaluates the function body with that value
            // 3. If piped never produces values (SKIP), this stream also never produces values
            let result_stream = piped_for_closure.stream().flat_map(move |piped_value| {
                // Generate unique invocation_id for this call (used for both recording and scope)
                // This ensures each invocation of the function gets its own scope for internal HOLDs
                let invocation_id = ctx_for_closure
                    .actor_context
                    .recording_counter
                    .as_ref()
                    .map(|counter| {
                        format!(
                            "call_{}",
                            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        )
                    })
                    .unwrap_or_else(|| format!("call_{}", ulid::Ulid::new()));

                // Record the call if we're in a persisting scope
                if let Some(call_recorder) = &ctx_for_closure.actor_context.call_recorder {
                    let captured_input = CapturedValue::capture(&piped_value);
                    if LOG_DEBUG {
                        zoon::println!(
                            "[DEBUG] Recording call: {:?} with id: {} and input: {:?}",
                            path_for_recording,
                            invocation_id,
                            captured_input
                        );
                    }
                    let recorded_call = RecordedCall {
                        id: invocation_id.clone(),
                        path: path_for_recording.clone(),
                        inputs: captured_input,
                    };
                    call_recorder.send_or_drop(recorded_call);
                }

                // Create a constant actor for this specific piped value
                let value_actor: ActorHandle = create_constant_actor(
                    PersistenceId::new(),
                    piped_value,
                    ctx_for_closure.actor_context.scope_id(),
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
                    parameters: Arc::new(params),
                    sequential_processing: ctx_for_closure.actor_context.sequential_processing,
                    backpressure_permit: ctx_for_closure.actor_context.backpressure_permit.clone(),
                    hold_state_update_callback: None,
                    use_lazy_actors: ctx_for_closure.actor_context.use_lazy_actors,
                    // Don't inherit snapshot mode - function body evaluates in normal streaming context
                    is_snapshot_context: false,
                    // Clear object_locals - function body is a new scope
                    object_locals: Arc::new(HashMap::new()),
                    scope: call_scope,
                    subscription_scope: ctx_for_closure.actor_context.subscription_scope.clone(),
                    call_recorder: ctx_for_closure.actor_context.call_recorder.clone(),
                    is_restoring: ctx_for_closure.actor_context.is_restoring,
                    list_append_storage_key: ctx_for_closure
                        .actor_context
                        .list_append_storage_key
                        .clone(),
                    recording_counter: ctx_for_closure.actor_context.recording_counter.clone(),
                    // User-defined function bodies create a fresh scoped graph. Carrying the
                    // caller's subscription_after_seq into that new graph can starve late-linked
                    // structural values/events inside the function result (for example
                    // `new_todo().todo_elements.todo_checkbox.event.click`).
                    subscription_after_seq: None,
                    snapshot_emission_seq: None,
                    registry_scope_id: ctx_for_closure.actor_context.registry_scope_id,
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
                    current_module: func_module_name.clone(),
                };

                // Evaluate the function body with this piped value
                match evaluate_expression(func_body.clone(), new_ctx) {
                    Ok(Some(result_actor)) => {
                        // Use value() for type-safe single-value semantics (like THEN does)
                        let result_stream: Pin<Box<dyn Stream<Item = Value>>> =
                            Box::pin(one_shot_actor_value_stream(result_actor, ()));
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
            let wrapper_actor =
                create_actor(result_stream, persistence_id, ctx.actor_context.scope_id());

            return Ok(Some(wrapper_actor));
        }

        // No piped value or no parameter to bind - evaluate immediately (original behavior)
        // Collect argument actors to keep them alive for the duration of the function result
        // Note: we collect from parameters which now contains the arg_map values
        let arg_actors: Vec<ActorHandle> = parameters.values().cloned().collect();

        // Create a nested scope using the function call's persistence_id.
        // This ensures that HOLDs inside the function body get unique persistence IDs
        // for each call site (e.g., each call to new_todo() gets its own scope).
        let call_scope = match &ctx.actor_context.scope {
            Scope::Root => Scope::Nested(persistence_id.to_string()),
            Scope::Nested(existing) => Scope::Nested(format!("{}:{}", existing, persistence_id)),
        };

        let new_actor_context = ActorContext {
            output_valve_signal: ctx.actor_context.output_valve_signal.clone(),
            piped: ctx.actor_context.piped.clone(),
            passed: ctx.actor_context.passed.clone(),
            parameters: Arc::new(parameters),
            sequential_processing: ctx.actor_context.sequential_processing,
            backpressure_permit: ctx.actor_context.backpressure_permit.clone(),
            // Don't propagate HOLD callback into user-defined functions - they have their own scope
            hold_state_update_callback: None,
            use_lazy_actors: ctx.actor_context.use_lazy_actors,
            // Don't inherit snapshot mode - function body evaluates in normal streaming context
            is_snapshot_context: false,
            // Clear object_locals - function body is a new scope
            object_locals: Arc::new(HashMap::new()),
            scope: call_scope,
            subscription_scope: ctx.actor_context.subscription_scope.clone(),
            call_recorder: ctx.actor_context.call_recorder.clone(),
            is_restoring: ctx.actor_context.is_restoring,
            list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
            recording_counter: ctx.actor_context.recording_counter.clone(),
            // User-defined function bodies create a fresh scoped graph. Carrying the caller's
            // subscription_after_seq into that new graph can starve late-linked structural
            // values/events inside the function result.
            subscription_after_seq: None,
            snapshot_emission_seq: None,
            registry_scope_id: ctx.actor_context.registry_scope_id,
        };

        let new_ctx = EvaluationContext {
            construct_context: ctx.construct_context.clone(),
            actor_context: new_actor_context,
            reference_connector: ctx.reference_connector,
            link_connector: ctx.link_connector,
            pass_through_connector: ctx.pass_through_connector,
            function_registry_snapshot: ctx.function_registry_snapshot,
            current_module: func_def.module_name.clone(),
            module_loader: ctx.module_loader,
            source_code: ctx.source_code,
        };

        let result = evaluate_expression(func_def.body, new_ctx);

        // If we have argument actors, wrap the result to keep them alive
        match result {
            Ok(Some(result_actor)) if !arg_actors.is_empty() => {
                retain_actor_handles(&result_actor, arg_actors);
                return Ok(Some(result_actor));
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
                object_locals: Arc::new(HashMap::new()),
                scope: ctx.actor_context.scope.clone(),
                subscription_scope: ctx.actor_context.subscription_scope.clone(),
                call_recorder: ctx.actor_context.call_recorder.clone(),
                is_restoring: ctx.actor_context.is_restoring,
                list_append_storage_key: ctx.actor_context.list_append_storage_key.clone(),
                recording_counter: ctx.actor_context.recording_counter.clone(),
                // Inherit subscription_after_seq - builtin function calls should respect caller's filtering
                subscription_after_seq: ctx.actor_context.subscription_after_seq,
                snapshot_emission_seq: ctx.actor_context.snapshot_emission_seq,
                registry_scope_id: ctx.actor_context.registry_scope_id,
            };

            Ok(Some(FunctionCall::new_arc_value_actor(
                construct_info,
                ctx.construct_context,
                call_actor_context,
                definition,
                builtin_args,
            )))
        }
        Err(_) => Err(format!("Function '{}' not found", full_path)),
    }
}

/// Pre-resolve all ValueComparison patterns in arms to concrete values.
/// Looks up variables from the actor context's parameters, then traverses field paths.
async fn resolve_comparison_values(
    arms: &[static_expression::Arm],
    actor_context: &ActorContext,
    reference_connector: &Weak<ReferenceConnector>,
) -> HashMap<String, Value> {
    let mut resolved = HashMap::new();
    for arm in arms {
        if let static_expression::Pattern::ValueComparison {
            path,
            referenced_span,
        } = &arm.pattern
        {
            let base_name = path[0].as_str().to_string();

            // Try parameters first, then fall back to reference_connector
            let base_actor = if let Some(actor) = actor_context.parameters.get(&base_name) {
                Some(actor.clone())
            } else if let Some(ref_span) = referenced_span {
                // Look up in object_locals or via reference_connector
                if let Some(local) = actor_context.object_locals.get(ref_span) {
                    Some(local.clone())
                } else if let Some(rc) = reference_connector.upgrade() {
                    Some(rc.referenceable(*ref_span).await)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(actor) = base_actor {
                if let Ok(base_value) = actor.current_value() {
                    if path.len() == 1 {
                        resolved.insert(base_name, base_value);
                    } else {
                        let field_path: Vec<String> =
                            path[1..].iter().map(|s| s.as_str().to_string()).collect();
                        if let Some(field_value) =
                            extract_field_path(&base_value, &field_path).await
                        {
                            let key = path
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(".");
                            resolved.insert(key, field_value);
                        }
                    }
                }
            }
        }
    }
    resolved
}

/// Pattern matching that properly extracts bindings from Objects.
/// This can handle reactive Object values by awaiting subscriptions to get current field values.
async fn match_pattern(
    pattern: &static_expression::Pattern,
    value: &Value,
    comparison_values: &HashMap<String, Value>,
) -> Option<HashMap<String, Value>> {
    let mut bindings = HashMap::new();

    match pattern {
        static_expression::Pattern::WildCard => Some(bindings),

        static_expression::Pattern::Alias { name } => {
            bindings.insert(name.to_string(), value.clone());
            Some(bindings)
        }

        static_expression::Pattern::Literal(lit) => match (lit, value) {
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
        },

        static_expression::Pattern::TaggedObject { tag, variables } => {
            if let Value::TaggedObject(to, _) = value {
                if to.tag() == tag.as_str() {
                    // Extract field values from the tagged object
                    for pattern_var in variables {
                        let var_name = pattern_var.name.as_str();
                        // Find the variable in the tagged object by name
                        if let Some(variable) = to.variables().iter().find(|v| v.name() == var_name)
                        {
                            // Await the current value from the reactive actor
                            if let Some(field_value) = variable.value_actor().current_value().ok() {
                                // Handle nested patterns if present
                                if let Some(ref nested_pattern) = pattern_var.value {
                                    // Recursively match nested pattern
                                    if let Some(nested_bindings) = Box::pin(match_pattern(
                                        nested_pattern,
                                        &field_value,
                                        comparison_values,
                                    ))
                                    .await
                                    {
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
                comparison_values: &HashMap<String, Value>,
            ) -> bool {
                for pattern_var in pattern_vars {
                    let var_name = pattern_var.name.as_str();
                    // Find the variable in the object by name
                    if let Some(variable) = variables.iter().find(|v| v.name() == var_name) {
                        // Await the current value from the reactive actor
                        if let Some(field_value) = variable.value_actor().current_value().ok() {
                            // Handle nested patterns if present
                            if let Some(ref nested_pattern) = pattern_var.value {
                                // Recursively match nested pattern
                                if let Some(nested_bindings) = Box::pin(match_pattern(
                                    nested_pattern,
                                    &field_value,
                                    comparison_values,
                                ))
                                .await
                                {
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
                if extract_object_bindings(
                    obj.variables(),
                    variables,
                    &mut bindings,
                    comparison_values,
                )
                .await
                {
                    Some(bindings)
                } else {
                    None
                }
            } else if let Value::TaggedObject(to, _) = value {
                if extract_object_bindings(
                    to.variables(),
                    variables,
                    &mut bindings,
                    comparison_values,
                )
                .await
                {
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
                    if let Some(item_value) = item_actor.clone().current_value().ok() {
                        // Recursively match the pattern
                        if let Some(nested_bindings) =
                            Box::pin(match_pattern(item_pattern, &item_value, comparison_values))
                                .await
                        {
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

        static_expression::Pattern::ValueComparison { path, .. } => {
            let key = path
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(comparison_value) = comparison_values.get(&key) {
                // Compare like literals: check tag, text, or number equality
                match (comparison_value, value) {
                    (Value::Tag(a, _), Value::Tag(b, _)) if a.tag() == b.tag() => Some(bindings),
                    (Value::Text(a, _), Value::Text(b, _)) if a.text() == b.text() => {
                        Some(bindings)
                    }
                    (Value::Number(a, _), Value::Number(b, _))
                        if (a.number() - b.number()).abs() < f64::EPSILON =>
                    {
                        Some(bindings)
                    }
                    _ => None,
                }
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
    /// Module this function belongs to (e.g., "Theme" for functions in Theme.bn).
    /// Used to resolve intra-module calls: when `Theme/material()` calls `get()`,
    /// we try `Theme/get` as a fallback.
    pub module_name: Option<String>,
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
    GetBaseDir {
        reply: oneshot::Sender<String>,
    },
    GetCached {
        name: String,
        reply: oneshot::Sender<Option<ModuleData>>,
    },
    Cache {
        name: String,
        data: ModuleData,
    },
}

/// Module loader with caching for loading and parsing Boon modules.
/// Resolves module paths like "Theme" to file paths and caches parsed modules.
/// Uses actor model with channels - no locks, no RefCell.
#[derive(Clone)]
pub struct ModuleLoader {
    request_sender: NamedChannel<ModuleLoaderRequest>,
    _task: Arc<TaskHandle>,
}

impl Default for ModuleLoader {
    fn default() -> Self {
        Self::new("")
    }
}

impl ModuleLoader {
    pub fn new(base_dir: impl Into<String>) -> Self {
        let (tx, mut rx) =
            NamedChannel::new("module_loader.requests", MODULE_LOADER_REQUEST_CAPACITY);
        let initial_base_dir = base_dir.into();

        let actor_loop = Task::start_droppable(async move {
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
                            zoon::println!(
                                "[MODULE_LOADER] GetCached reply receiver dropped for {}",
                                name
                            );
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
            _task: Arc::new(actor_loop),
        }
    }

    /// Set the base directory for module resolution (fire-and-forget).
    pub fn set_base_dir(&self, dir: impl Into<String>) {
        if let Err(e) = self
            .request_sender
            .try_send(ModuleLoaderRequest::SetBaseDir(dir.into()))
        {
            zoon::eprintln!("[MODULE_LOADER] Failed to send SetBaseDir: {e}");
        }
    }

    /// Get the base directory (async).
    pub async fn base_dir(&self) -> String {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .request_sender
            .try_send(ModuleLoaderRequest::GetBaseDir { reply: tx })
        {
            zoon::eprintln!("[MODULE_LOADER] Failed to send GetBaseDir: {e}");
            return String::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Get a cached module (async).
    async fn get_cached(&self, name: &str) -> Option<ModuleData> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .request_sender
            .try_send(ModuleLoaderRequest::GetCached {
                name: name.to_string(),
                reply: tx,
            })
        {
            zoon::println!("[MODULE_LOADER] Failed to send GetCached for {}: {e}", name);
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Cache a module (fire-and-forget).
    fn cache(&self, name: String, data: ModuleData) {
        if let Err(e) = self.request_sender.try_send(ModuleLoaderRequest::Cache {
            name: name.clone(),
            data,
        }) {
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
            make_path(
                &base_dir_owned,
                &format!("{}/{}.bn", module_name, module_name),
            ),
            make_path(&base_dir_owned, &format!("Generated/{}.bn", module_name)),
        ];

        for path in paths_to_try {
            if let Some(source_code) = virtual_fs.read_text(&path).await {
                zoon::println!(
                    "[ModuleLoader] Loading module '{}' from '{}'",
                    module_name,
                    path
                );
                if let Some(module_data) = parse_module(&path, &source_code) {
                    // Cache the module
                    self.cache(module_name.to_string(), module_data.clone());
                    return Some(module_data);
                }
            }
        }

        zoon::eprintln!(
            "[ModuleLoader] Could not find module '{}' (tried from base '{}')",
            module_name,
            base
        );
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
        let module = self
            .load_module(module_name, virtual_fs, current_dir)
            .await?;
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
        let module = self
            .load_module(module_name, virtual_fs, current_dir)
            .await?;
        module.variables.get(variable_name).cloned()
    }
}

/// Parse module source code into ModuleData (free function, no state needed).
pub fn parse_module(filename: &str, source_code: &str) -> Option<ModuleData> {
    // Create SourceCode FIRST so all parsing borrows from this Arc'd String.
    // This is critical: the AST will contain &str slices that point into this allocation.
    // If we create SourceCode after parsing, the pointers won't match.
    let source_code_arc = SourceCode::new(source_code.to_string());
    let source_code = source_code_arc.as_str();

    // Lexer
    let (tokens, errors) = lexer().parse(source_code).into_output_errors();
    if !errors.is_empty() {
        zoon::eprintln!(
            "[ModuleLoader] Lex errors in '{}': {:?}",
            filename,
            errors.len()
        );
        return None;
    }
    let mut tokens = tokens?;
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    // Parser
    let (ast, errors) = parser()
        .parse(ChumskyStream::from_iter(tokens).map(
            span_at(source_code.len()),
            |Spanned {
                 node,
                 span,
                 persistence: _,
             }| (node, span),
        ))
        .into_output_errors();
    if !errors.is_empty() {
        for err in &errors {
            zoon::eprintln!("[ModuleLoader] Parse error in '{}': {:?}", filename, err);
        }
        return None;
    }
    let ast = ast?;

    // Reference resolution
    let ast = match resolve_references(ast) {
        Ok(ast) => ast,
        Err(errors) => {
            zoon::eprintln!(
                "[ModuleLoader] Reference errors in '{}': {:?}",
                filename,
                errors.len()
            );
            return None;
        }
    };

    // Persistence resolution (modules don't persist state, but IDs must be assigned)
    let (ast, _new_span_id_pairs, _changed_variable_ids) =
        match resolve_persistence(ast, None::<Vec<Spanned<boon::parser::Expression>>>, "") {
            Ok(result) => result,
            Err(errors) => {
                zoon::eprintln!(
                    "[ModuleLoader] Persistence errors in '{}': {:?}",
                    filename,
                    errors.len()
                );
                return None;
            }
        };

    // Convert to static expressions (clone SourceCode since it's still borrowed by the AST)
    let static_ast = static_expression::convert_expressions(source_code_arc.clone(), ast);

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
            static_expression::Expression::Function {
                name,
                parameters,
                body,
            } => {
                functions.insert(
                    name.to_string(),
                    StaticFunctionDefinition {
                        parameters: parameters.into_iter().map(|p| p.node.to_string()).collect(),
                        body: *body,
                        module_name: None,
                    },
                );
            }
            _ => {}
        }
    }

    Some(ModuleData {
        functions,
        variables,
    })
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
    let (obj, ctx, _, _, _, _, _, _) = evaluate_with_registry(
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
/// - `ScopeDestroyGuard`: Guard for the root registry scope (destroys all actor scopes on drop)
///
/// IMPORTANT: The ReferenceConnector, LinkConnector, PassThroughConnector, and ScopeDestroyGuard
/// MUST be dropped when the program is finished (e.g., when switching examples) to allow actors
/// to be cleaned up. The connectors hold strong references to all top-level actors, and the
/// ScopeDestroyGuard recursively destroys all registry scopes and their actors.
pub fn evaluate_with_registry(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    mut function_registry: FunctionRegistry,
    module_loader: ModuleLoader,
) -> Result<
    (
        Arc<Object>,
        ConstructContext,
        FunctionRegistry,
        ModuleLoader,
        Arc<ReferenceConnector>,
        Arc<LinkConnector>,
        Arc<PassThroughConnector>,
        ScopeDestroyGuard,
    ),
    String,
> {
    // Create root scope in the actor registry for deterministic lifetime management
    let root_scope_id = create_registry_scope(None);
    let construct_context = ConstructContext {
        construct_storage: Arc::new(ConstructStorage::new(states_local_storage_key)),
        virtual_fs,
        bridge_scope_id: Some(root_scope_id),
        scene_ctx: None,
    };
    let actor_context = ActorContext {
        registry_scope_id: Some(root_scope_id),
        ..ActorContext::default()
    };
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
                        module_name: None,
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
    let root_scope_guard = ScopeDestroyGuard::new(root_scope_id);
    Ok((
        root_object,
        construct_context,
        function_registry,
        module_loader,
        reference_connector,
        link_connector,
        pass_through_connector,
        root_scope_guard,
    ))
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
        is_referenced: _is_referenced,
        value_changed: _,
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
        Variable::new_link_arc(construct_info, name_string, actor_context, persistence_id)
    } else {
        Variable::new_arc(
            construct_info,
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
) -> Result<ActorHandle, String> {
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
) -> Result<ActorHandle, String> {
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
) -> Result<ActorHandle, String> {
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
        current_module: None,
    };

    // Delegate to the stack-safe evaluator
    evaluate_expression(expression, ctx)?
        .ok_or_else(|| "Top-level expression cannot be SKIP".to_string())
}

/// Get function definition for static function calls.
fn static_function_call_path_to_definition(
    path: &[&str],
    _span: Span,
) -> Result<
    impl Fn(
        Arc<Vec<ActorHandle>>,
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
            api::function_math_sum(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Math", "round"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_math_round(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Math", "min"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_math_min(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Math", "max"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_math_max(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_empty(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "space"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_space(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "trim"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_trim(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "is_empty"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_is_empty(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "is_not_empty"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_is_not_empty(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "to_number"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_to_number(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "starts_with"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_starts_with(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Bool", "not"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_not(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Bool", "toggle"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_toggle(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Bool", "or"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_or(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "count"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_count(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "append"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_append(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "clear"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_clear(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "latest"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_latest(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "last"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_last(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "remove_last"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_list_remove_last(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["List", "is_empty"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_list_is_empty(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["List", "is_not_empty"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_list_is_not_empty(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["List", "get"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_get(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "range"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_range(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "sum"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_sum(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["List", "product"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_product(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "length"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_length(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "char_at"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_char_at(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "find"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_find(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Text", "find_closing"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_find_closing(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "substring"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_substring(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "to_uppercase"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_to_uppercase(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "char_code"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_char_code(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Text", "from_char_code"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_text_from_char_code(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Math", "modulo"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_math_modulo(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Router", "route"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_router_route(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Router", "go_to"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_router_go_to(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Ulid", "generate"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_ulid_generate(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Document", "new"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_document_new(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Element", "stripe"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_stripe(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "container"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_container(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "stack"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_stack(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "button"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_button(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "text_input"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_text_input(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "checkbox"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_checkbox(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "slider"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_slider(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "select"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_select(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "svg"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_svg(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Element", "svg_circle"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_svg_circle(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "label"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_label(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "paragraph"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_paragraph(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Element", "link"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_link(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Timer", "interval"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_timer_interval(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Log", "info"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_log_info(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Log", "error"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_log_error(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Build", "succeed"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_build_succeed(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Build", "fail"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_build_fail(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Scene", "new"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_scene_new(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        // --- Scene/Element/* aliases (call the same underlying Element/* functions) ---
        ["Scene", "Element", "stripe"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_stripe(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "container"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_container(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "stack"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_stack(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "button"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_button(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "text_input"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_text_input(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "checkbox"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_checkbox(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "label"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_label(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "paragraph"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_paragraph(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "link"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_link(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        // --- Element/text and Element/block (new element types) ---
        ["Element", "text"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_text(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Element", "block"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_block(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "text"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_text(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Scene", "Element", "block"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_element_block(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        // --- Light/* data constructors ---
        ["Light", "directional"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_light_directional(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Light", "ambient"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_light_ambient(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Theme", "background_color"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_theme_background_color(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Theme", "text_color"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_theme_text_color(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Theme", "accent_color"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_theme_accent_color(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["File", "read_text"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_file_read_text(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["File", "write_text"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_file_write_text(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Directory", "entries"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_directory_entries(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Stream", "skip"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_skip(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Stream", "take"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_stream_take(
                arguments,
                id,
                persistence_id,
                construct_context,
                actor_context,
            )
            .boxed_local()
        },
        ["Stream", "distinct"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_stream_distinct(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Stream", "pulses"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_stream_pulses(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        ["Stream", "debounce"] => {
            |arguments, id, persistence_id, construct_context, actor_context| {
                api::function_stream_debounce(
                    arguments,
                    id,
                    persistence_id,
                    construct_context,
                    actor_context,
                )
                .boxed_local()
            }
        }
        _ => {
            return Err(format!(
                "Unknown function '{}(..)' in static context",
                path.join("/")
            ));
        }
    };
    Ok(definition)
}

/// Match result containing bindings if match succeeded
type PatternBindings = HashMap<String, ActorHandle>;

/// Spawn an actor that stores recorded calls to localStorage.
/// Each recorded call represents an item added to a list (e.g., new_todo() call).
/// Calls are stored in order so they can be replayed on restoration.
fn spawn_recorded_calls_storage_actor(
    storage_key: String,
    mut call_receiver: mpsc::Receiver<RecordedCall>,
) -> TaskHandle {
    use zoon::futures_util::StreamExt;
    use zoon::{WebStorage, local_storage};

    Task::start_droppable(async move {
        // Load existing recorded calls from storage (if any)
        let mut recorded_calls: Vec<RecordedCall> =
            match local_storage().get::<Vec<RecordedCall>>(&storage_key) {
                None => Vec::new(),
                Some(Ok(calls)) => calls,
                Some(Err(error)) => {
                    zoon::eprintln!(
                        "[DEBUG] Failed to deserialize recorded calls for {}: {:#}",
                        storage_key,
                        error
                    );
                    Vec::new()
                }
            };
        if LOG_DEBUG {
            zoon::println!(
                "[DEBUG] Storage actor loaded {} existing calls for {}",
                recorded_calls.len(),
                storage_key
            );
        }

        // Process incoming recorded calls
        while let Some(call) = call_receiver.next().await {
            if LOG_DEBUG {
                zoon::println!("[DEBUG] Storage actor received call: {:?}", call);
            }
            recorded_calls.push(call);

            // Save to localStorage after each call
            if let Err(error) = local_storage().insert(&storage_key, &recorded_calls) {
                zoon::eprintln!(
                    "[DEBUG] Failed to save recorded calls for {}: {:#}",
                    storage_key,
                    error
                );
            } else if LOG_DEBUG {
                zoon::println!(
                    "[DEBUG] Storage actor saved {} calls to {}",
                    recorded_calls.len(),
                    storage_key
                );
            }
        }

        if LOG_DEBUG {
            zoon::println!("[DEBUG] Storage actor for {} shutting down", storage_key);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{FunctionRegistry, ModuleLoader, evaluate_with_registry, flatten_pipe_chain};
    use crate::engine::{
        ActorContext, ConstructContext, ConstructId, Object, ScopeDestroyGuard, Value, Variable,
        VirtualFilesystem,
    };
    use boon::parser::{
        SourceCode, Spanned, Token, lexer, parser, resolve_references, span_at, static_expression,
    };
    use boon::platform::browser::kernel::{
        KernelValue, LatestCandidate, TickId, TickSeq, select_latest,
    };
    use chumsky::Parser as ChumskyParser;
    use chumsky::input::Input as ChumskyInput;
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;
    use zoon::futures_util::stream::StreamExt;
    use zoon::serde_json::{self, json};

    fn latest_conformance_source() -> &'static str {
        r#"
left_button: LINK
right_button: LINK

selected: LATEST {
    left_button.event.press |> THEN { TEXT { left } }
    right_button.event.press |> THEN { TEXT { right } }
}
"#
    }

    fn hold_conformance_source() -> &'static str {
        r#"
increment_button: LINK

counter: 0 |> HOLD state {
    increment_button.event.press |> THEN { state + 1 }
}
"#
    }

    fn link_conformance_source() -> &'static str {
        r#"
increment_button: LINK

pressed: increment_button.event.press |> THEN { TEXT { pressed } }
"#
    }

    fn parse_static_variables(
        source: &str,
    ) -> (
        SourceCode,
        HashMap<String, static_expression::Spanned<static_expression::Expression>>,
    ) {
        let source_code = SourceCode::new(source.to_string());
        let source_str = source_code.as_str();
        let (mut tokens, lex_errors) = lexer().parse(source_str).into_output_errors();
        assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}");
        let Some(mut tokens) = tokens.take() else {
            panic!("source should lex");
        };
        tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

        let (ast, parse_errors) = parser()
            .parse(tokens.map(
                span_at(source_str.len()),
                |Spanned {
                     node,
                     span,
                     persistence: _,
                 }| { (node, span) },
            ))
            .into_output_errors();
        assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}");
        let ast = ast.expect("source should parse");
        let ast = resolve_references(ast).expect("source should resolve references");
        let static_ast = static_expression::convert_expressions(source_code.clone(), ast);

        let mut variables = HashMap::new();
        for expr in static_ast {
            if let static_expression::Expression::Variable(variable) = expr.node {
                variables.insert(variable.name.to_string(), variable.value);
            }
        }
        (source_code, variables)
    }

    fn parse_static_program(
        source: &str,
    ) -> (
        SourceCode,
        Vec<static_expression::Spanned<static_expression::Expression>>,
    ) {
        let source_code = SourceCode::new(source.to_string());
        let source_str = source_code.as_str();
        let (mut tokens, lex_errors) = lexer().parse(source_str).into_output_errors();
        assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}");
        let Some(mut tokens) = tokens.take() else {
            panic!("source should lex");
        };
        tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

        let (ast, parse_errors) = parser()
            .parse(tokens.map(
                span_at(source_str.len()),
                |Spanned {
                     node,
                     span,
                     persistence: _,
                 }| { (node, span) },
            ))
            .into_output_errors();
        assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}");
        let ast = ast.expect("source should parse");
        let ast = resolve_references(ast).expect("source should resolve references");
        let static_ast = static_expression::convert_expressions(source_code.clone(), ast);
        (source_code, static_ast)
    }

    fn evaluate_program(source: &str) -> (Arc<Object>, ConstructContext, ScopeDestroyGuard) {
        let (source_code, expressions) = parse_static_program(source);
        let (
            root_object,
            construct_context,
            _function_registry,
            _module_loader,
            _references,
            _links,
            _pass_throughs,
            scope_guard,
        ) = evaluate_with_registry(
            source_code,
            expressions,
            "",
            VirtualFilesystem::new(),
            FunctionRegistry::new(),
            ModuleLoader::default(),
        )
        .expect("program should evaluate");
        (root_object, construct_context, scope_guard)
    }

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

    async fn send_edit_committed(
        construct_context: ConstructContext,
        sender: crate::engine::NamedChannel<Value>,
        row: f64,
        column: f64,
        text: &str,
    ) {
        let value = Value::from_json(
            &json!({
                "row": row,
                "column": column,
                "text": text,
            }),
            ConstructId::new("test.edit_committed"),
            construct_context,
            boon::parser::PersistenceId::new(),
            ActorContext::default(),
        );

        sender
            .send(value)
            .await
            .expect("edit_committed send should succeed");
    }

    async fn send_link_signal(
        construct_context: ConstructContext,
        sender: crate::engine::NamedChannel<Value>,
        value: serde_json::Value,
        construct_id: &'static str,
    ) {
        let value = Value::from_json(
            &value,
            ConstructId::new(construct_id),
            construct_context,
            boon::parser::PersistenceId::new(),
            ActorContext::default(),
        );

        sender
            .send(value)
            .await
            .expect("link signal send should succeed");
    }

    fn append_from_link_source() -> &'static str {
        r#"
edit_committed: LINK

overrides: LIST { } |> HOLD state {
    edit_committed |> THEN {
        state |> List/append(item:
            [row: edit_committed.row, column: edit_committed.column, text: edit_committed.text]
        )
    }
}
"#
    }

    fn cells_override_upsert_source() -> &'static str {
        r#"
FUNCTION default_formula(column, row) {
    TEXT { 5 }
}

edit_committed: LINK

overrides: LIST { } |> HOLD state {
    edit_committed |> THEN {
        state
        |> List/remove(item, on:
            edit_committed |> THEN {
                item.row == edit_committed.row |> WHEN {
                    True => item.column == edit_committed.column |> WHEN {
                        True => []
                        __ => SKIP
                    }
                    __ => SKIP
                }
            }
        )
        |> List/append(item:
            edit_committed.text == default_formula(column: edit_committed.column, row: edit_committed.row) |> WHEN {
                True => SKIP
                __ => [row: edit_committed.row, column: edit_committed.column, text: edit_committed.text]
            }
        )
    }
}
"#
    }

    fn repeated_scalar_condition_source() -> &'static str {
        r#"
trigger: LINK

count: 0 |> HOLD state {
    trigger.key == TEXT { Enter } |> WHEN {
        True => state + 1
        __ => SKIP
    }
}
"#
    }

    fn nested_cells_map_source(with_link: bool) -> String {
        let mapped_element = if with_link {
            "Element/label(element: [], style: [width: 80], label: TEXT { x }) |> LINK { cell.cell_elements.display }"
        } else {
            "Element/label(element: [], style: [width: 80], label: TEXT { x })"
        };

        format!(
            r#"
FUNCTION make_cell(column, row) {{
    [column: column row: row cell_elements: [display: LINK, editing: LINK]]
}}

FUNCTION make_row_cells(row) {{ LIST {{
    make_cell(column: 1, row: row)
    make_cell(column: 2, row: row)
}} }}

FUNCTION make_cell_element(cell) {{
    {mapped_element}
}}

cells: make_row_cells(row: 1)
elements: cells |> List/map(cell, new: make_cell_element(cell: cell))
"#
        )
    }

    fn nested_cells_map_helper_when_while_source() -> String {
        r#"
FUNCTION make_cell(column, row) {
    [column: column row: row]
}

FUNCTION make_row_cells(row) { LIST {
    make_cell(column: 1, row: row)
    make_cell(column: 2, row: row)
} }

FUNCTION is_editing_cell(cell) {
    editing_cell.row == cell.row |> WHEN {
        True => editing_cell.column == cell.column
        __ => False
    }
}

FUNCTION make_cell_element(cell) {
    is_editing_cell(cell: cell) |> WHILE {
        True => Element/label(element: [], style: [width: 80], label: TEXT { y })
        False => Element/label(element: [], style: [width: 80], label: TEXT { x })
    }
}

cells: make_row_cells(row: 1)
editing_cell: [row: 0, column: 0]
elements: cells |> List/map(cell, new: make_cell_element(cell: cell))
"#
        .to_string()
    }

    fn nested_cells_map_helper_when_while_text_input_source() -> String {
        r#"
FUNCTION make_cell(column, row) {
    [column: column row: row cell_elements: [display: LINK, editing: LINK]]
}

FUNCTION make_row_cells(row) { LIST {
    make_cell(column: 1, row: row)
    make_cell(column: 2, row: row)
} }

FUNCTION is_editing_cell(cell) {
    editing_cell.row == cell.row |> WHEN {
        True => editing_cell.column == cell.column
        __ => False
    }
}

FUNCTION make_cell_element(cell) {
    is_editing_cell(cell: cell) |> WHILE {
        True => Element/text_input(element: [event: [input: LINK, key_down: LINK, blur: LINK]] style: [width: 80] label: Hidden[text: TEXT { cell }] text: TEXT {  } placeholder: [text: Text/empty()] focus: True) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [], style: [width: 80], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}

cells: make_row_cells(row: 1)
editing_cell: [row: 0, column: 0]
elements: cells |> List/map(cell, new: make_cell_element(cell: cell))
"#
        .to_string()
    }

    fn nested_cells_map_helper_when_while_nested_link_source() -> String {
        r#"
FUNCTION make_cell(column, row) {
    [column: column row: row cell_elements: [display: LINK, editing: LINK]]
}

FUNCTION make_row_cells(row) { LIST {
    make_cell(column: 1, row: row)
    make_cell(column: 2, row: row)
} }

FUNCTION is_editing_cell(cell) {
    editing_cell.row == cell.row |> WHEN {
        True => editing_cell.column == cell.column
        __ => False
    }
}

FUNCTION make_cell_element(cell) {
    is_editing_cell(cell: cell) |> WHILE {
        True => Element/label(element: [], style: [width: 80], label: TEXT { y })
        False => Element/label(element: [], style: [width: 80], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}

cells: make_row_cells(row: 1)
editing_cell: [row: 0, column: 0]
elements: cells |> List/map(cell, new: make_cell_element(cell: cell))
"#
        .to_string()
    }

    fn nested_cells_map_helper_when_while_nested_link_toggle_source() -> String {
        r#"
FUNCTION make_cell(column, row) {
    [column: column row: row cell_elements: [display: LINK, editing: LINK]]
}

FUNCTION make_row_cells(row) { LIST {
    make_cell(column: 1, row: row)
    make_cell(column: 2, row: row)
} }

FUNCTION is_editing_cell(cell) {
    editing_cell.row == cell.row |> WHEN {
        True => editing_cell.column == cell.column
        __ => False
    }
}

FUNCTION make_cell_element(cell) {
    is_editing_cell(cell: cell) |> WHILE {
        True => Element/label(element: [], style: [width: 80], label: TEXT { y }) |> LINK { cell.cell_elements.editing }
        False => Element/label(element: [], style: [width: 80], label: TEXT { x }) |> LINK { cell.cell_elements.display }
    }
}

toggle: LINK
cells: make_row_cells(row: 1)
editing_cell: [row: 1, column: 1] |> HOLD state {
    toggle |> THEN { [row: 0, column: 0] }
}
elements: cells |> List/map(cell, new: make_cell_element(cell: cell))
"#
        .to_string()
    }

    fn nested_cells_reopen_editing_source() -> String {
        r#"
FUNCTION make_cell(column, row) {
    [column: column row: row cell_elements: [display: LINK, editing: LINK]]
}

FUNCTION make_row_cells(row) { LIST {
    make_cell(column: 1, row: row)
} }

FUNCTION is_editing_cell(cell) {
    editing_active |> WHEN {
        True => editing_row == cell.row |> WHEN {
            True => editing_column == cell.column
            __ => False
        }
        __ => False
    }
}

FUNCTION make_cell_element(cell) {
    BLOCK {
        display_element: [event: [double_click: LINK]]
        editing_element: [event: [key_down: LINK, blur: LINK]]

        edit_started_row_link:
            display_element.event.double_click |> THEN { cell.row }
            |> LINK { event_ports.edit_started_row }
        edit_started_column_link:
            display_element.event.double_click |> THEN { cell.column }
            |> LINK { event_ports.edit_started_column }
        edit_started_active_link:
            display_element.event.double_click |> THEN { True }
            |> LINK { event_ports.edit_active_event }

        edit_cancelled_link:
            editing_element.event.key_down.key
            |> WHEN { Escape => False __ => SKIP }
            |> LINK { event_ports.edit_active_event }
        edit_blurred_link:
            editing_element.event.blur
            |> THEN { False }
            |> LINK { event_ports.edit_active_event }

        is_editing_cell(cell: cell) |> WHILE {
            True => Element/text_input(
                element: editing_element
                style: [width: 80]
                label: Hidden[text: TEXT { cell }]
                text: Text/empty()
                placeholder: [text: Text/empty()]
                focus: True
            )
            |> LINK { cell.cell_elements.editing }

            False => Element/label(
                element: display_element
                style: [width: 80]
                label: TEXT { x }
            )
            |> LINK { cell.cell_elements.display }
        }
    }
}

event_ports: [
    edit_started_row: LINK
    edit_started_column: LINK
    edit_active_event: LINK
]

editing_row: 0 |> HOLD state { event_ports.edit_started_row }
editing_column: 0 |> HOLD state { event_ports.edit_started_column }
editing_active: False |> HOLD state { event_ports.edit_active_event }

cells: make_row_cells(row: 1)
elements: cells |> List/map(cell, new: make_cell_element(cell: cell))
"#
        .to_string()
    }

    fn mapped_todo_item_event_link_source() -> &'static str {
        r#"
FUNCTION new_todo(title) {
    [
        title: title
        todo_elements: [
            todo_title_element: LINK
            todo_checkbox: LINK
        ]
    ]
}

FUNCTION todo_checkbox(todo) {
    Element/checkbox(
        element: [event: [click: LINK]]
        style: []
        label: Hidden[text: TEXT { Toggle }]
        checked: False
        icon: Element/container(element: [], style: [], child: NoElement)
    )
    |> LINK { todo.todo_elements.todo_checkbox }
}

FUNCTION todo_item(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 0
        style: []
        items: LIST {
            todo_checkbox(todo: todo)
            Element/label(
                element: [event: [double_click: LINK]]
                style: []
                label: todo.title
            )
            |> LINK { todo.todo_elements.todo_title_element }
        }
    )
}

todos: LIST {
    new_todo(title: TEXT { Buy groceries })
}

elements: todos |> List/map(todo, new: todo_item(todo: todo))
"#
    }

    fn linked_checkbox_updates_sibling_hold_source() -> &'static str {
        r#"
FUNCTION todo_checkbox(todo) {
    Element/checkbox(
        element: [event: [click: LINK]]
        style: []
        label: Hidden[text: TEXT { Toggle }]
        checked: False
        icon: Element/container(element: [], style: [], child: NoElement)
    )
    |> LINK { todo.todo_elements.todo_checkbox }
}

FUNCTION new_todo() {
    [
        todo_elements: [
            todo_checkbox: LINK
        ]

        completed: False |> HOLD state {
            todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

todo: new_todo()
checkbox: todo_checkbox(todo: todo)
"#
    }

    fn read_example_source(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|error| {
            panic!("failed to read example source {path}: {error}");
        })
    }

    fn list_map_external_dependency_source() -> &'static str {
        r#"
toggle: LINK

show_shifted: False |> HOLD state {
    toggle |> THEN { state |> Bool/not() }
}

items: LIST { 1 2 3 }

mapped: items |> List/map(item, new:
    show_shifted |> WHEN {
        True => item + 10
        __ => item
    }
)
"#
    }

    fn cells_mapped_formula_dependency_source() -> &'static str {
        r#"
FUNCTION matching_overrides(column, row) {
    overrides |> List/retain(item, if:
        item.row == row |> WHEN {
            True => item.column == column
            __ => False
        }
    )
}

FUNCTION cell_formula(column, row) {
    BLOCK {
        matches: matching_overrides(column: column, row: row)
        match_count: matches |> List/count()
        match_count == 0 |> WHEN {
            True => default_formula(column: column, row: row)
            __ => BLOCK {
                latest_match: matches |> List/get(index: match_count)
                latest_match.text
            }
        }
    }
}

FUNCTION default_formula(column, row) {
    column == 1 |> WHEN {
        True => row == 1 |> WHEN {
            True => TEXT { 5 }
            __ => TEXT { 10 }
        }
        __ => TEXT { =add(A1, A2) }
    }
}

FUNCTION compute_value(formula_text) {
    formula_text |> Text/starts_with(prefix: TEXT { = }) |> WHILE {
        False => formula_text |> Text/to_number() |> WHILE { NaN => 0 number => number }
        __ => compute_value(formula_text: cell_formula(column: 1, row: 1))
            + compute_value(formula_text: cell_formula(column: 1, row: 2))
    }
}

FUNCTION make_cell(column, row) { [column: column row: row] }

FUNCTION make_cell_text(cell) {
    BLOCK {
        formula_text: cell_formula(column: cell.column, row: cell.row)
        display_value: compute_value(formula_text: formula_text)
        TEXT { {display_value} }
    }
}

edit_committed: LINK

overrides: LIST { } |> HOLD state {
    edit_committed |> THEN {
        state
        |> List/retain(item, if:
            item.row == edit_committed.row |> WHEN {
                True => item.column == edit_committed.column |> WHEN {
                    True => False
                    __ => True
                }
                __ => True
            }
        )
        |> List/append(item: [
            row: edit_committed.row
            column: edit_committed.column
            text: edit_committed.text
        ])
    }
}

formula_a1: cell_formula(column: 1, row: 1)
cells: LIST { make_cell(column: 1, row: 1) make_cell(column: 2, row: 1) }
mapped: cells |> List/map(cell, new: make_cell_text(cell: cell))
"#
    }

    fn shared_snapshot_root_toggle_source() -> &'static str {
        r#"
toggle_all: LINK

FUNCTION make_todo(initial_completed) {
    [
        completed: initial_completed |> HOLD state {
            toggle_all |> THEN { all_completed |> Bool/not() }
        }
    ]
}

todos: LIST {
    make_todo(initial_completed: False)
    make_todo(initial_completed: False)
    make_todo(initial_completed: False)
    make_todo(initial_completed: True)
}

completed_todos_count: todos |> List/retain(item, if: item.completed) |> List/count()
all_completed: completed_todos_count == 4
"#
    }

    #[test]
    fn latest_press_sequence_preserves_two_then_arms_for_evaluator() {
        let (_source_code, variables) = parse_static_variables(latest_conformance_source());
        let selected = variables
            .get("selected")
            .cloned()
            .expect("selected variable should exist");

        let static_expression::Expression::Latest { inputs } = selected.node else {
            panic!("expected selected to remain a LATEST expression");
        };

        assert_eq!(inputs.len(), 2, "expected two LATEST inputs");

        let arm_texts: Vec<String> = inputs
            .iter()
            .map(|input| {
                let chain = flatten_pipe_chain(input.clone());
                assert_eq!(
                    chain.len(),
                    2,
                    "expected each LATEST arm to stay a two-step pipe for Actors evaluation"
                );

                let then_expr = chain.last().expect("pipe should have THEN step");
                let static_expression::Expression::Then { body } = &then_expr.node else {
                    panic!("expected LATEST arm to end in THEN");
                };
                match &body.node {
                    static_expression::Expression::TextLiteral { parts, .. }
                        if parts.len() == 1 =>
                    {
                        match &parts[0] {
                            static_expression::TextPart::Text(text) => text.to_string(),
                            other => panic!("expected text literal part, got {other:?}"),
                        }
                    }
                    other => {
                        panic!("expected THEN body to be single-part TEXT literal, got {other:?}")
                    }
                }
            })
            .collect();

        assert_eq!(arm_texts, vec!["left".to_string(), "right".to_string()]);

        let expected = select_latest(&[
            LatestCandidate::new(KernelValue::from("left"), TickSeq::new(TickId(1), 1)),
            LatestCandidate::new(KernelValue::from("right"), TickSeq::new(TickId(1), 2)),
        ]);
        assert_eq!(expected, KernelValue::from("right"));
    }

    #[test]
    fn hold_press_sequence_preserves_initial_state_and_then_body_shape() {
        let (_source_code, variables) = parse_static_variables(hold_conformance_source());
        let counter = variables
            .get("counter")
            .cloned()
            .expect("counter variable should exist");

        let chain = flatten_pipe_chain(counter);
        assert_eq!(
            chain.len(),
            2,
            "expected HOLD expression to remain a two-step pipe in Actors evaluator input"
        );
        assert!(
            matches!(chain[0].node, static_expression::Expression::Literal(static_expression::Literal::Number(n)) if (n - 0.0).abs() < f64::EPSILON),
            "expected HOLD initial value to stay as literal 0"
        );

        let static_expression::Expression::Hold { state_param, body } = &chain[1].node else {
            panic!("expected second pipe step to be HOLD");
        };
        assert_eq!(state_param.to_string(), "state");

        let body_chain = flatten_pipe_chain((**body).clone());
        assert_eq!(
            body_chain.len(),
            2,
            "expected HOLD body to remain an event |> THEN pipeline"
        );
        let static_expression::Expression::Then { body: then_body } = &body_chain[1].node else {
            panic!("expected HOLD body to end in THEN");
        };
        match &then_body.node {
            static_expression::Expression::ArithmeticOperator(
                static_expression::ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                },
            ) => {
                assert!(
                    matches!(
                        operand_a.node,
                        static_expression::Expression::Alias(
                            static_expression::Alias::WithoutPassed { ref parts, .. }
                        ) if parts.len() == 1 && parts[0].to_string() == "state"
                    ),
                    "expected HOLD body lhs to reference state"
                );
                assert!(
                    matches!(
                        operand_b.node,
                        static_expression::Expression::Literal(
                            static_expression::Literal::Number(n)
                        ) if (n - 1.0).abs() < f64::EPSILON
                    ),
                    "expected HOLD body rhs to stay literal 1"
                );
            }
            other => panic!("expected HOLD THEN body to be `state + 1`, got {other:?}"),
        }
    }

    #[test]
    fn link_press_sequence_preserves_alias_path_into_then() {
        let (_source_code, variables) = parse_static_variables(link_conformance_source());

        let increment_button = variables
            .get("increment_button")
            .cloned()
            .expect("increment_button variable should exist");
        assert!(
            matches!(increment_button.node, static_expression::Expression::Link),
            "expected increment_button to stay a LINK placeholder"
        );

        let pressed = variables
            .get("pressed")
            .cloned()
            .expect("pressed variable should exist");
        let chain = flatten_pipe_chain(pressed);
        assert_eq!(
            chain.len(),
            2,
            "expected LINK event consumer to remain a two-step pipe"
        );
        assert!(
            matches!(
                &chain[0].node,
                static_expression::Expression::Alias(
                    static_expression::Alias::WithoutPassed { parts, .. }
                ) if parts.iter().map(|part| part.to_string()).collect::<Vec<_>>()
                    == vec!["increment_button".to_string(), "event".to_string(), "press".to_string()]
            ),
            "expected first step to keep increment_button.event.press alias path"
        );
        let static_expression::Expression::Then { body } = &chain[1].node else {
            panic!("expected LINK event consumer to end in THEN");
        };
        assert!(
            matches!(
                body.node,
                static_expression::Expression::TextLiteral { ref parts, .. }
                    if parts.len() == 1
                        && matches!(&parts[0], static_expression::TextPart::Text(text) if text.to_string() == "pressed")
            ),
            "expected THEN body to stay TEXT {{ pressed }}"
        );
    }

    #[test]
    fn spread_resolution_fast_path_uses_current_values_only_when_ready() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let scope_id = actor_context.scope_id();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let spread_actor = crate::engine::create_constant_actor(
            boon::parser::PersistenceId::new(),
            Object::new_value(
                crate::engine::ConstructInfo::new("test.spread.object", None, "test spread object"),
                construct_context.clone(),
                crate::engine::ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    crate::engine::ConstructInfo::new(
                        "test.spread.field",
                        None,
                        "test spread field",
                    ),
                    "from_spread",
                    crate::engine::create_constant_actor(
                        boon::parser::PersistenceId::new(),
                        Value::Text(
                            Arc::new(crate::engine::Text::new(
                                crate::engine::ConstructInfo::new(
                                    "test.spread.text",
                                    None,
                                    "test spread text",
                                ),
                                construct_context.clone(),
                                "spread",
                            )),
                            crate::engine::ValueMetadata::new(
                                crate::engine::ValueIdempotencyKey::new(),
                            ),
                        ),
                        scope_id,
                    ),
                    boon::parser::PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );

        let explicit_variable = Variable::new_arc(
            crate::engine::ConstructInfo::new("test.explicit.field", None, "test explicit field"),
            "explicit",
            crate::engine::create_constant_actor(
                boon::parser::PersistenceId::new(),
                Value::Text(
                    Arc::new(crate::engine::Text::new(
                        crate::engine::ConstructInfo::new(
                            "test.explicit.text",
                            None,
                            "test explicit text",
                        ),
                        construct_context,
                        "explicit",
                    )),
                    crate::engine::ValueMetadata::new(crate::engine::ValueIdempotencyKey::new()),
                ),
                scope_id,
            ),
            boon::parser::PersistenceId::new(),
            actor_context.scope.clone(),
        );

        let resolved = super::try_resolve_spread_struct_variables_now(
            &[spread_actor],
            vec![explicit_variable.clone()],
        )
        .expect("constant spread actors should resolve synchronously");
        assert_eq!(
            resolved.len(),
            2,
            "spread fields plus explicit fields should be merged"
        );
        assert_eq!(resolved[0].name(), "from_spread");
        assert_eq!(resolved[1].name(), "explicit");

        let unresolved_spread =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        assert!(
            super::try_resolve_spread_struct_variables_now(
                &[unresolved_spread],
                vec![explicit_variable]
            )
            .is_none(),
            "not-ready spread actors should fall back to the async merge path"
        );
    }

    #[test]
    fn async_spread_resolution_uses_current_value_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::new(),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let old_spread = crate::engine::Object::new_value(
            crate::engine::ConstructInfo::new(
                "test.async_spread.old",
                None,
                "test async spread old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            [Variable::new_arc(
                crate::engine::ConstructInfo::new(
                    "test.async_spread.old_var",
                    None,
                    "test async spread old var",
                ),
                "from_spread",
                crate::engine::create_constant_actor(
                    boon::parser::PersistenceId::new(),
                    Value::Text(
                        Arc::new(crate::engine::Text::new(
                            crate::engine::ConstructInfo::new(
                                "test.async_spread.old_text",
                                None,
                                "test async spread old text",
                            ),
                            construct_context.clone(),
                            "old",
                        )),
                        crate::engine::ValueMetadata::new(crate::engine::ValueIdempotencyKey::new()),
                    ),
                    scope_id,
                ),
                boon::parser::PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );
        let current_spread = crate::engine::Object::new_value(
            crate::engine::ConstructInfo::new(
                "test.async_spread.current",
                None,
                "test async spread current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            [Variable::new_arc(
                crate::engine::ConstructInfo::new(
                    "test.async_spread.current_var",
                    None,
                    "test async spread current var",
                ),
                "from_spread",
                crate::engine::create_constant_actor(
                    boon::parser::PersistenceId::new(),
                    Value::Text(
                        Arc::new(crate::engine::Text::new(
                            crate::engine::ConstructInfo::new(
                                "test.async_spread.current_text",
                                None,
                                "test async spread current text",
                            ),
                            construct_context.clone(),
                            "current",
                        )),
                        crate::engine::ValueMetadata::new(crate::engine::ValueIdempotencyKey::new()),
                    ),
                    scope_id,
                ),
                boon::parser::PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );

        let spread_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        spread_actor.store_value_directly(old_spread);
        spread_actor.store_value_directly(current_spread);

        let resolved = block_on(super::resolve_spread_struct_variables(
            vec![spread_actor],
            Vec::new(),
        ));
        assert_eq!(
            resolved.len(),
            1,
            "async spread merge should keep one spread field"
        );
        assert_eq!(resolved[0].name(), "from_spread");

        let Value::Text(text, _) = resolved[0]
            .value_actor()
            .current_value()
            .expect("resolved spread field should expose current value")
        else {
            panic!("resolved spread field should stay text");
        };
        assert_eq!(text.text(), "current");
    }

    #[test]
    fn list_append_hold_snapshot_fast_path_returns_immediate_value_when_ready() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let base_actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::new(),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let existing_item = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.list_append.existing_item",
                None,
                "test list append existing item",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            base_actor_context.clone(),
            "existing",
        );
        let list_actor = crate::engine::List::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.list_append.source_list",
                None,
                "test list append source list",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            base_actor_context.clone(),
            vec![existing_item],
        );

        let hold_actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            piped: Some(list_actor),
            backpressure_permit: Some(crate::engine::BackpressureCoordinator::new()),
            ..ActorContext::default()
        };
        let appended_item = crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.list_append.appended_item",
                None,
                "test list append appended item",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "appended",
        );

        let Value::List(result_list, _) = super::try_build_list_append_hold_snapshot_now(
            &hold_actor_context
                .piped
                .clone()
                .expect("HOLD snapshot context should carry the piped source list"),
            &crate::engine::create_constant_actor(
                boon::parser::PersistenceId::new(),
                appended_item,
                scope_id,
            ),
            construct_context.clone(),
            hold_actor_context,
        )
        .expect("ready HOLD snapshot append should now produce an immediate value") else {
            panic!("List/append HOLD snapshot result should be a list");
        };

        let snapshot = block_on(result_list.snapshot());
        assert_eq!(
            snapshot.len(),
            2,
            "snapshot should contain existing and appended items"
        );
        let first_text = snapshot[0]
            .1
            .current_value()
            .expect("first item should have current value");
        let second_text = snapshot[1]
            .1
            .current_value()
            .expect("second item should have current value");

        let Value::Text(first_text, _) = first_text else {
            panic!("first item should stay text");
        };
        let Value::Text(second_text, _) = second_text else {
            panic!("second item should stay text");
        };

        assert_eq!(first_text.text(), "existing");
        assert_eq!(second_text.text(), "appended");
    }

    #[test]
    fn list_append_hold_snapshot_async_helper_uses_current_item_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::new(),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let existing_item = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.list_append.async_snapshot.existing_item",
                None,
                "test list append async snapshot existing item",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            actor_context.clone(),
            "existing",
        );
        let list_actor = crate::engine::List::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.list_append.async_snapshot.source_list",
                None,
                "test list append async snapshot source list",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            actor_context.clone(),
            vec![existing_item],
        );

        let item_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        item_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.list_append.async_snapshot.old_item",
                None,
                "test list append async snapshot old item",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        item_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.list_append.async_snapshot.current_item",
                None,
                "test list append async snapshot current item",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let Value::List(result_list, _) = block_on(super::build_list_append_hold_snapshot_value(
            &list_actor,
            &item_actor,
            construct_context,
            actor_context,
        )) else {
            panic!("async List/append HOLD snapshot helper should return a list");
        };

        let snapshot = block_on(result_list.snapshot());
        assert_eq!(
            snapshot.len(),
            2,
            "snapshot should contain existing and appended items"
        );

        let Value::Text(first_text, _) = snapshot[0]
            .1
            .current_value()
            .expect("existing item should expose current value")
        else {
            panic!("existing item should stay text");
        };
        let Value::Text(second_text, _) = snapshot[1]
            .1
            .current_value()
            .expect("appended item should expose current value")
        else {
            panic!("appended item should stay text");
        };

        assert_eq!(first_text.text(), "existing");
        assert_eq!(
            second_text.text(),
            "current",
            "async snapshot helper should append the latest current item value, not stale buffered history"
        );
    }

    #[test]
    fn freeze_snapshot_actor_now_returns_immediate_materialized_list_actor_when_ready() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::new(),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let item_actor = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.freeze_snapshot.item",
                None,
                "test freeze snapshot item",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            actor_context.clone(),
            "frozen",
        );
        let list_actor = crate::engine::List::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.freeze_snapshot.list",
                None,
                "test freeze snapshot list",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            actor_context.clone(),
            vec![item_actor],
        );

        let frozen_actor =
            super::try_freeze_snapshot_actor_now(&list_actor, construct_context, actor_context)
                .expect("ready list actor should freeze synchronously");

        let Value::List(frozen_list, _) = frozen_actor
            .current_value()
            .expect("frozen actor should expose current value immediately")
        else {
            panic!("frozen snapshot actor should contain a list");
        };

        let snapshot = block_on(frozen_list.snapshot());
        assert_eq!(snapshot.len(), 1, "frozen list should keep its single item");
        let Value::Text(text, _) = snapshot[0]
            .1
            .current_value()
            .expect("frozen item should expose current value immediately")
        else {
            panic!("frozen list item should stay text");
        };
        assert_eq!(text.text(), "frozen");
    }

    #[test]
    fn one_shot_actor_value_stream_uses_current_value_immediately_when_ready() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = crate::engine::create_constant_actor(
            boon::parser::PersistenceId::new(),
            Value::Text(
                Arc::new(crate::engine::Text::new(
                    crate::engine::ConstructInfo::new(
                        "test.one_shot.current_value",
                        None,
                        "test one shot current value",
                    ),
                    ConstructContext {
                        construct_storage: Arc::new(
                            crate::engine::ConstructStorage::in_memory_for_tests(
                                std::collections::BTreeMap::new(),
                            ),
                        ),
                        virtual_fs: VirtualFilesystem::new(),
                        bridge_scope_id: None,
                        scene_ctx: None,
                    },
                    "ready",
                )),
                crate::engine::ValueMetadata::new(crate::engine::ValueIdempotencyKey::new()),
            ),
            scope_id,
        );

        let mut stream = std::pin::pin!(super::one_shot_actor_value_stream(actor, ()));
        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.as_mut().next()) else {
            panic!("one_shot_actor_value_stream should emit immediately from current actor state");
        };
        assert_eq!(text.text(), "ready");
    }

    #[test]
    fn seeded_actor_value_and_future_stream_uses_current_value_immediately_when_ready() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor = crate::engine::create_constant_actor(
            boon::parser::PersistenceId::new(),
            Value::Text(
                Arc::new(crate::engine::Text::new(
                    crate::engine::ConstructInfo::new(
                        "test.seeded_actor_stream.current_value",
                        None,
                        "test seeded actor stream current value",
                    ),
                    ConstructContext {
                        construct_storage: Arc::new(
                            crate::engine::ConstructStorage::in_memory_for_tests(
                                std::collections::BTreeMap::new(),
                            ),
                        ),
                        virtual_fs: VirtualFilesystem::new(),
                        bridge_scope_id: None,
                        scene_ctx: None,
                    },
                    "ready",
                )),
                crate::engine::ValueMetadata::new(crate::engine::ValueIdempotencyKey::new()),
            ),
            scope_id,
        );

        let Poll::Ready(Some((Value::Text(text, _), mut future_stream))) = poll_once(
            super::take_first_seeded_actor_value_and_future_stream(actor),
        ) else {
            panic!("seeded helper should resolve the ready current value on first poll");
        };
        assert_eq!(text.text(), "ready");
        assert!(
            matches!(poll_once(future_stream.next()), Poll::Ready(None)),
            "constant actor should have no future updates after the seeded current value"
        );
    }

    #[test]
    fn hold_body_subscription_uses_current_value_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.hold_body_subscription.old",
                None,
                "test hold body subscription old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.hold_body_subscription.current",
                None,
                "test hold body subscription current",
            ),
            construct_context,
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let mut stream = std::pin::pin!(super::hold_body_current_and_future_values(actor));
        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.as_mut().next()) else {
            panic!("HOLD body helper should emit the current value immediately");
        };
        assert_eq!(text.text(), "current");
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "HOLD body helper should skip buffered history and wait for future updates"
        );
    }

    #[test]
    fn restored_list_append_item_stream_uses_current_value_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.restored_list_append_item.old",
                None,
                "test restored list append item old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.restored_list_append_item.current",
                None,
                "test restored list append item current",
            ),
            construct_context,
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let mut stream = std::pin::pin!(
            super::restored_list_append_item_current_and_future_values(actor)
        );
        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.as_mut().next()) else {
            panic!("restored List/append stream should emit the current value immediately");
        };
        assert_eq!(text.text(), "current");
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "restored List/append stream should skip buffered history and wait for future updates"
        );
    }

    #[test]
    fn actor_current_value_or_wait_uses_current_value_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.link_resolution_value.old",
                None,
                "test link resolution value old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.link_resolution_value.current",
                None,
                "test link resolution value current",
            ),
            construct_context,
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let Poll::Ready(Some(Value::Text(text, _))) =
            poll_once(super::actor_current_value_or_wait(&actor))
        else {
            panic!("current-value helper should use the current value immediately");
        };
        assert_eq!(text.text(), "current");
    }

    #[test]
    fn text_literal_field_helper_uses_current_object_value_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let old_text_actor = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.text_literal_field.old",
                None,
                "test text literal field old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            actor_context.clone(),
            "old",
        );
        let current_text_actor = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.text_literal_field.current",
                None,
                "test text literal field current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            actor_context.clone(),
            "current",
        );

        let old_item_value = Object::new_value(
            crate::engine::ConstructInfo::new(
                "test.text_literal_field.object.old",
                None,
                "test text literal field object old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            [Variable::new_arc(
                crate::engine::ConstructInfo::new(
                    "test.text_literal_field.variable.old",
                    None,
                    "test text literal field variable old",
                ),
                "text",
                old_text_actor,
                boon::parser::PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );
        let current_item_value = Object::new_value(
            crate::engine::ConstructInfo::new(
                "test.text_literal_field.object.current",
                None,
                "test text literal field object current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            [Variable::new_arc(
                crate::engine::ConstructInfo::new(
                    "test.text_literal_field.variable.current",
                    None,
                    "test text literal field variable current",
                ),
                "text",
                current_text_actor,
                boon::parser::PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );

        let item_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        item_actor.store_value_directly(old_item_value);
        item_actor.store_value_directly(current_item_value);

        let field_path = vec![String::from("text")];
        let Poll::Ready(Some(field_actor)) = poll_once(
            super::actor_field_actor_from_current_or_wait(&item_actor, &field_path),
        ) else {
            panic!("text literal field helper should resolve the current object field actor");
        };

        let Value::Text(text, _) = field_actor
            .current_value()
            .expect("resolved field actor should expose its current value")
        else {
            panic!("resolved field actor should expose a text value");
        };
        assert_eq!(text.text(), "current");
    }

    #[test]
    fn extract_field_path_uses_current_field_value_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };

        let field_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        field_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.extract_field_path.old",
                None,
                "test extract field path old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        field_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.extract_field_path.current",
                None,
                "test extract field path current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let base_value = Object::new_value(
            crate::engine::ConstructInfo::new(
                "test.extract_field_path.object",
                None,
                "test extract field path object",
            ),
            construct_context,
            crate::engine::ValueIdempotencyKey::new(),
            [Variable::new_arc(
                crate::engine::ConstructInfo::new(
                    "test.extract_field_path.variable",
                    None,
                    "test extract field path variable",
                ),
                "text",
                field_actor,
                boon::parser::PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );

        let field_path = vec![String::from("text")];
        let Value::Text(text, _) = block_on(super::extract_field_path(&base_value, &field_path))
            .expect("field path should resolve")
        else {
            panic!("resolved field path should stay text");
        };
        assert_eq!(text.text(), "current");
    }

    #[test]
    fn variable_persistence_stream_reads_restored_value_from_direct_storage_state() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let persistence_id = boon::parser::PersistenceId::new();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::from([(
                    persistence_id.in_scope(&actor_context.scope).to_string(),
                    zoon::serde_json::json!("restored"),
                )]),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let source_actor = crate::engine::create_constant_actor(
            boon::parser::PersistenceId::new(),
            Value::Text(
                Arc::new(crate::engine::Text::new(
                    crate::engine::ConstructInfo::new(
                        "test.variable.persistence.source",
                        None,
                        "test variable persistence source",
                    ),
                    construct_context.clone(),
                    "source",
                )),
                crate::engine::ValueMetadata::new(crate::engine::ValueIdempotencyKey::new()),
            ),
            scope_id,
        );

        let mut stream = std::pin::pin!(super::create_variable_persistence_stream(
            source_actor,
            construct_context.construct_storage.clone(),
            persistence_id,
            actor_context.scope.clone(),
            construct_context.clone(),
            actor_context.clone(),
            false,
        ));

        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.as_mut().next()) else {
            panic!(
                "persistence stream should emit restored value immediately from direct storage state"
            );
        };
        assert_eq!(text.text(), "restored");
    }

    #[test]
    fn variable_persistence_stream_skips_stale_buffered_history_after_restored_value() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let persistence_id = boon::parser::PersistenceId::new();
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::from([(
                    persistence_id.in_scope(&actor_context.scope).to_string(),
                    zoon::serde_json::json!("restored"),
                )]),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let source_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        source_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.variable.persistence.source.old",
                None,
                "test variable persistence source old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        source_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.variable.persistence.source.current",
                None,
                "test variable persistence source current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let mut stream = std::pin::pin!(super::create_variable_persistence_stream(
            source_actor,
            construct_context.construct_storage.clone(),
            persistence_id,
            actor_context.scope.clone(),
            construct_context,
            actor_context,
            false,
        ));

        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.as_mut().next()) else {
            panic!("persistence stream should emit restored value first");
        };
        assert_eq!(text.text(), "restored");

        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "restored persistence stream should skip buffered source history and wait for future updates"
        );
    }

    #[test]
    fn variable_persistence_stream_ignores_storage_and_skips_stale_history_when_value_changed() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let actor_context = ActorContext {
            registry_scope_id: Some(scope_id),
            ..ActorContext::default()
        };
        let persistence_id = boon::parser::PersistenceId::new();
        let scoped_id = persistence_id.in_scope(&actor_context.scope);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::in_memory_for_tests(
                std::collections::BTreeMap::from([(
                    scoped_id.to_string(),
                    zoon::serde_json::json!("restored"),
                )]),
            )),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let source_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        source_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.variable.persistence.value_changed.old",
                None,
                "test variable persistence value changed old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        source_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.variable.persistence.value_changed.current",
                None,
                "test variable persistence value changed current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));

        let mut stream = std::pin::pin!(super::create_variable_persistence_stream(
            source_actor,
            construct_context.construct_storage.clone(),
            persistence_id,
            actor_context.scope.clone(),
            construct_context.clone(),
            actor_context,
            true,
        ));

        let Poll::Ready(Some(Value::Text(text, _))) = poll_once(stream.as_mut().next()) else {
            panic!("value-changed persistence stream should emit current source value first");
        };
        assert_eq!(text.text(), "current");
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "value-changed persistence stream should skip stale buffered history and wait for future updates"
        );

        let saved = construct_context
            .construct_storage
            .load_state_now::<zoon::serde_json::Value>(scoped_id)
            .expect("current value should be persisted back to storage");
        assert_eq!(
            saved,
            zoon::serde_json::json!("current"),
            "value-changed path should overwrite stale stored value with the live current source value"
        );
    }

    #[test]
    fn text_literal_combiner_uses_current_part_values_without_replaying_stale_history() {
        let scope_id = crate::engine::create_registry_scope(None);
        let _scope_guard = ScopeDestroyGuard::new(scope_id);
        let construct_context = ConstructContext {
            construct_storage: Arc::new(crate::engine::ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        };
        let hello_actor = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.text_literal.hello",
                None,
                "test text literal hello",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..ActorContext::default()
            },
            "Hello ",
        );
        let name_actor =
            crate::engine::create_actor_forwarding(boon::parser::PersistenceId::new(), scope_id);
        name_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.text_literal.name.old",
                None,
                "test text literal name old",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "old",
        ));
        name_actor.store_value_directly(crate::engine::Text::new_value(
            crate::engine::ConstructInfo::new(
                "test.text_literal.name.current",
                None,
                "test text literal name current",
            ),
            construct_context.clone(),
            crate::engine::ValueIdempotencyKey::new(),
            "current",
        ));
        let bang_actor = crate::engine::Text::new_arc_value_actor(
            crate::engine::ConstructInfo::new(
                "test.text_literal.bang",
                None,
                "test text literal bang",
            ),
            construct_context,
            crate::engine::ValueIdempotencyKey::new(),
            ActorContext {
                registry_scope_id: Some(scope_id),
                ..ActorContext::default()
            },
            "!",
        );

        let mut stream = std::pin::pin!(super::combine_text_part_current_and_future_values(&[
            (true, hello_actor),
            (false, name_actor),
            (true, bang_actor),
        ]));
        let Poll::Ready(Some(text)) = poll_once(stream.as_mut().next()) else {
            panic!("combined text stream should emit current combined value immediately");
        };
        assert_eq!(text, "Hello current!");
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "combined text stream should not replay stale part history once current values were seeded"
        );
    }

    #[test]
    fn list_append_from_link_event_snapshots_object_fields() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(append_from_link_source());
        let edit_committed = root_object.expect_variable("edit_committed");
        let overrides = root_object.expect_variable("overrides");
        let sender = edit_committed.expect_link_value_sender();

        let updated_json = block_on(async move {
            let mut updates = overrides.value_actor().stream_from_now();
            send_edit_committed(construct_context, sender, 1.0, 1.0, "7").await;
            updates
                .next()
                .await
                .expect("overrides should emit updated list")
                .to_json()
                .await
        });

        assert_eq!(
            updated_json,
            json!([{
                "row": 1.0,
                "column": 1.0,
                "text": "7"
            }])
        );
    }

    #[test]
    fn cells_override_hold_upsert_tracks_commit_and_default_removal() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(cells_override_upsert_source());
        let edit_committed = root_object.expect_variable("edit_committed");
        let overrides = root_object.expect_variable("overrides");
        let sender = edit_committed.expect_link_value_sender();

        let (committed_json, reverted_json) = block_on(async move {
            let mut updates = overrides.value_actor().stream_from_now();

            send_edit_committed(construct_context.clone(), sender.clone(), 1.0, 1.0, "7").await;
            let committed_json = updates
                .next()
                .await
                .expect("overrides should emit committed override")
                .to_json()
                .await;

            send_edit_committed(construct_context, sender, 1.0, 1.0, "5").await;
            let reverted_json = updates
                .next()
                .await
                .expect("overrides should emit removal when reverting to default")
                .to_json()
                .await;

            (committed_json, reverted_json)
        });

        assert_eq!(
            committed_json,
            json!([{
                "row": 1.0,
                "column": 1.0,
                "text": "7"
            }])
        );
        assert_eq!(reverted_json, serde_json::Value::Array(Vec::new()));
    }

    #[test]
    #[ignore = "requires wasm/js runtime; host lib tests still touch js-sys statics"]
    fn repeated_equal_scalar_conditions_from_distinct_emissions_are_not_deduped() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(repeated_scalar_condition_source());
        let trigger = root_object.expect_variable("trigger");
        let count = root_object.expect_variable("count");
        let sender = trigger.expect_link_value_sender();

        let (first_json, second_json) = block_on(async move {
            let mut updates = count.value_actor().stream_from_now();

            send_link_signal(
                construct_context.clone(),
                sender.clone(),
                json!({
                    "key": "Enter",
                    "text": ""
                }),
                "test.trigger.first",
            )
            .await;

            let first_json = updates
                .next()
                .await
                .expect("count should emit after first Enter")
                .to_json()
                .await;

            send_link_signal(
                construct_context,
                sender,
                json!({
                    "key": "Enter",
                    "text": ""
                }),
                "test.trigger.second",
            )
            .await;

            let second_json = updates
                .next()
                .await
                .expect("count should emit after second Enter")
                .to_json()
                .await;

            (first_json, second_json)
        });

        assert_eq!(first_json, json!(1.0));
        assert_eq!(second_json, json!(2.0));
    }

    #[test]
    fn nested_cells_map_without_link_produces_label_elements() {
        let source = nested_cells_map_source(false);
        let (root_object, _construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");

        block_on(async move {
            let value = elements
                .value_actor()
                .current_value()
                .expect("elements list should have a current value");
            let Value::List(list, _) = value else {
                panic!("elements should evaluate to a list");
            };
            let snapshot = list.snapshot().await;
            assert_eq!(snapshot.len(), 2, "expected two mapped cell elements");

            for (_pid, item_actor) in snapshot {
                let item_value = item_actor
                    .current_value()
                    .expect("mapped cell item should have value");
                let Value::TaggedObject(tagged, _) = item_value else {
                    panic!("mapped cell item should be a tagged element object");
                };
                assert_eq!(tagged.tag(), "ElementLabel");
            }
        });
    }

    #[test]
    fn nested_cells_map_with_link_produces_label_elements() {
        let source = nested_cells_map_source(true);
        let (root_object, _construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");

        block_on(async move {
            let value = elements
                .value_actor()
                .current_value()
                .expect("elements list should have a current value");
            let Value::List(list, _) = value else {
                panic!("elements should evaluate to a list");
            };
            let snapshot = list.snapshot().await;
            assert_eq!(
                snapshot.len(),
                2,
                "expected two mapped linked cell elements"
            );

            for (_pid, item_actor) in snapshot {
                let item_value = item_actor
                    .current_value()
                    .expect("mapped linked cell item should have value");
                let Value::TaggedObject(tagged, _) = item_value else {
                    panic!("mapped linked cell item should be a tagged element object");
                };
                assert_eq!(tagged.tag(), "ElementLabel");
            }
        });
    }

    #[test]
    fn nested_cells_map_helper_when_while_produces_label_elements() {
        let source = nested_cells_map_helper_when_while_source();
        let (root_object, _construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");

        block_on(async move {
            let value = elements
                .value_actor()
                .current_value()
                .expect("elements list should have a current value");
            let Value::List(list, _) = value else {
                panic!("elements should evaluate to a list");
            };
            let snapshot = list.snapshot().await;
            assert_eq!(
                snapshot.len(),
                2,
                "expected two mapped helper-fed cell elements"
            );

            for (_pid, item_actor) in snapshot {
                let item_value = item_actor
                    .current_value()
                    .expect("mapped helper-fed cell item should have value");
                let Value::TaggedObject(tagged, _) = item_value else {
                    panic!("mapped helper-fed cell item should be a tagged element object");
                };
                assert_eq!(tagged.tag(), "ElementLabel");
            }
        });
    }

    #[test]
    fn nested_cells_map_helper_when_while_with_text_input_inactive_branch_produces_label_elements()
    {
        let source = nested_cells_map_helper_when_while_text_input_source();
        let (root_object, _construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");

        block_on(async move {
            let value = elements
                .value_actor()
                .current_value()
                .expect("elements list should have a current value");
            let Value::List(list, _) = value else {
                panic!("elements should evaluate to a list");
            };
            let snapshot = list.snapshot().await;
            assert_eq!(
                snapshot.len(),
                2,
                "expected two mapped helper-fed cell elements with inactive text input branch"
            );

            for (_pid, item_actor) in snapshot {
                let item_value = item_actor
                    .current_value()
                    .expect("mapped helper-fed item should have value");
                let Value::TaggedObject(tagged, _) = item_value else {
                    panic!("mapped helper-fed item should be a tagged element object");
                };
                assert_eq!(tagged.tag(), "ElementLabel");
            }
        });
    }

    #[test]
    fn nested_cells_map_helper_when_while_with_nested_link_produces_label_elements() {
        let source = nested_cells_map_helper_when_while_nested_link_source();
        let (root_object, _construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");

        block_on(async move {
            let value = elements
                .value_actor()
                .current_value()
                .expect("elements list should have a current value");
            let Value::List(list, _) = value else {
                panic!("elements should evaluate to a list");
            };
            let snapshot = list.snapshot().await;
            assert_eq!(
                snapshot.len(),
                2,
                "expected two mapped helper-fed cell elements with nested link target"
            );

            for (_pid, item_actor) in snapshot {
                let item_value = item_actor
                    .current_value()
                    .expect("mapped helper-fed nested-link item should have value");
                let Value::TaggedObject(tagged, _) = item_value else {
                    panic!("mapped helper-fed nested-link item should be a tagged element object");
                };
                assert_eq!(tagged.tag(), "ElementLabel");
            }
        });
    }

    #[test]
    #[ignore = "requires wasm/js runtime; host lib tests still touch js-sys statics"]
    fn nested_cells_map_helper_when_while_nested_link_switches_branch_on_toggle() {
        let source = nested_cells_map_helper_when_while_nested_link_toggle_source();
        let (root_object, construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");
        let toggle = root_object.expect_variable("toggle");
        let sender = toggle.expect_link_value_sender();

        block_on(async move {
            let snapshot_to_json = |value: Value| async move {
                let Value::List(list, _) = value else {
                    panic!("elements should evaluate to a list");
                };
                let snapshot = list.snapshot().await;
                let (_pid, first_item_actor) = snapshot
                    .into_iter()
                    .next()
                    .expect("elements should contain first item");
                first_item_actor
                    .current_value()
                    .expect("first mapped item should have current value")
                    .to_json()
                    .await
            };

            let initial_value = elements
                .value_actor()
                .current_value()
                .expect("elements should have initial value");
            let initial_json = snapshot_to_json(initial_value).await;

            send_link_signal(
                construct_context,
                sender,
                serde_json::json!(true),
                "test.toggle.nested_link_switch",
            )
            .await;

            let updated_value = elements
                .value_actor()
                .stream_from_now()
                .next()
                .await
                .expect("elements should emit updated value after toggle");
            let updated_json = snapshot_to_json(updated_value).await;

            let initial_text = initial_json.to_string();
            let updated_text = updated_json.to_string();
            assert!(
                initial_text.contains("\"y\""),
                "expected initial first item to be editing branch label, got {}",
                initial_text
            );
            assert!(
                updated_text.contains("\"x\""),
                "expected updated first item to switch to display branch label, got {}",
                updated_text
            );
        });
    }

    #[test]
    #[ignore = "requires browser-side actors runtime; host lib tests still touch js-sys statics"]
    fn mapped_cell_can_reopen_after_escape_via_nested_link_events() {
        let source = nested_cells_reopen_editing_source();
        let (root_object, construct_context, _scope_guard) = evaluate_program(&source);
        let elements = root_object.expect_variable("elements");
        let cells = root_object.expect_variable("cells");

        block_on(async move {
            async fn first_mapped_item(elements: &Variable) -> Value {
                let elements_value = elements
                    .value_actor()
                    .current_value()
                    .expect("elements should have a current value");
                let Value::List(list, _) = elements_value else {
                    panic!("elements should evaluate to a list");
                };
                let snapshot = list.snapshot().await;
                let (_pid, first_item_actor) = snapshot
                    .into_iter()
                    .next()
                    .expect("elements should contain first mapped item");
                first_item_actor
                    .current_value()
                    .expect("first mapped item should have current value")
            }

            async fn first_cell(cells: &Variable) -> Arc<Object> {
                let cells_value = cells
                    .value_actor()
                    .current_value()
                    .expect("cells should have a current value");
                let Value::List(list, _) = cells_value else {
                    panic!("cells should evaluate to a list");
                };
                let snapshot = list.snapshot().await;
                let (_pid, first_cell_actor) = snapshot
                    .into_iter()
                    .next()
                    .expect("cells should contain first cell");
                let first_cell_value = first_cell_actor
                    .current_value()
                    .expect("first cell should have current value");
                let Value::Object(cell_obj, _) = first_cell_value else {
                    panic!("first cell should be an object");
                };
                cell_obj
            }

            async fn nested_link_sender(
                object: Arc<Object>,
                value_field: &str,
                event_field: &str,
            ) -> crate::engine::NamedChannel<Value> {
                let cell_elements = object
                    .variable("cell_elements")
                    .expect("cell should have cell_elements");
                let cell_elements_value = cell_elements
                    .value_actor()
                    .current_value()
                    .expect("cell_elements should have current value");
                let Value::Object(cell_elements_obj, _) = cell_elements_value else {
                    panic!("cell_elements should be an object");
                };

                let linked_element = cell_elements_obj
                    .variable(value_field)
                    .unwrap_or_else(|| panic!("cell_elements should contain {}", value_field));
                let linked_element_value = linked_element
                    .value_actor()
                    .current_value()
                    .unwrap_or_else(|_| panic!("{value_field} should have current value"));
                let Value::TaggedObject(linked_tagged, _) = linked_element_value else {
                    panic!("{value_field} should be a tagged element object");
                };
                let element = linked_tagged
                    .variable("element")
                    .expect("linked element should have element");
                let element_value = element
                    .value_actor()
                    .current_value()
                    .expect("linked element object should have current value");
                let Value::Object(element_obj, _) = element_value else {
                    panic!("linked element field should be an object");
                };
                let event = element_obj
                    .variable("event")
                    .expect("element should have event");
                let event_value = event
                    .value_actor()
                    .current_value()
                    .expect("event should have current value");
                let Value::Object(event_obj, _) = event_value else {
                    panic!("event should be an object");
                };
                event_obj
                    .variable(event_field)
                    .and_then(|var| var.link_value_sender())
                    .unwrap_or_else(|| {
                        panic!("{value_field}.element.event.{event_field} should have LINK sender")
                    })
            }

            let _initial = first_mapped_item(&elements).await;
            let cell_obj = first_cell(&cells).await;

            let display_sender =
                nested_link_sender(cell_obj.clone(), "display", "double_click").await;
            send_link_signal(
                construct_context.clone(),
                display_sender,
                serde_json::json!(true),
                "test.cells.reopen.open_first",
            )
            .await;

            let opened_value = first_mapped_item(&elements).await;
            let Value::TaggedObject(opened_tagged, _) = opened_value else {
                panic!("opened mapped item should be a tagged element object");
            };
            assert_eq!(
                opened_tagged.tag(),
                "ElementTextInput",
                "double click should switch mapped cell into editing branch"
            );

            let editing_sender = nested_link_sender(cell_obj.clone(), "editing", "key_down").await;
            send_link_signal(
                construct_context.clone(),
                editing_sender,
                serde_json::json!({
                    "key": "Escape",
                    "text": ""
                }),
                "test.cells.reopen.escape",
            )
            .await;

            let closed_value = first_mapped_item(&elements).await;
            let Value::TaggedObject(closed_tagged, _) = closed_value else {
                panic!("closed mapped item should be a tagged element object");
            };
            assert_eq!(
                closed_tagged.tag(),
                "ElementLabel",
                "Escape should switch mapped cell back to display branch"
            );

            let display_sender = nested_link_sender(cell_obj, "display", "double_click").await;
            send_link_signal(
                construct_context,
                display_sender,
                serde_json::json!(true),
                "test.cells.reopen.open_second",
            )
            .await;

            let reopened_value = first_mapped_item(&elements).await;
            let Value::TaggedObject(reopened_tagged, _) = reopened_value else {
                panic!("reopened mapped item should be a tagged element object");
            };
            assert_eq!(
                reopened_tagged.tag(),
                "ElementTextInput",
                "cell should reopen after Escape via the nested display LINK path"
            );
        });
    }

    #[test]
    fn mapped_todo_item_preserves_nested_event_link_senders() {
        let source = mapped_todo_item_event_link_source();
        let (root_object, _construct_context, _scope_guard) = evaluate_program(source);
        let elements = root_object.expect_variable("elements");

        block_on(async move {
            eprintln!("test: reading elements current_value");
            let elements_value = elements
                .value_actor()
                .current_value()
                .expect("elements should have a current value");
            let Value::List(list, _) = elements_value else {
                panic!("elements should evaluate to a list");
            };
            let snapshot = list.snapshot().await;
            let (_pid, first_item_actor) = snapshot
                .into_iter()
                .next()
                .expect("mapped todo element should exist");

            eprintln!("test: reading first mapped item");
            let first_item_value = first_item_actor
                .current_value()
                .expect("mapped todo item should have a current value");
            let Value::TaggedObject(tagged, _) = first_item_value else {
                panic!("mapped todo item should be a tagged element object");
            };

            let settings = tagged
                .variable("settings")
                .expect("mapped todo item should have settings");
            eprintln!("test: reading settings");
            let settings_value = settings
                .value_actor()
                .current_value()
                .expect("settings should have current value");
            let Value::Object(settings_obj, _) = settings_value else {
                panic!("settings should be an object");
            };

            let items = settings_obj
                .variable("items")
                .expect("stripe settings should contain items");
            eprintln!("test: reading items");
            let items_value = items
                .value_actor()
                .current_value()
                .expect("items should have current value");
            let Value::List(items_list, _) = items_value else {
                panic!("items should be a list");
            };
            let item_snapshot = items_list.snapshot().await;
            assert_eq!(item_snapshot.len(), 2, "expected checkbox + label items");

            let (_checkbox_pid, checkbox_actor) = &item_snapshot[0];
            eprintln!("test: reading checkbox item");
            let checkbox_value = checkbox_actor
                .current_value()
                .expect("checkbox item should have current value");
            let Value::TaggedObject(checkbox_tagged, _) = checkbox_value else {
                panic!("first item should be checkbox tagged object");
            };
            let checkbox_element = checkbox_tagged
                .variable("element")
                .expect("checkbox should have element object");
            eprintln!("test: reading checkbox element");
            let checkbox_element_value = checkbox_element
                .value_actor()
                .current_value()
                .expect("checkbox element should have current value");
            let Value::Object(checkbox_element_obj, _) = checkbox_element_value else {
                panic!("checkbox element should be object");
            };
            let checkbox_event = checkbox_element_obj
                .variable("event")
                .expect("checkbox element should have event");
            eprintln!("test: reading checkbox event");
            let checkbox_event_value = checkbox_event
                .value_actor()
                .current_value()
                .expect("checkbox event should have current value");
            let Value::Object(checkbox_event_obj, _) = checkbox_event_value else {
                panic!("checkbox event should be object");
            };
            assert!(
                checkbox_event_obj
                    .variable("click")
                    .and_then(|var| var.link_value_sender())
                    .is_some(),
                "checkbox click LINK sender should survive mapped nested link target"
            );

            let (_label_pid, label_actor) = &item_snapshot[1];
            eprintln!("test: reading label item");
            let label_value = label_actor
                .current_value()
                .expect("label item should have current value");
            let Value::TaggedObject(label_tagged, _) = label_value else {
                panic!("second item should be label tagged object");
            };
            let label_element = label_tagged
                .variable("element")
                .expect("label should have element object");
            eprintln!("test: reading label element");
            let label_element_value = label_element
                .value_actor()
                .current_value()
                .expect("label element should have current value");
            let Value::Object(label_element_obj, _) = label_element_value else {
                panic!("label element should be object");
            };
            let label_event = label_element_obj
                .variable("event")
                .expect("label element should have event");
            eprintln!("test: reading label event");
            let label_event_value = label_event
                .value_actor()
                .current_value()
                .expect("label event should have current value");
            let Value::Object(label_event_obj, _) = label_event_value else {
                panic!("label event should be object");
            };
            assert!(
                label_event_obj
                    .variable("double_click")
                    .and_then(|var| var.link_value_sender())
                    .is_some(),
                "label double_click LINK sender should survive mapped nested link target"
            );
        });
    }

    #[test]
    fn mapped_todo_item_link_target_receives_nested_event_link_senders() {
        let source = mapped_todo_item_event_link_source();
        let (root_object, _construct_context, _scope_guard) = evaluate_program(source);
        let elements = root_object.expect_variable("elements");
        let todos = root_object.expect_variable("todos");

        block_on(async move {
            eprintln!("target-test: forcing elements evaluation");
            let _elements_value = elements
                .value_actor()
                .current_value()
                .expect("elements should have a current value");

            eprintln!("target-test: reading todos");
            let todos_value = todos
                .value_actor()
                .current_value()
                .expect("todos should have a current value");
            let Value::List(todos_list, _) = todos_value else {
                panic!("todos should evaluate to a list");
            };

            let todo_snapshot = todos_list.snapshot().await;
            let (_todo_pid, todo_actor) = todo_snapshot
                .into_iter()
                .next()
                .expect("first todo should exist");

            eprintln!("target-test: reading first todo object");
            let todo_value = todo_actor
                .current_value()
                .expect("todo item should have a current value");
            let Value::Object(todo_obj, _) = todo_value else {
                panic!("todo item should be an object");
            };

            let todo_elements = todo_obj
                .variable("todo_elements")
                .expect("todo should have todo_elements");
            eprintln!("target-test: reading todo_elements");
            let todo_elements_value = todo_elements
                .value_actor()
                .current_value()
                .expect("todo_elements should have current value");
            let Value::Object(todo_elements_obj, _) = todo_elements_value else {
                panic!("todo_elements should be an object");
            };

            let todo_checkbox = todo_elements_obj
                .variable("todo_checkbox")
                .expect("todo_elements should contain todo_checkbox");
            eprintln!("target-test: reading linked todo_checkbox");
            let todo_checkbox_value = todo_checkbox
                .value_actor()
                .current_value()
                .expect("linked todo_checkbox should have current value");
            let Value::TaggedObject(todo_checkbox_tagged, _) = todo_checkbox_value else {
                panic!("linked todo_checkbox should be a tagged element object");
            };
            let todo_checkbox_element = todo_checkbox_tagged
                .variable("element")
                .expect("linked todo_checkbox should have element");
            let todo_checkbox_element_value = todo_checkbox_element
                .value_actor()
                .current_value()
                .expect("linked todo_checkbox element should have current value");
            let Value::Object(todo_checkbox_element_obj, _) = todo_checkbox_element_value else {
                panic!("linked todo_checkbox element should be object");
            };
            let todo_checkbox_event = todo_checkbox_element_obj
                .variable("event")
                .expect("linked todo_checkbox should have event");
            let todo_checkbox_event_value = todo_checkbox_event
                .value_actor()
                .current_value()
                .expect("linked todo_checkbox event should have current value");
            let Value::Object(todo_checkbox_event_obj, _) = todo_checkbox_event_value else {
                panic!("linked todo_checkbox event should be object");
            };
            assert!(
                todo_checkbox_event_obj
                    .variable("click")
                    .and_then(|var| var.link_value_sender())
                    .is_some(),
                "linked todo_checkbox click LINK sender should survive link target forwarding"
            );

            let todo_title = todo_elements_obj
                .variable("todo_title_element")
                .expect("todo_elements should contain todo_title_element");
            eprintln!("target-test: reading linked todo_title_element");
            let todo_title_value = todo_title
                .value_actor()
                .current_value()
                .expect("linked todo_title_element should have current value");
            let Value::TaggedObject(todo_title_tagged, _) = todo_title_value else {
                panic!("linked todo_title_element should be a tagged element object");
            };
            let todo_title_element = todo_title_tagged
                .variable("element")
                .expect("linked todo_title_element should have element");
            let todo_title_element_value = todo_title_element
                .value_actor()
                .current_value()
                .expect("linked todo_title_element element should have current value");
            let Value::Object(todo_title_element_obj, _) = todo_title_element_value else {
                panic!("linked todo_title_element element should be object");
            };
            let todo_title_event = todo_title_element_obj
                .variable("event")
                .expect("linked todo_title_element should have event");
            let todo_title_event_value = todo_title_event
                .value_actor()
                .current_value()
                .expect("linked todo_title_element event should have current value");
            let Value::Object(todo_title_event_obj, _) = todo_title_event_value else {
                panic!("linked todo_title_element event should be object");
            };
            assert!(
                todo_title_event_obj
                    .variable("double_click")
                    .and_then(|var| var.link_value_sender())
                    .is_some(),
                "linked todo_title_element double_click LINK sender should survive link target forwarding"
            );
        });
    }

    #[test]
    #[ignore = "requires browser-side actors runtime; host lib tests still touch js-sys statics"]
    fn crud_linked_row_preserves_press_sender() {
        let source = read_example_source("../../playground/frontend/src/examples/crud/crud.bn");
        let (root_object, _construct_context, _scope_guard) = evaluate_program(&source);
        let document = root_object.expect_variable("document");
        let store = root_object.expect_variable("store");

        block_on(async move {
            let _document_value = document
                .value_actor()
                .current_value()
                .expect("document should have a current value");

            let store_value = store
                .value_actor()
                .current_value()
                .expect("store should have a current value");
            let Value::Object(store_obj, _) = store_value else {
                panic!("store should be an object");
            };

            let people = store_obj
                .variable("people")
                .expect("store should contain people");
            let people_value = people
                .value_actor()
                .current_value()
                .expect("people should have a current value");
            let Value::List(people_list, _) = people_value else {
                panic!("people should evaluate to a list");
            };
            let people_snapshot = people_list.snapshot().await;
            let (_tansen_pid, tansen_actor) = people_snapshot
                .get(2)
                .cloned()
                .expect("third CRUD person should exist");

            let tansen_value = tansen_actor
                .current_value()
                .expect("Tansen row object should have a current value");
            let Value::Object(tansen_obj, _) = tansen_value else {
                panic!("CRUD person should be an object");
            };

            let person_elements = tansen_obj
                .variable("person_elements")
                .expect("CRUD person should have person_elements");
            let person_elements_value = person_elements
                .value_actor()
                .current_value()
                .expect("person_elements should have current value");
            let Value::Object(person_elements_obj, _) = person_elements_value else {
                panic!("person_elements should be object");
            };

            let row = person_elements_obj
                .variable("row")
                .expect("person_elements should contain row");
            let row_value = row
                .value_actor()
                .current_value()
                .expect("linked row should have current value after document evaluation");
            let Value::TaggedObject(row_tagged, _) = row_value else {
                panic!("linked row should be tagged element object");
            };
            let row_element = row_tagged
                .variable("element")
                .expect("linked row should have element");
            let row_element_value = row_element
                .value_actor()
                .current_value()
                .expect("linked row element should have current value");
            let Value::Object(row_element_obj, _) = row_element_value else {
                panic!("linked row element should be object");
            };
            let row_event = row_element_obj
                .variable("event")
                .expect("linked row should have event");
            let row_event_value = row_event
                .value_actor()
                .current_value()
                .expect("linked row event should have current value");
            let Value::Object(row_event_obj, _) = row_event_value else {
                panic!("linked row event should be object");
            };

            assert!(
                row_event_obj
                    .variable("press")
                    .and_then(|var| var.link_value_sender())
                    .is_some(),
                "linked CRUD row press LINK sender should survive nested link target forwarding"
            );
        });
    }

    #[test]
    #[ignore = "requires browser-side actors runtime; host lib tests still touch js-sys statics"]
    fn crud_mapped_row_press_updates_selected_id() {
        let source = read_example_source("../../playground/frontend/src/examples/crud/crud.bn");
        let (root_object, construct_context, _scope_guard) = evaluate_program(&source);
        let document = root_object.expect_variable("document");
        let store = root_object.expect_variable("store");

        block_on(async move {
            let _document_value = document
                .value_actor()
                .current_value()
                .expect("document should have a current value");

            let store_value = store
                .value_actor()
                .current_value()
                .expect("store should have a current value");
            let Value::Object(store_obj, _) = store_value else {
                panic!("store should be an object");
            };

            let people = store_obj
                .variable("people")
                .expect("store should contain people");
            let selected_id = store_obj
                .variable("selected_id")
                .expect("store should contain selected_id");

            let people_value = people
                .value_actor()
                .current_value()
                .expect("people should have a current value");
            let Value::List(people_list, _) = people_value else {
                panic!("people should evaluate to a list");
            };
            let people_snapshot = people_list.snapshot().await;
            let (_tansen_pid, tansen_actor) = people_snapshot
                .get(2)
                .cloned()
                .expect("third CRUD person should exist");

            let tansen_value = tansen_actor
                .current_value()
                .expect("Tansen row object should have a current value");
            let Value::Object(tansen_obj, _) = tansen_value else {
                panic!("CRUD person should be an object");
            };

            let tansen_id = tansen_obj
                .variable("id")
                .expect("CRUD person should have id")
                .value_actor()
                .current_value()
                .expect("CRUD person id should have current value")
                .to_json()
                .await;

            let person_elements_value = tansen_obj
                .variable("person_elements")
                .expect("CRUD person should have person_elements")
                .value_actor()
                .current_value()
                .expect("person_elements should have current value");
            let Value::Object(person_elements_obj, _) = person_elements_value else {
                panic!("person_elements should be object");
            };
            let row_var = person_elements_obj
                .variable("row")
                .expect("person_elements should contain row");
            let row_value = row_var
                .value_actor()
                .current_value()
                .expect("linked row should have current value after document evaluation");
            let Value::TaggedObject(row_tagged, _) = row_value else {
                panic!("linked row should be tagged element object");
            };
            let row_element = row_tagged
                .variable("element")
                .expect("linked row should have element");
            let row_element_value = row_element
                .value_actor()
                .current_value()
                .expect("linked row element should have current value");
            let Value::Object(row_element_obj, _) = row_element_value else {
                panic!("linked row element should be object");
            };
            let row_event = row_element_obj
                .variable("event")
                .expect("linked row should have event");
            let row_event_value = row_event
                .value_actor()
                .current_value()
                .expect("linked row event should have current value");
            let Value::Object(row_event_obj, _) = row_event_value else {
                panic!("linked row event should be object");
            };
            let press_sender = row_event_obj
                .variable("press")
                .and_then(|var| var.link_value_sender())
                .expect("linked row press sender should exist");

            let initial_selected_id = selected_id
                .value_actor()
                .current_value()
                .expect("selected_id should have initial value")
                .to_json()
                .await;

            send_link_signal(
                construct_context,
                press_sender,
                serde_json::json!({}),
                "test.crud.row_press",
            )
            .await;

            let mut updated_selected_id = selected_id
                .value_actor()
                .current_value()
                .expect("selected_id should still have current value")
                .to_json()
                .await;
            for _ in 0..50 {
                if updated_selected_id == tansen_id {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                updated_selected_id = selected_id
                    .value_actor()
                    .current_value()
                    .expect("selected_id should still have current value")
                    .to_json()
                    .await;
            }

            assert_eq!(initial_selected_id, serde_json::json!("None"));
            assert_eq!(updated_selected_id, tansen_id);
        });
    }

    #[test]
    #[ignore = "requires wasm/js runtime; host lib tests still touch js-sys statics"]
    fn linked_checkbox_updates_sibling_hold_state() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(linked_checkbox_updates_sibling_hold_source());
        let checkbox = root_object.expect_variable("checkbox");
        let todo = root_object.expect_variable("todo");

        let (initial_completed_json, updated_completed_json) = block_on(async move {
            let checkbox_value = checkbox
                .value_actor()
                .current_value()
                .expect("checkbox should have a current value");
            let Value::TaggedObject(checkbox_tagged, _) = checkbox_value else {
                panic!("checkbox should be a tagged element object");
            };
            let checkbox_element = checkbox_tagged
                .variable("element")
                .expect("checkbox should have element object");
            let checkbox_element_value = checkbox_element
                .value_actor()
                .current_value()
                .expect("checkbox element should have current value");
            let Value::Object(checkbox_element_obj, _) = checkbox_element_value else {
                panic!("checkbox element should be object");
            };
            let checkbox_event = checkbox_element_obj
                .variable("event")
                .expect("checkbox element should have event");
            let checkbox_event_value = checkbox_event
                .value_actor()
                .current_value()
                .expect("checkbox event should have current value");
            let Value::Object(checkbox_event_obj, _) = checkbox_event_value else {
                panic!("checkbox event should be object");
            };
            let click_sender = checkbox_event_obj
                .variable("click")
                .and_then(|var| var.link_value_sender())
                .expect("checkbox click sender should exist");

            let todo_value = todo
                .value_actor()
                .current_value()
                .expect("todo should have current value");
            let Value::Object(todo_obj, _) = todo_value else {
                panic!("todo should be an object");
            };
            let completed = todo_obj
                .variable("completed")
                .expect("todo should contain completed");

            let initial_completed_json = completed
                .value_actor()
                .current_value()
                .expect("completed should have initial value")
                .to_json()
                .await;

            send_link_signal(
                construct_context,
                click_sender,
                serde_json::json!({}),
                "test.checkbox.click",
            )
            .await;

            let mut updated_completed_json = completed
                .value_actor()
                .current_value()
                .expect("completed should still have current value")
                .to_json()
                .await;
            for _ in 0..50 {
                if updated_completed_json == serde_json::json!(true) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                updated_completed_json = completed
                    .value_actor()
                    .current_value()
                    .expect("completed should still have current value")
                    .to_json()
                    .await;
            }

            (initial_completed_json, updated_completed_json)
        });

        assert_eq!(initial_completed_json, serde_json::json!(false));
        assert_eq!(updated_completed_json, serde_json::json!(true));
    }

    #[test]
    #[ignore = "requires browser-side actors runtime; host lib tests still touch js-sys statics"]
    fn list_map_reacts_to_external_dependency_changes() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(list_map_external_dependency_source());
        let toggle = root_object.expect_variable("toggle");
        let mapped = root_object.expect_variable("mapped");
        let sender = toggle.expect_link_value_sender();

        let (initial_json, updated_json) = block_on(async move {
            let Value::List(list, _) = mapped
                .value_actor()
                .current_value()
                .expect("mapped should have current list value")
            else {
                panic!("mapped should evaluate to a list");
            };

            let items_to_json = |items: Vec<crate::engine::ActorHandle>| async move {
                let mut values = Vec::new();
                for item_actor in items {
                    let value = item_actor
                        .current_value()
                        .expect("mapped item should have current value")
                        .to_json()
                        .await;
                    values.push(value);
                }
                serde_json::Value::Array(values)
            };

            let initial_snapshot = list.snapshot().await;
            let initial_json = items_to_json(
                initial_snapshot
                    .iter()
                    .map(|(_id, item_actor)| item_actor.clone())
                    .collect(),
            )
            .await;
            let current_items: Vec<crate::engine::ActorHandle> = initial_snapshot
                .into_iter()
                .map(|(_id, item_actor)| item_actor)
                .collect();
            send_link_signal(
                construct_context,
                sender,
                json!({"toggle": true}),
                "test.toggle",
            )
            .await;

            let mut updated_json = items_to_json(current_items.clone()).await;
            for _ in 0..50 {
                if updated_json == json!([11.0, 12.0, 13.0]) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                updated_json = items_to_json(current_items.clone()).await;
            }

            (initial_json, updated_json)
        });

        assert_eq!(initial_json, json!([1.0, 2.0, 3.0]));
        assert_eq!(updated_json, json!([11.0, 12.0, 13.0]));
    }

    #[test]
    fn mapped_formula_dependency_updates_after_override_commit() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(cells_mapped_formula_dependency_source());
        let edit_committed = root_object.expect_variable("edit_committed");
        let formula_a1 = root_object.expect_variable("formula_a1");
        let mapped = root_object.expect_variable("mapped");
        let sender = edit_committed.expect_link_value_sender();

        let (initial_formula_json, updated_formula_json, initial_mapped_json, updated_mapped_json) =
            block_on(async move {
                let Value::List(list, _) = mapped
                    .value_actor()
                    .current_value()
                    .expect("mapped should have current list value")
                else {
                    panic!("mapped should evaluate to a list");
                };

                let items_to_json = |items: Vec<crate::engine::ActorHandle>| async move {
                    let mut values = Vec::new();
                    for item_actor in items {
                        let value = item_actor
                            .current_value()
                            .expect("mapped item should have current value")
                            .to_json()
                            .await;
                        values.push(value);
                    }
                    serde_json::Value::Array(values)
                };

                let initial_formula_json = formula_a1
                    .value_actor()
                    .current_value()
                    .expect("formula_a1 should have current value")
                    .to_json()
                    .await;

                let initial_snapshot = list.snapshot().await;
                let current_items: Vec<crate::engine::ActorHandle> = initial_snapshot
                    .iter()
                    .map(|(_id, item_actor)| item_actor.clone())
                    .collect();
                let initial_mapped_json = items_to_json(current_items.clone()).await;

                send_edit_committed(construct_context, sender, 1.0, 1.0, "7").await;

                let mut updated_formula_json = formula_a1
                    .value_actor()
                    .current_value()
                    .expect("formula_a1 should still have current value")
                    .to_json()
                    .await;
                let mut updated_mapped_json = items_to_json(current_items.clone()).await;

                for _ in 0..50 {
                    if updated_formula_json == json!("7")
                        && updated_mapped_json == json!(["7", "17"])
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    updated_formula_json = formula_a1
                        .value_actor()
                        .current_value()
                        .expect("formula_a1 should still have current value")
                        .to_json()
                        .await;
                    updated_mapped_json = items_to_json(current_items.clone()).await;
                }

                (
                    initial_formula_json,
                    updated_formula_json,
                    initial_mapped_json,
                    updated_mapped_json,
                )
            });

        assert_eq!(initial_formula_json, json!("5"));
        assert_eq!(updated_formula_json, json!("7"));
        assert_eq!(initial_mapped_json, json!(["5", "15"]));
        assert_eq!(updated_mapped_json, json!(["7", "17"]));
    }

    #[test]
    #[ignore = "requires browser-side actors runtime; host lib tests still touch js-sys statics"]
    fn snapshot_then_body_freezes_referenced_roots_across_sibling_hold_updates() {
        let (root_object, construct_context, _scope_guard) =
            evaluate_program(shared_snapshot_root_toggle_source());
        let toggle_all = root_object.expect_variable("toggle_all");
        let todos = root_object.expect_variable("todos");
        let sender = toggle_all.expect_link_value_sender();

        let (initial_json, updated_json) = block_on(async move {
            let Value::List(list, _) = todos
                .value_actor()
                .current_value()
                .expect("todos should have current list value")
            else {
                panic!("todos should evaluate to a list");
            };

            let items_to_json = |items: Vec<crate::engine::ActorHandle>| async move {
                let mut values = Vec::new();
                for item_actor in items {
                    let value = item_actor
                        .current_value()
                        .expect("todo item should have current value");
                    let Value::Object(todo_obj, _) = value else {
                        panic!("todo item should be an object");
                    };
                    let completed = todo_obj
                        .variable("completed")
                        .expect("todo item should have completed");
                    values.push(
                        completed
                            .value_actor()
                            .current_value()
                            .expect("completed should have current value")
                            .to_json()
                            .await,
                    );
                }
                serde_json::Value::Array(values)
            };

            let snapshot = list.snapshot().await;
            let current_items: Vec<crate::engine::ActorHandle> = snapshot
                .into_iter()
                .map(|(_id, item_actor)| item_actor)
                .collect();
            let initial_json = items_to_json(current_items.clone()).await;

            send_link_signal(
                construct_context,
                sender,
                json!({}),
                "test.toggle_all.snapshot_root",
            )
            .await;

            let mut updated_json = items_to_json(current_items.clone()).await;
            for _ in 0..50 {
                if updated_json == json!([true, true, true, true]) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                updated_json = items_to_json(current_items.clone()).await;
            }

            (initial_json, updated_json)
        });

        assert_eq!(initial_json, json!([false, false, false, true]));
        assert_eq!(updated_json, json!([true, true, true, true]));
    }
}
