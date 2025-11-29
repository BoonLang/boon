use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use zoon::futures_util::stream;
use zoon::{Stream, StreamExt};

use super::super::super::parser::{PersistenceId, SourceCode, Span, static_expression};
use super::api;
use super::engine::*;

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

/// Main evaluation function - takes static expressions (owned, 'static, no lifetimes).
pub fn evaluate(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
) -> Result<(Arc<Object>, ConstructContext), String> {
    let construct_context = ConstructContext {
        construct_storage: Arc::new(ConstructStorage::new(states_local_storage_key)),
    };
    let actor_context = ActorContext::default();
    let reference_connector = Arc::new(ReferenceConnector::new());
    let function_registry = StaticFunctionRegistry::default();

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
                function_registry.clone(),
                source_code.clone(),
            )
        })
        .collect();

    let root_object = Object::new_arc(
        ConstructInfo::new("root", None, "root"),
        construct_context.clone(),
        evaluated_variables?,
    );
    Ok((root_object, construct_context))
}

/// Evaluates a static variable into a Variable.
fn static_spanned_variable_into_variable(
    variable: static_expression::Spanned<static_expression::Variable>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    function_registry: StaticFunctionRegistry,
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

    let variable = if matches!(
        &value.node,
        static_expression::Expression::Link
    ) {
        Variable::new_link_arc(construct_info, construct_context, name_string, actor_context)
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
                function_registry,
                source_code,
            )?,
        )
    };
    if is_referenced {
        reference_connector.register_referenceable(span, variable.value_actor());
    }
    Ok(variable)
}

/// Evaluates a static expression, returning a ValueActor.
///
/// This is used by ListBindingFunction to evaluate transform expressions
/// for each list item. The binding variable is passed via `actor_context.parameters`.
///
/// Note: User-defined function calls inside the expression will not work
/// (the function registry is empty). Built-in functions and operators work fine.
pub fn evaluate_static_expression(
    static_expr: &static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    static_spanned_expression_into_value_actor(
        static_expr.clone(),
        construct_context,
        actor_context,
        reference_connector,
        StaticFunctionRegistry::default(),
        source_code,
    )
}

/// Evaluates a static expression directly (no to_borrowed conversion).
/// This is the core static evaluator used for List binding functions.
fn static_spanned_expression_into_value_actor(
    expression: static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    function_registry: StaticFunctionRegistry,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    let static_expression::Spanned {
        span,
        node: expression,
        persistence,
    } = expression;

    let persistence_id = persistence.clone().ok_or("Failed to get Persistence")?.id;
    let idempotency_key = persistence_id;

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
                        function_registry.clone(),
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
                        function_registry.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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

            // Special handling for List binding functions (map, retain, every, any)
            // These need the unevaluated expression to evaluate per-item with bindings
            match path_strs.as_slice() {
                ["List", "map"] | ["List", "retain"] | ["List", "every"] | ["List", "any"] => {
                    let operation = match path_strs[1] {
                        "map" => ListBindingOperation::Map,
                        "retain" => ListBindingOperation::Retain,
                        "every" => ListBindingOperation::Every,
                        "any" => ListBindingOperation::Any,
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
                            function_registry.clone(),
                            source_code.clone(),
                        )?
                    } else if let Some(ref passed) = actor_context.passed {
                        passed.clone()
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
                    // Standard function call - evaluate all arguments
                    let mut evaluated_args: Vec<Arc<ValueActor>> = Vec::new();
                    for arg in arguments {
                        if let Some(value) = arg.node.value {
                            let actor = static_spanned_expression_into_value_actor(
                                value,
                                construct_context.clone(),
                                actor_context.clone(),
                                reference_connector.clone(),
                                function_registry.clone(),
                                source_code.clone(),
                            )?;
                            evaluated_args.push(actor);
                        }
                    }

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
                        actor_context,
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
                stream::empty(),
            )
        }
        // Remaining expression types not yet supported in static context
        static_expression::Expression::Object(_) => {
            return Err("Object expressions not yet supported in static context".to_string());
        }
        static_expression::Expression::TaggedObject { .. } => {
            return Err("TaggedObject expressions not yet supported in static context".to_string());
        }
        static_expression::Expression::Map { .. } => {
            return Err("Map expressions not yet supported in static context".to_string());
        }
        static_expression::Expression::Function { .. } => {
            return Err("Function definitions not supported in static context".to_string());
        }
        static_expression::Expression::LinkSetter { .. } => {
            return Err("LinkSetter not yet supported in static context".to_string());
        }
        static_expression::Expression::Link => {
            return Err("Link not yet supported in static context".to_string());
        }
        static_expression::Expression::Latest { .. } => {
            return Err("Latest not yet supported in static context".to_string());
        }
        static_expression::Expression::Then { .. } => {
            return Err("Then not yet supported in static context".to_string());
        }
        static_expression::Expression::When { .. } => {
            return Err("When not yet supported in static context".to_string());
        }
        static_expression::Expression::While { .. } => {
            return Err("While not yet supported in static context".to_string());
        }
        static_expression::Expression::Pipe { .. } => {
            return Err("Pipe not yet supported in static context".to_string());
        }
        static_expression::Expression::Block { .. } => {
            return Err("Block not yet supported in static context".to_string());
        }
        static_expression::Expression::TextLiteral { .. } => {
            return Err("TextLiteral not yet supported in static context".to_string());
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
        _ => return Err(format!("Unknown function '{}(..)' in static context", path.join("/"))),
    };
    Ok(definition)
}

