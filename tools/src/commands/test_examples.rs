//! Test runner for Boon playground examples

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

use crate::ws_server::{send_command_to_server, Command as WsCommand, Response as WsResponse};

use super::expected::{matches_inline, ExpectedSpec, MatchMode, ParsedAction};

/// Options for test-examples command
pub struct TestOptions {
    pub port: u16,
    pub filter: Option<String>,
    pub interactive: bool,
    pub screenshot_on_fail: bool,
    pub verbose: bool,
    pub examples_dir: Option<PathBuf>,
}

/// Result of a single test
#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub skipped: Option<String>,
    pub duration: Duration,
    pub error: Option<String>,
    pub actual_output: Option<String>,
    pub expected_output: Option<String>,
    pub steps: Vec<StepResult>,
}

#[derive(Debug)]
pub struct StepResult {
    pub description: String,
    pub passed: bool,
    pub actual: Option<String>,
    pub expected: Option<String>,
}

/// Discovered example with its .expected file
#[derive(Debug)]
pub struct DiscoveredExample {
    pub name: String,
    pub bn_path: PathBuf,
    pub expected_path: PathBuf,
}

/// Discover examples that have matching .expected files
pub fn discover_examples(examples_dir: &Path) -> Result<Vec<DiscoveredExample>> {
    let mut examples = Vec::new();

    for entry in WalkDir::new(examples_dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().map(|e| e == "expected").unwrap_or(false) {
            // Found an .expected file, look for matching .bn
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let bn_path = path.with_extension("bn");

            if bn_path.exists() {
                let name = stem.to_string();
                examples.push(DiscoveredExample {
                    name,
                    bn_path,
                    expected_path: path.to_path_buf(),
                });
            }
        }
    }

    // Sort by name for consistent ordering
    examples.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(examples)
}

/// Run all discovered examples
pub async fn run_tests(opts: TestOptions) -> Result<Vec<TestResult>> {
    // Pre-flight check: verify WebSocket server and browser extension are running
    match check_server_connection(opts.port).await {
        Ok(status) => {
            if !status.connected {
                eprintln!("ERROR: Browser extension not connected!");
                eprintln!();
                eprintln!("To run playground tests, you need:");
                eprintln!("  1. WebSocket server running:");
                eprintln!("     cd tools && cargo run --release -- server start");
                eprintln!();
                eprintln!("  2. Browser with extension loaded:");
                eprintln!("     - Open Chromium");
                eprintln!("     - Load extension from tools/extension/");
                eprintln!("     - Navigate to http://localhost:8081");
                eprintln!();
                eprintln!("Or use MCP tools if available (boon_status, boon_launch_browser)");
                anyhow::bail!("Browser extension not connected");
            }
            if !status.api_ready {
                eprintln!("ERROR: Boon playground API not ready!");
                eprintln!("Make sure the playground is loaded at http://localhost:8081");
                anyhow::bail!("Playground API not ready");
            }
        }
        Err(e) => {
            eprintln!("ERROR: Cannot connect to WebSocket server on port {}!", opts.port);
            eprintln!();
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("To run playground tests, start the WebSocket server first:");
            eprintln!("  cd tools && cargo run --release -- server start");
            eprintln!();
            eprintln!("Or use MCP tools if available (boon_start_playground, boon_launch_browser)");
            anyhow::bail!("WebSocket server not running");
        }
    }

    // Find examples directory
    let examples_dir = if let Some(ref dir) = opts.examples_dir {
        dir.clone()
    } else {
        find_examples_dir()?
    };

    // Discover examples
    let mut examples = discover_examples(&examples_dir)?;

    if examples.is_empty() {
        println!("No examples with .expected files found in {}", examples_dir.display());
        return Ok(vec![]);
    }

    // Apply filter
    if let Some(ref filter) = opts.filter {
        examples.retain(|e| e.name.contains(filter));
        if examples.is_empty() {
            println!("No examples match filter '{}'", filter);
            return Ok(vec![]);
        }
    }

    println!("Boon Example Tests");
    println!("==================\n");
    println!("Running {} example(s)...\n", examples.len());

    let mut results = Vec::new();

    for example in examples {
        let result = run_single_test(&example, &opts).await?;

        // Print result
        print_test_result(&result, opts.verbose);

        // Handle failure
        if !result.passed {
            if opts.screenshot_on_fail {
                let screenshot_path = format!("test-failure-{}.png", example.name);
                if let Err(e) = save_screenshot(opts.port, &screenshot_path).await {
                    eprintln!("  Failed to save screenshot: {}", e);
                } else {
                    println!("  Screenshot: {}", screenshot_path);
                }
            }

            if opts.interactive {
                match interactive_menu(opts.port, &example).await? {
                    InteractiveAction::Retry => {
                        // Re-run the test
                        let retry_result = run_single_test(&example, &opts).await?;
                        print_test_result(&retry_result, opts.verbose);
                        results.push(retry_result);
                        continue;
                    }
                    InteractiveAction::Next => {}
                    InteractiveAction::Quit => {
                        results.push(result);
                        break;
                    }
                }
            }
        }

        results.push(result);
    }

    // Print summary
    println!("\n==================");
    let passed = results.iter().filter(|r| r.passed && r.skipped.is_none()).count();
    let skipped = results.iter().filter(|r| r.skipped.is_some()).count();
    let total = results.len();
    if skipped > 0 {
        println!("{}/{} passed ({} skipped)", passed, total - skipped, skipped);
    } else {
        println!("{}/{} passed", passed, total);
    }

    Ok(results)
}

