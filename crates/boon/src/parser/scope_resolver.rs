use super::{Alias, ArithmeticOperator, Comparator, Expression, ParseError, Pattern, Span, Spanned, TextPart, Token};
use std::collections::{BTreeMap, HashSet};

// @TODO Immutables or different tree traversal algorithm?
pub type ReachableReferenceables<'code> = BTreeMap<&'code str, Vec<Referenceable<'code>>>;

#[derive(Debug, Clone)]
pub struct Referenceables<'code> {
    pub referenced: Option<Referenceable<'code>>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct Referenceable<'code> {
    pub name: &'code str,
    pub span: Span,
    pub level: usize,
}

pub type ResolveError<'code> = ParseError<'code, Token<'code>>;

// @TODO How to handle loops?
pub fn resolve_references(
    mut expressions: Vec<Spanned<Expression>>,
) -> Result<Vec<Spanned<Expression>>, Vec<ResolveError>> {
    let mut reachable_referenceables = ReachableReferenceables::default();
    let level = 0;
    let parent_name = None::<&str>;
    for expressions in &expressions {
        let Spanned {
            span,
            node: expression,
            persistence: _,
        } = expressions;
        match expression {
            Expression::Variable(variable) => {
                let name = &variable.name;
                reachable_referenceables
                    .entry(name)
                    .or_default()
                    .push(Referenceable {
                        name,
                        span: *span,
                        level,
                    });
            }
            Expression::Function { name, .. } => {
                reachable_referenceables
                    .entry(name)
                    .or_default()
                    .push(Referenceable {
                        name,
                        span: *span,
                        level,
                    });
            }
            _ => {}
        }
    }
    let mut errors = Vec::new();
    let mut all_referenced = HashSet::new();
    for expression in &mut expressions {
        set_is_referenced_and_alias_referenceables(
            expression,
            reachable_referenceables.clone(),
            level,
            parent_name,
            &mut errors,
            &mut all_referenced,
        );
    }
    for expressions in &mut expressions {
        let Spanned {
            span,
            node: expression,
            persistence: _,
        } = expressions;
        if let Expression::Variable(variable) = expression {
            let name = &variable.name;
            if all_referenced.contains(&Referenceable {
                name,
                span: *span,
                level,
            }) {
                variable.is_referenced = true;
            }
        }
    }
    if errors.is_empty() {
        Ok(expressions)
    } else {
        Err(errors)
    }
}

