mod commands;
mod mcp;
mod ws_server;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use ws_server::{Command as WsCommand, Response as WsResponse};

#[derive(Parser)]
#[command(name = "boon-tools")]
#[command(about = "Boon Playground Browser Automation Tools")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start WebSocket server for extension communication
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },

    /// Launch and manage browser with extension
    Browser {
        #[command(subcommand)]
        action: BrowserAction,
    },

    /// Execute command via WebSocket server (requires extension)
    Exec {
        #[command(subcommand)]
        action: ExecAction,

        /// Server port
        #[arg(short, long, default_value = "9223")]
        port: u16,
    },

    /// Run MCP server for Claude Code integration (stdio JSON-RPC)
    Mcp {
        /// WebSocket server port to connect to
        #[arg(long, default_value = "9223")]
        port: u16,
    },
}

#[derive(Subcommand)]
enum BrowserAction {
    /// Launch Chromium with Boon extension pre-loaded
    Launch {
        /// Playground server port
        #[arg(long, default_value = "8083")]
        playground_port: u16,

        /// WebSocket server port
        #[arg(long, default_value = "9223")]
        ws_port: u16,

        /// Run in headless mode
        #[arg(long)]
        headless: bool,

        /// Keep browser open (don't wait for connection)
        #[arg(long)]
        keep_open: bool,

        /// Override browser binary path
        #[arg(long)]
        browser: Option<PathBuf>,

        /// Connection timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,
    },

    /// Kill all browser automation instances
    Kill,

    /// Check if Chromium is available
    Check,
}

#[derive(Subcommand)]
enum ServerAction {
    /// Start the WebSocket server
    Start {
        /// Port to listen on
        #[arg(short, long, default_value = "9223")]
        port: u16,

        /// Watch directory for extension hot reload
        #[arg(short, long)]
        watch: Option<String>,
    },
}

#[derive(Subcommand)]
enum ExecAction {
    /// Inject code into editor
    Inject {
        /// Code to inject
        code: String,
    },

    /// Trigger run
    Run,

    /// Take screenshot
    Screenshot {
        /// Output file path
        #[arg(short, long, default_value = "screenshot.png")]
        output: String,
    },

    /// Get preview text
    Preview,

    /// Click element by selector
    Click {
        /// CSS selector
        selector: String,
    },

    /// Type text into element
    Type {
        /// CSS selector
        selector: String,
        /// Text to type
        text: String,
    },

    /// Press a special key (Enter, Tab, Escape, Backspace, Delete)
    Key {
        /// Key name: Enter, Tab, Escape, Backspace, Delete
        key: String,
    },

    /// Check connection status
    Status,

    /// Get console messages from browser
    Console,

    /// Scroll the preview panel
    Scroll {
        /// Scroll to absolute Y position
        #[arg(short, long)]
        y: Option<i32>,

        /// Scroll by relative amount
        #[arg(short, long)]
        delta: Option<i32>,

        /// Scroll to bottom
        #[arg(long)]
        to_bottom: bool,
    },

    /// Detach CDP debugger (use when "debugger already attached" errors occur)
    Detach,

    /// Refresh the page without reloading extension (safer than reload)
    Refresh,

    /// Reload the extension (WARNING: disconnects extension, prefer 'refresh' for page reload)
    Reload,

    /// Full test: inject, run, check
    Test {
        /// Code to inject
        code: String,
        /// Expected text in preview
        #[arg(long)]
        expect: Option<String>,
        /// Screenshot output
        #[arg(short, long)]
        screenshot: Option<String>,
    },

    /// Get DOM structure (for debugging)
    Dom {
        /// CSS selector to start from (default: body)
        #[arg(short, long)]
        selector: Option<String>,
        /// Max depth to traverse
        #[arg(short, long, default_value = "4")]
        depth: u32,
    },

    /// Get preview panel elements with bounding boxes
    Elements,

    /// Click at absolute screen coordinates
    ClickAt {
        /// X coordinate
        x: i32,
        /// Y coordinate
        y: i32,
    },

    /// Hover at absolute screen coordinates (move mouse without clicking)
    HoverAt {
        /// X coordinate
        x: i32,
        /// Y coordinate
        y: i32,
    },