/// Run a single example test
async fn run_single_test(example: &DiscoveredExample, opts: &TestOptions) -> Result<TestResult> {
    let start = Instant::now();
    let mut steps = Vec::new();

    // Parse expected spec
    let spec = ExpectedSpec::from_file(&example.expected_path)?;

    // Check if test should be skipped
    if let Some(skip_reason) = &spec.test.skip {
        return Ok(TestResult {
            name: example.name.clone(),
            passed: true, // Skipped tests count as passed
            skipped: Some(skip_reason.clone()),
            duration: start.elapsed(),
            error: None,
            actual_output: None,
            expected_output: None,
            steps,
        });
    }

    // Read example code
    let code = std::fs::read_to_string(&example.bn_path)
        .with_context(|| format!("Failed to read {}", example.bn_path.display()))?;

    // Clear states before test
    let _ = send_command_to_server(opts.port, WsCommand::ClearStates).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Inject code with filename for persistence
    let filename = format!("{}.bn", example.name);
    let response = send_command_to_server(opts.port, WsCommand::InjectCode { code, filename: Some(filename) }).await?;
    if let WsResponse::Error { message } = response {
        return Ok(TestResult {
            name: example.name.clone(),
            passed: false,
            skipped: None,
            duration: start.elapsed(),
            error: Some(format!("Inject failed: {}", message)),
            actual_output: None,
            expected_output: None,
            steps,
        });
    }

    // Trigger run
    tokio::time::sleep(Duration::from_millis(spec.timing.initial_delay)).await;
    let response = send_command_to_server(opts.port, WsCommand::TriggerRun).await?;
    if let WsResponse::Error { message } = response {
        return Ok(TestResult {
            name: example.name.clone(),
            passed: false,
            skipped: None,
            duration: start.elapsed(),
            error: Some(format!("Run failed: {}", message)),
            actual_output: None,
            expected_output: None,
            steps,
        });
    }

    // Wait for initial output with smart waiting
    let initial_result = wait_for_output(
        opts.port,
        &spec.output,
        spec.timing.timeout,
        spec.timing.poll_interval,
    )
    .await;

    let (initial_passed, actual_output) = match initial_result {
        Ok(text) => (true, text),
        Err(WaitError::Timeout { actual }) => (false, actual),
        Err(WaitError::Other(e)) => {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(e.to_string()),
                actual_output: None,
                expected_output: spec.output.text.clone(),
                steps,
            });
        }
    };

    if !initial_passed && spec.sequence.is_empty() {
        return Ok(TestResult {
            name: example.name.clone(),
            passed: false,
            skipped: None,
            duration: start.elapsed(),
            error: None,
            actual_output: Some(actual_output),
            expected_output: spec.output.text.clone(),
            steps,
        });
    }

    // Run interaction sequences
    let mut all_passed = initial_passed || !spec.sequence.is_empty();

    for seq in &spec.sequence {
        // Execute actions
        for action in &seq.actions {
            let parsed = action.parse()?;
            if let Err(e) = execute_action(opts.port, &parsed).await {
                // Action failed (including assertions) - record as test failure
                steps.push(StepResult {
                    description: seq.description.clone().unwrap_or_else(|| format!("{:?}", parsed)),
                    passed: false,
                    actual: Some(e.to_string()),
                    expected: None,
                });
                return Ok(TestResult {
                    name: example.name.clone(),
                    passed: false,
                    skipped: None,
                    duration: start.elapsed(),
                    error: Some(e.to_string()),
                    actual_output: None,
                    expected_output: None,
                    steps,
                });
            }
        }

        // Check expected output if specified
        if let Some(ref expected) = seq.expect {
            let step_result = wait_for_inline_output(
                opts.port,
                expected,
                &seq.expect_match,
                spec.timing.timeout,
                spec.timing.poll_interval,
            )
            .await;

            let (passed, actual) = match step_result {
                Ok(text) => (true, text),
                Err(WaitError::Timeout { actual }) => (false, actual),
                Err(WaitError::Other(e)) => {
                    return Ok(TestResult {
                        name: example.name.clone(),
                        passed: false,
                        skipped: None,
                        duration: start.elapsed(),
                        error: Some(e.to_string()),
                        actual_output: None,
                        expected_output: Some(expected.clone()),
                        steps,
                    });
                }
            };

            steps.push(StepResult {
                description: seq.description.clone().unwrap_or_default(),
                passed,
                actual: Some(actual),
                expected: Some(expected.clone()),
            });

            if !passed {
                all_passed = false;
                break;
            }
        }
    }

    // --- PERSISTENCE TEST ---
    // If there are persistence sequences, do a FULL PAGE REFRESH and verify state was restored
    if all_passed && !spec.persistence.is_empty() {
        // Full page refresh - this clears JavaScript memory and forces restoration from localStorage
        // NOTE: This properly tests persistence - TriggerRun would just keep memory state
        let response = send_command_to_server(opts.port, WsCommand::Refresh).await?;
        if let WsResponse::Error { message } = response {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(format!("Persistence refresh failed: {}", message)),
                actual_output: None,
                expected_output: None,
                steps,
            });
        }

        // Wait for page refresh to complete - full page reload takes longer than re-run
        // Use a minimum of 2 seconds to ensure page is fully loaded
        let refresh_delay = std::cmp::max(spec.timing.initial_delay, 2000);
        tokio::time::sleep(Duration::from_millis(refresh_delay)).await;

        // NOTE: We intentionally do NOT call SelectExample here!
        // The code is already saved in PROJECT_FILES_STORAGE_KEY from the initial inject.
        // After refresh, it loads automatically. Calling SelectExample would clear the
        // persistence state (boon-playground-v2-states) because the playground clears
        // localStorage when switching examples. Since we're testing persistence,
        // we just need to TriggerRun to execute the already-loaded code.

        // Trigger run after refresh - the playground doesn't auto-run on page load
        // This is where persistence should restore state from localStorage
        let response = send_command_to_server(opts.port, WsCommand::TriggerRun).await?;
        if let WsResponse::Error { message } = response {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(format!("Persistence run failed: {}", message)),
                actual_output: None,
                expected_output: None,
                steps,
            });
        }

        // Wait for code execution to stabilize
        tokio::time::sleep(Duration::from_millis(spec.timing.initial_delay)).await;

        // Run persistence verification sequences
        for seq in &spec.persistence {
            // Execute actions
            for action in &seq.actions {
                let parsed = action.parse()?;
                if let Err(e) = execute_action(opts.port, &parsed).await {
                    steps.push(StepResult {
                        description: format!("[PERSISTENCE] {}", seq.description.clone().unwrap_or_else(|| format!("{:?}", parsed))),
                        passed: false,
                        actual: Some(e.to_string()),
                        expected: None,
                    });
                    return Ok(TestResult {
                        name: example.name.clone(),
                        passed: false,
                        skipped: None,
                        duration: start.elapsed(),
                        error: Some(format!("Persistence test failed: {}", e)),
                        actual_output: None,
                        expected_output: None,
                        steps,
                    });
                }
            }

            // Check expected output if specified
            if let Some(ref expected) = seq.expect {
                // For persistence checks, do an IMMEDIATE check of the current state.
                // We don't poll/wait because we want to verify the INITIAL state after
                // refresh, not wait for timer-based values to change over time.
                let response = send_command_to_server(opts.port, WsCommand::GetPreviewText).await?;
                let preview = match response {
                    WsResponse::PreviewText { text } => text,
                    WsResponse::Error { message } => {
                        return Ok(TestResult {
                            name: example.name.clone(),
                            passed: false,
                            skipped: None,
                            duration: start.elapsed(),
                            error: Some(format!("Persistence check failed: {}", message)),
                            actual_output: None,
                            expected_output: Some(expected.clone()),
                            steps,
                        });
                    }
                    _ => {
                        return Ok(TestResult {
                            name: example.name.clone(),
                            passed: false,
                            skipped: None,
                            duration: start.elapsed(),
                            error: Some("Unexpected response for GetPreviewText".to_string()),
                            actual_output: None,
                            expected_output: Some(expected.clone()),
                            steps,
                        });
                    }
                };

                let passed = matches_inline(&preview, expected, &seq.expect_match)?;
                let actual = preview;

                steps.push(StepResult {
                    description: format!("[PERSISTENCE] {}", seq.description.clone().unwrap_or_default()),
                    passed,
                    actual: Some(actual),
                    expected: Some(expected.clone()),
                });

                if !passed {
                    all_passed = false;
                    break;
                }
            }
        }
    }

    Ok(TestResult {
        name: example.name.clone(),
        passed: all_passed,
        skipped: None,
        duration: start.elapsed(),
        error: None,
        actual_output: Some(actual_output),
        expected_output: spec.output.text.clone(),
        steps,
    })
}

