// @TODO remove
#![allow(dead_code)]

use chumsky::{input::ValueInput, pratt::*, prelude::*};
use std::fmt;

mod lexer;
pub use lexer::{Token, lexer};

mod scope_resolver;
pub use scope_resolver::{Referenceables, resolve_references};

mod persistence_resolver;
pub use persistence_resolver::{Persistence, PersistenceId, resolve_persistence};

mod source;
pub use source::{SourceCode, StrSlice};

mod static_expression;
pub use static_expression::*;

pub use chumsky::prelude::{Input, Parser};

pub type Span = SimpleSpan;
pub type ParseError<'code, T> = Rich<'code, T, Span>;

#[derive(Debug, Clone)]
pub struct Spanned<T> {
    pub span: Span,
    pub persistence: Option<Persistence>,
    pub node: T,
}

pub fn parser<'code, I>()
-> impl Parser<'code, I, Vec<Spanned<Expression<'code>>>, extra::Err<ParseError<'code, Token<'code>>>>
where
    I: ValueInput<'code, Token = Token<'code>, Span = Span>,
{
    let newlines = just(Token::Newline).repeated();

    recursive(|expression| {
        let colon = just(Token::Colon);
        let slash = just(Token::Slash);
        let comma = just(Token::Comma);
        let dot = just(Token::Dot);
        let bracket_round_open = just(Token::BracketRoundOpen);
        let bracket_round_close = just(Token::BracketRoundClose);
        let bracket_curly_open = just(Token::BracketCurlyOpen);
        let bracket_curly_close = just(Token::BracketCurlyClose);
        let bracket_square_open = just(Token::BracketSquareOpen);
        let bracket_square_close = just(Token::BracketSquareClose);

        let snake_case_identifier =
            select! { Token::SnakeCaseIdentifier(identifier) => identifier };
        let pascal_case_identifier =
            select! { Token::PascalCaseIdentifier(identifier) => identifier };

        let variable =
            group((snake_case_identifier, colon, expression.clone())).map(|(name, _, value)| {
                Variable {
                    name,
                    is_referenced: false,
                    value,
                }
            });

        let expression_variable = variable
            .clone()
            .map(|variable| Expression::Variable(Box::new(variable)));

        let function_call = {
            let path = pascal_case_identifier
                .then_ignore(slash)
                .repeated()
                .collect::<Vec<_>>()
                .then(snake_case_identifier)
                .map(|(mut path, variable_name)| {
                    path.push(variable_name);
                    path
                });

            // Regular argument: name: value or just name
            let regular_argument = snake_case_identifier
                .then(group((colon.clone(), expression.clone())).or_not())
                .map_with(|(name, value), extra| {
                    let value = value.map(|(_, value)| value);
                    Spanned {
                        node: Argument {
                            name,
                            is_referenced: false,
                            value,
                        },
                        span: extra.span(),
                        persistence: None,
                    }
                });

            // PASS: value - special argument for implicit context
            let pass_argument = just(Token::Pass)
                .ignore_then(colon.clone())
                .ignore_then(expression.clone())
                .map_with(|value, extra| {
                    Spanned {
                        node: Argument {
                            name: "PASS",
                            is_referenced: false,
                            value: Some(value),
                        },
                        span: extra.span(),
                        persistence: None,
                    }
                });

            let argument = choice((pass_argument, regular_argument));

            path.then(
                argument
                    .separated_by(comma.ignored().or(newlines))
                    .collect()
                    .delimited_by(
                        bracket_round_open.then(newlines),
                        newlines.then(bracket_round_close),
                    ),
            )
            .map(|(path, arguments)| Expression::FunctionCall { path, arguments })
        };

        let number = select! { Token::Number(number) => Literal::Number(number) };
        let text = select! { Token::Text(text) => Literal::Text(text) };
        let tag = pascal_case_identifier.map(Literal::Tag);

        let literal = choice((number, text, tag));
        let expression_literal = literal.map(Expression::Literal);

        let list = just(Token::List)
            .ignore_then(
                expression
                    .clone()
                    .separated_by(comma.ignored().or(newlines))
                    .collect()
                    .delimited_by(
                        bracket_curly_open.then(newlines),
                        newlines.then(bracket_curly_close),
                    ),
            )
            .map(|items| Expression::List { items });

        let object = variable
            .map_with(|variable, extra| Spanned {
                node: variable,
                span: extra.span(),
                persistence: None,
            })
            .separated_by(comma.ignored().or(newlines))
            .collect()
            .delimited_by(
                bracket_square_open.then(newlines),
                newlines.then(bracket_square_close),
            )
            .map(|variables| Object { variables });

        let expression_object = object.clone().map(Expression::Object);

        let tagged_object = pascal_case_identifier
            .then(object)
            .map(|(tag, object)| Expression::TaggedObject { tag, object });

        let alias = {
            let alias_with_passed = just(Token::Passed)
                .ignore_then(
                    snake_case_identifier
                        .separated_by(dot)
                        .allow_leading()
                        .collect::<Vec<_>>(),
                )
                .map(|extra_parts| Alias::WithPassed { extra_parts });

            let alias_without_passed = snake_case_identifier
                .separated_by(dot)
                .at_least(1)
                .collect::<Vec<_>>()
                .map(|parts| Alias::WithoutPassed {
                    parts,
                    referenceables: None,
                });

            alias_with_passed.or(alias_without_passed)
        };

        let expression_alias = alias.map(Expression::Alias);

        let map = {
            let key = literal
                .map(MapEntryKey::Literal)
                .or(alias.map(MapEntryKey::Alias))
                .map_with(|key, extra| Spanned {
                    span: extra.span(),
                    node: key,
                    persistence: None,
                });

            let key_value_pair = group((key, colon, expression.clone()))
                .map(|(key, _, value)| MapEntry { key, value });

            just(Token::Map)
                .ignore_then(
                    key_value_pair
                        .separated_by(comma.ignored().or(newlines))
                        .collect()
                        .delimited_by(
                            bracket_curly_open.then(newlines),
                            newlines.then(bracket_curly_close),
                        ),
                )
                .map(|entries| Expression::Map { entries })
        };

        let function = {
            let parameters = snake_case_identifier
                .map_with(|parameter_name, extra| Spanned {
                    node: parameter_name,
                    span: extra.span(),
                    persistence: None,
                })
                .separated_by(comma.ignored().or(newlines))
                .collect()
                .delimited_by(
                    bracket_round_open.then(newlines),
                    newlines.then(bracket_round_close),
                );

            just(Token::Function)
                .ignore_then(snake_case_identifier)
                .then(parameters)
                .then(expression.clone().delimited_by(
                    bracket_curly_open.then(newlines),
                    newlines.then(bracket_curly_close),
                ))
                .map(|((name, parameters), body)| Expression::Function {
                    name,
                    parameters,
                    body: Box::new(body),
                })
        };

        let link = just(Token::Link);
        let link_expression = link.map(|_| Expression::Link);

        let link_setter = link.ignore_then(
            alias
                .delimited_by(
                    bracket_curly_open.then(newlines),
                    newlines.then(bracket_curly_close),
                )
                .map_with(|alias, extra| Expression::LinkSetter {
                    alias: Spanned {
                        span: extra.span(),
                        node: alias,
                        persistence: None,
                    },
                }),
        );

        let latest = just(Token::Latest)
            .ignore_then(
                expression
                    .clone()
                    .separated_by(comma.ignored().or(newlines))
                    .collect()
                    .delimited_by(
                        bracket_curly_open.then(newlines),
                        newlines.then(bracket_curly_close),
                    ),
            )
            .map(|inputs| Expression::Latest { inputs });

        let then = just(Token::Then).ignore_then(
            expression
                .clone()
                .delimited_by(
                    bracket_curly_open.then(newlines),
                    newlines.then(bracket_curly_close),
                )
                .map(|body| Expression::Then {
                    body: Box::new(body),
                }),
        );

        // Pattern parser for WHEN/WHILE arms
        let pattern = recursive(|pattern| {
            let pattern_wildcard = just(Token::Wildcard).map(|_| Pattern::WildCard);

            let pattern_literal_number = select! { Token::Number(number) => Pattern::Literal(Literal::Number(number)) };
            let pattern_literal_text = select! { Token::Text(text) => Pattern::Literal(Literal::Text(text)) };
            let pattern_literal_tag = pascal_case_identifier.map(|tag| Pattern::Literal(Literal::Tag(tag)));

            let pattern_alias = snake_case_identifier.map(|name| Pattern::Alias { name });

            let pattern_variable = snake_case_identifier
                .then(group((colon.clone(), pattern.clone())).or_not())
                .map(|(name, value)| PatternVariable {
                    name,
                    value: value.map(|(_, v)| v),
                });

            let pattern_object = pattern_variable
                .clone()
                .separated_by(comma.clone().ignored().or(newlines.clone()))
                .collect()
                .delimited_by(
                    bracket_square_open.clone().then(newlines.clone()),
                    newlines.clone().then(bracket_square_close.clone()),
                )
                .map(|variables| Pattern::Object { variables });

            let pattern_tagged_object = pascal_case_identifier
                .then(
                    pattern_variable
                        .separated_by(comma.clone().ignored().or(newlines.clone()))
                        .collect()
                        .delimited_by(
                            bracket_square_open.clone().then(newlines.clone()),
                            newlines.clone().then(bracket_square_close.clone()),
                        ),
                )
                .map(|(tag, variables)| Pattern::TaggedObject { tag, variables });

            let pattern_list = just(Token::List)
                .ignore_then(
                    pattern
                        .clone()
                        .separated_by(comma.clone().ignored().or(newlines.clone()))
                        .collect()
                        .delimited_by(
                            bracket_curly_open.clone().then(newlines.clone()),
                            newlines.clone().then(bracket_curly_close.clone()),
                        ),
                )
                .map(|items| Pattern::List { items });

            choice((
                pattern_wildcard,
                pattern_list,
                pattern_tagged_object,
                pattern_object,
                pattern_literal_number,
                pattern_literal_text,
                pattern_literal_tag,
                pattern_alias,
            ))
        });

        // Arm parser: pattern => body
        let arm = pattern
            .then_ignore(just(Token::Implies))
            .then(expression.clone())
            .map(|(pattern, body)| Arm { pattern, body: body.node });

        let when = just(Token::When)
            .ignore_then(
                arm.clone()
                    .separated_by(comma.clone().ignored().or(newlines.clone()))
                    .collect()
                    .delimited_by(
                        bracket_curly_open.clone().then(newlines.clone()),
                        newlines.clone().then(bracket_curly_close.clone()),
                    ),
            )
            .map(|arms| Expression::When { arms });

        let while_ = just(Token::While)
            .ignore_then(
                arm
                    .separated_by(comma.clone().ignored().or(newlines.clone()))
                    .collect()
                    .delimited_by(
                        bracket_curly_open.clone().then(newlines.clone()),
                        newlines.clone().then(bracket_curly_close.clone()),
                    ),
            )
            .map(|arms| Expression::While { arms });

        let skip = select! { Token::Skip => Expression::Skip };

        // TEXT { content with {var} interpolation }
        let text_literal = select! { Token::TextContent(content) => content }
            .map(|content: &str| {
                let mut parts = Vec::new();
                let mut current_text_start = 0;
                let mut chars = content.char_indices().peekable();

                while let Some((i, c)) = chars.next() {
                    if c == '{' {
                        // Save any text before this interpolation
                        if i > current_text_start {
                            parts.push(TextPart::Text(&content[current_text_start..i]));
                        }

                        // Find the closing brace
                        let var_start = i + 1;
                        let mut var_end = var_start;
                        while let Some((j, c2)) = chars.next() {
                            if c2 == '}' {
                                var_end = j;
                                break;
                            }
                        }

                        let var_name = content[var_start..var_end].trim();
                        if !var_name.is_empty() {
                            parts.push(TextPart::Interpolation { var: var_name });
                        }

                        current_text_start = var_end + 1;
                    }
                }

                // Add any remaining text after the last interpolation
                if current_text_start < content.len() {
                    parts.push(TextPart::Text(&content[current_text_start..]));
                }

                Expression::TextLiteral { parts }
            });

        // BLOCK { var: value, var2: value2, output_expression }
        // Variables are bindings, the last expression without colon is the output
        let variable_for_block = group((snake_case_identifier, colon.clone(), expression.clone())).map(|(name, _, value)| {
            Variable {
                name,
                is_referenced: false,
                value,
            }
        });
        let block_variable = variable_for_block
            .map_with(|variable, extra| Spanned {
                node: variable,
                span: extra.span(),
                persistence: None,
            });

        let block = just(Token::Block)
            .ignore_then(
                block_variable
                    .separated_by(comma.clone().ignored().or(newlines.clone()))
                    .collect::<Vec<_>>()
                    .then(expression.clone())
                    .delimited_by(
                        bracket_curly_open.clone().then(newlines.clone()),
                        newlines.clone().then(bracket_curly_close.clone()),
                    ),
            )
            .map(|(variables, output)| Expression::Block {
                variables,
                output: Box::new(output),
            });

        // @TODO PASS, a part of function calls?
        // @TODO when, while
        // @TODO comparator + arithmetic operator (in pratt, update pipe binding power accordingly)
        // @TODO text interpolation with {}, what about escaping {} and ''?
        // @TODO parse todo_mvc.bn

        let nested = bracket_round_open
            .ignore_then(expression)
            .then_ignore(bracket_round_close);

        let expression = choice((
            expression_variable,
            function_call,
            list,
            expression_object,
            tagged_object,
            map,
            expression_literal,
            text_literal,
            function,
            expression_alias,
            link_setter,
            link_expression,
            latest,
            then,
            when,
            while_,
            skip,
            block,
        ));

        expression
            .map_with(|expression, extra| Spanned {
                node: expression,
                span: extra.span(),
                persistence: None,
            })
            .or(nested)
            .padded_by(newlines)
            .pratt((
                // Precedence 1 (lowest): Pipe
                infix(left(1), just(Token::Pipe), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Pipe {
                            from: Box::new(l),
                            to: Box::new(r),
                        },
                        persistence: None,
                    }
                }),
                // Precedence 3: Comparison operators
                infix(left(3), just(Token::Equal), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Comparator(Comparator::Equal {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                infix(left(3), just(Token::NotEqual), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Comparator(Comparator::NotEqual {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                infix(left(3), just(Token::Greater), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Comparator(Comparator::Greater {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                infix(left(3), just(Token::GreaterOrEqual), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Comparator(Comparator::GreaterOrEqual {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                infix(left(3), just(Token::Less), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Comparator(Comparator::Less {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                infix(left(3), just(Token::LessOrEqual), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::Comparator(Comparator::LessOrEqual {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                // Precedence 5: Additive operators
                infix(left(5), just(Token::Plus), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::ArithmeticOperator(ArithmeticOperator::Add {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                infix(left(5), just(Token::Minus), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::ArithmeticOperator(ArithmeticOperator::Subtract {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                // Precedence 7: Multiplicative operators
                infix(left(7), just(Token::Asterisk), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::ArithmeticOperator(ArithmeticOperator::Multiply {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
                // Note: Token::Slash is also used for function paths (Module/function)
                // This may cause ambiguity - division should only work between expressions
                infix(left(7), just(Token::Slash), |l, _, r, extra| {
                    Spanned {
                        span: extra.span(),
                        node: Expression::ArithmeticOperator(ArithmeticOperator::Divide {
                            operand_a: Box::new(l),
                            operand_b: Box::new(r),
                        }),
                        persistence: None,
                    }
                }),
            ))
    })
    .repeated()
    .collect()
    .padded_by(newlines)
}

// @TODO not everything is expression, FUNCTIONs can be defined only in the root, etc.
#[derive(Debug, Clone)]
pub enum Expression<'code> {
    Variable(Box<Variable<'code>>),
    Literal(Literal<'code>),
    List {
        items: Vec<Spanned<Self>>,
    },
    Object(Object<'code>),
    TaggedObject {
        tag: &'code str,
        object: Object<'code>,
    },
    Map {
        entries: Vec<MapEntry<'code>>,
    },
    Function {
        name: &'code str,
        parameters: Vec<Spanned<&'code str>>,
        body: Box<Spanned<Self>>,
    },
    FunctionCall {
        path: Vec<&'code str>,
        arguments: Vec<Spanned<Argument<'code>>>,
    },
    Alias(Alias<'code>),
    LinkSetter {
        alias: Spanned<Alias<'code>>,
    },
    Link,
    Latest {
        inputs: Vec<Spanned<Self>>,
    },
    Then {
        body: Box<Spanned<Self>>,
    },
    When {
        arms: Vec<Arm<'code>>,
    },
    While {
        arms: Vec<Arm<'code>>,
    },
    Pipe {
        from: Box<Spanned<Self>>,
        to: Box<Spanned<Self>>,
    },
    Skip,
    Block {
        variables: Vec<Spanned<Variable<'code>>>,
        output: Box<Spanned<Self>>,
    },
    Comparator(Comparator<'code>),
    ArithmeticOperator(ArithmeticOperator<'code>),
    // TEXT { content with {var} interpolation }
    TextLiteral {
        parts: Vec<TextPart<'code>>,
    },
}

#[derive(Debug, Clone)]
pub enum Comparator<'code> {
    Equal {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    NotEqual {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    Greater {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    GreaterOrEqual {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    Less {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    LessOrEqual {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
}

#[derive(Debug, Clone)]
pub enum ArithmeticOperator<'code> {
    Negate {
        operand: Box<Spanned<Expression<'code>>>,
    },
    Add {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    Subtract {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    Multiply {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
    Divide {
        operand_a: Box<Spanned<Expression<'code>>>,
        operand_b: Box<Spanned<Expression<'code>>>,
    },
}

#[derive(Debug, Clone)]
pub enum TextPart<'code> {
    // Plain text content
    Text(&'code str),
    // Interpolated variable: {var_name}
    Interpolation { var: &'code str },
}

#[derive(Debug, Clone)]
pub struct Object<'code> {
    pub variables: Vec<Spanned<Variable<'code>>>,
}

#[derive(Debug, Clone)]
pub struct Variable<'code> {
    pub name: &'code str,
    pub is_referenced: bool,
    pub value: Spanned<Expression<'code>>,
}

#[derive(Debug, Clone)]
pub enum Literal<'code> {
    Number(f64),
    Text(&'code str),
    Tag(&'code str),
}

#[derive(Debug, Clone)]
pub struct MapEntry<'code> {
    pub key: Spanned<MapEntryKey<'code>>,
    pub value: Spanned<Expression<'code>>,
}

#[derive(Debug, Clone)]
pub enum MapEntryKey<'code> {
    Literal(Literal<'code>),
    Alias(Alias<'code>),
}

#[derive(Debug, Clone)]
pub struct Argument<'code> {
    pub name: &'code str,
    pub is_referenced: bool,
    pub value: Option<Spanned<Expression<'code>>>,
}

#[derive(Debug, Clone)]
pub enum Alias<'code> {
    WithoutPassed {
        parts: Vec<&'code str>,
        referenceables: Option<Referenceables<'code>>,
    },
    WithPassed {
        extra_parts: Vec<&'code str>,
    },
}

impl<'code> fmt::Display for Alias<'code> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WithPassed { extra_parts } => {
                let passed = Token::Passed;
                if extra_parts.is_empty() {
                    write!(f, "{passed}")
                } else {
                    write!(f, "{passed}.{}", extra_parts.join("."))
                }
            }
            Self::WithoutPassed {
                parts,
                referenceables: _,
            } => {
                write!(f, "{}", parts.join("."))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Arm<'code> {
    pub pattern: Pattern<'code>,
    pub body: Expression<'code>,
}

#[derive(Debug, Clone)]
pub enum Pattern<'code> {
    Literal(Literal<'code>),
    List {
        items: Vec<Pattern<'code>>,
    },
    Object {
        variables: Vec<PatternVariable<'code>>,
    },
    TaggedObject {
        tag: &'code str,
        variables: Vec<PatternVariable<'code>>,
    },
    Map {
        entries: Vec<PatternMapEntry<'code>>,
    },
    Alias {
        name: &'code str,
    },
    WildCard,
}

#[derive(Debug, Clone)]
pub struct PatternVariable<'code> {
    pub name: &'code str,
    pub value: Option<Pattern<'code>>,
}

#[derive(Debug, Clone)]
pub struct PatternMapEntry<'code> {
    pub key: Pattern<'code>,
    pub value: Option<Pattern<'code>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chumsky::prelude::Parser;

    macro_rules! parse_and_test {
        ($code:expr, $test:expr) => {{
            let tokens = lexer().parse($code).unwrap();
            let input = tokens.map(
                Span::splat($code.len()),
                |Spanned { node, span, persistence: _ }| (node, span),
            );
            let expressions = parser().parse(input).unwrap();
            let expr = &expressions.into_iter().next().unwrap().node;
            $test(expr)
        }};
    }

    #[test]
    fn test_text_literal_simple() {
        parse_and_test!("TEXT { hello world }", |expr: &Expression| {
            if let Expression::TextLiteral { parts } = expr {
                assert_eq!(parts.len(), 1);
                assert!(matches!(parts[0], TextPart::Text("hello world")));
            } else {
                panic!("Expected TextLiteral, got {:?}", expr);
            }
        });
    }

    #[test]
    fn test_text_literal_with_interpolation() {
        parse_and_test!("TEXT { hello {name} }", |expr: &Expression| {
            if let Expression::TextLiteral { parts } = expr {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[0], TextPart::Text("hello ")));
                assert!(matches!(parts[1], TextPart::Interpolation { var: "name" }));
            } else {
                panic!("Expected TextLiteral, got {:?}", expr);
            }
        });
    }

    #[test]
    fn test_text_literal_multiple_interpolations() {
        parse_and_test!("TEXT { Hello {name}! You have {count} messages. }", |expr: &Expression| {
            if let Expression::TextLiteral { parts } = expr {
                assert_eq!(parts.len(), 5);
                assert!(matches!(parts[0], TextPart::Text("Hello ")));
                assert!(matches!(parts[1], TextPart::Interpolation { var: "name" }));
                assert!(matches!(parts[2], TextPart::Text("! You have ")));
                assert!(matches!(parts[3], TextPart::Interpolation { var: "count" }));
                assert!(matches!(parts[4], TextPart::Text(" messages.")));
            } else {
                panic!("Expected TextLiteral, got {:?}", expr);
            }
        });
    }

    #[test]
    fn test_when_statement() {
        parse_and_test!("WHEN { True => 1, False => 0 }", |expr: &Expression| {
            if let Expression::When { arms } = expr {
                assert_eq!(arms.len(), 2);
            } else {
                panic!("Expected When, got {:?}", expr);
            }
        });
    }

    #[test]
    fn test_while_statement() {
        parse_and_test!("WHILE { __ => value }", |expr: &Expression| {
            if let Expression::While { arms } = expr {
                assert_eq!(arms.len(), 1);
            } else {
                panic!("Expected While, got {:?}", expr);
            }
        });
    }

    #[test]
    fn test_block_statement() {
        // BLOCK uses newlines between variables, output is the last expression (no colon)
        parse_and_test!("BLOCK {\nx: 1\ny: 2\nx\n}", |expr: &Expression| {
            if let Expression::Block { variables, output } = expr {
                assert_eq!(variables.len(), 2);
                assert!(matches!(output.node, Expression::Alias(_)));
            } else {
                panic!("Expected Block, got {:?}", expr);
            }
        });
    }

    #[test]
    fn test_comparison_equal() {
        parse_and_test!("1 == 2", |expr: &Expression| {
            assert!(matches!(expr, Expression::Comparator(Comparator::Equal { .. })));
        });
    }

    #[test]
    fn test_comparison_not_equal() {
        parse_and_test!("1 =/= 2", |expr: &Expression| {
            assert!(matches!(expr, Expression::Comparator(Comparator::NotEqual { .. })));
        });
    }

    #[test]
    fn test_comparison_less() {
        parse_and_test!("1 < 2", |expr: &Expression| {
            assert!(matches!(expr, Expression::Comparator(Comparator::Less { .. })));
        });
    }

    #[test]
    fn test_comparison_greater() {
        parse_and_test!("1 > 2", |expr: &Expression| {
            assert!(matches!(expr, Expression::Comparator(Comparator::Greater { .. })));
        });
    }

    #[test]
    fn test_arithmetic_add() {
        parse_and_test!("1 + 2", |expr: &Expression| {
            assert!(matches!(expr, Expression::ArithmeticOperator(ArithmeticOperator::Add { .. })));
        });
    }

    #[test]
    fn test_arithmetic_subtract() {
        parse_and_test!("3 - 1", |expr: &Expression| {
            assert!(matches!(expr, Expression::ArithmeticOperator(ArithmeticOperator::Subtract { .. })));
        });
    }

    #[test]
    fn test_arithmetic_multiply() {
        parse_and_test!("2 * 3", |expr: &Expression| {
            assert!(matches!(expr, Expression::ArithmeticOperator(ArithmeticOperator::Multiply { .. })));
        });
    }
}