    /// Click element containing specific text in the preview panel
    ClickText {
        /// Text to find and click
        text: String,
        /// Match exact text (default: contains match)
        #[arg(long)]
        exact: bool,
    },

    /// Click checkbox by index in preview panel (0-indexed)
    ClickCheckbox {
        /// Checkbox index (0-based)
        index: u32,
    },

    /// Click button by index in preview panel (0-indexed, skips checkboxes)
    ClickButton {
        /// Button index (0-based)
        index: u32,
    },

    /// Double-click at absolute screen coordinates
    DblclickAt {
        /// X coordinate
        x: i32,
        /// Y coordinate
        y: i32,
    },

    /// Double-click element containing specific text in the preview panel
    DblclickText {
        /// Text to find and double-click
        text: String,
        /// Match exact text (default: contains match)
        #[arg(long)]
        exact: bool,
    },

    /// Clear saved states (reset localStorage for tests)
    ClearStates,

    /// Select an example by name (e.g., "todo_mvc.bn")
    Select {
        /// Example name (e.g., "todo_mvc.bn" or "todo_mvc")
        name: String,
    },

    /// Run all example tests (examples with .expected files)
    TestExamples {
        /// Only run examples matching pattern (e.g., "counter", "todo")
        #[arg(short, long)]
        filter: Option<String>,

        /// Pause and wait for user input on test failure
        #[arg(short, long)]
        interactive: bool,

        /// Save screenshots on failure
        #[arg(long)]
        screenshot_on_fail: bool,

        /// Show detailed output including step results
        #[arg(short, long)]
        verbose: bool,

        /// Path to examples directory (default: auto-detect)
        #[arg(long)]
        examples_dir: Option<PathBuf>,
    },

    /// Get localStorage entries (for debugging persistence)
    LocalStorage {
        /// Filter keys containing this pattern
        #[arg(short, long)]
        pattern: Option<String>,
    },

    /// Verify example file integrity (check for unauthorized modifications)
    VerifyIntegrity {
        /// Path to examples directory (default: auto-detect)
        #[arg(long)]
        examples_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Server { action } => match action {
            ServerAction::Start { port, watch } => {
                let rt = tokio::runtime::Runtime::new()?;
                let watch_path = watch.as_ref().map(std::path::Path::new);
                rt.block_on(ws_server::start_server(port, watch_path))?;
            }
        },

        Commands::Browser { action } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(handle_browser(action))?;
        }

        Commands::Exec { action, port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(handle_exec(action, port))?;
        }

        Commands::Mcp { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(port));
        }
    }

    Ok(())
}

