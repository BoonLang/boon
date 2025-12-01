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
//! The conversion from `Expression<'code>` to `Expression` happens
//! once after parsing. Since `StrSlice` stores offsets into the source
//! (which is in `Arc<String>`), there are zero string allocations during
//! conversion.

use super::source::{SourceCode, StrSlice};
use super::{Persistence, Span};
use crate::parser;

/// Spanned wrapper for static expressions.
#[derive(Debug, Clone)]
pub struct Spanned<T> {
    pub span: Span,
    pub persistence: Option<Persistence>,
    pub node: T,
}

/// Static expression type - all strings are StrSlice (zero-copy).
#[derive(Debug, Clone)]
pub enum Expression {
    Variable(Box<Variable>),
    Literal(Literal),
    List {
        items: Vec<Spanned<Self>>,
    },
    Object(Object),
    TaggedObject {
        tag: StrSlice,
        object: Object,
    },
    Map {
        entries: Vec<MapEntry>,
    },
    Function {
        name: StrSlice,
        parameters: Vec<Spanned<StrSlice>>,
        body: Box<Spanned<Self>>,
    },
    FunctionCall {
        path: Vec<StrSlice>,
        arguments: Vec<Spanned<Argument>>,
    },
    Alias(Alias),
    LinkSetter {
        alias: Spanned<Alias>,
    },
    Link,
    Latest {
        inputs: Vec<Spanned<Self>>,
    },
    // HOLD - stateful accumulator: `initial |> HOLD state_param { body }`
    Hold {
        state_param: StrSlice,
        body: Box<Spanned<Self>>,
    },
    Then {
        body: Box<Spanned<Self>>,
    },
    // FLUSH for fail-fast error handling
    Flush {
        value: Box<Spanned<Self>>,
    },
    // PULSES for iteration
    Pulses {
        count: Box<Spanned<Self>>,
    },
    // Spread operator: ...expression (in objects)
    Spread {
        value: Box<Spanned<Self>>,
    },
    When {
        arms: Vec<Arm>,
    },
    While {
        arms: Vec<Arm>,
    },
    Pipe {
        from: Box<Spanned<Self>>,
        to: Box<Spanned<Self>>,
    },
    Skip,
    Block {
        variables: Vec<Spanned<Variable>>,
        output: Box<Spanned<Self>>,
    },
    Comparator(Comparator),
    ArithmeticOperator(ArithmeticOperator),
    TextLiteral {
        parts: Vec<TextPart>,
    },
    // Hardware types (parse-only for now)
    Bits {
        size: Box<Spanned<Self>>,
    },
    Memory {
        address: Box<Spanned<Self>>,
    },
    Bytes {
        data: Vec<Spanned<Self>>,
    },
}

