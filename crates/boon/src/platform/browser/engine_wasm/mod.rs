//! WASM compilation engine — compiles Boon source directly to WebAssembly bytecode.
//!
//! This module is gated behind the `engine-wasm` feature flag.
//! Pipeline: source → parse → IR lowering → WASM codegen → runtime instantiation → bridge UI

pub mod ir;
mod lower;
mod codegen;
pub mod runtime;
pub mod bridge;

use std::rc::Rc;

use zoon::*;

use crate::parser::{
    lexer, parser, reset_expression_depth, resolve_references, span_at,
    static_expression, SourceCode, Token,
};

/// Run the WASM engine: compile source → generate WASM → instantiate → build UI.
/// Returns a Zoon element tree.
pub fn run_wasm(source: &str) -> RawElOrText {
    match compile_and_run(source) {
        Ok(element) => element,
        Err(msg) => {
            El::new()
                .s(Font::new().color(color!("LightCoral")))
                .child(msg)
                .unify()
        }
    }
}

fn compile_and_run(source: &str) -> Result<RawElOrText, String> {
    // 1. Parse and lower to IR.
    let program = Rc::new(compile(source)?);

    // 2. Generate WASM binary.
    let wasm_output = codegen::emit_wasm(&program);

    // 3. Instantiate WASM module with host imports.
    let instance = runtime::WasmInstance::new(&wasm_output.wasm_bytes, program.clone(), wasm_output.text_patterns)
        .map_err(|e| format!("WASM instantiation failed: {}", e))?;

    // 4. Wrap in Rc early (needed for router setup).
    let instance = Rc::new(instance);

    // 5. Set up router BEFORE init so WHEN/WHILE arms see route text.
    bridge::setup_router(&program, &instance);

    // 6. Call init() to set initial cell values.
    instance.call_init()
        .map_err(|e| format!("init() failed: {}", e))?;

    // 7. Start timers.
    instance.start_timers(&program);

    // 8. Build Zoon element tree from IR + runtime.
    Ok(bridge::build_ui(&program, instance))
}

fn compile(source: &str) -> Result<ir::IrProgram, String> {
    let ast = parse_source(source)?;
    lower::lower(&ast).map_err(|errors| format_errors(source, &errors))
}

fn format_errors(source: &str, errors: &[lower::CompileError]) -> String {
    use ariadne::{Config, Label, Report, ReportKind, Source};
    use std::io::Cursor;

    let filename = "source.bn";
    let mut out = String::new();
    let mut buf = Cursor::new(Vec::new());

    for err in errors {
        buf.set_position(0);
        buf.get_mut().clear();
        let span = err.span.start..err.span.end;
        let result = Report::build(ReportKind::Error, (filename, span.clone()))
            .with_config(Config::default().with_color(false))
            .with_message(&err.message)
            .with_label(
                Label::new((filename, span))
                    .with_message(&err.message),
            )
            .finish()
            .write((filename, Source::from(source)), &mut buf);

        if result.is_ok() {
            if let Ok(s) = String::from_utf8(buf.get_ref().clone()) {
                out.push_str(&s);
            }
        } else {
            // Fallback if ariadne fails.
            out.push_str(&format!("[{}-{}] {}\n", err.span.start, err.span.end, err.message));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Parser integration
// ---------------------------------------------------------------------------

fn parse_source(
    source_code: &str,
) -> Result<Vec<static_expression::Spanned<static_expression::Expression>>, String> {
    use chumsky::prelude::*;

    let source_code_arc = SourceCode::new(source_code.to_string());
    let source_for_parsing = source_code_arc.clone();
    let source_ref = source_for_parsing.as_str();

    // 1. Lex
    let (tokens, lex_errors) = lexer().parse(source_ref).into_output_errors();
    if !lex_errors.is_empty() {
        return Err(format!("Lex errors: {:?}", lex_errors));
    }
    let Some(mut tokens) = tokens else {
        return Err("Lexer produced no output".to_string());
    };

    // 2. Filter comments
    tokens.retain(|t| !matches!(t.node, Token::Comment(_)));

    // 3. Parse
    reset_expression_depth();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source_ref.len()),
            |crate::parser::Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();

    if !parse_errors.is_empty() {
        return Err(format!("Parse errors: {:?}", parse_errors));
    }
    let Some(ast) = ast else {
        return Err("Parser produced no output".to_string());
    };

    // 4. Resolve references
    let ast = resolve_references(ast).map_err(|e| format!("Reference errors: {:?}", e))?;

    // 5. Convert to static expressions
    let static_ast = static_expression::convert_expressions(source_code_arc, ast);
    Ok(static_ast)
}
