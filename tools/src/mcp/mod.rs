//! MCP (Model Context Protocol) server implementation for Boon browser automation
//!
//! Provides browser automation tools to Claude Code via stdio JSON-RPC.
//! Automatically starts the WebSocket server for extension communication.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use crate::commands::browser;
use crate::ws_server::{self, Command, Response};

/// MCP JSON-RPC request
#[derive(Debug, Deserialize)]
struct McpRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// MCP JSON-RPC response
#[derive(Debug, Serialize)]
struct McpResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<McpError>,
}

#[derive(Debug, Serialize)]
struct McpError {
    code: i32,
    message: String,
}

/// Tool definition for MCP
#[derive(Debug, Serialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

/// Find the extension directory relative to the binary or workspace
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

/// Run the MCP server (stdio-based)
/// Automatically starts the WebSocket server and optionally launches browser
pub async fn run_mcp_server(ws_port: u16) {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    eprintln!("[MCP] Boon browser server starting (ws_port: {})...", ws_port);

    // Find extension directory for hot-reload watching
    let extension_dir = find_extension_dir();
    if let Some(ref dir) = extension_dir {
        eprintln!("[MCP] Found extension directory: {}", dir.display());
    } else {
        eprintln!("[MCP] Extension directory not found, hot-reload disabled");
    }

    // Start WebSocket server in background
    let watch_path = extension_dir.clone();
    tokio::spawn(async move {
        eprintln!("[MCP] Starting WebSocket server on port {}...", ws_port);
        if let Err(e) = ws_server::start_server(ws_port, watch_path.as_deref()).await {
            eprintln!("[MCP] WebSocket server error: {}", e);
        }
    });

    // Give the WebSocket server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    eprintln!("[MCP] WebSocket server started, ready for browser connections");
    eprintln!("[MCP] Use boon_launch_browser tool to start browser with extension");

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[MCP] Read error: {}", e);
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        eprintln!("[MCP] Received: {}", line);

        let request: McpRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[MCP] Parse error: {}", e);
                continue;
            }
        };

        let response = handle_request(request, ws_port).await;

        let response_json = serde_json::to_string(&response).unwrap();
        eprintln!("[MCP] Sending: {}", response_json);

        writeln!(stdout, "{}", response_json).unwrap();
        stdout.flush().unwrap();
    }
}

async fn handle_request(request: McpRequest, ws_port: u16) -> McpResponse {
    let id = request.id.unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => McpResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "boon-browser",
                    "version": "0.1.0"
                }
            })),
            error: None,
        },

        "tools/list" => McpResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!({
                "tools": get_tools()
            })),
            error: None,
        },

        "tools/call" => {
            let tool_name = request.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = request.params.get("arguments").cloned().unwrap_or(json!({}));

            match call_tool(tool_name, arguments, ws_port).await {
                Ok(result) => McpResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({
                        "content": [{
                            "type": "text",
                            "text": result
                        }]
                    })),
                    error: None,
                },
                Err(e) => McpResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(McpError {
                        code: -32000,
                        message: e,
                    }),
                },
            }
        }

        "notifications/initialized" => {
            // No response needed for notifications
            McpResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!(null)),
                error: None,
            }
        }

        _ => McpResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(McpError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
            }),
        },
    }
}