enum WaitError {
    Timeout { actual: String },
    Other(anyhow::Error),
}

/// Smart wait for output to match expected
async fn wait_for_output(
    port: u16,
    output_spec: &super::expected::OutputSpec,
    timeout_ms: u64,
    poll_interval_ms: u64,
) -> Result<String, WaitError> {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let mut interval = Duration::from_millis(poll_interval_ms);
    let max_interval = Duration::from_secs(1);

    let mut last_preview = String::new();

    loop {
        // Check timeout
        if start.elapsed() > timeout {
            return Err(WaitError::Timeout { actual: last_preview });
        }

        // Get current preview
        let response = send_command_to_server(port, WsCommand::GetPreviewText)
            .await
            .map_err(|e| WaitError::Other(e))?;

        let preview = match response {
            WsResponse::PreviewText { text } => text,
            WsResponse::Error { message } => {
                return Err(WaitError::Other(anyhow::anyhow!("GetPreview failed: {}", message)));
            }
            _ => {
                return Err(WaitError::Other(anyhow::anyhow!("Unexpected response")));
            }
        };

        // Check match
        let matches = output_spec.matches(&preview).map_err(|e| WaitError::Other(e))?;

        if matches {
            // Stability check - wait and verify again
            tokio::time::sleep(interval).await;
            let response = send_command_to_server(port, WsCommand::GetPreviewText)
                .await
                .map_err(|e| WaitError::Other(e))?;

            if let WsResponse::PreviewText { text } = response {
                let still_matches = output_spec.matches(&text).map_err(|e| WaitError::Other(e))?;
                if still_matches {
                    return Ok(text);
                }
            }
        }

        last_preview = preview;

        // Wait with exponential backoff (capped)
        tokio::time::sleep(interval).await;
        interval = std::cmp::min(interval * 2, max_interval);
    }
}

