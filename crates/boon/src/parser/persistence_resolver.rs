use super::{ArithmeticOperator, Comparator, Expression, Literal, ParseError, Span, Spanned, Token};

use std::collections::HashMap;

use serde_json_any_key::*;
use ulid::Ulid;
use zoon::{WebStorage, eprintln, local_storage};

pub type PersistenceId = Ulid;

#[derive(Debug, Clone, Copy)]
pub struct Persistence {
    pub id: PersistenceId,
    pub status: PersistenceStatus,
}

#[derive(Debug, Clone, Copy)]
pub enum PersistenceStatus {
    NewOrChanged,
    Unchanged,
}

pub type ResolveError<'code> = ParseError<'code, Token<'code>>;

pub fn resolve_persistence<'new_code, 'old_code>(
    mut new_expressions: Vec<Spanned<Expression<'new_code>>>,
    old_expressions: Option<Vec<Spanned<Expression<'old_code>>>>,
    old_span_id_pairs_local_storage_key: &str,
) -> Result<
    (
        Vec<Spanned<Expression<'new_code>>>,
        HashMap<Span, PersistenceId>,
    ),
    Vec<ResolveError<'new_code>>,
> {
    // @TODO add a new Zoon WebStorage method like `get_raw_string`?
    let old_span_id_pairs = if let Some(Ok(old_span_id_pairs)) =
        local_storage().get::<String>(&old_span_id_pairs_local_storage_key)
    {
        match json_to_map::<Span, PersistenceId>(&old_span_id_pairs) {
            Ok(old_span_id_pairs) => {
                println!("old_span_id_pairs LOADED!");
                Some(old_span_id_pairs)
            }
            Err(error) => {
                eprintln!("Failed to deserialize Span-PersistenceId pairs: {error:#}");
                None
            }
        }
    } else {
        None
    };

    let old_expressions = old_expressions.unwrap_or_default();
    let old_span_id_pairs = old_span_id_pairs.unwrap_or_default();

    let mut new_span_id_pairs = HashMap::new();
    let mut errors = Vec::new();
    for new_expression in &mut new_expressions {
        set_persistence(
            new_expression,
            &old_expressions.iter().collect::<Vec<_>>(),
            &old_span_id_pairs,
            &mut new_span_id_pairs,
            &mut errors,
        );
    }
    if errors.is_empty() {
        Ok((new_expressions, new_span_id_pairs))
    } else {
        Err(errors)
    }
}

// @TODO Do all expressions need Persistence?

