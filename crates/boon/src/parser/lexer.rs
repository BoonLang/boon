use super::{ParseError, Spanned};
use chumsky::prelude::*;
use std::borrow::Cow;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Token<'code> {
    BracketRoundOpen,
    BracketRoundClose,
    BracketCurlyOpen,
    BracketCurlyClose,
    BracketSquareOpen,
    BracketSquareClose,
    Comment(&'code str),
    // @TODO decimal?
    Number(f64),
    Pipe,
    Wildcard,
    Implies,
    Colon,
    Comma,
    Dot,
    Newline,
    NotEqual,
    GreaterOrEqual,
    Greater,
    LessOrEqual,
    Less,
    Equal,
    Minus,
    Plus,
    Asterisk,
    Slash,
    SnakeCaseIdentifier(&'code str),
    PascalCaseIdentifier(&'code str),
    List,
    Map,
    Function,
    Link,
    Latest,
    Hold,
    Then,
    When,
    While,
    Skip,
    Block,
    Pass,
    Passed,
    Flush,
    Spread,
    // TEXT literal content: TEXT { content with {var} interpolation }
    // or TEXT #{ content with #{var} interpolation } for hash escaping
    // The content is the raw string between TEXT [#*]{ and }
    // Second field is hash_count (0 for no hashes, 1 for TEXT #{, etc.)
    TextContent(&'code str, usize),
    // Hardware types (parse-only for now)
    Bits,
    Memory,
    Bytes,
}

impl<'code> Token<'code> {
    pub fn into_cow_str(self) -> Cow<'code, str> {
        match self {
            Self::BracketRoundOpen => "(".into(),
            Self::BracketRoundClose => ")".into(),
            Self::BracketCurlyOpen => "{".into(),
            Self::BracketCurlyClose => "}".into(),
            Self::BracketSquareOpen => "[".into(),
            Self::BracketSquareClose => "]".into(),
            Self::Comment(comment) => comment.into(),
            Self::Number(number) => number.to_string().into(),
            Self::Pipe => "|>".into(),
            Self::Wildcard => "__".into(),
            Self::Implies => "=>".into(),
            Self::Colon => ":".into(),
            Self::Comma => ",".into(),
            Self::Dot => ".".into(),
            Self::Newline => "\n".into(),
            Self::NotEqual => "=/=".into(),
            Self::GreaterOrEqual => ">=".into(),
            Self::Greater => ">".into(),
            Self::LessOrEqual => "<=".into(),
            Self::Less => "<".into(),
            Self::Equal => "==".into(),
            Self::Minus => "-".into(),
            Self::Plus => "+".into(),
            Self::Asterisk => "*".into(),
            Self::Slash => "/".into(),
            Self::SnakeCaseIdentifier(identifier) => identifier.into(),
            Self::PascalCaseIdentifier(identifier) => identifier.into(),
            Self::List => "LIST".into(),
            Self::Map => "MAP".into(),
            Self::Function => "FUNCTION".into(),
            Self::Link => "LINK".into(),
            Self::Latest => "LATEST".into(),
            Self::Hold => "HOLD".into(),
            Self::Then => "THEN".into(),
            Self::When => "WHEN".into(),
            Self::While => "WHILE".into(),
            Self::Skip => "SKIP".into(),
            Self::Block => "BLOCK".into(),
            Self::Pass => "PASS".into(),
            Self::Passed => "PASSED".into(),
            Self::Flush => "FLUSH".into(),
            Self::Spread => "...".into(),
            Self::TextContent(content, hash_count) => {
                let hashes = "#".repeat(hash_count);
                format!("TEXT {}{{ {} }}", hashes, content).into()
            }
            Self::Bits => "BITS".into(),
            Self::Memory => "MEMORY".into(),
            Self::Bytes => "BYTES".into(),
        }
    }
}

impl fmt::Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.into_cow_str())
    }
}