fn get_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "boon_console".to_string(),
            description: "Get browser console logs from the Boon playground. Returns all console messages (log, warn, error, info) captured since page load.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_preview".to_string(),
            description: "Get the text content of the Boon playground preview panel. Returns the rendered output of the current Boon code.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_refresh".to_string(),
            description: "Refresh the Boon playground page without disconnecting the browser extension. Use this instead of reload to keep automation working.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_status".to_string(),
            description: "Check the browser extension connection status. Returns whether the extension is connected, the current page URL, and if the Boon API is ready.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_screenshot".to_string(),
            description: "Take a screenshot of the current browser tab. Saves PNG to /tmp/boon-screenshots/ and returns the file path.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_select_example".to_string(),
            description: "Select and load a Boon example by name. Examples include: counter, interval, todo_mvc, shopping_list, etc.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Example name without .bn extension (e.g., 'counter', 'shopping_list', 'todo_mvc')"
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "boon_run".to_string(),
            description: "Trigger execution of the current Boon code in the playground. Equivalent to clicking the Run button.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_inject".to_string(),
            description: "Inject Boon code into the playground editor. Replaces the current editor content with the provided code.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The Boon code to inject into the editor"
                    }
                },
                "required": ["code"]
            }),
        },
        Tool {
            name: "boon_detach".to_string(),
            description: "Detach the CDP debugger. Use when encountering 'debugger already attached' errors.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_launch_browser".to_string(),
            description: "Launch Chromium browser with the Boon extension pre-loaded. Opens the playground at localhost:8081. The browser will automatically connect to the WebSocket server.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "headless": {
                        "type": "boolean",
                        "description": "Run browser in headless mode (default: false)"
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "boon_get_code".to_string(),
            description: "Get the current Boon code from the playground editor.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_screenshot_preview".to_string(),
            description: "Take a screenshot of just the preview pane (not the whole page). Saves PNG to /tmp/boon-screenshots/ and returns the file path.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_screenshot_element".to_string(),
            description: "Take a screenshot of a specific element by CSS selector. Saves PNG to /tmp/boon-screenshots/ and returns the file path.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the element to screenshot"
                    }
                },
                "required": ["selector"]
            }),
        },
        Tool {
            name: "boon_accessibility_tree".to_string(),
            description: "Get the accessibility tree of the preview pane. Useful for understanding the semantic structure of rendered UI.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_clear_states".to_string(),
            description: "Clear saved states in the Boon playground (resets localStorage). Useful for testing fresh state.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_playground_status".to_string(),
            description: "Check if the Boon playground dev server (mzoon) is running and healthy. Returns server status and any compilation errors.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_start_playground".to_string(),
            description: "Start the Boon playground dev server (mzoon). Runs 'cd playground && makers mzoon start' in background.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "boon_click_text".to_string(),
            description: "Click an element in the preview panel by its text content. More reliable than coordinate-based clicking when UI positions change.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to find and click (e.g., 'All', 'Active', 'Completed')"
                    },
                    "exact": {
                        "type": "boolean",
                        "description": "If true, match exact text. If false (default), match if text contains the search string."
                    }
                },
                "required": ["text"]
            }),
        },
    ]
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

async fn call_tool(name: &str, args: Value, ws_port: u16) -> Result<String, String> {
    // Handle playground status check (no WebSocket)
    if name == "boon_playground_status" {
        return check_playground_status().await;
    }

    // Handle playground start (no WebSocket)
    if name == "boon_start_playground" {
        return start_playground().await;
    }

    // Handle click-by-text (compound command)
    if name == "boon_click_text" {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or("text parameter required")?;
        let exact = args.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);

        return click_element_by_text(text, exact, ws_port).await;
    }

    // Handle browser launch separately (doesn't use WebSocket command)
    if name == "boon_launch_browser" {
        let headless = args.get("headless").and_then(|v| v.as_bool()).unwrap_or(false);

        let opts = browser::LaunchOptions {
            playground_port: 8081,
            ws_port,
            headless,
            keep_open: true,  // Don't block waiting
            browser_path: None,
        };

        match browser::launch_browser(opts) {
            Ok(child) => {
                // Wait for extension to connect (with timeout)
                let timeout = std::time::Duration::from_secs(15);
                match browser::wait_for_extension_connection(ws_port, timeout).await {
                    Ok(()) => Ok(format!(
                        "Browser launched successfully (PID: {}).\nExtension connected and ready.",
                        child.id()
                    )),
                    Err(e) => Ok(format!(
                        "Browser launched (PID: {}) but extension connection timed out: {}\n\
                        Check that the playground is running at localhost:8081",
                        child.id(), e
                    )),
                }
            }
            Err(e) => Err(format!("Failed to launch browser: {}", e)),
        }
    } else {
        call_ws_tool(name, args, ws_port).await
    }
}

