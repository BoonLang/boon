//! Test runner for Boon playground examples

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

use crate::commands::browser;
use crate::ws_server::{self, send_command_to_server, Command as WsCommand, Response as WsResponse};

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

/// Find the extension directory relative to the boon-tools binary
fn find_extension_dir() -> Option<PathBuf> {
    // Try relative to current exe (for installed binary)
    if let Ok(exe_path) = std::env::current_exe() {
        // Binary is at target/release/boon-tools, extension at tools/extension/
        if let Some(parent) = exe_path.parent() {
            // Check if we're in target/release/
            let ext_path = parent.join("../../tools/extension");
            if ext_path.exists() {
                return Some(ext_path.canonicalize().ok()?);
            }
            // Also check parent/parent/parent for nested structures
            let ext_path = parent.join("../../../tools/extension");
            if ext_path.exists() {
                return Some(ext_path.canonicalize().ok()?);
            }
        }
    }

    // Try from current working directory
    let cwd_paths = [
        PathBuf::from("extension"),
        PathBuf::from("tools/extension"),
        PathBuf::from("../tools/extension"),
    ];

    for path in &cwd_paths {
        if path.exists() {
            return path.canonicalize().ok();
        }
    }

    None
}

/// Find the boon repo root directory
fn find_boon_root() -> Option<PathBuf> {
    // Try relative to current exe
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            // target/release -> target -> boon
            let root = parent.join("../..");
            if root.join("playground").exists() {
                return Some(root.canonicalize().ok()?);
            }
        }
    }

    // Try from current working directory
    let cwd_paths = [
        PathBuf::from("."),
        PathBuf::from(".."),
        PathBuf::from("../.."),
    ];

    for path in &cwd_paths {
        if path.join("playground").exists() {
            return path.canonicalize().ok();
        }
    }

    None
}