async fn handle_browser(action: BrowserAction) -> Result<()> {
    use commands::browser;

    match action {
        BrowserAction::Launch {
            playground_port,
            ws_port,
            headless,
            keep_open,
            browser,
            timeout,
        } => {
            let opts = browser::LaunchOptions {
                playground_port,
                ws_port,
                headless,
                keep_open,
                browser_path: browser,
            };

            let mut child = browser::launch_browser(opts)?;

            if keep_open {
                println!("Browser launched. Process will run in background.");
                // Detach from the child process
                std::mem::forget(child);
            } else {
                // Wait for extension to connect
                let timeout_duration = std::time::Duration::from_secs(timeout);
                browser::wait_for_extension_connection(ws_port, timeout_duration).await?;
                println!("Browser ready. Press Ctrl+C to terminate.");

                // Wait for the browser process
                child.wait()?;
            }
        }

        BrowserAction::Kill => {
            browser::kill_browser_instances()?;
        }

        BrowserAction::Check => {
            match browser::check_chromium_available() {
                Ok(path) => {
                    println!("Chromium found: {}", path.display());
                }
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

async fn handle_exec(action: ExecAction, port: u16) -> Result<()> {
    use ws_server::send_command_to_server;

    match action {
        ExecAction::Inject { code } => {
            // Support @filename syntax to read code from file
            let code = if code.starts_with('@') {
                let path = &code[1..];
                std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", path, e))?
            } else {
                code
            };
            let response = send_command_to_server(port, WsCommand::InjectCode { code, filename: None }).await?;
            print_response(response);
        }

        ExecAction::Run => {
            let response = send_command_to_server(port, WsCommand::TriggerRun).await?;
            print_response(response);
        }

        ExecAction::Screenshot { output } => {
            let response = send_command_to_server(port, WsCommand::Screenshot).await?;
            match response {
                WsResponse::Screenshot { base64 } => {
                    let data = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &base64,
                    )?;
                    std::fs::write(&output, data)?;
                    println!("Screenshot saved to: {}", output);
                }
                WsResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }

        ExecAction::Preview => {
            let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
            match response {
                WsResponse::PreviewText { text } => {
                    println!("{}", text);
                }
                _ => print_response(response),
            }
        }

        ExecAction::Click { selector } => {
            let response = send_command_to_server(port, WsCommand::Click { selector }).await?;
            print_response(response);
        }

        ExecAction::Type { selector, text } => {
            let response = send_command_to_server(port, WsCommand::Type { selector, text }).await?;
            print_response(response);
        }

        ExecAction::Key { key } => {
            let response = send_command_to_server(port, WsCommand::Key { key }).await?;
            print_response(response);
        }

        ExecAction::Status => {
            let response = send_command_to_server(port, WsCommand::GetStatus).await?;
            print_response(response);
        }

        ExecAction::Console => {
            let response = send_command_to_server(port, WsCommand::GetConsole).await?;
            match response {
                WsResponse::Console { messages } => {
                    if messages.is_empty() {
                        println!("No console messages captured.");
                    } else {
                        for msg in messages {
                            let level_indicator = match msg.level.as_str() {
                                "error" => "[ERROR]",
                                "warn" => "[WARN]",
                                "info" => "[INFO]",
                                _ => "[LOG]",
                            };
                            println!("{} {}", level_indicator, msg.text);
                        }
                    }
                }
                _ => print_response(response),
            }
        }

        ExecAction::Scroll { y, delta, to_bottom } => {
            let response = send_command_to_server(
                port,
                WsCommand::Scroll { y, delta, to_bottom },
            )
            .await?;
            print_response(response);
        }

        ExecAction::Detach => {
            println!("Detaching CDP debugger...");
            let response = send_command_to_server(port, WsCommand::Detach).await?;
            print_response(response);
        }

        ExecAction::Refresh => {
            println!("Refreshing page (extension stays connected)...");
            let response = send_command_to_server(port, WsCommand::Refresh).await?;
            print_response(response);
        }

        ExecAction::Reload => {
            eprintln!("WARNING: 'reload' disconnects the extension. Consider using 'refresh' instead.");
            println!("Sending reload command to extension...");
            let response = send_command_to_server(port, WsCommand::Reload).await?;
            print_response(response);
        }

        ExecAction::Test { code, expect, screenshot } => {
            // Support @filename syntax to read code from file
            let code = if code.starts_with('@') {
                let path = &code[1..];
                std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", path, e))?
            } else {
                code
            };
            // Inject code
            println!("Injecting code...");
            let response = send_command_to_server(port, WsCommand::InjectCode { code, filename: None }).await?;
            if matches!(response, WsResponse::Error { .. }) {
                print_response(response);
                return Ok(());
            }

            // Trigger run
            println!("Triggering run...");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let response = send_command_to_server(port, WsCommand::TriggerRun).await?;
            if matches!(response, WsResponse::Error { .. }) {
                print_response(response);
                return Ok(());
            }

            // Wait for execution
            println!("Waiting for execution...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // Get preview text
            let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
            match response {
                WsResponse::PreviewText { text } => {
                    println!("Preview: {}", text);
                    if let Some(expected) = expect {
                        if text.contains(&expected) {
                            println!("PASS: Found expected text '{}'", expected);
                        } else {
                            println!("FAIL: Expected '{}' not found", expected);
                        }
                    }
                }
                _ => print_response(response),
            }

            // Take screenshot if requested
            if let Some(output) = screenshot {
                let response = send_command_to_server(port, WsCommand::Screenshot).await?;
                if let WsResponse::Screenshot { base64 } = response {
                    let data = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &base64,
                    )?;
                    std::fs::write(&output, data)?;
                    println!("Screenshot saved to: {}", output);
                }
            }
        }

        ExecAction::Dom { selector, depth } => {
            let response = send_command_to_server(
                port,
                WsCommand::GetDOM { selector, depth: Some(depth) },
            )
            .await?;
            match response {
                WsResponse::Dom { structure } => {
                    println!("{}", structure);
                }
                _ => print_response(response),
            }
        }

        ExecAction::Elements => {
            let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
            match response {
                WsResponse::PreviewElements { data } => {
                    println!("{}", serde_json::to_string_pretty(&data).unwrap());
                }
                _ => print_response(response),
            }
        }

        ExecAction::ClickAt { x, y } => {
            let response = send_command_to_server(port, WsCommand::ClickAt { x, y }).await?;
            print_response(response);
        }

        ExecAction::HoverAt { x, y } => {
            let response = send_command_to_server(port, WsCommand::HoverAt { x, y }).await?;
            print_response(response);
        }

        ExecAction::ClickText { text, exact } => {
            // Get preview elements to find the one containing the text
            let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
            match response {
                WsResponse::PreviewElements { data } => {
                    if let Some(element) = find_element_by_text(&data, &text, exact) {
                        let x = element.x + element.width / 2;
                        let y = element.y + element.height / 2;
                        println!("Found '{}' at ({}, {}), clicking...", text, x, y);
                        let response = send_command_to_server(port, WsCommand::ClickAt { x, y }).await?;
                        print_response(response);
                    } else {
                        eprintln!("Error: No element found containing text '{}'", text);
                        std::process::exit(1);
                    }
                }
                WsResponse::Error { message } => {
                    eprintln!("Error getting elements: {}", message);
                    std::process::exit(1);
                }
                _ => {
                    eprintln!("Unexpected response");
                    std::process::exit(1);
                }
            }
        }

        ExecAction::ClickCheckbox { index } => {
            println!("Clicking checkbox {}...", index);
            let response = send_command_to_server(port, WsCommand::ClickCheckbox { index }).await?;
            print_response(response);
        }

        ExecAction::ClickButton { index } => {
            println!("Clicking button {}...", index);
            let response = send_command_to_server(port, WsCommand::ClickButton { index }).await?;
            print_response(response);
        }

        ExecAction::DblclickAt { x, y } => {
            let response = send_command_to_server(port, WsCommand::DoubleClickAt { x, y }).await?;
            print_response(response);
        }

        ExecAction::DblclickText { text, exact } => {
            // Get preview elements to find the one containing the text
            let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
            match response {
                WsResponse::PreviewElements { data } => {
                    if let Some(element) = find_element_by_text(&data, &text, exact) {
                        let x = element.x + element.width / 2;
                        let y = element.y + element.height / 2;
                        println!("Found '{}' at ({}, {}), double-clicking...", text, x, y);
                        let response = send_command_to_server(port, WsCommand::DoubleClickAt { x, y }).await?;
                        print_response(response);
                    } else {
                        eprintln!("Error: No element found containing text '{}'", text);
                        std::process::exit(1);
                    }
                }
                WsResponse::Error { message } => {
                    eprintln!("Error getting elements: {}", message);
                    std::process::exit(1);
                }
                _ => {
                    eprintln!("Unexpected response");
                    std::process::exit(1);
                }
            }
        }

        ExecAction::ClearStates => {
            println!("Clearing saved states...");
            let response = send_command_to_server(port, WsCommand::ClearStates).await?;
            print_response(response);
        }

        ExecAction::Select { name } => {
            // Add .bn suffix if not present
            let example_name = if name.ends_with(".bn") {
                name
            } else {
                format!("{}.bn", name)
            };
            println!("Selecting example: {}", example_name);
            let response =
                send_command_to_server(port, WsCommand::SelectExample { name: example_name })
                    .await?;
            print_response(response);
        }

        ExecAction::TestExamples {
            filter,
            interactive,
            screenshot_on_fail,
            verbose,
            examples_dir,
        } => {
            use commands::test_examples::{run_tests, TestOptions};

            let opts = TestOptions {
                port,
                filter,
                interactive,
                screenshot_on_fail,
                verbose,
                examples_dir,
            };

            let results = run_tests(opts).await?;

            // Exit with error code if any tests failed
            let all_passed = results.iter().all(|r| r.passed);
            if !all_passed {
                std::process::exit(1);
            }
        }

        ExecAction::LocalStorage { pattern } => {
            let response = send_command_to_server(port, WsCommand::GetLocalStorage { pattern }).await?;
            match response {
                WsResponse::LocalStorage { entries } => {
                    if let Some(obj) = entries.as_object() {
                        if obj.is_empty() {
                            println!("No localStorage entries found.");
                        } else {
                            println!("Found {} localStorage entries:", obj.len());
                            for (key, value) in obj {
                                let value_owned = value.to_string();
                                let value_str = value.as_str().unwrap_or(&value_owned);
                                // Truncate very long values
                                let display_value = if value_str.len() > 100 {
                                    format!("{}...[truncated]", &value_str[..100])
                                } else {
                                    value_str.to_string()
                                };
                                println!("  {}: {}", key, display_value);
                            }
                        }
                    } else {
                        println!("{}", entries);
                    }
                }
                _ => print_response(response),
            }
        }

        ExecAction::VerifyIntegrity { examples_dir } => {
            use commands::verify_integrity::run_integrity_check;

            let passed = run_integrity_check(examples_dir)?;
            if !passed {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

/// Element bounds for click-by-text
struct ElementBounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// Recursively find an element containing the specified text
fn find_element_by_text(data: &serde_json::Value, text: &str, exact: bool) -> Option<ElementBounds> {
    // Try to find a matching element in the JSON structure
    // The GetPreviewElements returns a nested structure with text and bounds
    find_element_by_text_recursive(data, text, exact)
}

fn find_element_by_text_recursive(value: &serde_json::Value, text: &str, exact: bool) -> Option<ElementBounds> {
    match value {
        serde_json::Value::Object(obj) => {
            // Check if this element has matching text
            // GetPreviewElements returns 'directText' (direct child text nodes) and 'fullText' (all text content)
            let has_match = {
                // Try 'directText' field first (more precise match)
                if let Some(elem_text) = obj.get("directText").and_then(|t| t.as_str()) {
                    if exact {
                        elem_text.trim() == text
                    } else {
                        elem_text.contains(text)
                    }
                // Then try 'fullText' field
                } else if let Some(elem_text) = obj.get("fullText").and_then(|t| t.as_str()) {
                    if exact {
                        elem_text.trim() == text
                    } else {
                        elem_text.contains(text)
                    }
                // Legacy: Try 'text' field
                } else if let Some(elem_text) = obj.get("text").and_then(|t| t.as_str()) {
                    if exact {
                        elem_text.trim() == text
                    } else {
                        elem_text.contains(text)
                    }
                } else {
                    false
                }
            };

            if has_match {
                // Try to extract bounds
                if let (Some(x), Some(y), Some(width), Some(height)) = (
                    obj.get("x").and_then(|v| v.as_f64()),
                    obj.get("y").and_then(|v| v.as_f64()),
                    obj.get("width").and_then(|v| v.as_f64()),
                    obj.get("height").and_then(|v| v.as_f64()),
                ) {
                    return Some(ElementBounds {
                        x: x as i32,
                        y: y as i32,
                        width: width as i32,
                        height: height as i32,
                    });
                }
            }

            // Search in children
            if let Some(children) = obj.get("children") {
                if let Some(result) = find_element_by_text_recursive(children, text, exact) {
                    return Some(result);
                }
            }

            // Search in other object values
            for (key, val) in obj {
                if key != "text" && key != "children" {
                    if let Some(result) = find_element_by_text_recursive(val, text, exact) {
                        return Some(result);
                    }
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(result) = find_element_by_text_recursive(item, text, exact) {
                    return Some(result);
                }
            }
        }
        _ => {}
    }
    None
}

fn print_response(response: WsResponse) {
    match response {
        WsResponse::Success { data } => {
            println!("Success");
            if let Some(data) = data {
                println!("{}", serde_json::to_string_pretty(&data).unwrap());
            }
        }
        WsResponse::Error { message } => {
            eprintln!("Error: {}", message);
        }
        WsResponse::Pong => {
            println!("Pong");
        }
        WsResponse::Status { connected, page_url, api_ready } => {
            println!("Connected: {}", connected);
            println!("Page URL: {:?}", page_url);
            println!("API Ready: {}", api_ready);
        }
        other => {
            println!("{:?}", other);
        }
    }
}