async fn call_ws_tool(name: &str, args: Value, ws_port: u16) -> Result<String, String> {
    let command = match name {
        "boon_console" => Command::GetConsole,
        "boon_preview" => Command::GetPreviewText,
        "boon_refresh" => Command::Refresh,
        "boon_status" => Command::GetStatus,
        "boon_screenshot" => Command::Screenshot,
        "boon_run" => Command::TriggerRun,
        "boon_detach" => Command::Detach,

        "boon_select_example" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("name parameter required")?
                .to_string();
            // Add .bn suffix if not present
            let example_name = if name.ends_with(".bn") {
                name
            } else {
                format!("{}.bn", name)
            };
            Command::SelectExample { name: example_name }
        }

        "boon_inject" => {
            let code = args
                .get("code")
                .and_then(|v| v.as_str())
                .ok_or("code parameter required")?
                .to_string();
            Command::InjectCode { code }
        }

        "boon_get_code" => Command::GetEditorCode,

        "boon_screenshot_preview" => Command::ScreenshotElement {
            selector: "[data-boon-panel=\"preview\"]".to_string(),
        },

        "boon_screenshot_element" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or("selector parameter required")?
                .to_string();
            Command::ScreenshotElement { selector }
        }

        "boon_accessibility_tree" => Command::GetAccessibilityTree,

        "boon_clear_states" => Command::ClearStates,

        _ => return Err(format!("Unknown tool: {}", name)),
    };

    let response = ws_server::send_command_to_server(ws_port, command)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        Response::Console { messages } => {
            if messages.is_empty() {
                Ok("No console messages captured.".to_string())
            } else {
                let formatted: Vec<String> = messages
                    .iter()
                    .map(|msg| {
                        let level = match msg.level.as_str() {
                            "error" => "[ERROR]",
                            "warn" => "[WARN]",
                            "info" => "[INFO]",
                            _ => "[LOG]",
                        };
                        format!("{} {}", level, msg.text)
                    })
                    .collect();
                Ok(formatted.join("\n"))
            }
        }

        Response::PreviewText { text } => Ok(text),

        Response::Screenshot { base64: _ } => {
            // This shouldn't happen - WS server transforms to ScreenshotFile
            Err("Unexpected base64 screenshot response".to_string())
        }

        Response::ScreenshotFile { filepath } => {
            Ok(format!("Screenshot saved: {}", filepath))
        }

        Response::Status { connected, page_url, api_ready } => {
            let mut status = format!("Connected: {}", connected);
            if let Some(url) = page_url {
                status.push_str(&format!("\nPage URL: {}", url));
            }
            status.push_str(&format!("\nAPI Ready: {}", api_ready));
            Ok(status)
        }

        Response::Success { data } => {
            if let Some(d) = data {
                Ok(format!("Success: {}", serde_json::to_string_pretty(&d).unwrap_or_default()))
            } else {
                Ok("Success".to_string())
            }
        }

        Response::Pong => Ok("Pong!".to_string()),

        Response::Dom { structure } => Ok(format!("DOM:\n{}", structure)),

        Response::PreviewElements { data } => {
            Ok(serde_json::to_string_pretty(&data).unwrap_or_default())
        }

        Response::EditorCode { code } => Ok(code),

        Response::AccessibilityTree { tree } => {
            Ok(serde_json::to_string_pretty(&tree).unwrap_or_default())
        }

        Response::Error { message } => Err(message),
    }
}

/// Check if the playground dev server is running and healthy
async fn check_playground_status() -> Result<String, String> {
    use std::process::Command as StdCommand;

    let mut status = String::new();

    // Check if port 8081 is listening
    let port_check = StdCommand::new("sh")
        .args(["-c", "lsof -i :8081 2>/dev/null | grep LISTEN | head -5"])
        .output();

    match port_check {
        Ok(output) if !output.stdout.is_empty() => {
            status.push_str("Port 8081: LISTENING\n");
            let processes = String::from_utf8_lossy(&output.stdout);
            status.push_str(&format!("Processes:\n{}\n", processes));
        }
        _ => {
            status.push_str("Port 8081: NOT LISTENING\n");
            status.push_str("Playground server is not running.\n");
            status.push_str("Start with: cd playground && makers mzoon start\n");
            return Ok(status);
        }
    }

    // Try to fetch the playground page
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    match client.get("http://localhost:8081").send().await {
        Ok(response) => {
            status.push_str(&format!("HTTP Status: {}\n", response.status()));

            if response.status().is_success() {
                let body = response.text().await.unwrap_or_default();

                // Check for WASM loading
                if body.contains("wasm") || body.contains("boon") {
                    status.push_str("WASM: Appears to be loading\n");
                }

                // Check for error indicators
                if body.contains("error") || body.contains("Error") {
                    status.push_str("WARNING: Page may contain errors\n");
                }

                status.push_str("Server: HEALTHY\n");
            } else {
                status.push_str("Server: UNHEALTHY\n");
            }
        }
        Err(e) => {
            status.push_str(&format!("HTTP Error: {}\n", e));
            status.push_str("Server: UNREACHABLE\n");
        }
    }

    // Check for recent mzoon/cargo processes
    let mzoon_check = StdCommand::new("sh")
        .args(["-c", "ps aux | grep -E 'mzoon|cargo.*boon' | grep -v grep | head -5"])
        .output();

    if let Ok(output) = mzoon_check {
        if !output.stdout.is_empty() {
            status.push_str("\nBuild processes:\n");
            status.push_str(&String::from_utf8_lossy(&output.stdout));
        }
    }

    Ok(status)
}