#[derive(Debug, Clone)]
pub enum Comparator {
    Equal {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    NotEqual {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    Greater {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    GreaterOrEqual {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    Less {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    LessOrEqual {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
}

#[derive(Debug, Clone)]
pub enum ArithmeticOperator {
    Negate {
        operand: Box<Spanned<Expression>>,
    },
    Add {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    Subtract {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    Multiply {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
    Divide {
        operand_a: Box<Spanned<Expression>>,
        operand_b: Box<Spanned<Expression>>,
    },
}

#[derive(Debug, Clone)]
pub enum TextPart {
    Text(StrSlice),
    Interpolation { var: StrSlice },
}

#[derive(Debug, Clone)]
pub struct Object {
    pub variables: Vec<Spanned<Variable>>,
}

#[derive(Debug, Clone)]
pub struct Variable {
    pub name: StrSlice,
    pub is_referenced: bool,
    pub value: Spanned<Expression>,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Number(f64),
    Tag(StrSlice),
}

#[derive(Debug, Clone)]
pub struct MapEntry {
    pub key: Spanned<MapEntryKey>,
    pub value: Spanned<Expression>,
}

#[derive(Debug, Clone)]
pub enum MapEntryKey {
    Literal(Literal),
    Alias(Alias),
}

#[derive(Debug, Clone)]
pub struct Argument {
    pub name: StrSlice,
    pub is_referenced: bool,
    pub value: Option<Spanned<Expression>>,
}

#[derive(Debug, Clone)]
pub enum Alias {
    WithoutPassed {
        parts: Vec<StrSlice>,
        /// The span of the referenced variable (from scope resolution).
        /// Used to look up variables via reference_connector.referenceable(span).
        /// None if no reference was resolved (e.g., for function parameters bound at runtime).
        referenced_span: Option<Span>,
    },
    WithPassed {
        extra_parts: Vec<StrSlice>,
    },
}

#[derive(Debug, Clone)]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Expression,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Literal(Literal),
    List {
        items: Vec<Pattern>,
    },
    Object {
        variables: Vec<PatternVariable>,
    },
    TaggedObject {
        tag: StrSlice,
        variables: Vec<PatternVariable>,
    },
    Map {
        entries: Vec<PatternMapEntry>,
    },
    Alias {
        name: StrSlice,
    },
    WildCard,
}

#[derive(Debug, Clone)]
pub struct PatternVariable {
    pub name: StrSlice,
    pub value: Option<Pattern>,
}

#[derive(Debug, Clone)]
pub struct PatternMapEntry {
    pub key: Pattern,
    pub value: Option<Pattern>,
}

// ============================================================================
// Conversion from borrowed Expression<'code> to Expression
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

    /// Convert parser::Spanned<parser::Expression> to Spanned<Expression>.
    pub fn convert_spanned(&self, spanned: &parser::Spanned<parser::Expression>) -> Spanned<Expression> {
        Spanned {
            span: spanned.span,
            persistence: spanned.persistence.clone(),
            node: self.convert_expr(&spanned.node),
        }
    }

    /// Convert parser::Expression to Expression.
    pub fn convert_expr(&self, expr: &parser::Expression) -> Expression {
        match expr {
            parser::Expression::Variable(var) => {
                Expression::Variable(Box::new(self.convert_variable(var)))
            }
            parser::Expression::Literal(lit) => Expression::Literal(self.convert_literal(lit)),
            parser::Expression::List { items } => Expression::List {
                items: items.iter().map(|i| self.convert_spanned(i)).collect(),
            },
            parser::Expression::Object(obj) => Expression::Object(self.convert_object(obj)),
            parser::Expression::TaggedObject { tag, object } => Expression::TaggedObject {
                tag: self.str_to_slice(tag),
                object: self.convert_object(object),
            },
            parser::Expression::Map { entries } => Expression::Map {
                entries: entries.iter().map(|e| self.convert_map_entry(e)).collect(),
            },
            parser::Expression::Function { name, parameters, body } => Expression::Function {
                name: self.str_to_slice(name),
                parameters: parameters
                    .iter()
                    .map(|p| Spanned {
                        span: p.span,
                        persistence: p.persistence.clone(),
                        node: self.str_to_slice(p.node),
                    })
                    .collect(),
                body: Box::new(self.convert_spanned(body)),
            },
            parser::Expression::FunctionCall { path, arguments } => Expression::FunctionCall {
                path: path.iter().map(|s| self.str_to_slice(s)).collect(),
                arguments: arguments
                    .iter()
                    .map(|a| Spanned {
                        span: a.span,
                        persistence: a.persistence.clone(),
                        node: self.convert_argument(&a.node),
                    })
                    .collect(),
            },
            parser::Expression::Alias(alias) => Expression::Alias(self.convert_alias(alias)),
            parser::Expression::LinkSetter { alias } => Expression::LinkSetter {
                alias: Spanned {
                    span: alias.span,
                    persistence: alias.persistence.clone(),
                    node: self.convert_alias(&alias.node),
                },
            },
            parser::Expression::Link => Expression::Link,
            parser::Expression::Latest { inputs } => Expression::Latest {
                inputs: inputs.iter().map(|i| self.convert_spanned(i)).collect(),
            },
            parser::Expression::Hold { state_param, body } => Expression::Hold {
                state_param: self.str_to_slice(state_param),
                body: Box::new(self.convert_spanned(body)),
            },
            parser::Expression::Then { body } => Expression::Then {
                body: Box::new(self.convert_spanned(body)),
            },
            parser::Expression::Flush { value } => Expression::Flush {
                value: Box::new(self.convert_spanned(value)),
            },
            parser::Expression::Pulses { count } => Expression::Pulses {
                count: Box::new(self.convert_spanned(count)),
            },
            parser::Expression::Spread { value } => Expression::Spread {
                value: Box::new(self.convert_spanned(value)),
            },
            parser::Expression::When { arms } => Expression::When {
                arms: arms.iter().map(|a| self.convert_arm(a)).collect(),
            },
            parser::Expression::While { arms } => Expression::While {
                arms: arms.iter().map(|a| self.convert_arm(a)).collect(),
            },
            parser::Expression::Pipe { from, to } => Expression::Pipe {
                from: Box::new(self.convert_spanned(from)),
                to: Box::new(self.convert_spanned(to)),
            },
            parser::Expression::Skip => Expression::Skip,
            parser::Expression::Block { variables, output } => Expression::Block {
                variables: variables
                    .iter()
                    .map(|v| Spanned {
                        span: v.span,
                        persistence: v.persistence.clone(),
                        node: self.convert_variable(&v.node),
                    })
                    .collect(),
                output: Box::new(self.convert_spanned(output)),
            },
            parser::Expression::Comparator(cmp) => {
                Expression::Comparator(self.convert_comparator(cmp))
            }
            parser::Expression::ArithmeticOperator(op) => {
                Expression::ArithmeticOperator(self.convert_arithmetic(op))
            }
            parser::Expression::TextLiteral { parts } => Expression::TextLiteral {
                parts: parts.iter().map(|p| self.convert_text_part(p)).collect(),
            },
            // Hardware types (parse-only for now)
            parser::Expression::Bits { size } => Expression::Bits {
                size: Box::new(self.convert_spanned(size)),
            },
            parser::Expression::Memory { address } => Expression::Memory {
                address: Box::new(self.convert_spanned(address)),
            },
            parser::Expression::Bytes { data } => Expression::Bytes {
                data: data.iter().map(|item| self.convert_spanned(item)).collect(),
            },
        }
    }

    fn convert_variable(&self, var: &parser::Variable) -> Variable {
        Variable {
            name: self.str_to_slice(var.name),
            is_referenced: var.is_referenced,
            value: self.convert_spanned(&var.value),
        }
    }

    fn convert_literal(&self, lit: &parser::Literal) -> Literal {
        match lit {
            parser::Literal::Number(n) => Literal::Number(*n),
            parser::Literal::Tag(s) => Literal::Tag(self.str_to_slice(s)),
        }
    }

    fn convert_object(&self, obj: &parser::Object) -> Object {
        Object {
            variables: obj
                .variables
                .iter()
                .map(|v| Spanned {
                    span: v.span,
                    persistence: v.persistence.clone(),
                    node: self.convert_variable(&v.node),
                })
                .collect(),
        }
    }

    fn convert_map_entry(&self, entry: &parser::MapEntry) -> MapEntry {
        MapEntry {
            key: Spanned {
                span: entry.key.span,
                persistence: entry.key.persistence.clone(),
                node: self.convert_map_entry_key(&entry.key.node),
            },
            value: self.convert_spanned(&entry.value),
        }
    }

    fn convert_map_entry_key(&self, key: &parser::MapEntryKey) -> MapEntryKey {
        match key {
            parser::MapEntryKey::Literal(lit) => {
                MapEntryKey::Literal(self.convert_literal(lit))
            }
            parser::MapEntryKey::Alias(alias) => {
                MapEntryKey::Alias(self.convert_alias(alias))
            }
        }
    }

    fn convert_argument(&self, arg: &parser::Argument) -> Argument {
        Argument {
            name: self.str_to_slice(arg.name),
            is_referenced: arg.is_referenced,
            value: arg.value.as_ref().map(|v| self.convert_spanned(v)),
        }
    }

    fn convert_alias(&self, alias: &parser::Alias) -> Alias {
        match alias {
            parser::Alias::WithoutPassed { parts, referenceables } => Alias::WithoutPassed {
                parts: parts.iter().map(|s| self.str_to_slice(s)).collect(),
                referenced_span: referenceables.as_ref().and_then(|r| r.referenced.map(|ref_| ref_.span)),
            },
            parser::Alias::WithPassed { extra_parts } => Alias::WithPassed {
                extra_parts: extra_parts.iter().map(|s| self.str_to_slice(s)).collect(),
            },
        }
    }

    fn convert_arm(&self, arm: &parser::Arm) -> Arm {
        Arm {
            pattern: self.convert_pattern(&arm.pattern),
            body: self.convert_expr(&arm.body),
        }
    }

    fn convert_pattern(&self, pattern: &parser::Pattern) -> Pattern {
        match pattern {
            parser::Pattern::Literal(lit) => Pattern::Literal(self.convert_literal(lit)),
            parser::Pattern::List { items } => Pattern::List {
                items: items.iter().map(|i| self.convert_pattern(i)).collect(),
            },
            parser::Pattern::Object { variables } => Pattern::Object {
                variables: variables.iter().map(|v| self.convert_pattern_variable(v)).collect(),
            },
            parser::Pattern::TaggedObject { tag, variables } => Pattern::TaggedObject {
                tag: self.str_to_slice(tag),
                variables: variables.iter().map(|v| self.convert_pattern_variable(v)).collect(),
            },
            parser::Pattern::Map { entries } => Pattern::Map {
                entries: entries.iter().map(|e| self.convert_pattern_map_entry(e)).collect(),
            },
            parser::Pattern::Alias { name } => Pattern::Alias {
                name: self.str_to_slice(name),
            },
            parser::Pattern::WildCard => Pattern::WildCard,
        }
    }

    fn convert_pattern_variable(&self, var: &parser::PatternVariable) -> PatternVariable {
        PatternVariable {
            name: self.str_to_slice(var.name),
            value: var.value.as_ref().map(|v| self.convert_pattern(v)),
        }
    }

    fn convert_pattern_map_entry(&self, entry: &parser::PatternMapEntry) -> PatternMapEntry {
        PatternMapEntry {
            key: self.convert_pattern(&entry.key),
            value: entry.value.as_ref().map(|v| self.convert_pattern(v)),
        }
    }

    fn convert_comparator(&self, cmp: &parser::Comparator) -> Comparator {
        match cmp {
            parser::Comparator::Equal { operand_a, operand_b } => Comparator::Equal {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::NotEqual { operand_a, operand_b } => Comparator::NotEqual {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::Greater { operand_a, operand_b } => Comparator::Greater {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::GreaterOrEqual { operand_a, operand_b } => Comparator::GreaterOrEqual {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::Less { operand_a, operand_b } => Comparator::Less {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::Comparator::LessOrEqual { operand_a, operand_b } => Comparator::LessOrEqual {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
        }
    }

    fn convert_arithmetic(&self, op: &parser::ArithmeticOperator) -> ArithmeticOperator {
        match op {
            parser::ArithmeticOperator::Negate { operand } => ArithmeticOperator::Negate {
                operand: Box::new(self.convert_spanned(operand)),
            },
            parser::ArithmeticOperator::Add { operand_a, operand_b } => ArithmeticOperator::Add {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::ArithmeticOperator::Subtract { operand_a, operand_b } => ArithmeticOperator::Subtract {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::ArithmeticOperator::Multiply { operand_a, operand_b } => ArithmeticOperator::Multiply {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
            parser::ArithmeticOperator::Divide { operand_a, operand_b } => ArithmeticOperator::Divide {
                operand_a: Box::new(self.convert_spanned(operand_a)),
                operand_b: Box::new(self.convert_spanned(operand_b)),
            },
        }
    }

    fn convert_text_part(&self, part: &parser::TextPart) -> TextPart {
        match part {
            parser::TextPart::Text(s) => TextPart::Text(self.str_to_slice(s)),
            parser::TextPart::Interpolation { var } => TextPart::Interpolation {
                var: self.str_to_slice(var),
            },
        }
    }
}

/// Convenience function to convert parsed expressions to static expressions.
pub fn convert_expressions(
    source: SourceCode,
    expressions: Vec<parser::Spanned<parser::Expression>>,
) -> Vec<Spanned<Expression>> {
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
        let expr = Expression::Literal(Literal::Tag(slice));
        takes_static(expr);
    }

    #[test]
    fn test_static_expression_is_send_sync() {
        fn is_send<T: Send>() {}
        fn is_sync<T: Sync>() {}
        is_send::<Expression>();
        is_sync::<Expression>();
    }
}
