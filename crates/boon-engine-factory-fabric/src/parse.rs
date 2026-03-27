use boon::parser::{
    Input as _, Parser as _, SourceCode, Token, lexer, parser, reset_expression_depth,
    resolve_references, span_at, static_expression,
};
use std::collections::BTreeMap;

pub type StaticExpression = static_expression::Expression;
pub type StaticSpannedExpression = static_expression::Spanned<StaticExpression>;

pub fn parse_static_expressions(source: &str) -> Result<Vec<StaticSpannedExpression>, String> {
    let source_code = SourceCode::new(source.to_string());
    let parse_source = source_code.clone();
    let source = parse_source.as_str();
    let (tokens, lex_errors) = lexer().parse(source).into_output_errors();
    if let Some(error) = lex_errors.into_iter().next() {
        return Err(format!("lex error: {error}"));
    }
    let mut tokens = tokens.ok_or_else(|| "lex error: no tokens produced".to_string())?;
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    reset_expression_depth();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source.len()),
            |boon::parser::Spanned {
                 node,
                 span,
                 persistence: _,
             }| (node, span),
        ))
        .into_output_errors();
    if let Some(error) = parse_errors.into_iter().next() {
        return Err(format!("parse error: {error}"));
    }
    let ast = ast.ok_or_else(|| "parse error: no AST produced".to_string())?;
    let ast = resolve_references(ast).map_err(|errors| {
        errors.into_iter().next().map_or_else(
            || "reference error".to_string(),
            |error| format!("reference error: {error}"),
        )
    })?;

    Ok(static_expression::convert_expressions(source_code, ast))
}

pub fn top_level_bindings<'a>(
    expressions: &'a [StaticSpannedExpression],
) -> BTreeMap<String, &'a StaticSpannedExpression> {
    expressions
        .iter()
        .filter_map(|expression| match &expression.node {
            StaticExpression::Variable(variable) => {
                Some((variable.name.as_str().to_string(), &variable.value))
            }
            _ => None,
        })
        .collect()
}