/// Click an element by its text content
async fn click_element_by_text(text: &str, exact: bool, ws_port: u16) -> Result<String, String> {
    // First get preview elements
    let response = ws_server::send_command_to_server(ws_port, Command::GetPreviewElements)
        .await
        .map_err(|e| e.to_string())?;

    match response {
        Response::PreviewElements { data } => {
            if let Some((x, y, width, height)) = find_element_bounds_by_text(&data, text, exact) {
                let click_x = x + width / 2;
                let click_y = y + height / 2;

                // Click at the center of the element
                let response = ws_server::send_command_to_server(
                    ws_port,
                    Command::ClickAt { x: click_x, y: click_y },
                )
                .await
                .map_err(|e| e.to_string())?;

                match response {
                    Response::Success { .. } => Ok(format!(
                        "Clicked '{}' at ({}, {})",
                        text, click_x, click_y
                    )),
                    Response::Error { message } => Err(format!("Click failed: {}", message)),
                    _ => Ok(format!("Clicked '{}' at ({}, {})", text, click_x, click_y)),
                }
            } else {
                Err(format!("No element found containing text '{}'", text))
            }
        }
        Response::Error { message } => Err(format!("Failed to get elements: {}", message)),
        _ => Err("Unexpected response from GetPreviewElements".to_string()),
    }
}

/// Recursively find element bounds by text content
fn find_element_bounds_by_text(value: &Value, text: &str, exact: bool) -> Option<(i32, i32, i32, i32)> {
    match value {
        Value::Object(obj) => {
            // Check if this element has matching text (in 'text' or 'html' field)
            let has_match = {
                // Try 'text' field first
                if let Some(elem_text) = obj.get("text").and_then(|t| t.as_str()) {
                    if exact {
                        elem_text.trim() == text
                    } else {
                        elem_text.contains(text)
                    }
                // Then try 'html' field (preview elements use this)
                } else if let Some(html) = obj.get("html").and_then(|t| t.as_str()) {
                    // Extract text content from HTML (simple extraction between > and <)
                    if exact {
                        // For exact match, look for >text< pattern
                        html.contains(&format!(">{}<", text)) || html.contains(&format!(">{}\"", text))
                    } else {
                        html.contains(text)
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
                    return Some((x as i32, y as i32, width as i32, height as i32));
                }
            }

            // Search in children first
            if let Some(children) = obj.get("children") {
                if let Some(result) = find_element_bounds_by_text(children, text, exact) {
                    return Some(result);
                }
            }

            // Search in other object values
            for (key, val) in obj {
                if key != "text" && key != "children" {
                    if let Some(result) = find_element_bounds_by_text(val, text, exact) {
                        return Some(result);
                    }
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(result) = find_element_bounds_by_text(item, text, exact) {
                    return Some(result);
                }
            }
        }
        _ => {}
    }
    None
}

/// Start the playground dev server
async fn start_playground() -> Result<String, String> {
    use std::process::Command as StdCommand;

    let boon_root = find_boon_root()
        .ok_or("Could not find boon repository root")?;

    let playground_dir = boon_root.join("playground");

    if !playground_dir.exists() {
        return Err(format!("Playground directory not found: {}", playground_dir.display()));
    }

    // Start mzoon in background
    let result = StdCommand::new("sh")
        .args(["-c", &format!(
            "cd {} && nohup makers mzoon start > /tmp/mzoon.log 2>&1 &",
            playground_dir.display()
        )])
        .output();

    match result {
        Ok(_) => {
            // Wait a moment for server to start
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            Ok(format!(
                "Started mzoon in background.\n\
                Log file: /tmp/mzoon.log\n\
                Working directory: {}\n\
                Note: Initial compilation takes 1-2 minutes.\n\
                Use boon_playground_status to check progress.",
                playground_dir.display()
            ))
        }
        Err(e) => Err(format!("Failed to start mzoon: {}", e)),
    }
}
