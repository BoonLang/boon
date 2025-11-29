//! Static expression types using StrSlice for zero-copy string references.
//!
//! These types are the "final" AST representation used by the evaluator.
//! They differ from the parser's `Expression<'code>` in that they use
//! `StrSlice` instead of `&'code str`, making them:
//!
//! - `'static` (no lifetime parameter)
//! - `Clone` (cheap - just Arc increment + offset copy)
//! - `Send + Sync` (can be used with WebWorkers)
//!
//! The conversion from `Expression<'code>` to `StaticExpression` happens
//! once after parsing. Since `StrSlice` stores offsets into the source
//! (which is in `Arc<String>`), there are zero string allocations during
//! conversion.

use super::source::{SourceCode, StrSlice};
use super::{Expression, Persistence, Span, Spanned};
use crate::parser;

/// Spanned wrapper for static expressions.
#[derive(Debug, Clone)]
pub struct StaticSpanned<T> {
    pub span: Span,
    pub persistence: Option<Persistence>,
    pub node: T,
}

/// Static expression type - all strings are StrSlice (zero-copy).
#[derive(Debug, Clone)]
pub enum StaticExpression {
    Variable(Box<StaticVariable>),
    Literal(StaticLiteral),
    List {
        items: Vec<StaticSpanned<Self>>,
    },
    Object(StaticObject),
    TaggedObject {
        tag: StrSlice,
        object: StaticObject,
    },
    Map {
        entries: Vec<StaticMapEntry>,
    },
    Function {
        name: StrSlice,
        parameters: Vec<StaticSpanned<StrSlice>>,
        body: Box<StaticSpanned<Self>>,
    },
    FunctionCall {
        path: Vec<StrSlice>,
        arguments: Vec<StaticSpanned<StaticArgument>>,
    },
    Alias(StaticAlias),
    LinkSetter {
        alias: StaticSpanned<StaticAlias>,
    },
    Link,
    Latest {
        inputs: Vec<StaticSpanned<Self>>,
    },
    Then {
        body: Box<StaticSpanned<Self>>,
    },
    When {
        arms: Vec<StaticArm>,
    },
    While {
        arms: Vec<StaticArm>,
    },
    Pipe {
        from: Box<StaticSpanned<Self>>,
        to: Box<StaticSpanned<Self>>,
    },
    Skip,
    Block {
        variables: Vec<StaticSpanned<StaticVariable>>,
        output: Box<StaticSpanned<Self>>,
    },
    Comparator(StaticComparator),
    ArithmeticOperator(StaticArithmeticOperator),
    TextLiteral {
        parts: Vec<StaticTextPart>,
    },
}