/// Smart wait for inline expected string
async fn wait_for_inline_output(
    port: u16,
    expected: &str,
    mode: &MatchMode,
    timeout_ms: u64,
    poll_interval_ms: u64,
) -> Result<String, WaitError> {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let mut interval = Duration::from_millis(poll_interval_ms);
    let max_interval = Duration::from_secs(1);

    let mut last_preview = String::new();

    loop {
        if start.elapsed() > timeout {
            return Err(WaitError::Timeout { actual: last_preview });
        }

        let response = send_command_to_server(port, WsCommand::GetPreviewText)
            .await
            .map_err(|e| WaitError::Other(e))?;

        let preview = match response {
            WsResponse::PreviewText { text } => text,
            WsResponse::Error { message } => {
                return Err(WaitError::Other(anyhow::anyhow!("GetPreview failed: {}", message)));
            }
            _ => {
                return Err(WaitError::Other(anyhow::anyhow!("Unexpected response")));
            }
        };

        let matches = matches_inline(&preview, expected, mode).map_err(|e| WaitError::Other(e))?;

        if matches {
            // Stability check
            tokio::time::sleep(interval).await;
            let response = send_command_to_server(port, WsCommand::GetPreviewText)
                .await
                .map_err(|e| WaitError::Other(e))?;

            if let WsResponse::PreviewText { text } = response {
                let still_matches = matches_inline(&text, expected, mode).map_err(|e| WaitError::Other(e))?;
                if still_matches {
                    return Ok(text);
                }
            }
        }

        last_preview = preview;
        tokio::time::sleep(interval).await;
        interval = std::cmp::min(interval * 2, max_interval);
    }
}

