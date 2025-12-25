use clap::{Parser as ClapParser, Subcommand};
use std::path::PathBuf;
use std::fs;
use boon::engine_v2::event_loop::EventLoop;
use boon::evaluator_v2::CompileContext;
use boon::parser::{lexer, parser, reset_expression_depth, Parser, Input, Spanned, span_at};

#[derive(ClapParser)]
#[command(name = "boon")]
#[command(about = "Boon language CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Evaluate inline Boon code
    Eval {
        /// The code to evaluate
        code: String,
        /// Number of ticks to run
        #[arg(long)]
        ticks: Option<u64>,
    },
    /// Run a Boon file
    Run {
        /// Path to .bn file
        file: PathBuf,
        /// Number of ticks to run
        #[arg(long)]
        ticks: Option<u64>,
        /// State file for persistence (load on start, save on exit)
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// Check if code parses correctly
    Check {
        /// Path to .bn file
        file: PathBuf,
    },
    /// Run test files with expected output verification
    Test {
        /// Path to test file(s) - can be a glob pattern
        files: Vec<PathBuf>,
        /// Update expected outputs instead of verifying
        #[arg(long)]
        update: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Eval { code, ticks } => {
            eval_code(&code, ticks.unwrap_or(100));
        }
        Commands::Run { file, ticks, state } => {
            match fs::read_to_string(&file) {
                Ok(code) => {
                    eprintln!("Running: {}", file.display());
                    eval_code_with_persistence(&code, ticks.unwrap_or(100), state);
                }
                Err(e) => {
                    eprintln!("Error reading file: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Check { file } => {
            match fs::read_to_string(&file) {
                Ok(code) => {
                    check_code(&code, &file);
                }
                Err(e) => {
                    eprintln!("Error reading file: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Test { files, update } => {
            run_tests(&files, update);
        }
    }
}

/// Run test files with expected output verification.
/// Test file format:
/// ```
/// -- test: test_name
/// code here
/// -- expect: expected_json_value
/// ```
fn run_tests(files: &[PathBuf], update: bool) {
    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;

    for file in files {
        match fs::read_to_string(file) {
            Ok(content) => {
                let results = run_test_file(file, &content, update);
                total += results.0;
                passed += results.1;
                failed += results.2;
            }
            Err(e) => {
                eprintln!("Error reading {}: {}", file.display(), e);
                failed += 1;
            }
        }
    }

    eprintln!("\n{} tests: {} passed, {} failed", total, passed, failed);
    if failed > 0 {
        std::process::exit(1);
    }
}

/// Parse and run tests from a single test file.
/// Returns (total, passed, failed) counts.
fn run_test_file(file: &PathBuf, content: &str, update: bool) -> (usize, usize, usize) {
    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;

    // Simple parsing: split by "-- test:" and "-- expect:"
    let mut current_test: Option<(&str, String)> = None;

    for line in content.lines() {
        if line.starts_with("-- test:") {
            // Save previous test if any
            if let Some((name, code)) = current_test.take() {
                // No expect found, treat as parse-only test
                if run_single_test(file, name, &code, None, update) {
                    passed += 1;
                } else {
                    failed += 1;
                }
                total += 1;
            }
            let name = line.strip_prefix("-- test:").unwrap().trim();
            current_test = Some((name, String::new()));
        } else if line.starts_with("-- expect:") {
            if let Some((name, code)) = current_test.take() {
                let expected = line.strip_prefix("-- expect:").unwrap().trim();
                if run_single_test(file, name, &code, Some(expected), update) {
                    passed += 1;
                } else {
                    failed += 1;
                }
                total += 1;
            }
        } else if let Some((name, ref mut code)) = current_test {
            if !code.is_empty() {
                code.push('\n');
            }
            code.push_str(line);
        }
    }

    // Handle last test without expect
    if let Some((name, code)) = current_test {
        if run_single_test(file, name, &code, None, update) {
            passed += 1;
        } else {
            failed += 1;
        }
        total += 1;
    }

    (total, passed, failed)
}

/// Run a single test case.
fn run_single_test(file: &PathBuf, name: &str, code: &str, expected: Option<&str>, _update: bool) -> bool {
    eprint!("  {} ... ", name);

    // Evaluate the code
    let result = eval_code_to_json(code, 100);

    match (&result, expected) {
        (Ok(actual), Some(expected)) => {
            // Parse expected JSON
            match serde_json::from_str::<serde_json::Value>(expected) {
                Ok(expected_val) => {
                    if actual == &expected_val {
                        eprintln!("ok");
                        true
                    } else {
                        eprintln!("FAILED");
                        eprintln!("    expected: {}", expected);
                        eprintln!("    actual:   {}", actual);
                        false
                    }
                }
                Err(e) => {
                    eprintln!("FAILED (invalid expected JSON: {})", e);
                    false
                }
            }
        }
        (Ok(actual), None) => {
            // No expected value - just check it runs
            eprintln!("ok ({})", actual);
            true
        }
        (Err(e), _) => {
            eprintln!("FAILED: {}", e);
            false
        }
    }
}

/// Evaluate code and return the result as JSON value.
fn eval_code_to_json(code: &str, max_ticks: u64) -> Result<serde_json::Value, String> {
    reset_expression_depth();

    // Lex the code
    let (tokens, lex_errors) = lexer().parse(code).into_output_errors();

    if !lex_errors.is_empty() {
        return Err(format!("Lexer errors: {:?}", lex_errors));
    }

    let mut tokens = tokens.ok_or("No tokens from lexer")?;
    tokens.retain(|t| !matches!(t.node, boon::parser::Token::Comment(_)));

    let input = tokens.map(
        span_at(code.len()),
        |Spanned { node, span, persistence: _ }| (node, span),
    );

    let (expressions, parse_errors) = parser().parse(input).into_output_errors();

    if !parse_errors.is_empty() {
        return Err(format!("Parser errors: {:?}", parse_errors));
    }

    let expressions = expressions.ok_or("No expressions from parser")?;

    let mut event_loop = EventLoop::new();
    let mut ctx = CompileContext::new(&mut event_loop);
    let result_slot = ctx.compile_program(&expressions);

    // Mark all nodes dirty
    let all_slots: Vec<_> = (0..event_loop.arena_len() as u32)
        .filter_map(|idx| {
            let slot = boon::engine_v2::arena::SlotId { index: idx, generation: 0 };
            if event_loop.is_valid(slot) { Some(slot) } else { None }
        })
        .collect();

    for slot in all_slots {
        event_loop.mark_dirty(slot, boon::engine_v2::address::Port::Output);
    }

    // Run ticks
    for _ in 0..max_ticks {
        event_loop.run_tick();
        if event_loop.dirty_nodes.is_empty() && event_loop.timer_queue.is_empty() {
            break;
        }
    }

    // Get result - use expand_payload_to_json to resolve ListHandle/ObjectHandle
    if let Some(slot) = result_slot {
        if let Some(value) = event_loop.get_current_value(slot) {
            Ok(event_loop.expand_payload_to_json(value))
        } else {
            Ok(serde_json::Value::Null)
        }
    } else {
        Ok(serde_json::Value::Null)
    }
}

fn check_code(code: &str, file: &PathBuf) {
    eprintln!("Checking: {}", file.display());

    reset_expression_depth();

    // Lex the code
    let (tokens, lex_errors) = lexer().parse(code).into_output_errors();

    if !lex_errors.is_empty() {
        eprintln!("Lexer errors: {:?}", lex_errors);
        std::process::exit(1);
    }

    let mut tokens = match tokens {
        Some(t) => t,
        None => {
            eprintln!("No tokens from lexer");
            std::process::exit(1);
        }
    };

    // Filter comments
    tokens.retain(|t| !matches!(t.node, boon::parser::Token::Comment(_)));

    // Create input with span mapping
    let input = tokens.map(
        span_at(code.len()),
        |Spanned { node, span, persistence: _ }| (node, span),
    );

    // Parse
    let (expressions, parse_errors) = parser().parse(input).into_output_errors();

    if !parse_errors.is_empty() {
        eprintln!("Parser errors: {:?}", parse_errors);
        std::process::exit(1);
    }

    match expressions {
        Some(exprs) => {
            eprintln!("Parse OK: {} top-level expressions", exprs.len());
        }
        None => {
            eprintln!("No expressions from parser");
            std::process::exit(1);
        }
    }
}

fn eval_code_with_persistence(code: &str, max_ticks: u64, state_file: Option<PathBuf>) {
    use boon::engine_v2::snapshot::GraphSnapshot;

    reset_expression_depth();

    // Lex the code
    let (tokens, lex_errors) = lexer().parse(code).into_output_errors();

    if !lex_errors.is_empty() {
        eprintln!("Lexer errors: {:?}", lex_errors);
        println!("{}", serde_json::json!({
            "status": "error",
            "error": format!("Lexer errors: {:?}", lex_errors)
        }));
        return;
    }

    let mut tokens = match tokens {
        Some(t) => t,
        None => {
            eprintln!("No tokens from lexer");
            println!("{}", serde_json::json!({
                "status": "error",
                "error": "No tokens from lexer"
            }));
            return;
        }
    };

    // Filter comments
    tokens.retain(|t| !matches!(t.node, boon::parser::Token::Comment(_)));

    // Create input with span mapping
    let input = tokens.map(
        span_at(code.len()),
        |Spanned { node, span, persistence: _ }| (node, span),
    );

    // Parse
    let (expressions, parse_errors) = parser().parse(input).into_output_errors();

    if !parse_errors.is_empty() {
        eprintln!("Parser errors: {:?}", parse_errors);
        println!("{}", serde_json::json!({
            "status": "error",
            "error": format!("Parser errors: {:?}", parse_errors)
        }));
        return;
    }

    let expressions = match expressions {
        Some(e) => e,
        None => {
            eprintln!("No expressions from parser");
            println!("{}", serde_json::json!({
                "status": "ok",
                "ticks": 0,
                "note": "No expressions to evaluate"
            }));
            return;
        }
    };

    // Create event loop and compile context
    let mut event_loop = EventLoop::new();
    let mut ctx = CompileContext::new(&mut event_loop);

    // Compile the program
    let result_slot = ctx.compile_program(&expressions);

    // Load state from file if provided
    if let Some(ref state_path) = state_file {
        if state_path.exists() {
            match fs::read_to_string(state_path) {
                Ok(json_str) => {
                    match GraphSnapshot::from_json(&json_str) {
                        Ok(snapshot) => {
                            event_loop.restore_snapshot(&snapshot);
                            eprintln!("Loaded state from: {}", state_path.display());
                        }
                        Err(e) => {
                            eprintln!("Warning: Failed to parse state file: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Failed to read state file: {}", e);
                }
            }
        }
    }

    // Mark all nodes as dirty to trigger initial evaluation
    let all_slots: Vec<_> = (0..event_loop.arena_len() as u32)
        .filter_map(|idx| {
            let slot = boon::engine_v2::arena::SlotId { index: idx, generation: 0 };
            if event_loop.is_valid(slot) { Some(slot) } else { None }
        })
        .collect();

    for slot in all_slots {
        event_loop.mark_dirty(slot, boon::engine_v2::address::Port::Output);
    }

    // Run until quiescent or max ticks
    for tick in 0..max_ticks {
        event_loop.run_tick();
        if event_loop.dirty_nodes.is_empty() && event_loop.timer_queue.is_empty() {
            eprintln!("Quiescent after {} ticks", tick + 1);
            break;
        }
    }

    // Save state to file if provided
    if let Some(ref state_path) = state_file {
        let snapshot = event_loop.create_snapshot();
        match snapshot.to_json() {
            Ok(json_str) => {
                match fs::write(state_path, json_str) {
                    Ok(_) => {
                        eprintln!("Saved state to: {}", state_path.display());
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to write state file: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to serialize state: {}", e);
            }
        }
    }

    // Output result as JSON
    if let Some(slot) = result_slot {
        if let Some(value) = event_loop.get_current_value(slot) {
            println!("{}", serde_json::json!({
                "status": "ok",
                "ticks": event_loop.current_tick,
                "result": event_loop.expand_payload_to_json(value)
            }));
        } else {
            println!("{}", serde_json::json!({
                "status": "ok",
                "ticks": event_loop.current_tick,
            }));
        }
    } else {
        println!("{}", serde_json::json!({
            "status": "ok",
            "ticks": event_loop.current_tick,
            "note": "No expressions to evaluate"
        }));
    }
}

fn eval_code(code: &str, max_ticks: u64) {
    reset_expression_depth();

    // Lex the code
    let (tokens, lex_errors) = lexer().parse(code).into_output_errors();

    if !lex_errors.is_empty() {
        eprintln!("Lexer errors: {:?}", lex_errors);
        println!("{}", serde_json::json!({
            "status": "error",
            "error": format!("Lexer errors: {:?}", lex_errors)
        }));
        return;
    }

    let mut tokens = match tokens {
        Some(t) => t,
        None => {
            eprintln!("No tokens from lexer");
            println!("{}", serde_json::json!({
                "status": "error",
                "error": "No tokens from lexer"
            }));
            return;
        }
    };

    // Filter comments
    tokens.retain(|t| !matches!(t.node, boon::parser::Token::Comment(_)));

    // Create input with span mapping
    let input = tokens.map(
        span_at(code.len()),
        |Spanned { node, span, persistence: _ }| (node, span),
    );

    // Parse
    let (expressions, parse_errors) = parser().parse(input).into_output_errors();

    if !parse_errors.is_empty() {
        eprintln!("Parser errors: {:?}", parse_errors);
        println!("{}", serde_json::json!({
            "status": "error",
            "error": format!("Parser errors: {:?}", parse_errors)
        }));
        return;
    }

    let expressions = match expressions {
        Some(e) => e,
        None => {
            eprintln!("No expressions from parser");
            println!("{}", serde_json::json!({
                "status": "ok",
                "ticks": 0,
                "note": "No expressions to evaluate"
            }));
            return;
        }
    };

    // Create event loop and compile context
    let mut event_loop = EventLoop::new();
    let mut ctx = CompileContext::new(&mut event_loop);

    // Compile the program
    let result_slot = ctx.compile_program(&expressions);

    // Mark all nodes as dirty to trigger initial evaluation
    // This ensures all Producers emit their initial values
    let all_slots: Vec<_> = (0..event_loop.arena_len() as u32)
        .filter_map(|idx| {
            let slot = boon::engine_v2::arena::SlotId { index: idx, generation: 0 };
            if event_loop.is_valid(slot) { Some(slot) } else { None }
        })
        .collect();

    for slot in all_slots {
        event_loop.mark_dirty(slot, boon::engine_v2::address::Port::Output);
    }

    // Run until quiescent or max ticks
    // Note: Timers can re-fire, so we need to check both dirty_nodes AND timer_queue
    for tick in 0..max_ticks {
        event_loop.run_tick();
        if event_loop.dirty_nodes.is_empty() && event_loop.timer_queue.is_empty() {
            eprintln!("Quiescent after {} ticks", tick + 1);
            break;
        }
    }

    // Output result as JSON - use expand_payload_to_json for lists/objects
    if let Some(slot) = result_slot {
        if let Some(value) = event_loop.get_current_value(slot) {
            println!("{}", serde_json::json!({
                "status": "ok",
                "ticks": event_loop.current_tick,
                "result": event_loop.expand_payload_to_json(value)
            }));
        } else {
            println!("{}", serde_json::json!({
                "status": "ok",
                "ticks": event_loop.current_tick,
            }));
        }
    } else {
        println!("{}", serde_json::json!({
            "status": "ok",
            "ticks": event_loop.current_tick,
            "note": "No expressions to evaluate"
        }));
    }
}