fn set_persistence<'a, 'code, 'old_code>(
    mut new_expression: &'a mut Spanned<Expression<'code>>,
    // @TODO rewrite to use root Object and only one `old_expression` here?
    old_expressions: &'a [&Spanned<Expression<'old_code>>],
    old_span_id_pairs: &HashMap<Span, PersistenceId>,
    new_span_id_pairs: &mut HashMap<Span, PersistenceId>,
    errors: &mut Vec<ResolveError>,
) {
    let Spanned {
        span,
        node: expression,
        persistence,
    } = &mut new_expression;

    match expression {
        Expression::Variable(variable) => {
            let old_variable_value_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Variable(old_variable),
                        } if variable.name == old_variable.name => {
                            Some((&old_variable.value, old_span_id_pairs[span]))
                        }
                        _ => None,
                    });
            if let Some((old_variable_value, id)) = old_variable_value_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    &mut variable.value,
                    &[old_variable_value],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(
                    &mut variable.value,
                    &[],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                )
            }
        }
        Expression::Object(object) => {
            let old_object_variables_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Object(old_object),
                        } => Some((&old_object.variables, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_object_variables, id)) = old_object_variables_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for variable in &mut object.variables {
                    let Spanned {
                        span,
                        node: variable,
                        persistence,
                    } = variable;
                    let old_variable_value_and_id =
                        old_object_variables
                            .iter()
                            .find_map(|old_variable| match old_variable {
                                Spanned {
                                    span,
                                    persistence: _,
                                    node: old_variable,
                                } if variable.name == old_variable.name => {
                                    Some((&old_variable.value, old_span_id_pairs[span]))
                                }
                                _ => None,
                            });
                    if let Some((old_variable_value, id)) = old_variable_value_and_id {
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::Unchanged,
                        });
                        set_persistence(
                            &mut variable.value,
                            &[old_variable_value],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        );
                    } else {
                        let id: Ulid = PersistenceId::new();
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::NewOrChanged,
                        });
                        set_persistence(
                            &mut variable.value,
                            &[],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        )
                    }
                }
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for variable in &mut object.variables {
                    let Spanned {
                        span,
                        node: variable,
                        persistence,
                    } = variable;
                    let id: Ulid = PersistenceId::new();
                    new_span_id_pairs.insert(*span, id);
                    *persistence = Some(Persistence {
                        id,
                        status: PersistenceStatus::NewOrChanged,
                    });
                    set_persistence(
                        &mut variable.value,
                        &[],
                        &old_span_id_pairs,
                        new_span_id_pairs,
                        errors,
                    );
                }
            }
        }
        Expression::TaggedObject { tag, object } => {
            let old_object_variables_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node:
                                Expression::TaggedObject {
                                    tag: old_tag,
                                    object: old_object,
                                },
                        } if tag == old_tag => {
                            Some((&old_object.variables, old_span_id_pairs[span]))
                        }
                        _ => None,
                    });
            if let Some((old_object_variables, id)) = old_object_variables_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for variable in &mut object.variables {
                    let Spanned {
                        span,
                        node: variable,
                        persistence,
                    } = variable;
                    let old_variable_value_and_id =
                        old_object_variables
                            .iter()
                            .find_map(|old_variable| match old_variable {
                                Spanned {
                                    span,
                                    persistence: _,
                                    node: old_variable,
                                } if variable.name == old_variable.name => {
                                    Some((&old_variable.value, old_span_id_pairs[span]))
                                }
                                _ => None,
                            });
                    if let Some((old_variable_value, id)) = old_variable_value_and_id {
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::Unchanged,
                        });
                        set_persistence(
                            &mut variable.value,
                            &[old_variable_value],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        );
                    } else {
                        let id: Ulid = PersistenceId::new();
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::NewOrChanged,
                        });
                        set_persistence(
                            &mut variable.value,
                            &[],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        )
                    }
                }
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for variable in &mut object.variables {
                    let Spanned {
                        span,
                        node: variable,
                        persistence,
                    } = variable;
                    let id: Ulid = PersistenceId::new();
                    new_span_id_pairs.insert(*span, id);
                    *persistence = Some(Persistence {
                        id,
                        status: PersistenceStatus::NewOrChanged,
                    });
                    set_persistence(
                        &mut variable.value,
                        &[],
                        &old_span_id_pairs,
                        new_span_id_pairs,
                        errors,
                    );
                }
            }
        }
        Expression::FunctionCall { path, arguments } => {
            let old_arguments_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node:
                                Expression::FunctionCall {
                                    path: old_path,
                                    arguments: old_arguments,
                                },
                        } if old_path == path => Some((old_arguments, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_arguments, id)) = old_arguments_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for (
                    index,
                    Spanned {
                        span,
                        persistence: _,
                        node: argument,
                    },
                ) in arguments.iter_mut().enumerate()
                {
                    // @TODO is only position relevant or do name diff as well?
                    // @TODO what about the change from a piped to named argument?
                    // @TODO what about different function versions?
                    let old_argument_value_and_id = if let Some(Spanned {
                        span,
                        persistence: _,
                        node: old_argument,
                    }) = old_arguments.get(index)
                    {
                        Some((&old_argument.value, old_span_id_pairs[span]))
                    } else {
                        None
                    };
                    if let Some((old_argument_value, id)) = old_argument_value_and_id {
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::Unchanged,
                        });
                        if let Some(argument_value) = argument.value.as_mut() {
                            if let Some(old_argument_value) = old_argument_value {
                                set_persistence(
                                    argument_value,
                                    &[old_argument_value],
                                    &old_span_id_pairs,
                                    new_span_id_pairs,
                                    errors,
                                )
                            } else {
                                set_persistence(
                                    argument_value,
                                    &[],
                                    &old_span_id_pairs,
                                    new_span_id_pairs,
                                    errors,
                                )
                            }
                        }
                    } else {
                        let id: Ulid = PersistenceId::new();
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::NewOrChanged,
                        });
                        if let Some(argument_value) = argument.value.as_mut() {
                            set_persistence(
                                argument_value,
                                &[],
                                &old_span_id_pairs,
                                new_span_id_pairs,
                                errors,
                            )
                        }
                    }
                }
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for Spanned {
                    span,
                    persistence: _,
                    node: argument,
                } in arguments
                {
                    let id: Ulid = PersistenceId::new();
                    new_span_id_pairs.insert(*span, id);
                    *persistence = Some(Persistence {
                        id,
                        status: PersistenceStatus::NewOrChanged,
                    });
                    if let Some(argument_value) = argument.value.as_mut() {
                        set_persistence(
                            argument_value,
                            &[],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        )
                    }
                }
            }
        }
        Expression::Block { variables, output } => {
            let old_variables_output_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Block { variables: old_variables, output: old_output },
                        } => Some((old_variables, old_output, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_variables, old_output, id)) = old_variables_output_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for variable in variables.iter_mut() {
                    let Spanned {
                        span,
                        node: variable,
                        persistence,
                    } = variable;
                    let old_variable_value_and_id =
                        old_variables
                            .iter()
                            .find_map(|old_variable| match old_variable {
                                Spanned {
                                    span,
                                    persistence: _,
                                    node: old_variable,
                                } if variable.name == old_variable.name => {
                                    Some((&old_variable.value, old_span_id_pairs[span]))
                                }
                                _ => None,
                            });
                    if let Some((old_variable_value, id)) = old_variable_value_and_id {
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::Unchanged,
                        });
                        set_persistence(
                            &mut variable.value,
                            &[old_variable_value],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        );
                    } else {
                        let id: Ulid = PersistenceId::new();
                        new_span_id_pairs.insert(*span, id);
                        *persistence = Some(Persistence {
                            id,
                            status: PersistenceStatus::NewOrChanged,
                        });
                        set_persistence(
                            &mut variable.value,
                            &[],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        )
                    }
                }
                set_persistence(
                    output,
                    &[old_output],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for variable in variables.iter_mut() {
                    let Spanned {
                        span,
                        node: variable,
                        persistence,
                    } = variable;
                    let id: Ulid = PersistenceId::new();
                    new_span_id_pairs.insert(*span, id);
                    *persistence = Some(Persistence {
                        id,
                        status: PersistenceStatus::NewOrChanged,
                    });
                    set_persistence(
                        &mut variable.value,
                        &[],
                        &old_span_id_pairs,
                        new_span_id_pairs,
                        errors,
                    );
                }
                set_persistence(output, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::List { items } => {
            let old_items_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::List { items: old_items },
                        } => Some((old_items, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_items, id)) = old_items_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for (index, item) in items.iter_mut().enumerate() {
                    let old_item_and_id = if let Some(old_item) = old_items.get(index) {
                        Some((old_item, old_span_id_pairs[&old_item.span]))
                    } else {
                        None
                    };
                    if let Some((old_item, id)) = old_item_and_id {
                        set_persistence(
                            item,
                            &[old_item],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        );
                    } else {
                        set_persistence(item, &[], &old_span_id_pairs, new_span_id_pairs, errors)
                    }
                }
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for item in items {
                    set_persistence(item, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                }
            }
        }
        Expression::Map { entries } => {
            // For maps, just assign new persistence IDs to all entries
            // (More sophisticated comparison would require comparing keys)
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
            for entry in entries {
                set_persistence(
                    &mut entry.value,
                    &[],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            }
        }
        Expression::Latest { inputs } => {
            let old_inputs_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Latest { inputs: old_inputs },
                        } => Some((old_inputs, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_inputs, id)) = old_inputs_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for (index, input) in inputs.iter_mut().enumerate() {
                    let old_input_and_id = if let Some(old_input) = old_inputs.get(index) {
                        Some((old_input, old_span_id_pairs[&old_input.span]))
                    } else {
                        None
                    };
                    if let Some((old_input, id)) = old_input_and_id {
                        set_persistence(
                            input,
                            &[old_input],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        );
                    } else {
                        set_persistence(input, &[], &old_span_id_pairs, new_span_id_pairs, errors)
                    }
                }
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for input in inputs {
                    set_persistence(input, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                }
            }
        }
        Expression::Then { body } => {
            let old_body_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Then { body: old_body },
                        } => Some((old_body, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_body, id)) = old_body_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    body,
                    &[old_body],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(body, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::When { arms } => {
            // For When expressions, assign a new ID (arms have non-Spanned bodies)
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
        }
        Expression::While { arms } => {
            // For While expressions, assign a new ID (arms have non-Spanned bodies)
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
        }
        Expression::Pipe { from, to } => {
            let old_from_to_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node:
                                Expression::Pipe {
                                    from: old_from,
                                    to: old_to,
                                },
                        } => Some((old_from, old_to, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_from, old_to, id)) = old_from_to_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    from,
                    &[old_from],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
                set_persistence(to, &[old_to], &old_span_id_pairs, new_span_id_pairs, errors);
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(from, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                set_persistence(to, &[], &old_span_id_pairs, new_span_id_pairs, errors);
            }
        }
        Expression::ArithmeticOperator(op) => {
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
            // Recurse into operands
            match op {
                ArithmeticOperator::Negate { operand } => {
                    set_persistence(operand, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                }
                ArithmeticOperator::Add { operand_a, operand_b }
                | ArithmeticOperator::Subtract { operand_a, operand_b }
                | ArithmeticOperator::Multiply { operand_a, operand_b }
                | ArithmeticOperator::Divide { operand_a, operand_b } => {
                    set_persistence(operand_a, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                    set_persistence(operand_b, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                }
            }
        }
        Expression::Comparator(cmp) => {
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
            // Recurse into operands
            match cmp {
                Comparator::Equal { operand_a, operand_b }
                | Comparator::NotEqual { operand_a, operand_b }
                | Comparator::Greater { operand_a, operand_b }
                | Comparator::GreaterOrEqual { operand_a, operand_b }
                | Comparator::Less { operand_a, operand_b }
                | Comparator::LessOrEqual { operand_a, operand_b } => {
                    set_persistence(operand_a, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                    set_persistence(operand_b, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                }
            }
        }
        Expression::Function {
            name,
            parameters,
            body,
        } => {
            let old_body_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node:
                                Expression::Function {
                                    name: old_name,
                                    parameters: _,
                                    body: old_body,
                                },
                        } if old_name == name => Some((old_body, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_body, id)) = old_body_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    body,
                    &[old_body],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(body, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::LinkSetter { alias: _ } => {
            // LinkSetter just needs a persistence ID like Alias
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
        }
        Expression::Alias(alias) => {
            let alias_string = alias.to_string();
            let id = old_expressions.iter().find_map(|old_expression| {
                match old_expression {
                    // @TODO diff referenced variable/argument instead of stringified alias?
                    Spanned {
                        span,
                        persistence: _,
                        node: Expression::Alias(old_alias),
                    } if old_alias.to_string() == alias_string => Some(old_span_id_pairs[span]),
                    _ => None,
                }
            });
            if let Some(id) = id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
            }
        }
        Expression::Literal(literal) => {
            let id = old_expressions
                .iter()
                .find_map(|old_expression| match literal {
                    Literal::Number(number) => match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Literal(Literal::Number(old_number)),
                        } if old_number == number => Some(old_span_id_pairs[span]),
                        _ => None,
                    },
                    Literal::Tag(tag) => match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Literal(Literal::Tag(old_tag)),
                        } if old_tag == tag => Some(old_span_id_pairs[span]),
                        _ => None,
                    },
                    Literal::Text(text) => match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Literal(Literal::Text(old_text)),
                        } if old_text == text => Some(old_span_id_pairs[span]),
                        _ => None,
                    },
                });
            if let Some(id) = id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
            }
        }
        Expression::Link => {
            let id = old_expressions
                .iter()
                .find_map(|old_expression| match old_expression {
                    Spanned {
                        span,
                        persistence: _,
                        node: Expression::Link,
                    } => Some(old_span_id_pairs[span]),
                    _ => None,
                });
            if let Some(id) = id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
            }
        }
        Expression::Skip => {
            let id = old_expressions
                .iter()
                .find_map(|old_expression| match old_expression {
                    Spanned {
                        span,
                        persistence: _,
                        node: Expression::Skip,
                    } => Some(old_span_id_pairs[span]),
                    _ => None,
                });
            if let Some(id) = id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
            }
        }
        Expression::TextLiteral { parts: _ } => {
            // TextLiteral is like a Literal - just assign a new ID
            let id: Ulid = PersistenceId::new();
            new_span_id_pairs.insert(*span, id);
            *persistence = Some(Persistence {
                id,
                status: PersistenceStatus::NewOrChanged,
            });
        }
        Expression::LatestWithState { state_param, body } => {
            let old_body_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::LatestWithState { state_param: old_state_param, body: old_body },
                        } if old_state_param == state_param => Some((old_body, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_body, id)) = old_body_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    body,
                    &[old_body],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(body, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::Flush { value } => {
            let old_value_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Flush { value: old_value },
                        } => Some((old_value, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_value, id)) = old_value_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    value,
                    &[old_value],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(value, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::Pulses { count } => {
            let old_count_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Pulses { count: old_count },
                        } => Some((old_count, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_count, id)) = old_count_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    count,
                    &[old_count],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(count, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::Spread { value } => {
            let old_value_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Spread { value: old_value },
                        } => Some((old_value, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_value, id)) = old_value_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    value,
                    &[old_value],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(value, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        // Hardware types (parse-only for now)
        Expression::Bits { size } => {
            let old_size_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Bits { size: old_size },
                        } => Some((old_size, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_size, id)) = old_size_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    size,
                    &[old_size],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(size, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::Memory { address } => {
            let old_address_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Memory { address: old_address },
                        } => Some((old_address, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_address, id)) = old_address_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                set_persistence(
                    address,
                    &[old_address],
                    &old_span_id_pairs,
                    new_span_id_pairs,
                    errors,
                );
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                set_persistence(address, &[], &old_span_id_pairs, new_span_id_pairs, errors)
            }
        }
        Expression::Bytes { data } => {
            let old_data_and_id =
                old_expressions
                    .iter()
                    .find_map(|old_expression| match old_expression {
                        Spanned {
                            span,
                            persistence: _,
                            node: Expression::Bytes { data: old_data },
                        } => Some((old_data, old_span_id_pairs[span])),
                        _ => None,
                    });
            if let Some((old_data, id)) = old_data_and_id {
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::Unchanged,
                });
                for (index, item) in data.iter_mut().enumerate() {
                    let old_item_and_id = if let Some(old_item) = old_data.get(index) {
                        Some((old_item, old_span_id_pairs[&old_item.span]))
                    } else {
                        None
                    };
                    if let Some((old_item, _id)) = old_item_and_id {
                        set_persistence(
                            item,
                            &[old_item],
                            &old_span_id_pairs,
                            new_span_id_pairs,
                            errors,
                        );
                    } else {
                        set_persistence(item, &[], &old_span_id_pairs, new_span_id_pairs, errors)
                    }
                }
            } else {
                let id: Ulid = PersistenceId::new();
                new_span_id_pairs.insert(*span, id);
                *persistence = Some(Persistence {
                    id,
                    status: PersistenceStatus::NewOrChanged,
                });
                for item in data {
                    set_persistence(item, &[], &old_span_id_pairs, new_span_id_pairs, errors);
                }
            }
        }
    }
}