/// Execute a parsed action
async fn execute_action(port: u16, action: &ParsedAction) -> Result<()> {
    match action {
        ParsedAction::Click { selector } => {
            let response = send_command_to_server(port, WsCommand::Click { selector: selector.clone() }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click failed: {}", message);
            }
            // Small delay after click for UI to update
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::Type { selector, text } => {
            let response = send_command_to_server(port, WsCommand::Type { selector: selector.clone(), text: text.clone() }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Type failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::Wait { ms } => {
            tokio::time::sleep(Duration::from_millis(*ms)).await;
        }
        ParsedAction::ClearStates => {
            let _ = send_command_to_server(port, WsCommand::ClearStates).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::Run => {
            let response = send_command_to_server(port, WsCommand::TriggerRun).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Run failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        ParsedAction::Key { key } => {
            let response = send_command_to_server(port, WsCommand::Key { key: key.clone() }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Key press failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::FocusInput { index } => {
            let response = send_command_to_server(port, WsCommand::FocusInput { index: *index }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Focus input failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::TypeText { text } => {
            let response = send_command_to_server(port, WsCommand::TypeText { text: text.clone() }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Type text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::ClickText { text } => {
            let response = send_command_to_server(port, WsCommand::ClickByText { text: text.clone(), exact: false }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::ClickButton { index } => {
            let response = send_command_to_server(port, WsCommand::ClickButton { index: *index }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click button failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::ClickCheckbox { index } => {
            let response = send_command_to_server(port, WsCommand::ClickCheckbox { index: *index }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click checkbox failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::DblClickText { text } => {
            let response = send_command_to_server(port, WsCommand::DoubleClickByText {
                text: text.clone(),
                exact: false
            }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Double-click text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::HoverText { text } => {
            let response = send_command_to_server(port, WsCommand::HoverByText {
                text: text.clone(),
                exact: false
            }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Hover text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::AssertFocused { input_index } => {
            let response = send_command_to_server(port, WsCommand::GetFocusedElement).await?;
            match response {
                WsResponse::FocusedElement { tag_name, input_index: actual_index, .. } => {
                    // Check if any element is focused
                    if tag_name.is_none() {
                        anyhow::bail!("Assert focused failed: no element is focused");
                    }
                    // If a specific index is expected, verify it matches
                    if let Some(expected_idx) = input_index {
                        if actual_index != Some(*expected_idx) {
                            anyhow::bail!(
                                "Assert focused failed: expected input index {}, got {:?}",
                                expected_idx, actual_index
                            );
                        }
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert focused failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetFocusedElement"),
            }
        }
        ParsedAction::AssertInputPlaceholder { index, expected } => {
            let response = send_command_to_server(port, WsCommand::GetInputProperties { index: *index }).await?;
            match response {
                WsResponse::InputProperties { found, placeholder, .. } => {
                    if !found {
                        anyhow::bail!("Assert input placeholder failed: input {} not found", index);
                    }
                    let actual = placeholder.unwrap_or_default();
                    if !actual.contains(expected) {
                        anyhow::bail!(
                            "Assert input placeholder failed: expected '{}' in placeholder, got '{}'",
                            expected, actual
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert input placeholder failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetInputProperties"),
            }
        }
        ParsedAction::AssertUrl { pattern } => {
            let response = send_command_to_server(port, WsCommand::GetCurrentUrl).await?;
            match response {
                WsResponse::CurrentUrl { url } => {
                    if !url.contains(pattern) {
                        anyhow::bail!(
                            "Assert URL failed: expected '{}' in URL, got '{}'",
                            pattern, url
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert URL failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetCurrentUrl"),
            }
        }
        ParsedAction::AssertInputTypeable { index } => {
            let response = send_command_to_server(port, WsCommand::VerifyInputTypeable { index: *index }).await?;
            match response {
                WsResponse::InputTypeableStatus { typeable, disabled, readonly, hidden, reason } => {
                    if !typeable {
                        let reason_str = reason.unwrap_or_else(|| {
                            let mut reasons = vec![];
                            if disabled { reasons.push("disabled"); }
                            if readonly { reasons.push("readonly"); }
                            if hidden { reasons.push("hidden"); }
                            reasons.join(", ")
                        });
                        anyhow::bail!(
                            "Input {} is NOT typeable: {}",
                            index, reason_str
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert input typeable failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for VerifyInputTypeable"),
            }
        }
    }
    Ok(())
}

/// Print test result
fn print_test_result(result: &TestResult, verbose: bool) {
    let status = if let Some(ref reason) = result.skipped {
        // Print skip with reason
        println!("  [SKIP] {} ({:.0?})", result.name, result.duration);
        println!("         Reason: {}", reason);
        return;
    } else if result.passed {
        "[PASS]"
    } else {
        "[FAIL]"
    };
    let duration = format!("({:.0?})", result.duration);

    println!("  {} {} {}", status, result.name, duration);

    if !result.passed {
        if let Some(ref error) = result.error {
            println!("         Error: {}", error);
        } else {
            if let Some(ref expected) = result.expected_output {
                println!("         Expected: \"{}\"", expected);
            }
            if let Some(ref actual) = result.actual_output {
                println!("         Actual:   \"{}\"", truncate(actual, 60));
            }
        }
    }

    // Print step results if verbose or if there are failures
    if verbose || !result.passed {
        for step in &result.steps {
            let step_status = if step.passed { "|--" } else { "|XX" };
            if let Some(ref desc) = step.description.is_empty().then_some(&step.description) {
                println!("         {} {}", step_status, desc);
            } else if !step.description.is_empty() {
                println!("         {} {}", step_status, step.description);
            }
            if !step.passed {
                if let Some(ref expected) = step.expected {
                    println!("             Expected: \"{}\"", expected);
                }
                if let Some(ref actual) = step.actual {
                    println!("             Actual:   \"{}\"", truncate(actual, 50));
                }
            }
        }
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    let s = s.replace('\n', " ").replace('\r', "");
    if s.len() > max_len {
        format!("{}...", &s[..max_len])
    } else {
        s
    }
}

enum InteractiveAction {
    Retry,
    Next,
    Quit,
}

/// Interactive menu for debugging failures
async fn interactive_menu(port: u16, example: &DiscoveredExample) -> Result<InteractiveAction> {
    println!();
    println!("  Interactive debugging for '{}':", example.name);
    println!("    [s] Screenshot  [p] Preview  [c] Console");
    println!("    [r] Retry       [n] Next     [q] Quit");
    println!();

    loop {
        print!("  > ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        match input.trim().to_lowercase().as_str() {
            "s" => {
                let path = format!("debug-{}.png", example.name);
                match save_screenshot(port, &path).await {
                    Ok(_) => println!("    Screenshot saved: {}", path),
                    Err(e) => println!("    Failed: {}", e),
                }
            }
            "p" => {
                match get_preview(port).await {
                    Ok(text) => println!("    Preview:\n{}", text),
                    Err(e) => println!("    Failed: {}", e),
                }
            }
            "c" => {
                match get_console(port).await {
                    Ok(msgs) => {
                        if msgs.is_empty() {
                            println!("    No console messages");
                        } else {
                            println!("    Console:");
                            for msg in msgs {
                                println!("      {}", msg);
                            }
                        }
                    }
                    Err(e) => println!("    Failed: {}", e),
                }
            }
            "r" => return Ok(InteractiveAction::Retry),
            "n" => return Ok(InteractiveAction::Next),
            "q" => return Ok(InteractiveAction::Quit),
            _ => println!("    Unknown command. Use s/p/c/r/n/q"),
        }
    }
}

async fn save_screenshot(port: u16, path: &str) -> Result<()> {
    let response = send_command_to_server(port, WsCommand::Screenshot).await?;
    match response {
        WsResponse::Screenshot { base64 } => {
            let data = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &base64,
            )?;
            std::fs::write(path, data)?;
            Ok(())
        }
        WsResponse::Error { message } => anyhow::bail!("{}", message),
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn get_preview(port: u16) -> Result<String> {
    let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
    match response {
        WsResponse::PreviewText { text } => Ok(text),
        WsResponse::Error { message } => anyhow::bail!("{}", message),
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn get_console(port: u16) -> Result<Vec<String>> {
    let response = send_command_to_server(port, WsCommand::GetConsole).await?;
    match response {
        WsResponse::Console { messages } => {
            Ok(messages.into_iter().map(|m| format!("[{}] {}", m.level, m.text)).collect())
        }
        WsResponse::Error { message } => anyhow::bail!("{}", message),
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Server connection status
struct ServerStatus {
    connected: bool,
    api_ready: bool,
}

/// Check if WebSocket server is running and browser extension is connected
async fn check_server_connection(port: u16) -> Result<ServerStatus> {
    let response = send_command_to_server(port, WsCommand::GetStatus).await?;
    match response {
        WsResponse::Status { connected, api_ready, .. } => {
            Ok(ServerStatus { connected, api_ready })
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Status check failed: {}", message)
        }
        _ => {
            anyhow::bail!("Unexpected response from status check")
        }
    }
}

/// Find examples directory relative to cwd or tools directory
fn find_examples_dir() -> Result<PathBuf> {
    // Try relative to cwd
    let cwd = std::env::current_dir()?;

    // If we're in tools/ or tools/src/, look for playground/frontend/src/examples
    let candidates = [
        cwd.join("../playground/frontend/src/examples"),
        cwd.join("playground/frontend/src/examples"),
        cwd.join("../../playground/frontend/src/examples"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.canonicalize()?);
        }
    }

    anyhow::bail!(
        "Could not find examples directory. Run from project root or use --examples-dir"
    )
}