#[derive(Debug, Clone)]
pub enum StaticComparator {
    Equal {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    NotEqual {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    Greater {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    GreaterOrEqual {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    Less {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    LessOrEqual {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
}

#[derive(Debug, Clone)]
pub enum StaticArithmeticOperator {
    Negate {
        operand: Box<StaticSpanned<StaticExpression>>,
    },
    Add {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    Subtract {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    Multiply {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
    Divide {
        operand_a: Box<StaticSpanned<StaticExpression>>,
        operand_b: Box<StaticSpanned<StaticExpression>>,
    },
}

#[derive(Debug, Clone)]
pub enum StaticTextPart {
    Text(StrSlice),
    Interpolation { var: StrSlice },
}

#[derive(Debug, Clone)]
pub struct StaticObject {
    pub variables: Vec<StaticSpanned<StaticVariable>>,
}

#[derive(Debug, Clone)]
pub struct StaticVariable {
    pub name: StrSlice,
    pub is_referenced: bool,
    pub value: StaticSpanned<StaticExpression>,
}

#[derive(Debug, Clone)]
pub enum StaticLiteral {
    Number(f64),
    Text(StrSlice),
    Tag(StrSlice),
}

#[derive(Debug, Clone)]
pub struct StaticMapEntry {
    pub key: StaticSpanned<StaticMapEntryKey>,
    pub value: StaticSpanned<StaticExpression>,
}

#[derive(Debug, Clone)]
pub enum StaticMapEntryKey {
    Literal(StaticLiteral),
    Alias(StaticAlias),
}

#[derive(Debug, Clone)]
pub struct StaticArgument {
    pub name: StrSlice,
    pub is_referenced: bool,
    pub value: Option<StaticSpanned<StaticExpression>>,
}

#[derive(Debug, Clone)]
pub enum StaticAlias {
    WithoutPassed {
        parts: Vec<StrSlice>,
    },
    WithPassed {
        extra_parts: Vec<StrSlice>,
    },
}

#[derive(Debug, Clone)]
pub struct StaticArm {
    pub pattern: StaticPattern,
    pub body: StaticExpression,
}

#[derive(Debug, Clone)]
pub enum StaticPattern {
    Literal(StaticLiteral),
    List {
        items: Vec<StaticPattern>,
    },
    Object {
        variables: Vec<StaticPatternVariable>,
    },
    TaggedObject {
        tag: StrSlice,
        variables: Vec<StaticPatternVariable>,
    },
    Map {
        entries: Vec<StaticPatternMapEntry>,
    },
    Alias {
        name: StrSlice,
    },
    WildCard,
}

#[derive(Debug, Clone)]
pub struct StaticPatternVariable {
    pub name: StrSlice,
    pub value: Option<StaticPattern>,
}

#[derive(Debug, Clone)]
pub struct StaticPatternMapEntry {
    pub key: StaticPattern,
    pub value: Option<StaticPattern>,
}

// ============================================================================
// Conversion from borrowed Expression<'code> to StaticExpression
// ============================================================================

/// Converter that holds the source code reference.
pub struct ExpressionConverter {
    source: SourceCode,
}

impl ExpressionConverter {
    /// Create a new converter for the given source code.
    pub fn new(source: SourceCode) -> Self {
        Self { source }
    }

    /// Convert a borrowed &str to StrSlice.
    fn str_to_slice(&self, s: &str) -> StrSlice {
        self.source.slice_from_str(s)
    }

    /// Convert Spanned<Expression> to StaticSpanned<StaticExpression>.
    pub fn convert_spanned(&self, spanned: &Spanned<Expression>) -> StaticSpanned<StaticExpression> {
        StaticSpanned {
            span: spanned.span,
            persistence: spanned.persistence.clone(),
            node: self.convert_expr(&spanned.node),
        }
    }

    /// Convert Expression to StaticExpression.
    pub fn convert_expr(&self, expr: &Expression) -> StaticExpression {
        match expr {
            Expression::Variable(var) => {
                StaticExpression::Variable(Box::new(self.convert_variable(var)))
            }
            Expression::Literal(lit) => StaticExpression::Literal(self.convert_literal(lit)),
            Expression::List { items } => StaticExpression::List {
                items: items.iter().map(|i| self.convert_spanned(i)).collect(),
            },
            Expression::Object(obj) => StaticExpression::Object(self.convert_object(obj)),
            Expression::TaggedObject { tag, object } => StaticExpression::TaggedObject {
                tag: self.str_to_slice(tag),
                object: self.convert_object(object),
            },
            Expression::Map { entries } => StaticExpression::Map {
                entries: entries.iter().map(|e| self.convert_map_entry(e)).collect(),
            },
            Expression::Function { name, parameters, body } => StaticExpression::Function {
                name: self.str_to_slice(name),
                parameters: parameters
                    .iter()
                    .map(|p| StaticSpanned {
                        span: p.span,
                        persistence: p.persistence.clone(),
                        node: self.str_to_slice(p.node),
                    })
                    .collect(),
                body: Box::new(self.convert_spanned(body)),
            },
            Expression::FunctionCall { path, arguments } => StaticExpression::FunctionCall {
                path: path.iter().map(|s| self.str_to_slice(s)).collect(),
                arguments: arguments
                    .iter()
                    .map(|a| StaticSpanned {
                        span: a.span,
                        persistence: a.persistence.clone(),
                        node: self.convert_argument(&a.node),
                    })
                    .collect(),
            },
            Expression::Alias(alias) => StaticExpression::Alias(self.convert_alias(alias)),
            Expression::LinkSetter { alias } => StaticExpression::LinkSetter {
                alias: StaticSpanned {
                    span: alias.span,
                    persistence: alias.persistence.clone(),
                    node: self.convert_alias(&alias.node),
                },
            },
            Expression::Link => StaticExpression::Link,
            Expression::Latest { inputs } => StaticExpression::Latest {
                inputs: inputs.iter().map(|i| self.convert_spanned(i)).collect(),
            },
            Expression::Then { body } => StaticExpression::Then {
                body: Box::new(self.convert_spanned(body)),
            },
            Expression::When { arms } => StaticExpression::When {
                arms: arms.iter().map(|a| self.convert_arm(a)).collect(),
            },
            Expression::While { arms } => StaticExpression::While {
                arms: arms.iter().map(|a| self.convert_arm(a)).collect(),
            },
            Expression::Pipe { from, to } => StaticExpression::Pipe {
                from: Box::new(self.convert_spanned(from)),
                to: Box::new(self.convert_spanned(to)),
            },
            Expression::Skip => StaticExpression::Skip,
            Expression::Block { variables, output } => StaticExpression::Block {
                variables: variables
                    .iter()
                    .map(|v| StaticSpanned {
                        span: v.span,
                        persistence: v.persistence.clone(),
                        node: self.convert_variable(&v.node),
                    })
                    .collect(),
                output: Box::new(self.convert_spanned(output)),
            },
            Expression::Comparator(cmp) => {
                StaticExpression::Comparator(self.convert_comparator(cmp))
            }
            Expression::ArithmeticOperator(op) => {
                StaticExpression::ArithmeticOperator(self.convert_arithmetic(op))
            }
            Expression::TextLiteral { parts } => StaticExpression::TextLiteral {
                parts: parts.iter().map(|p| self.convert_text_part(p)).collect(),
            },
        }
    }

    fn convert_variable(&self, var: &parser::Variable) -> StaticVariable {
        StaticVariable {
            name: self.str_to_slice(var.name),
            is_referenced: var.is_referenced,
            value: self.convert_spanned(&var.value),
        }
    }

    fn convert_literal(&self, lit: &parser::Literal) -> StaticLiteral {
        match lit {
            parser::Literal::Number(n) => StaticLiteral::Number(*n),
            parser::Literal::Text(s) => StaticLiteral::Text(self.str_to_slice(s)),
            parser::Literal::Tag(s) => StaticLiteral::Tag(self.str_to_slice(s)),
        }
    }

    fn convert_object(&self, obj: &parser::Object) -> StaticObject {
        StaticObject {
            variables: obj
                .variables
                .iter()
                .map(|v| StaticSpanned {
                    span: v.span,
                    persistence: v.persistence.clone(),
                    node: self.convert_variable(&v.node),
                })
                .collect(),
        }
    }

    fn convert_map_entry(&self, entry: &parser::MapEntry) -> StaticMapEntry {
        StaticMapEntry {
            key: StaticSpanned {
                span: entry.key.span,
                persistence: entry.key.persistence.clone(),
                node: self.convert_map_entry_key(&entry.key.node),
            },
            value: self.convert_spanned(&entry.value),
        }
    }

    fn convert_map_entry_key(&self, key: &parser::MapEntryKey) -> StaticMapEntryKey {
        match key {
            parser::MapEntryKey::Literal(lit) => {
                StaticMapEntryKey::Literal(self.convert_literal(lit))
            }
            parser::MapEntryKey::Alias(alias) => {
                StaticMapEntryKey::Alias(self.convert_alias(alias))
            }
        }
    }

    fn convert_argument(&self, arg: &parser::Argument) -> StaticArgument {
        StaticArgument {
            name: self.str_to_slice(arg.name),
            is_referenced: arg.is_referenced,
            value: arg.value.as_ref().map(|v| self.convert_spanned(v)),
        }
    }

    fn convert_alias(&self, alias: &parser::Alias) -> StaticAlias {
        match alias {
            parser::Alias::WithoutPassed { parts, .. } => StaticAlias::WithoutPassed {
                parts: parts.iter().map(|s| self.str_to_slice(s)).collect(),
            },
            parser::Alias::WithPassed { extra_parts } => StaticAlias::WithPassed {
                extra_parts: extra_parts.iter().map(|s| self.str_to_slice(s)).collect(),
            },
        }
    }

    fn convert_arm(&self, arm: &parser::Arm) -> StaticArm {
        StaticArm {
            pattern: self.convert_pattern(&arm.pattern),
            body: self.convert_expr(&arm.body),
        }
    }

    fn convert_pattern(&self, pattern: &parser::Pattern) -> StaticPattern {
        match pattern {
            parser::Pattern::Literal(lit) => StaticPattern::Literal(self.convert_literal(lit)),
            parser::Pattern::List { items } => StaticPattern::List {
                items: items.iter().map(|i| self.convert_pattern(i)).collect(),
            },
            parser::Pattern::Object { variables } => StaticPattern::Object {
                variables: variables.iter().map(|v| self.convert_pattern_variable(v)).collect(),
            },
            parser::Pattern::TaggedObject { tag, variables } => StaticPattern::TaggedObject {
                tag: self.str_to_slice(tag),
                variables: variables.iter().map(|v| self.convert_pattern_variable(v)).collect(),
            },
            parser::Pattern::Map { entries } => StaticPattern::Map {
                entries: entries.iter().map(|e| self.convert_pattern_map_entry(e)).collect(),
            },
            parser::Pattern::Alias { name } => StaticPattern::Alias {
                name: self.str_to_slice(name),
            },
            parser::Pattern::WildCard => StaticPattern::WildCard,
        }
    }

    fn convert_pattern_variable(&self, var: &parser::PatternVariable) -> StaticPatternVariable {
        StaticPatternVariable {
            name: self.str_to_slice(var.name),
            value: var.value.as_ref().map(|v| self.convert_pattern(v)),
        }
    }

    fn convert_pattern_map_entry(&self, entry: &parser::PatternMapEntry) -> StaticPatternMapEntry {
        StaticPatternMapEntry {
            key: self.convert_pattern(&entry.key),
            value: entry.value.as_ref().map(|v| self.convert_pattern(v)),
        }
    }

    fn convert_comparator(&self, cmp: &parser::Comparator) -> StaticComparator {
        match cmp {
            parser::Comparator::Equal { operand_a, operand_b } => StaticComparator::Equal {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::NotEqual { operand_a, operand_b } => StaticComparator::NotEqual {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::Greater { operand_a, operand_b } => StaticComparator::Greater {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::GreaterOrEqual { operand_a, operand_b } => StaticComparator::GreaterOrEqual {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::Less { operand_a, operand_b } => StaticComparator::Less {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::LessOrEqual { operand_a, operand_b } => StaticComparator::LessOrEqual {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
        }
    }

    fn convert_arithmetic(&self, op: &parser::ArithmeticOperator) -> StaticArithmeticOperator {
        match op {
            parser::ArithmeticOperator::Negate { operand } => StaticArithmeticOperator::Negate {
                operand: Box::new(self.convert_spanned(operand)),
            },
            parser::ArithmeticOperator::Add { operand_a, operand_b } => StaticArithmeticOperator::Add {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::ArithmeticOperator::Subtract { operand_a, operand_b } => StaticArithmeticOperator::Subtract {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::ArithmeticOperator::Multiply { operand_a, operand_b } => StaticArithmeticOperator::Multiply {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::ArithmeticOperator::Divide { operand_a, operand_b } => StaticArithmeticOperator::Divide {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
        }
    }

    fn convert_text_part(&self, part: &parser::TextPart) -> StaticTextPart {
        match part {
            parser::TextPart::Text(s) => StaticTextPart::Text(self.str_to_slice(s)),
            parser::TextPart::Interpolation { var } => StaticTextPart::Interpolation {
                var: self.str_to_slice(var),
            },
        }
    }
}

/// Convenience function to convert parsed expressions to static expressions.
pub fn to_static(
    source: SourceCode,
    expressions: Vec<Spanned<Expression>>,
) -> Vec<StaticSpanned<StaticExpression>> {
    let converter = ExpressionConverter::new(source);
    expressions.iter().map(|e| converter.convert_spanned(e)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_expression_is_static() {
        fn takes_static<T: 'static>(_: T) {}

        // This test just verifies the types compile with 'static
        let source = SourceCode::new("test: 42".to_string());
        let slice = source.slice(0, 4);
        let expr = StaticExpression::Literal(StaticLiteral::Text(slice));
        takes_static(expr);
    }

    #[test]
    fn test_static_expression_is_send_sync() {
        fn is_send<T: Send>() {}
        fn is_sync<T: Sync>() {}
        is_send::<StaticExpression>();
        is_sync::<StaticExpression>();
    }
}