fn set_is_referenced_and_alias_referenceables<'a, 'code>(
    mut expression: &'a mut Spanned<Expression<'code>>,
    mut reachable_referenceables: ReachableReferenceables<'code>,
    mut level: usize,
    parent_name: Option<&str>,
    errors: &mut Vec<ResolveError>,
    all_referenced: &mut HashSet<Referenceable<'code>>,
) {
    let Spanned {
        span,
        node: expression,
        persistence: _,
    } = &mut expression;
    match expression {
        Expression::Variable(variable) => {
            set_is_referenced_and_alias_referenceables(
                &mut variable.value,
                reachable_referenceables,
                level,
                Some(variable.name),
                errors,
                all_referenced,
            );
        }
        Expression::Object(object) => {
            level += 1;
            for variable in &object.variables {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                let name = &variable.name;
                reachable_referenceables
                    .entry(name)
                    .or_default()
                    .push(Referenceable {
                        name,
                        span: *span,
                        level,
                    });
            }
            for variable in &mut object.variables {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                set_is_referenced_and_alias_referenceables(
                    &mut variable.value,
                    reachable_referenceables.clone(),
                    level,
                    Some(variable.name),
                    errors,
                    all_referenced,
                );
            }
            for variable in &mut object.variables {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                let name = &variable.name;
                if all_referenced.contains(&Referenceable {
                    name,
                    span: *span,
                    level,
                }) {
                    variable.is_referenced = true;
                }
            }
        }
        Expression::TaggedObject { tag, object } => {
            level += 1;
            for variable in &object.variables {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                let name = &variable.name;
                reachable_referenceables
                    .entry(name)
                    .or_default()
                    .push(Referenceable {
                        name,
                        span: *span,
                        level,
                    });
            }
            for variable in &mut object.variables {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                set_is_referenced_and_alias_referenceables(
                    &mut variable.value,
                    reachable_referenceables.clone(),
                    level,
                    Some(variable.name),
                    errors,
                    all_referenced,
                );
            }
            for variable in &mut object.variables {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                let name = &variable.name;
                if all_referenced.contains(&Referenceable {
                    name,
                    span: *span,
                    level,
                }) {
                    variable.is_referenced = true;
                }
            }
        }
        Expression::FunctionCall { path, arguments } => {
            level += 1;
            for argument in arguments.iter() {
                let Spanned {
                    span,
                    node: argument,
                    persistence: _,
                } = argument;
                let name = &argument.name;
                reachable_referenceables
                    .entry(name)
                    .or_default()
                    .push(Referenceable {
                        name,
                        span: *span,
                        level,
                    });
            }
            for argument in arguments.iter_mut() {
                let Spanned {
                    span,
                    node: argument,
                    persistence: _,
                } = argument;
                if let Some(value) = argument.value.as_mut() {
                    set_is_referenced_and_alias_referenceables(
                        value,
                        reachable_referenceables.clone(),
                        level,
                        Some(argument.name),
                        errors,
                        all_referenced,
                    );
                }
            }
            for argument in arguments.iter_mut() {
                let Spanned {
                    span,
                    node: argument,
                    persistence: _,
                } = argument;
                let name = &argument.name;
                if all_referenced.contains(&Referenceable {
                    name,
                    span: *span,
                    level,
                }) {
                    argument.is_referenced = true;
                }
            }
        }
        Expression::Block { variables, output } => {
            level += 1;
            for variable in variables.iter() {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                let name = &variable.name;
                reachable_referenceables
                    .entry(name)
                    .or_default()
                    .push(Referenceable {
                        name,
                        span: *span,
                        level,
                    });
            }
            for variable in variables.iter_mut() {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                set_is_referenced_and_alias_referenceables(
                    &mut variable.value,
                    reachable_referenceables.clone(),
                    level,
                    Some(variable.name),
                    errors,
                    all_referenced,
                );
            }
            set_is_referenced_and_alias_referenceables(
                output,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
            for variable in variables.iter_mut() {
                let Spanned {
                    span,
                    node: variable,
                    persistence: _,
                } = variable;
                let name = &variable.name;
                if all_referenced.contains(&Referenceable {
                    name,
                    span: *span,
                    level,
                }) {
                    variable.is_referenced = true;
                }
            }
        }
        Expression::List { items } => {
            for item in items {
                set_is_referenced_and_alias_referenceables(
                    item,
                    reachable_referenceables.clone(),
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        Expression::Map { entries } => {
            // @TODO implement, see the error message below
            errors.push(ResolveError::custom(
                *span,
                "Scope resolver cannot resolve references in Expression::Map yet, sorry".to_owned(),
            ))
        }
        Expression::Latest { inputs } => {
            for input in inputs {
                set_is_referenced_and_alias_referenceables(
                    input,
                    reachable_referenceables.clone(),
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        Expression::Then { body } => {
            set_is_referenced_and_alias_referenceables(
                body,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        Expression::When { arms } => {
            for arm in arms {
                // Collect pattern bindings to add to scope for the body
                let body_level = level + 1;
                let bindings = collect_pattern_bindings(&arm.pattern, *span, body_level);

                // Create scope with pattern bindings for the body
                let mut body_reachables = reachable_referenceables.clone();
                for (name, referenceable) in bindings {
                    body_reachables.entry(name).or_default().push(referenceable);
                }

                // Resolve body references with pattern bindings in scope
                set_is_referenced_and_alias_referenceables(
                    &mut arm.body,
                    body_reachables,
                    body_level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        Expression::While { arms } => {
            for arm in arms {
                // Collect pattern bindings to add to scope for the body
                let body_level = level + 1;
                let bindings = collect_pattern_bindings(&arm.pattern, *span, body_level);

                // Create scope with pattern bindings for the body
                let mut body_reachables = reachable_referenceables.clone();
                for (name, referenceable) in bindings {
                    body_reachables.entry(name).or_default().push(referenceable);
                }

                // Resolve body references with pattern bindings in scope
                set_is_referenced_and_alias_referenceables(
                    &mut arm.body,
                    body_reachables,
                    body_level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        Expression::Pipe { from, to } => {
            set_is_referenced_and_alias_referenceables(
                from,
                reachable_referenceables.clone(),
                level,
                parent_name,
                errors,
                all_referenced,
            );
            set_is_referenced_and_alias_referenceables(
                to,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        Expression::ArithmeticOperator(op) => match op {
            ArithmeticOperator::Negate { operand } => {
                set_is_referenced_and_alias_referenceables(
                    operand,
                    reachable_referenceables,
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
            ArithmeticOperator::Add { operand_a, operand_b }
            | ArithmeticOperator::Subtract { operand_a, operand_b }
            | ArithmeticOperator::Multiply { operand_a, operand_b }
            | ArithmeticOperator::Divide { operand_a, operand_b } => {
                set_is_referenced_and_alias_referenceables(
                    operand_a,
                    reachable_referenceables.clone(),
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
                set_is_referenced_and_alias_referenceables(
                    operand_b,
                    reachable_referenceables,
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        Expression::Comparator(cmp) => match cmp {
            Comparator::Equal { operand_a, operand_b }
            | Comparator::NotEqual { operand_a, operand_b }
            | Comparator::Greater { operand_a, operand_b }
            | Comparator::GreaterOrEqual { operand_a, operand_b }
            | Comparator::Less { operand_a, operand_b }
            | Comparator::LessOrEqual { operand_a, operand_b } => {
                set_is_referenced_and_alias_referenceables(
                    operand_a,
                    reachable_referenceables.clone(),
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
                set_is_referenced_and_alias_referenceables(
                    operand_b,
                    reachable_referenceables,
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        Expression::Function {
            name,
            parameters,
            body,
        } => {
            // Add parameters to reachable referenceables so they can be referenced in the body
            level += 1;
            for parameter in parameters.iter() {
                let Spanned {
                    span,
                    node: param_name,
                    persistence: _,
                } = parameter;
                reachable_referenceables
                    .entry(param_name)
                    .or_default()
                    .push(Referenceable {
                        name: param_name,
                        span: *span,
                        level,
                    });
            }
            // Resolve references in the function body
            set_is_referenced_and_alias_referenceables(
                body,
                reachable_referenceables.clone(),
                level,
                Some(*name),
                errors,
                all_referenced,
            );
        }
        Expression::Alias(alias) => set_referenced_referenceable(
            alias,
            *span,
            reachable_referenceables,
            parent_name,
            errors,
            all_referenced,
        ),
        Expression::LinkSetter { alias } => {
            let Spanned {
                span,
                node: alias,
                persistence: _,
            } = alias;
            set_referenced_referenceable(
                alias,
                *span,
                reachable_referenceables,
                parent_name,
                errors,
                all_referenced,
            )
        }
        Expression::Literal(_) => (),
        Expression::Link => (),
        Expression::Skip => (),
        Expression::TextLiteral { parts } => {
            // Resolve interpolation variables in the text literal
            // Supports field access paths like "item.text" where "item" is the base variable
            for part in parts.iter_mut() {
                if let TextPart::Interpolation { var, referenced_span } = part {
                    // Split on '.' to handle field access paths
                    let var_parts: Vec<&str> = var.split('.').collect();
                    let base_var = var_parts[0];

                    // Look up the base variable in reachable referenceables
                    let reachable_map: BTreeMap<&str, Referenceable> = reachable_referenceables
                        .iter()
                        .filter_map(|(name, referenceables)| {
                            referenceables.iter().rev().enumerate().find_map(
                                |(index, referenceable)| {
                                    if index == 0 && Some(referenceable.name) == parent_name {
                                        None
                                    } else {
                                        Some((referenceable.name, referenceable.clone()))
                                    }
                                },
                            )
                        })
                        .collect();

                    if let Some(referenceable) = reachable_map.get(base_var) {
                        // Found the base variable - field access will be resolved at runtime
                        *referenced_span = Some(referenceable.span);
                        all_referenced.insert(*referenceable);
                    } else {
                        let reachable_names: Vec<_> = reachable_map.keys().collect();
                        errors.push(ResolveError::custom(*span, format!("Cannot find variable '{}' for text interpolation. Available: {:?}", var, reachable_names)));
                    }
                }
            }
        }
        Expression::Hold { state_param, body } => {
            // Add state_param to reachable referenceables so it can be referenced in the body
            level += 1;
            reachable_referenceables
                .entry(state_param)
                .or_default()
                .push(Referenceable {
                    name: state_param,
                    span: *span,
                    level,
                });
            set_is_referenced_and_alias_referenceables(
                body,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        Expression::Flush { value } => {
            set_is_referenced_and_alias_referenceables(
                value,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        Expression::Spread { value } => {
            set_is_referenced_and_alias_referenceables(
                value,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        // Hardware types (parse-only for now)
        Expression::Bits { size } => {
            set_is_referenced_and_alias_referenceables(
                size,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        Expression::Memory { address } => {
            set_is_referenced_and_alias_referenceables(
                address,
                reachable_referenceables,
                level,
                parent_name,
                errors,
                all_referenced,
            );
        }
        Expression::Bytes { data } => {
            for item in data {
                set_is_referenced_and_alias_referenceables(
                    item,
                    reachable_referenceables.clone(),
                    level,
                    parent_name,
                    errors,
                    all_referenced,
                );
            }
        }
        // FieldAccess has no sub-expressions to recurse into
        Expression::FieldAccess { .. } => {}
    }
}

fn set_referenced_referenceable<'code>(
    alias: &mut Alias<'code>,
    span: Span,
    reachable_referenceables: ReachableReferenceables<'code>,
    parent_name: Option<&str>,
    errors: &mut Vec<ResolveError>,
    all_referenced: &mut HashSet<Referenceable<'code>>,
) {
    match alias {
        Alias::WithPassed { extra_parts } => (),
        Alias::WithoutPassed {
            parts,
            referenceables: unset_referenceables,
        } => {
            // @TODO make the first part a standalone property (name or PASSED)?
            let first_part = *parts.first().expect("Failed to get first alias part");
            // @TODO make Argument name optional to model the case when an argument is piped better?
            if first_part.is_empty() {
                return;
            }
            let reachable_referenceables: BTreeMap<&str, Referenceable> = reachable_referenceables
                .into_iter()
                .filter_map(|(name, referenceables)| {
                    referenceables.into_iter().rev().enumerate().find_map(
                        |(index, referenceable)| {
                            if index == 0 && Some(referenceable.name) == parent_name {
                                None
                            } else {
                                Some((referenceable.name, referenceable))
                            }
                        },
                    )
                })
                .collect();
            let referenced = reachable_referenceables.get(first_part).copied();
            if let Some(referenced) = referenced {
                all_referenced.insert(referenced);
            } else {
                let reachable_names = reachable_referenceables.keys();
                errors.push(ResolveError::custom(span, format!("Cannot find the variable or argument '{first_part}'. You can refer to: {reachable_names:?}")))
            }
            let referenceables = Referenceables { referenced };
            *unset_referenceables = Some(referenceables);
        }
    }
}

/// Resolve references in patterns (for WHEN/WHILE arms)
fn resolve_pattern_references<'code>(
    pattern: &Pattern<'code>,
    span: Span,
    reachable_referenceables: &ReachableReferenceables<'code>,
    parent_name: Option<&str>,
    errors: &mut Vec<ResolveError>,
    all_referenced: &mut HashSet<Referenceable<'code>>,
) {
    match pattern {
        Pattern::Alias { name } => {
            // Pattern::Alias in WHEN/WHILE references an existing variable for comparison
            let reachable: BTreeMap<&str, Referenceable> = reachable_referenceables
                .iter()
                .filter_map(|(name, referenceables)| {
                    referenceables.iter().rev().enumerate().find_map(
                        |(index, referenceable)| {
                            if index == 0 && Some(referenceable.name) == parent_name {
                                None
                            } else {
                                Some((referenceable.name, *referenceable))
                            }
                        },
                    )
                })
                .collect();
            if let Some(referenced) = reachable.get(name).copied() {
                all_referenced.insert(referenced);
            }
            // Note: We don't emit an error here if the variable is not found,
            // because Pattern::Alias can also be used for pattern matching literals
        }
        Pattern::List { items } => {
            for item in items {
                resolve_pattern_references(item, span, reachable_referenceables, parent_name, errors, all_referenced);
            }
        }
        Pattern::Object { variables } => {
            for var in variables {
                if let Some(ref value) = var.value {
                    resolve_pattern_references(value, span, reachable_referenceables, parent_name, errors, all_referenced);
                }
            }
        }
        Pattern::TaggedObject { variables, .. } => {
            for var in variables {
                if let Some(ref value) = var.value {
                    resolve_pattern_references(value, span, reachable_referenceables, parent_name, errors, all_referenced);
                }
            }
        }
        Pattern::Map { entries } => {
            for entry in entries {
                resolve_pattern_references(&entry.key, span, reachable_referenceables, parent_name, errors, all_referenced);
                if let Some(ref value) = entry.value {
                    resolve_pattern_references(value, span, reachable_referenceables, parent_name, errors, all_referenced);
                }
            }
        }
        Pattern::Literal(_) | Pattern::WildCard => {
            // No references to resolve
        }
    }
}

/// Collect all pattern bindings (variable names introduced by the pattern).
/// Pattern::Alias always creates a binding - it binds the matched value to the name.
fn collect_pattern_bindings<'code>(
    pattern: &Pattern<'code>,
    span: Span,
    level: usize,
) -> Vec<(&'code str, Referenceable<'code>)> {
    let mut bindings = Vec::new();
    match pattern {
        Pattern::Alias { name } => {
            // Pattern::Alias binds the matched value to this name
            bindings.push((*name, Referenceable { name, span, level }));
        }
        Pattern::List { items } => {
            for item in items {
                bindings.extend(collect_pattern_bindings(item, span, level));
            }
        }
        Pattern::Object { variables } => {
            for var in variables {
                // The variable name itself is a binding
                bindings.push((var.name, Referenceable { name: var.name, span, level }));
                // If there's a nested pattern, collect its bindings too
                if let Some(ref value) = var.value {
                    bindings.extend(collect_pattern_bindings(value, span, level));
                }
            }
        }
        Pattern::TaggedObject { variables, .. } => {
            for var in variables {
                // The variable name itself is a binding
                bindings.push((var.name, Referenceable { name: var.name, span, level }));
                // If there's a nested pattern, collect its bindings too
                if let Some(ref value) = var.value {
                    bindings.extend(collect_pattern_bindings(value, span, level));
                }
            }
        }
        Pattern::Map { entries } => {
            for entry in entries {
                // Keys in map patterns could be bindings
                bindings.extend(collect_pattern_bindings(&entry.key, span, level));
                if let Some(ref value) = entry.value {
                    bindings.extend(collect_pattern_bindings(value, span, level));
                }
            }
        }
        Pattern::Literal(_) | Pattern::WildCard => {
            // Literals and wildcards don't create bindings
        }
    }
    bindings
}