/// Check if the playground dev server (mzoon) is running on port 8083
async fn is_playground_running() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    match client.get("http://localhost:8083").send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Start the playground dev server (mzoon) in background
async fn start_playground_server() -> Result<()> {
    use std::process::Command as StdCommand;

    let boon_root = find_boon_root()
        .context("Could not find boon repository root")?;

    let playground_dir = boon_root.join("playground");

    if !playground_dir.exists() {
        anyhow::bail!("Playground directory not found: {}", playground_dir.display());
    }

    println!("Starting mzoon server in {}...", playground_dir.display());

    // Start mzoon in background using nohup
    let result = StdCommand::new("sh")
        .args(["-c", &format!(
            "cd {} && nohup makers mzoon start > /tmp/mzoon.log 2>&1 &",
            playground_dir.display()
        )])
        .output();

    match result {
        Ok(_) => {
            println!("Mzoon starting in background (log: /tmp/mzoon.log)");
            println!("Note: Initial compilation takes 1-2 minutes...");

            // Wait for server to become available (with progress)
            let start = Instant::now();
            let timeout = Duration::from_secs(180); // 3 minutes for initial compile
            let mut last_dot = Instant::now();

            print!("Waiting for playground server");
            io::stdout().flush().ok();

            while start.elapsed() < timeout {
                if is_playground_running().await {
                    println!(" ready!");
                    return Ok(());
                }

                // Print progress dots every 5 seconds
                if last_dot.elapsed() > Duration::from_secs(5) {
                    print!(".");
                    io::stdout().flush().ok();
                    last_dot = Instant::now();
                }

                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            anyhow::bail!(
                "Playground server did not start within {}s. Check /tmp/mzoon.log for errors.",
                timeout.as_secs()
            );
        }
        Err(e) => anyhow::bail!("Failed to start mzoon: {}", e),
    }
}

/// Result of checking server/extension status
enum ConnectionStatus {
    /// Server not running (TCP connection failed)
    ServerNotRunning,
    /// Server running but no extension connected
    NoExtension,
    /// Server running, extension connected, but API not ready
    ExtensionConnectedNotReady,
    /// Fully ready
    Ready,
}

/// Check connection status, distinguishing between server issues and extension issues
async fn get_connection_status(port: u16) -> ConnectionStatus {
    match check_server_connection(port).await {
        Ok(status) => {
            if status.connected && status.api_ready {
                ConnectionStatus::Ready
            } else if status.connected {
                ConnectionStatus::ExtensionConnectedNotReady
            } else {
                ConnectionStatus::NoExtension
            }
        }
        Err(e) => {
            let error_msg = e.to_string();
            // "No extension connected" means server IS running, just no extension
            if error_msg.contains("No extension connected") {
                ConnectionStatus::NoExtension
            } else {
                // Likely "Failed to connect" - server not running
                ConnectionStatus::ServerNotRunning
            }
        }
    }
}

/// Result of setup - tracks what we started so we can clean up
pub struct SetupState {
    /// Did we start mzoon ourselves? If so, we should kill it when done.
    pub started_mzoon: bool,
}

/// Ensure WebSocket server is running and browser extension is connected.
/// Will start server and launch browser if needed.
/// Returns SetupState indicating what was started (for cleanup).
async fn ensure_browser_connection(port: u16) -> Result<SetupState> {
    let mut setup = SetupState { started_mzoon: false };

    // Step 0: Ensure playground (mzoon) is running
    if !is_playground_running().await {
        println!("Playground server not running on port 8083.");
        start_playground_server().await?;
        setup.started_mzoon = true;
    } else {
        println!("Playground server running on port 8083.");
    }

    // Step 1: Check initial status
    let initial_status = get_connection_status(port).await;

    match initial_status {
        ConnectionStatus::Ready => {
            println!("Browser extension already connected and ready.");
            return Ok(setup);
        }
        ConnectionStatus::ExtensionConnectedNotReady => {
            // Extension connected, just wait for API
            println!("Extension connected, waiting for playground API...");
        }
        ConnectionStatus::NoExtension => {
            // Server running but no extension - need to launch browser
            println!("WebSocket server running, but browser extension not connected.");
        }
        ConnectionStatus::ServerNotRunning => {
            // Need to start server first
            println!("WebSocket server not running, starting it...");

            // Find extension directory for hot-reload watching
            let extension_dir = find_extension_dir();

            // Start WebSocket server in background
            let watch_path = extension_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = ws_server::start_server(port, watch_path.as_deref()).await {
                    // Only log if it's not "address in use" (another server already running)
                    if !e.to_string().contains("address in use") && !e.to_string().contains("bind") {
                        eprintln!("WebSocket server error: {}", e);
                    }
                }
            });

            // Give the server a moment to start
            tokio::time::sleep(Duration::from_millis(300)).await;
            println!("WebSocket server started on port {}", port);
        }
    }

    // Step 2: Check if extension is connected now
    let status = get_connection_status(port).await;

    if matches!(status, ConnectionStatus::Ready) {
        println!("Browser extension connected and ready.");
        return Ok(setup);
    }

    // Step 3: Need to launch browser if extension not connected
    if matches!(status, ConnectionStatus::NoExtension | ConnectionStatus::ServerNotRunning) {
        println!("Browser extension not connected, launching Chromium...");

        let opts = browser::LaunchOptions {
            playground_port: 8083,
            ws_port: port,
            headless: false,
            keep_open: true,  // Don't block waiting
            browser_path: None,
        };

        match browser::launch_browser(opts) {
            Ok(child) => {
                println!("Chromium launched (PID: {}), waiting for extension to connect...", child.id());

                // Wait for extension to connect
                let timeout = Duration::from_secs(30);
                match browser::wait_for_extension_connection(port, timeout).await {
                    Ok(()) => {
                        println!("Extension connected!");
                    }
                    Err(e) => {
                        anyhow::bail!(
                            "Browser launched but extension connection timed out: {}\n\
                            Check that the playground is running at localhost:8083",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                anyhow::bail!(
                    "Failed to launch browser: {}\n\n\
                    Make sure Chromium is installed:\n  \
                    apt install chromium-browser (Debian/Ubuntu)\n  \
                    pacman -S chromium (Arch)\n  \
                    dnf install chromium (Fedora)",
                    e
                );
            }
        }
    }

    // Step 4: Final verification - wait for API to be ready
    // Note: WASM compilation can take 60-90s on first run, so we need a longer timeout
    let final_status = get_connection_status(port).await;
    if !matches!(final_status, ConnectionStatus::Ready) {
        // Wait for the playground WASM to load
        let initial_wait = Duration::from_secs(15);
        let max_retries = 3;

        println!("Waiting for playground API to be ready...");

        for retry in 0..max_retries {
            let start = Instant::now();

            // Wait with status updates
            while start.elapsed() < initial_wait {
                tokio::time::sleep(Duration::from_millis(500)).await;

                let status = get_connection_status(port).await;
                if matches!(status, ConnectionStatus::Ready) {
                    println!("Playground API ready!");
                    return Ok(setup);
                }
            }

            // If extension connected but API not ready, try refreshing the page
            // This fixes the issue where browser loads before WASM compilation finishes
            let status = get_connection_status(port).await;
            if matches!(status, ConnectionStatus::ExtensionConnectedNotReady) {
                if retry < max_retries - 1 {
                    println!("  WASM not ready after {}s, refreshing page (attempt {}/{})...",
                             initial_wait.as_secs(), retry + 1, max_retries);

                    // Send refresh command through WebSocket
                    let _ = send_command_to_server(port, WsCommand::Refresh).await;

                    // Wait a bit for refresh to complete
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            } else if matches!(status, ConnectionStatus::NoExtension) {
                println!("  Extension disconnected, waiting for reconnection...");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }

        // Final check
        if matches!(get_connection_status(port).await, ConnectionStatus::Ready) {
            println!("Playground API ready!");
            return Ok(setup);
        }

        anyhow::bail!(
            "Playground API not ready after {} retries. \
            Make sure the playground is running at localhost:8083",
            max_retries
        );
    }

    Ok(setup)
}

/// Kill mzoon server we started (port-based, like `makers kill`)
fn kill_mzoon_server() {
    use std::process::Command as StdCommand;

    println!("Stopping mzoon server we started...");

    // Find the process LISTENING on port 8083 (not browsers connecting to it)
    // This matches the approach in playground/Makefile.toml [tasks.kill]
    let pid_output = StdCommand::new("lsof")
        .args(["-ti:8083", "-sTCP:LISTEN"])
        .output();

    if let Ok(output) = pid_output {
        let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !pid_str.is_empty() {
            // Send TERM signal first (graceful shutdown)
            if let Ok(pid) = pid_str.parse::<i32>() {
                let _ = StdCommand::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
                println!("Sent TERM signal to server on port 8083 (PID: {})", pid);

                // Wait for graceful shutdown
                std::thread::sleep(std::time::Duration::from_secs(2));

                // Check if still running and force kill if needed
                let still_running = StdCommand::new("lsof")
                    .args(["-ti:8083", "-sTCP:LISTEN"])
                    .output()
                    .map(|o| !o.stdout.is_empty())
                    .unwrap_or(false);

                if still_running {
                    let _ = StdCommand::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .output();
                    println!("Force killed server (PID: {})", pid);
                }
            }
        }
    }

    println!("Mzoon server stopped.");
}

/// Run all discovered examples
pub async fn run_tests(opts: TestOptions) -> Result<Vec<TestResult>> {
    // Pre-flight check: ensure WebSocket server and browser extension are ready
    // This will auto-start the server and launch browser if needed
    let setup = ensure_browser_connection(opts.port).await?;

    // Run tests and ensure cleanup happens even on error
    let result = run_tests_inner(&opts).await;

    // Cleanup: if we started mzoon, kill it
    if setup.started_mzoon {
        kill_mzoon_server();
    }

    result
}

/// Inner test runner (separated for cleanup handling)
async fn run_tests_inner(opts: &TestOptions) -> Result<Vec<TestResult>> {
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

    // Clear states, reset URL to root, and refresh page before each test to ensure clean slate
    let _ = send_command_to_server(opts.port, WsCommand::ClearStates).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Navigate to root route - critical for Router/route() based apps like todo_mvc
    let _ = send_command_to_server(opts.port, WsCommand::NavigateTo { path: "/".to_string() }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = send_command_to_server(opts.port, WsCommand::Refresh).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

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
            // Check if there's already a focused input (e.g., in edit mode after dblclick)
            let focused_response = send_command_to_server(port, WsCommand::GetFocusedElement).await?;
            let input_already_focused = match focused_response {
                WsResponse::FocusedElement { tag_name, .. } => {
                    tag_name.as_deref() == Some("INPUT")
                }
                _ => false,
            };

            if input_already_focused {
                // Already in edit mode or input is focused
                // Use character-by-character typing to simulate real keyboard behavior
                let response = send_command_to_server(port, WsCommand::TypeTextCharByChar { text: text.clone() }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Type text char-by-char failed: {}", message);
                }
            } else {
                // Focus input first, then use char-by-char typing
                let focus_response = send_command_to_server(port, WsCommand::FocusInput { index: 0 }).await?;
                if let WsResponse::Error { message } = focus_response {
                    anyhow::bail!("Focus input failed: {}", message);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;

                let response = send_command_to_server(port, WsCommand::TypeTextCharByChar { text: text.clone() }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Type text char-by-char failed: {}", message);
                }
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
        ParsedAction::ClickButtonNearText { text, button_text } => {
            let response = send_command_to_server(port, WsCommand::ClickButtonNearText {
                text: text.clone(),
                button_text: button_text.clone()
            }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click button near text failed: {}", message);
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
        ParsedAction::AssertButtonCount { expected } => {
            // IMPORTANT: Clear hover state before counting buttons.
            // Delete buttons (×) in TodoMVC only appear on hover.
            //
            // NOTE: Zoon's on_hovered_change doesn't respond to synthetic
            // mouseenter/mouseleave events. We try multiple approaches but
            // this is a known limitation of the test infrastructure.
            //
            // Attempt 1: Move mouse outside preview area via CDP
            let _ = send_command_to_server(port, WsCommand::HoverAt { x: 0, y: 0 }).await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // Get preview elements and count buttons
            let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
            match response {
                WsResponse::PreviewElements { data } => {
                    let button_count = count_buttons_in_elements(&data);
                    if button_count != *expected {
                        anyhow::bail!(
                            "Assert button count failed: expected {} buttons, found {}",
                            expected, button_count
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert button count failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetPreviewElements"),
            }
        }
        ParsedAction::AssertCheckboxCount { expected } => {
            // Get preview elements and count checkboxes
            let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
            match response {
                WsResponse::PreviewElements { data } => {
                    let checkbox_count = count_checkboxes_in_elements(&data);
                    if checkbox_count != *expected {
                        anyhow::bail!(
                            "Assert checkbox count failed: expected {} checkboxes, found {}",
                            expected, checkbox_count
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert checkbox count failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetPreviewElements"),
            }
        }
        ParsedAction::AssertNotContains { text } => {
            // Get preview text and verify it does NOT contain the specified text
            let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
            match response {
                WsResponse::PreviewText { text: preview } => {
                    if preview.contains(text) {
                        anyhow::bail!(
                            "Assert not contains failed: preview should NOT contain '{}' but it does.\nPreview: {}",
                            text, truncate_for_error(&preview, 200)
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert not contains failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetPreviewText"),
            }
        }
        ParsedAction::AssertNotFocused { input_index } => {
            // Verify that a specific input does NOT have focus
            let response = send_command_to_server(port, WsCommand::GetFocusedElement).await?;
            match response {
                WsResponse::FocusedElement { input_index: actual_index, .. } => {
                    if actual_index == Some(*input_index) {
                        anyhow::bail!(
                            "Assert not focused failed: expected input {} to NOT be focused, but it is",
                            input_index
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert not focused failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetFocusedElement"),
            }
        }
        ParsedAction::AssertCheckboxUnchecked { index } => {
            // Verify that a specific checkbox is NOT checked
            let response = send_command_to_server(port, WsCommand::GetCheckboxState { index: *index }).await?;
            match response {
                WsResponse::CheckboxState { found, checked } => {
                    if !found {
                        anyhow::bail!("Assert checkbox unchecked failed: checkbox {} not found", index);
                    }
                    if checked {
                        anyhow::bail!(
                            "Assert checkbox unchecked failed: expected checkbox {} to be UNCHECKED, but it is checked",
                            index
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert checkbox unchecked failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetCheckboxState"),
            }
        }
        ParsedAction::AssertCheckboxChecked { index } => {
            // Verify that a specific checkbox IS checked
            let response = send_command_to_server(port, WsCommand::GetCheckboxState { index: *index }).await?;
            match response {
                WsResponse::CheckboxState { found, checked } => {
                    if !found {
                        anyhow::bail!("Assert checkbox checked failed: checkbox {} not found", index);
                    }
                    if !checked {
                        anyhow::bail!(
                            "Assert checkbox checked failed: expected checkbox {} to be CHECKED, but it is unchecked",
                            index
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert checkbox checked failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetCheckboxState"),
            }
        }
        ParsedAction::AssertButtonHasOutline { text } => {
            // Verify that a button with the given text has a visible outline
            let response = send_command_to_server(port, WsCommand::AssertButtonHasOutline { text: text.clone() }).await?;
            match response {
                WsResponse::Success { .. } => {}
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert button has outline failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for AssertButtonHasOutline"),
            }
        }
        ParsedAction::AssertToggleAllDarker => {
            // Verify that the toggle all checkbox icon is dark (all todos completed)
            let response = send_command_to_server(port, WsCommand::AssertToggleAllDarker).await?;
            match response {
                WsResponse::Success { .. } => {}
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert toggle all darker failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for AssertToggleAllDarker"),
            }
        }
        ParsedAction::AssertInputEmpty { index } => {
            // Verify that the input value is empty (cleared after action)
            let response = send_command_to_server(port, WsCommand::GetInputProperties { index: *index }).await?;
            match response {
                WsResponse::InputProperties { found, value, .. } => {
                    if !found {
                        anyhow::bail!("Assert input empty failed: input {} not found", index);
                    }
                    let actual_value = value.unwrap_or_default();
                    if !actual_value.is_empty() {
                        anyhow::bail!(
                            "Assert input empty failed: expected input {} to be empty, but got '{}'",
                            index, actual_value
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert input empty failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetInputProperties"),
            }
        }
        ParsedAction::AssertContains { text } => {
            // Verify that the preview contains the specified text
            let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
            match response {
                WsResponse::PreviewText { text: preview } => {
                    if !preview.contains(text) {
                        anyhow::bail!(
                            "Assert contains failed: preview should contain '{}' but it doesn't.\nPreview: {}",
                            text, truncate_for_error(&preview, 200)
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert contains failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetPreviewText"),
            }
        }
        ParsedAction::AssertCheckboxClickable { index } => {
            // Verify that a checkbox is ACTUALLY clickable by real user (not obscured)
            let response = send_command_to_server(port, WsCommand::AssertCheckboxClickable { index: *index }).await?;
            match response {
                WsResponse::Success { .. } => {}
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert checkbox clickable failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for AssertCheckboxClickable"),
            }
        }
    }
    Ok(())
}

/// Count DELETE buttons (×) in preview elements JSON
/// This specifically counts buttons that are delete buttons (text = "×"),
/// not navigation buttons like All/Active/Completed/Clear.
fn count_buttons_in_elements(data: &serde_json::Value) -> u32 {
    let mut count = 0;
    count_delete_buttons_recursive(data, &mut count);
    count
}

fn count_delete_buttons_recursive(value: &serde_json::Value, count: &mut u32) {
    match value {
        serde_json::Value::Object(obj) => {
            // Check if this element is a DELETE button (text = "×")
            let tag_name = obj.get("tagName").and_then(|v| v.as_str()).unwrap_or("");
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let text = obj.get("directText").and_then(|v| v.as_str()).unwrap_or("");

            // Only count buttons with × (delete buttons), not navigation buttons
            if (tag_name.eq_ignore_ascii_case("button") || role == "button") && text == "×" {
                *count += 1;
            }

            // Recurse into children and other values
            for (key, val) in obj {
                if key != "tagName" && key != "role" && key != "directText" {
                    count_delete_buttons_recursive(val, count);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                count_delete_buttons_recursive(item, count);
            }
        }
        _ => {}
    }
}

/// Count checkboxes in preview elements.
fn count_checkboxes_in_elements(data: &serde_json::Value) -> u32 {
    let mut count = 0;
    count_checkboxes_recursive(data, &mut count);
    count
}

fn count_checkboxes_recursive(value: &serde_json::Value, count: &mut u32) {
    match value {
        serde_json::Value::Object(obj) => {
            // Check if this element is a checkbox
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let tag_name = obj.get("tagName").and_then(|v| v.as_str()).unwrap_or("");
            let input_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // Count elements with role="checkbox" or input type="checkbox"
            if role == "checkbox" || (tag_name.eq_ignore_ascii_case("input") && input_type == "checkbox") {
                *count += 1;
            }

            // Recurse into children and other values
            for (key, val) in obj {
                if key != "tagName" && key != "role" && key != "type" {
                    count_checkboxes_recursive(val, count);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                count_checkboxes_recursive(item, count);
            }
        }
        _ => {}
    }
}

fn truncate_for_error(s: &str, max_len: usize) -> String {
    let s = s.replace('\n', " ").replace('\r', "");
    if s.len() > max_len {
        format!("{}...", &s[..max_len])
    } else {
        s
    }
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
        WsResponse::Screenshot { base64, .. } => {
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