pub fn lexer<'code>()
-> impl Parser<'code, &'code str, Vec<Spanned<Token<'code>>>, extra::Err<ParseError<'code, char>>> {
    let bracket = choice((
        just('(').to(Token::BracketRoundOpen),
        just(')').to(Token::BracketRoundClose),
        just('{').to(Token::BracketCurlyOpen),
        just('}').to(Token::BracketCurlyClose),
        just('[').to(Token::BracketSquareOpen),
        just(']').to(Token::BracketSquareClose),
    ));

    let comparator = choice((
        just("=/=").to(Token::NotEqual),
        just(">=").to(Token::GreaterOrEqual),
        just('>').to(Token::Greater),
        just("<=").to(Token::LessOrEqual),
        just('<').to(Token::Less),
        just("==").to(Token::Equal),
    ));

    let arithmetic_operator_or_path_separator = choice((
        just('-').to(Token::Minus),
        just('+').to(Token::Plus),
        just('*').to(Token::Asterisk),
        just('/').to(Token::Slash),
    ));

    let comment = just("--")
        .ignore_then(
            any()
                .and_is(text::inline_whitespace().then(text::newline()).not())
                .repeated(),
        )
        .to_slice()
        .map(Token::Comment);

    // @TODO support number format like 1_000?
    let number = just('-')
        .repeated()
        .at_most(1)
        .then(text::int(10).then(just('.').then(text::digits(10)).or_not()))
        .to_slice()
        .from_str()
        .unwrapped()
        .map(Token::Number);

    // Allow identifiers starting with underscore (like _log) but not just "_" alone
    // Note: "__" is the Wildcard token, handled separately
    let snake_case_identifier = any()
        .filter(|character: &char| {
            *character == '_' || character.is_ascii_lowercase()
        })
        .then(
            any()
                .filter(|character: &char| {
                    *character == '_'
                        || character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                })
                .repeated()
                .at_least(
                    // If starts with '_', must have at least one more character
                    // to distinguish from potential wildcards
                    0  // Actually 0 for regular identifiers, _foo is fine
                ),
        )
        .to_slice()
        // Filter out single underscore (that would conflict with partial wildcard)
        // and double underscore (that's the wildcard token)
        .filter(|s: &&str| *s != "_" && *s != "__")
        .map(Token::SnakeCaseIdentifier);

    let pascal_case_identifier = any()
        .filter(char::is_ascii_uppercase)
        .then(any().filter(|character: &char| character.is_ascii_lowercase() || character.is_ascii_uppercase() || character.is_ascii_digit()).repeated())
        .to_slice()
        .try_map(|identifier: &str, span| {
            if identifier.len() == 1 || identifier.chars().rev().any(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            }) {
                Ok(Token::PascalCaseIdentifier(identifier))
            } else {
                Err(ParseError::custom(span, format!("PascalCase identifier has to contain at least one digit or lowercase character. Identifier: '{identifier}'")))
            }
        });

    let keyword = any()
        .filter(char::is_ascii_uppercase)
        .repeated()
        .at_least(2)
        .to_slice()
        .try_map(|keyword, span| match keyword {
            "LIST" => Ok(Token::List),
            "MAP" => Ok(Token::Map),
            "FUNCTION" => Ok(Token::Function),
            "LINK" => Ok(Token::Link),
            "LATEST" => Ok(Token::Latest),
            "HOLD" => Ok(Token::Hold),
            "THEN" => Ok(Token::Then),
            "WHEN" => Ok(Token::When),
            "WHILE" => Ok(Token::While),
            "SKIP" => Ok(Token::Skip),
            "BLOCK" => Ok(Token::Block),
            "PASS" => Ok(Token::Pass),
            "PASSED" => Ok(Token::Passed),
            "FLUSH" => Ok(Token::Flush),
            // Hardware types (parse-only for now)
            "BITS" => Ok(Token::Bits),
            "MEMORY" => Ok(Token::Memory),
            "BYTES" => Ok(Token::Bytes),
            // TEXT is handled specially below, not as a keyword
            _ => Err(ParseError::custom(
                span,
                format!("Unknown keyword '{keyword}'"),
            )),
        });

    // TEXT { content } or TEXT #{ content } - captures content between TEXT [#*]{ and }
    // Content can include {var} or #{var} for interpolation based on hash count
    // We track brace depth to find the matching closing }
    let text_content_inner = recursive(|text_content_inner| {
        choice((
            // Match balanced braces: { ... }
            just('{')
                .then(text_content_inner)
                .then(just('}'))
                .to_slice(),
            // Match any single char except braces
            none_of("{}").to_slice(),
        ))
        .repeated()
        .to_slice()
    });

    let text_content = just("TEXT")
        .then(text::inline_whitespace())
        .ignore_then(
            just('#').repeated().collect::<Vec<_>>()
        )
        .then_ignore(just('{'))
        .then(text_content_inner)
        .then_ignore(just('}'))
        .map(|(hashes, content): (Vec<_>, &str)| {
            Token::TextContent(content.trim(), hashes.len())
        });

    let token = choice((
        bracket,
        comment,
        number,
        just("|>").to(Token::Pipe),
        just("__").to(Token::Wildcard),
        just("=>").to(Token::Implies),
        just(':').to(Token::Colon),
        just(',').to(Token::Comma),
        just("...").to(Token::Spread),
        just('.').to(Token::Dot),
        text::newline().to(Token::Newline),
        comparator,
        arithmetic_operator_or_path_separator,
        text_content,
        snake_case_identifier,
        pascal_case_identifier,
        keyword,
    ));

    token
        .map_with(|token, extra| Spanned {
            node: token,
            span: extra.span(),
            persistence: None,
        })
        .padded_by(text::inline_whitespace())
        .recover_with(skip_then_retry_until(any().ignored(), end()))
        .repeated()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chumsky::prelude::Parser;

    #[test]
    fn test_text_content_simple() {
        let result = lexer().parse("TEXT { hello }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("hello", 0)]);
    }

    #[test]
    fn test_text_content_with_interpolation() {
        let result = lexer().parse("TEXT { hello {name} }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("hello {name}", 0)]);
    }

    #[test]
    fn test_text_content_multiple_interpolations() {
        let result = lexer().parse("TEXT { Hello {name}! You have {count} messages. }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("Hello {name}! You have {count} messages.", 0)]);
    }

    #[test]
    fn test_text_content_one_hash() {
        let result = lexer().parse("TEXT #{ function() { return #{value}; } }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("function() { return #{value}; }", 1)]);
    }

    #[test]
    fn test_text_content_two_hashes() {
        let result = lexer().parse("TEXT ##{ a[href^=\"#{url}\"] { color: ##{color}; } }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("a[href^=\"#{url}\"] { color: ##{color}; }", 2)]);
    }

    #[test]
    fn test_text_content_empty_with_hash() {
        let result = lexer().parse("TEXT #{}");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("", 1)]);
    }

    #[test]
    fn test_text_content_with_slash() {
        let result = lexer().parse("TEXT { /active }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![&Token::TextContent("/active", 0)]);
    }

    #[test]
    fn test_text_pattern_in_when() {
        let result = lexer().parse("WHEN { TEXT { /active } => Active }");
        let tokens: Vec<_> = result.output().unwrap().iter().map(|t| &t.node).collect();
        assert_eq!(tokens, vec![
            &Token::When,
            &Token::BracketCurlyOpen,
            &Token::TextContent("/active", 0),
            &Token::Implies,
            &Token::PascalCaseIdentifier("Active"),
            &Token::BracketCurlyClose,
        ]);
    }
}
