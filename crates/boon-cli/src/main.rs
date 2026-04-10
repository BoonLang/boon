//! Boon CLI - stub implementation.
//!
//! The original CLI depended on engine_v2/evaluator_v2 which don't exist.
//! This stub provides basic functionality until the engine is complete.

use boon::parser::SourceCode;
use boon::parser::{
    Input, Parser, Spanned, lexer, parser, reset_expression_depth, resolve_references, span_at,
};
use clap::{Parser as ClapParser, Subcommand};
use std::fs;
use std::path::PathBuf;

#[derive(ClapParser)]
#[command(name = "boon")]
#[command(about = "Boon language CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse and show AST info
    Parse {
        /// Path to Boon source file
        path: PathBuf,
    },
    /// Format Boon source
    Format {
        /// Path to Boon source file
        path: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Parse { path } => cmd_parse(&path),
        Commands::Format { path } => cmd_format(&path),
    }
}

fn cmd_parse(path: &PathBuf) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading file: {e}");
            std::process::exit(1);
        }
    };
    match parse_and_report(&source) {
        Ok(count) => println!("Parsed {count} top-level expression(s) successfully"),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_format(path: &PathBuf) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading file: {e}");
            std::process::exit(1);
        }
    };
    match parse_and_report(&source) {
        Ok(_) => print!("{}", source),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn parse_and_report(source: &str) -> Result<usize, String> {
    let source_code = SourceCode::new(source.to_string());
    let source_str: &str = source_code.as_str();
    let (tokens, lex_errors) = lexer().parse(source_str).into_output_errors();
    if let Some(error) = lex_errors.into_iter().next() {
        return Err(format!("lex error: {error}"));
    }
    let mut tokens = tokens.ok_or_else(|| "lex error: no tokens produced".to_string())?;
    tokens.retain(|spanned_token| !matches!(spanned_token.node, boon::parser::Token::Comment(_)));

    reset_expression_depth();
    let source_len = source.len();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source_len),
            |Spanned {
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
    Ok(ast.len())
}
