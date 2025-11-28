use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use zoon::mpsc;
use zoon::futures_util::stream;
use zoon::{Stream, StreamExt};

use super::super::super::parser::{
    self, Expression, ParseError, PersistenceId, Span, Spanned, Token,
};
use super::api;
use super::engine::*;

type EvaluateResult<'code, T> = Result<T, ParseError<'code, Token<'code>>>;

/// Registry for user-defined functions.
/// Functions are stored by name and contain their parameter names and body.
#[derive(Clone, Default)]
pub struct FunctionRegistry<'code> {
    functions: Rc<RefCell<HashMap<&'code str, FunctionDefinition<'code>>>>,
}

/// A user-defined function definition.
#[derive(Clone)]
pub struct FunctionDefinition<'code> {
    pub parameters: Vec<&'code str>,
    pub body: Spanned<Expression<'code>>,
}

pub fn evaluate(
    expressions: Vec<Spanned<Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
) -> EvaluateResult<(Arc<Object>, ConstructContext)> {
    let construct_context = ConstructContext {
        construct_storage: Arc::new(ConstructStorage::new(states_local_storage_key)),
    };
    let actor_context = ActorContext::default();
    let reference_connector = Arc::new(ReferenceConnector::new());
    let function_registry = FunctionRegistry::default();

    // First pass: collect function definitions
    let mut variables = Vec::new();
    for expr in expressions {
        let Spanned {
            span,
            node: expression,
            persistence,
        } = expr;
        match expression {
            Expression::Variable(variable) => {
                variables.push(Spanned {
                    span,
                    node: *variable,
                    persistence,
                });
            }
            Expression::Function {
                name,
                parameters,
                body,
            } => {
                // Store function definition in registry
                function_registry.functions.borrow_mut().insert(
                    name,
                    FunctionDefinition {
                        parameters: parameters.into_iter().map(|p| p.node).collect(),
                        body: *body,
                    },
                );
            }
            _ => {
                return Err(ParseError::custom(
                    span,
                    "Only variables or functions expected at top level",
                ));
            }
        }
    }

    // Second pass: evaluate variables
    let root_object = Object::new_arc(
        ConstructInfo::new("root", None, "root"),
        construct_context.clone(),
        variables
            .into_iter()
            .map(|variable| {
                spanned_variable_into_variable(
                    variable,
                    construct_context.clone(),
                    actor_context.clone(),
                    reference_connector.clone(),
                    function_registry.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok((root_object, construct_context))
}

// @TODO Is the rule "LINK has to be the only variable value" necessary? Validate it by the parser?
fn spanned_variable_into_variable<'code>(
    variable: Spanned<parser::Variable<'code>>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    function_registry: FunctionRegistry<'code>,
) -> EvaluateResult<'code, Arc<Variable>> {
    let Spanned {
        span,
        node: variable,
        persistence,
    } = variable;
    let parser::Variable {
        name,
        value,
        is_referenced,
    } = variable;

    let persistence_id = persistence.expect("Failed to get Persistence").id;
    let name: String = name.to_owned();

    let construct_info = ConstructInfo::new(
        format!("PersistenceId: {persistence_id}"),
        persistence,
        format!("{span}; {name}"),
    );
    let variable = if matches!(
        &value,
        Spanned {
            span: _,
            node: Expression::Link,
            persistence: _,
        }
    ) {
        Variable::new_link_arc(construct_info, construct_context, name, actor_context)
    } else {
        Variable::new_arc(
            construct_info,
            construct_context.clone(),
            name,
            spanned_expression_into_value_actor(
                value,
                construct_context,
                actor_context,
                reference_connector.clone(),
                function_registry,
            )?,
        )
    };
    if is_referenced {
        reference_connector.register_referenceable(span, variable.value_actor());
    }
    Ok(variable)
}

// @TODO resolve ids
fn spanned_expression_into_value_actor<'code>(
    expression: Spanned<Expression<'code>>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    function_registry: FunctionRegistry<'code>,
) -> EvaluateResult<'code, Arc<ValueActor>> {
    let Spanned {
        span,
        node: expression,
        persistence,
    } = expression;

    let persistence_id = persistence.expect("Failed to get Persistence").id;
    let idempotency_key = persistence_id;

    let actor = match expression {
        Expression::Variable(variable) => Err(ParseError::custom(
            span,
            "Failed to evalute the variable in this context.",
        ))?,
        Expression::Literal(literal) => match literal {
            parser::Literal::Number(number) => Number::new_arc_value_actor(
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
            parser::Literal::Text(text) => {
                let text = text.to_owned();
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
            parser::Literal::Tag(tag) => {
                let tag = tag.to_owned();
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
        Expression::List { items } => List::new_arc_value_actor(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; LIST {{..}}"),
            ),
            construct_context.clone(),
            idempotency_key,
            actor_context.clone(),
            items
                .into_iter()
                .map(|item| {
                    spanned_expression_into_value_actor(
                        item,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expression::Object(object) => Object::new_arc_value_actor(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; [..]"),
            ),
            construct_context.clone(),
            idempotency_key,
            actor_context.clone(),
            object
                .variables
                .into_iter()
                .map(|variable| {
                    spanned_variable_into_variable(
                        variable,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expression::TaggedObject { tag, object } => TaggedObject::new_arc_value_actor(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; {tag}[..]"),
            ),
            construct_context.clone(),
            idempotency_key,
            actor_context.clone(),
            tag.to_owned(),
            object
                .variables
                .into_iter()
                .map(|variable| {
                    spanned_variable_into_variable(
                        variable,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expression::Map { entries } => Err(ParseError::custom(
            span,
            "Not supported yet, sorry [Expression::Map]",
        ))?,
        Expression::Function {
            name,
            parameters,
            body,
        } => Err(ParseError::custom(
            span,
            "Not supported yet, sorry [Expression::Function]",
        ))?,
        Expression::FunctionCall { path, arguments } => {
            // Evaluate all arguments, keeping track of names
            let mut evaluated_args: Vec<Arc<ValueActor>> = Vec::new();
            let mut named_args: HashMap<String, Arc<ValueActor>> = HashMap::new();
            let mut pass_arg: Option<Arc<ValueActor>> = None;

            for Spanned {
                span: arg_span,
                node: argument,
                persistence: _,
            } in arguments
            {
                let parser::Argument {
                    name,
                    value,
                    is_referenced,
                } = argument;
                let Some(value) = value else {
                    // @TODO support out arguments
                    Err(ParseError::custom(
                        arg_span,
                        "Out arguments not supported yet, sorry",
                    ))?
                };
                let actor = spanned_expression_into_value_actor(
                    value,
                    construct_context.clone(),
                    actor_context.clone(),
                    reference_connector.clone(),
                    function_registry.clone(),
                )?;
                if is_referenced {
                    reference_connector.register_referenceable(arg_span, actor.clone());
                }

                // Check for PASS: argument (name would be "PASS" if parser supports it)
                // For now, empty name "" means piped first argument
                if name == "PASS" {
                    pass_arg = Some(actor.clone());
                } else if !name.is_empty() {
                    named_args.insert(name.to_string(), actor.clone());
                }
                evaluated_args.push(actor);
            }

            // Check if it's a user-defined function (single-element path)
            if path.len() == 1 {
                let fn_name = path[0];
                if let Some(func_def) = function_registry.functions.borrow().get(fn_name).cloned() {
                    // User-defined function call
                    let mut new_actor_context = actor_context.clone();

                    // Set PASSED if there's a PASS: argument
                    if let Some(pass_value) = pass_arg {
                        new_actor_context.passed = Some(pass_value);
                    }

                    // Bind parameters: match function definition parameters to arguments
                    // Arguments can be passed by position or by name
                    new_actor_context.parameters = HashMap::new();
                    for (i, param_name) in func_def.parameters.iter().enumerate() {
                        // First check if argument was passed by name
                        if let Some(actor) = named_args.get(*param_name) {
                            new_actor_context.parameters.insert(param_name.to_string(), actor.clone());
                        } else if i < evaluated_args.len() {
                            // Fall back to positional argument
                            new_actor_context.parameters.insert(param_name.to_string(), evaluated_args[i].clone());
                        }
                    }

                    // Clone and evaluate the function body
                    let body = func_def.body.clone();
                    return spanned_expression_into_value_actor(
                        body,
                        construct_context.clone(),
                        new_actor_context,
                        reference_connector.clone(),
                        function_registry.clone(),
                    );
                }
            }

            // Built-in function call
            FunctionCall::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; {}(..)", path.join("/")),
                ),
                construct_context.clone(),
                actor_context.clone(),
                function_call_path_to_definition(&path, span)?,
                evaluated_args,
            )
        }
        Expression::Alias(alias) => {
            use std::pin::Pin;
            use std::future::Future;
            type BoxedFuture = Pin<Box<dyn Future<Output = Arc<ValueActor>>>>;

            let root_value_actor: BoxedFuture = match &alias {
                parser::Alias::WithPassed { extra_parts: _ } => {
                    // PASSED refers to the implicit context passed via PASS: argument
                    match &actor_context.passed {
                        Some(passed) => {
                            let passed = passed.clone();
                            Box::pin(async move { passed })
                        }
                        None => Err(ParseError::custom(
                            span,
                            "PASSED is not available in this context (no PASS: argument was provided)",
                        ))?,
                    }
                }
                parser::Alias::WithoutPassed {
                    parts,
                    referenceables,
                } => {
                    // First check if the first part is a function parameter
                    let first_part = parts.first().copied().unwrap_or("");
                    if let Some(param_actor) = actor_context.parameters.get(first_part) {
                        // This alias starts with a function parameter name
                        let param_actor = param_actor.clone();
                        Box::pin(async move { param_actor })
                    } else {
                        // Fall back to scope-resolved reference
                        let referenced = referenceables
                            .as_ref()
                            .expect("Failed to get alias referenceables in evaluator")
                            .referenced;
                        if let Some(referenced) = referenced {
                            Box::pin(reference_connector.referenceable(referenced.span))
                        } else {
                            Err(ParseError::custom(
                                span,
                                format!("Failed to get aliased variable or argument '{}'", first_part),
                            ))?
                        }
                    }
                }
            };
            VariableOrArgumentReference::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; {alias} (alias)"),
                ),
                construct_context,
                actor_context,
                alias,
                root_value_actor,
            )
        }
        Expression::LinkSetter { alias } => Err(ParseError::custom(
            span,
            "Not supported yet, sorry [Expression::LinkSetter]",
        ))?,
        Expression::Link => Err(ParseError::custom(
            span,
            "LINK has to be the only variable value - e.g. `press: LINK`",
        ))?,
        Expression::Latest { inputs } => LatestCombinator::new_arc_value_actor(
            ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; LATEST {{..}}"),
            ),
            construct_context.clone(),
            actor_context.clone(),
            inputs
                .into_iter()
                .map(|input| {
                    spanned_expression_into_value_actor(
                        input,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expression::Then { body } => Err(ParseError::custom(
            span,
            "You have to pipe things into THEN - e.g. `..press |> THEN { .. }`",
        ))?,
        Expression::When { arms } => Err(ParseError::custom(
            span,
            "Not supported yet, sorry [Expression::When]",
        ))?,
        Expression::While { arms } => Err(ParseError::custom(
            span,
            "Not supported yet, sorry [Expression::While]",
        ))?,
        Expression::Pipe { from, to } => pipe(
            from,
            to,
            construct_context,
            actor_context,
            reference_connector,
            function_registry,
        )?,
        Expression::Skip => {
            // SKIP represents "no value" - a stream that never emits
            // Used in WHEN patterns to skip values that don't match
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; SKIP"),
            );
            ValueActor::new_arc(
                construct_info,
                actor_context,
                stream::empty(), // Never emits any values
            )
        }
        Expression::Block { variables, output } => {
            // BLOCK creates a local scope with variables
            // Variables can reference each other (defined earlier in the block)
            // The output expression is evaluated in this scope

            // Evaluate each variable and register it
            for variable in variables {
                let var = spanned_variable_into_variable(
                    variable,
                    construct_context.clone(),
                    actor_context.clone(),
                    reference_connector.clone(),
                    function_registry.clone(),
                )?;
                // The variable is registered in reference_connector by spanned_variable_into_variable
                // We just need to keep it alive - but we don't have a place to store it
                // For now, leak it (not ideal, but works)
                // @TODO: proper lifetime management for block variables
                std::mem::forget(var);
            }

            // Evaluate the output expression
            spanned_expression_into_value_actor(
                *output,
                construct_context,
                actor_context,
                reference_connector,
                function_registry,
            )?
        }
        Expression::Comparator(comparator) => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; Comparator"),
            );
            match comparator {
                parser::Comparator::Equal { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ComparatorCombinator::new_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::Comparator::NotEqual { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ComparatorCombinator::new_not_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::Comparator::Greater { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ComparatorCombinator::new_greater(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::Comparator::GreaterOrEqual { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ComparatorCombinator::new_greater_or_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::Comparator::Less { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ComparatorCombinator::new_less(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::Comparator::LessOrEqual { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
        Expression::ArithmeticOperator(arithmetic_operator) => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; ArithmeticOperator"),
            );
            match arithmetic_operator {
                parser::ArithmeticOperator::Add { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ArithmeticCombinator::new_add(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::ArithmeticOperator::Subtract { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ArithmeticCombinator::new_subtract(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::ArithmeticOperator::Multiply { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ArithmeticCombinator::new_multiply(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::ArithmeticOperator::Divide { operand_a, operand_b } => {
                    let a = spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    let b = spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
                    )?;
                    ArithmeticCombinator::new_divide(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                parser::ArithmeticOperator::Negate { operand } => {
                    // Negate: multiply by -1
                    let a = spanned_expression_into_value_actor(
                        *operand,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        function_registry,
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
        Expression::TextLiteral { parts } => {
            // For now, only support static text (no interpolation)
            // Interpolation requires reactive variable lookups
            let mut text = String::new();
            for part in parts {
                match part {
                    parser::TextPart::Text(t) => text.push_str(t),
                    parser::TextPart::Interpolation { var } => {
                        // @TODO: Implement reactive interpolation
                        // For now, just include the variable reference as placeholder
                        text.push('{');
                        text.push_str(var);
                        text.push('}');
                    }
                }
            }
            Text::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; TEXT {{..}}"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                text,
            )
        }
    };
    Ok(actor)
}

fn function_call_path_to_definition<'code>(
    path: &[&'code str],
    span: Span,
) -> EvaluateResult<
    'code,
    impl Fn(
        Arc<Vec<Arc<ValueActor>>>,
        ConstructId,
        PersistenceId,
        ConstructContext,
        ActorContext,
    ) -> Pin<Box<dyn Stream<Item = Value>>>
    + 'static,
> {
    // Note: User-defined functions are handled separately in the FunctionCall expression handler
    let definition = match path {
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
        // Text functions
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
        // Bool functions
        ["Bool", "not"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_not(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "toggle"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_toggle(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        // Ulid functions
        ["Ulid", "generate"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_ulid_generate(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        _ => Err(ParseError::custom(
            span,
            format!("Unknown function '{}(..)'", path.join("/")),
        ))?,
    };
    Ok(definition)
}

fn pipe<'code>(
    from: Box<Spanned<Expression<'code>>>,
    mut to: Box<Spanned<Expression<'code>>>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    function_registry: FunctionRegistry<'code>,
) -> EvaluateResult<'code, Arc<ValueActor>> {
    // @TODO destructure `to`?
    let to_persistence_id = to.persistence.expect("Failed to get persistence").id;
    match to.node {
        Expression::FunctionCall {
            ref path,
            ref mut arguments,
        } => {
            let argument = Spanned {
                span: from.span,
                persistence: from.persistence,
                node: parser::Argument {
                    name: "",
                    value: Some(*from),
                    is_referenced: false,
                },
            };
            // @TODO arguments: Vec -> arguments: VecDeque?
            arguments.insert(0, argument);
            spanned_expression_into_value_actor(
                *to,
                construct_context,
                actor_context,
                reference_connector,
                function_registry,
            )
        }
        Expression::LinkSetter { alias } => {
            // LinkSetter connects the "from" stream to a LINK variable
            // It also passes through the values (so they can be used in a LIST, etc.)

            let from_actor = spanned_expression_into_value_actor(
                *from,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                function_registry,
            )?;

            // Resolve the alias to get the target LINK variable
            let Spanned {
                span: alias_span,
                node: alias_node,
                persistence: _,
            } = alias;

            use std::pin::Pin;
            use std::future::Future;
            type BoxedFuture = Pin<Box<dyn Future<Output = Arc<ValueActor>>>>;

            let target_actor: BoxedFuture = match &alias_node {
                parser::Alias::WithPassed { extra_parts: _ } => {
                    // PASSED.xxx - get the PASSED value and navigate
                    match &actor_context.passed {
                        Some(passed) => {
                            let passed = passed.clone();
                            Box::pin(async move { passed })
                        }
                        None => Err(ParseError::custom(
                            alias_span,
                            "PASSED is not available in this context for LinkSetter",
                        ))?,
                    }
                }
                parser::Alias::WithoutPassed {
                    parts: _,
                    referenceables,
                } => {
                    let referenced = referenceables
                        .as_ref()
                        .expect("Failed to get alias referenceables in LinkSetter")
                        .referenced;
                    if let Some(referenced) = referenced {
                        Box::pin(reference_connector.referenceable(referenced.span))
                    } else {
                        Err(ParseError::custom(
                            alias_span,
                            "Failed to resolve alias in LinkSetter",
                        ))?
                    }
                }
            };

            // Create a combinator that:
            // 1. Resolves the target value (following the alias path)
            // 2. Subscribes to "from" and forwards values to the target's LINK sender
            // 3. Passes through the values

            // For now, just pass through the from value
            // @TODO: Actually implement the LINK forwarding by navigating to the LINK variable
            // and sending values to its link_value_sender
            Ok(from_actor)
        }
        Expression::Then { body } => {
            let (impulse_sender, impulse_receiver) = mpsc::unbounded();
            let mut body_actor_context = actor_context.clone();
            body_actor_context.output_valve_signal =
                Some(Arc::new(ActorOutputValveSignal::new(impulse_receiver)));

            Ok(ThenCombinator::new_arc_value_actor(
                ConstructInfo::new(
                    format!("Persistence: {to_persistence_id}"),
                    to.persistence,
                    format!("{to_persistence_id}; THEN"),
                ),
                construct_context.clone(),
                actor_context.clone(),
                spanned_expression_into_value_actor(
                    *from,
                    construct_context.clone(),
                    actor_context.clone(),
                    reference_connector.clone(),
                    function_registry.clone(),
                )?,
                impulse_sender,
                spanned_expression_into_value_actor(
                    *body,
                    construct_context,
                    body_actor_context,
                    reference_connector,
                    function_registry,
                )?,
            ))
        }
        Expression::When { arms } => {
            // Evaluate the input
            let input = spanned_expression_into_value_actor(
                *from,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                function_registry.clone(),
            )?;

            // Compile each arm
            let compiled_arms: Vec<CompiledArm> = arms
                .into_iter()
                .map(|arm| {
                    let matcher = pattern_to_matcher(&arm.pattern);
                    // Create a spanned expression for the body
                    let body_expr = Spanned {
                        span: to.span,
                        persistence: to.persistence,
                        node: arm.body,
                    };
                    let body = spanned_expression_into_value_actor(
                        body_expr,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    Ok(CompiledArm { matcher, body })
                })
                .collect::<EvaluateResult<Vec<_>>>()?;

            Ok(WhenCombinator::new_arc_value_actor(
                ConstructInfo::new(
                    format!("Persistence: {to_persistence_id}"),
                    to.persistence,
                    format!("{to_persistence_id}; WHEN"),
                ),
                construct_context,
                actor_context,
                input,
                compiled_arms,
            ))
        }
        Expression::While { arms } => {
            // WHILE is similar to WHEN - pattern matching with continuous updates
            // The main difference may be in semantics (continuous vs one-shot)
            // For now, implement identically to WHEN

            // Evaluate the input
            let input = spanned_expression_into_value_actor(
                *from,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                function_registry.clone(),
            )?;

            // Compile each arm
            let compiled_arms: Vec<CompiledArm> = arms
                .into_iter()
                .map(|arm| {
                    let matcher = pattern_to_matcher(&arm.pattern);
                    // Create a spanned expression for the body
                    let body_expr = Spanned {
                        span: to.span,
                        persistence: to.persistence,
                        node: arm.body,
                    };
                    let body = spanned_expression_into_value_actor(
                        body_expr,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        function_registry.clone(),
                    )?;
                    Ok(CompiledArm { matcher, body })
                })
                .collect::<EvaluateResult<Vec<_>>>()?;

            Ok(WhenCombinator::new_arc_value_actor(
                ConstructInfo::new(
                    format!("Persistence: {to_persistence_id}"),
                    to.persistence,
                    format!("{to_persistence_id}; WHILE"),
                ),
                construct_context,
                actor_context,
                input,
                compiled_arms,
            ))
        }
        Expression::Pipe { from, to } => Err(ParseError::custom(
            to.span,
            "Piping into it is not supported yet, sorry [Expression::Pipe]",
        ))?,
        _ => Err(ParseError::custom(
            to.span,
            "Piping into this target is not supported",
        ))?,
    }
}
