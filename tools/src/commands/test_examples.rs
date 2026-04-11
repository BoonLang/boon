//! Test runner for Boon playground examples

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

use crate::commands::{browser, resolve_requested_engine};
use crate::ws_server::{
    self, send_command_to_server, Command as WsCommand, Response as WsResponse,
};

use super::expected::{
    matches_inline, parse_interaction_sequences, shared_example_parsed_sequences,
    validate_required_shared_sequences, ExpectedSpec, MatchMode, OutputSpec, ParsedAction,
};

/// Options for test-examples command
pub struct TestOptions {
    pub port: u16,
    pub playground_port: u16,
    pub filter: Option<String>,
    pub interactive: bool,
    pub screenshot_on_fail: bool,
    pub verbose: bool,
    pub examples_dir: Option<PathBuf>,
    #[allow(dead_code)]
    pub no_launch: bool,
    pub engine: Option<String>,
    pub skip_persistence: bool,
}

/// Options for smoke-examples command
pub struct SmokeOptions {
    pub port: u16,
    pub playground_port: u16,
    pub filter: Option<String>,
    #[allow(dead_code)]
    pub no_launch: bool,
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
    pub select_name: String,
    pub editor_filename: String,
    #[allow(dead_code)]
    pub bn_path: PathBuf,
    pub expected_path: PathBuf,
}

fn engine_query_value(engine: &str) -> Option<&'static str> {
    match engine {
        "Actors" => Some("actors"),
        "ActorsLite" => Some("actorslite"),
        "FactoryFabric" => Some("factoryfabric"),
        "DD" => Some("dd"),
        "Wasm" => Some("wasm"),
        _ => None,
    }
}

fn example_route_path(example_name: &str, engine: Option<&str>) -> String {
    let mut path = format!("/?example={}", example_name.trim_end_matches(".bn"));
    if let Some(engine) = engine.and_then(engine_query_value) {
        path.push_str("&engine=");
        path.push_str(engine);
    }
    path.push_str("&autorun=0");
    path
}

fn post_refresh_delay_ms(initial_delay_ms: u64) -> u64 {
    // The harness now disables autorun and triggers a single explicit run after refresh.
    // Keep a modest settle floor so the first example after extension reconnect has time
    // to finish wiring the page API before that explicit run.
    std::cmp::max(initial_delay_ms, 500)
}

fn should_reselect_example_after_refresh(
    engine: Option<&str>,
    target_file_already_loaded: bool,
) -> bool {
    engine == Some("ActorsLite") && !target_file_already_loaded
}

fn normalize_immediate_initial_preview(preview: Option<String>) -> Option<String> {
    match preview.as_deref() {
        None | Some("Run to see preview") => Some(String::new()),
        _ => preview,
    }
}

async fn send_command_with_timeout(
    port: u16,
    command: WsCommand,
    timeout: Duration,
) -> Result<WsResponse> {
    match tokio::time::timeout(timeout, send_command_to_server(port, command)).await {
        Ok(response) => response,
        Err(_) => anyhow::bail!("Extension response timeout"),
    }
}

async fn get_preview_text_via_eval(port: u16) -> Result<Option<String>> {
    let response = send_command_with_timeout(
        port,
        WsCommand::EvalJs {
            expression: r#"(function() {
                const preview = document.querySelector('[data-boon-panel="preview"]');
                if (!preview) return null;
                const limit = 8192;
                const walker = document.createTreeWalker(preview, NodeFilter.SHOW_TEXT);
                let text = '';
                let node = walker.nextNode();
                while (node) {
                    text += node.textContent || '';
                    if (text.length >= limit) {
                        text = text.substring(0, limit);
                        break;
                    }
                    node = walker.nextNode();
                }
                return text;
            })()"#
                .to_string(),
        },
        Duration::from_secs(2),
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::String(text)),
        } => Ok(Some(text)),
        WsResponse::Success {
            data: Some(serde_json::Value::Null) | None,
        } => Ok(None),
        WsResponse::Error { message } => anyhow::bail!("Eval preview text failed: {}", message),
        _ => Ok(None),
    }
}

async fn get_preview_text(port: u16) -> Result<Option<String>> {
    for attempt in 0..3 {
        let direct =
            send_command_with_timeout(port, WsCommand::GetPreviewText, Duration::from_secs(2))
                .await;
        match direct {
            Ok(WsResponse::PreviewText { text }) => return Ok(Some(text)),
            Ok(WsResponse::Error { message }) => {
                if !is_retryable_browser_error(&message) {
                    anyhow::bail!("GetPreviewText failed: {}", message);
                }
            }
            Ok(_) => return Ok(None),
            Err(error) => {
                if !is_retryable_browser_error(&error.to_string()) {
                    return Err(error);
                }
            }
        }

        if let Ok(Some(text)) = get_preview_text_via_eval(port).await {
            return Ok(Some(text));
        }

        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    Ok(None)
}

fn is_retryable_browser_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout")
        || lower.contains("frame with id 0 was removed")
        || lower.contains("execution context was destroyed")
        || lower.contains("inspected target navigated or closed")
        || lower.contains("cannot find context with specified id")
}

async fn wait_for_non_retry_preview_text(port: u16, timeout: Duration) -> Result<Option<String>> {
    let start = Instant::now();
    let mut last_preview = None;

    while start.elapsed() <= timeout {
        let preview = get_preview_text(port).await?;
        if !preview_needs_retry(preview.as_deref()) {
            return Ok(preview);
        }
        if preview.is_some() {
            last_preview = preview;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(last_preview)
}

async fn get_actors_lite_debug(port: u16) -> Result<Option<String>> {
    let response =
        send_command_with_timeout(port, WsCommand::GetActorsLiteDebug, Duration::from_secs(3))
            .await?;

    match response {
        WsResponse::ActorsLiteDebug { value } => Ok(value),
        WsResponse::Error { message } => anyhow::bail!("GetActorsLiteDebug failed: {}", message),
        _ => Ok(None),
    }
}

fn preview_needs_retry(preview: Option<&str>) -> bool {
    match preview.map(str::trim) {
        None => true,
        Some("") => true,
        Some("Run to see preview") => true,
        _ => false,
    }
}

fn timer_category_requires_immediate_initial_match(output: &OutputSpec) -> bool {
    matches!(output.r#match, MatchMode::Exact) && output.text.as_deref().map(str::trim) == Some("")
}

async fn clear_preview_input_memory(port: u16) -> Result<()> {
    let expression = r#"(function() {
        window.__boonLastPreviewTextInput = null;
        window.__boonLastPreviewTextInputNodeId = null;
        window.__boonLastPreviewTextInputIndex = null;
        window.__boonPreferredTextInputIndex = null;
        window.__boonLastPreviewTextSelectionStart = null;
        window.__boonLastPreviewTextSelectionEnd = null;
        return true;
    })()"#
        .to_string();

    for attempt in 0..3 {
        let response = send_command_with_timeout(
            port,
            WsCommand::EvalJs {
                expression: expression.clone(),
            },
            Duration::from_secs(3),
        )
        .await;

        match response {
            Ok(WsResponse::Error { message }) => {
                if !message.to_ascii_lowercase().contains("timeout") {
                    anyhow::bail!("Clear preview input memory failed: {}", message);
                }
            }
            Ok(_) => return Ok(()),
            Err(error) => {
                if !error.to_string().to_ascii_lowercase().contains("timeout") {
                    return Err(error);
                }
            }
        }

        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    println!("[boon-tools] warning: clear preview input memory timed out; continuing");
    Ok(())
}

async fn inject_example_code(port: u16, filename: &str, path: &Path) -> Result<()> {
    let code = std::fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!("Failed to read example '{}': {}", path.display(), error)
    })?;
    wait_for_playground_api_ready(port, Duration::from_secs(10)).await?;

    for attempt in 0..2 {
        let response = send_command_to_server(
            port,
            WsCommand::InjectCode {
                code: code.clone(),
                filename: Some(filename.to_string()),
            },
        )
        .await?;
        if let WsResponse::Error { message } = response {
            let lower = message.to_ascii_lowercase();
            let api_not_ready = lower.contains("boonplayground api not available")
                || lower.contains("cannot read properties of undefined")
                || lower.contains("setcurrentfile");
            if api_not_ready && attempt == 0 {
                wait_for_playground_api_ready(port, Duration::from_secs(10)).await?;
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            anyhow::bail!("InjectCode failed for '{}': {}", filename, message);
        }
        wait_for_editor_contents(port, filename, &code, Duration::from_secs(10)).await?;
        return Ok(());
    }

    anyhow::bail!("InjectCode failed for '{}' after retry", filename)
}

async fn trigger_run_then_capture(port: u16, _engine: Option<&str>) -> Result<Option<String>> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: r#"(function() {
                if (window.boonPlayground && typeof window.boonPlayground.run === 'function') {
                    window.boonPlayground.run();
                    return { ok: true, path: 'playground-api' };
                }
                return { ok: false, reason: 'window.boonPlayground.run unavailable' };
            })()"#
                .to_string(),
        },
    )
    .await?;
    if let WsResponse::Success { data } = &response {
        let payload = match data {
            Some(serde_json::Value::Object(map)) => Some(serde_json::Value::Object(map.clone())),
            Some(serde_json::Value::String(json)) => serde_json::from_str(json).ok(),
            _ => None,
        };
        if let Some(payload) = payload {
            if payload.get("ok").and_then(|value| value.as_bool()) == Some(false) {
                anyhow::bail!(
                    "TriggerRun failed: {}",
                    payload
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or("window.boonPlayground.run unavailable")
                );
            }
        }
    }
    if let WsResponse::Error { message } = response {
        let lower = message.to_ascii_lowercase();
        if _engine == Some("ActorsLite")
            && (lower.contains("timeout") || lower.contains("runtime.evaluate"))
        {
            return match run_and_capture_initial(port, true).await {
                Ok(initial_preview) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    wait_for_preview_to_settle(port).await;
                    let settled_preview = get_preview_text(port).await?;
                    if preview_needs_retry(settled_preview.as_deref()) {
                        Ok(initial_preview)
                    } else {
                        Ok(settled_preview)
                    }
                }
                Err(error) => {
                    let fallback_lower = error.to_string().to_ascii_lowercase();
                    if fallback_lower.contains("timeout") {
                        Ok(None)
                    } else {
                        Err(error)
                    }
                }
            };
        }
        anyhow::bail!("TriggerRun failed: {}", message);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    wait_for_preview_to_settle(port).await;
    get_preview_text(port).await
}

async fn trigger_run_then_capture_immediate(
    port: u16,
    engine: Option<&str>,
) -> Result<Option<String>> {
    if engine != Some("ActorsLite") {
        return run_and_capture_initial(port, false).await;
    }

    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: r#"(function() {
                    if (window.boonPlayground && typeof window.boonPlayground.run === 'function') {
                        window.boonPlayground.run();
                        return { ok: true, path: 'playground-api' };
                    }
                    return { ok: false, reason: 'window.boonPlayground.run unavailable' };
                })()"#
                .to_string(),
        },
    )
    .await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("TriggerRun failed: {}", message);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    wait_for_non_retry_preview_text(port, Duration::from_millis(250)).await
}

async fn trigger_run_until_preview_ready(
    port: u16,
    engine: Option<&str>,
    max_attempts: usize,
) -> Result<Option<String>> {
    let attempts = max_attempts.max(1);
    let mut preview = None;

    for attempt in 0..attempts {
        preview = trigger_run_then_capture(port, engine).await?;
        if !preview_needs_retry(preview.as_deref()) {
            break;
        }

        if attempt + 1 < attempts {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    Ok(preview)
}

async fn run_and_capture_initial(port: u16, retry_on_placeholder: bool) -> Result<Option<String>> {
    let response = send_command_with_timeout(
        port,
        WsCommand::RunAndCaptureInitial,
        Duration::from_secs(10),
    )
    .await?;
    match response {
        WsResponse::RunAndCaptureInitial {
            success,
            initial_preview,
            ..
        } => {
            if !success {
                anyhow::bail!("RunAndCaptureInitial reported failure");
            }
            if retry_on_placeholder && initial_preview == "Run to see preview" {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let retry = send_command_with_timeout(
                    port,
                    WsCommand::RunAndCaptureInitial,
                    Duration::from_secs(10),
                )
                .await?;
                match retry {
                    WsResponse::RunAndCaptureInitial {
                        success,
                        initial_preview,
                        ..
                    } => {
                        if !success {
                            anyhow::bail!("RunAndCaptureInitial retry reported failure");
                        }
                        Ok(Some(initial_preview))
                    }
                    WsResponse::Error { message } => {
                        anyhow::bail!("RunAndCaptureInitial retry failed: {}", message);
                    }
                    _ => Ok(Some("Run to see preview".to_string())),
                }
            } else {
                Ok(Some(initial_preview))
            }
        }
        WsResponse::Error { message } => {
            anyhow::bail!("RunAndCaptureInitial failed: {}", message);
        }
        _ => Ok(None),
    }
}

async fn wait_for_initial_interaction_settle(port: u16) {
    wait_for_preview_to_settle(port).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
}

fn use_factory_fabric_timing_bootstrap(
    engine: Option<&str>,
    example_name: &str,
    capture_stable_initial_preview: bool,
) -> bool {
    engine == Some("FactoryFabric")
        && !capture_stable_initial_preview
        && matches!(
            example_name,
            "interval" | "interval_hold" | "then" | "when" | "while"
        )
}

async fn wait_for_playground_api_ready(port: u16, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if matches!(get_connection_status(port).await, ConnectionStatus::Ready) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "Playground API not ready after waiting {} ms",
        timeout.as_millis()
    );
}

async fn wait_for_current_file(
    port: u16,
    expected_filename: &str,
    timeout: Duration,
) -> Result<()> {
    let start = Instant::now();
    let expected_json = serde_json::to_string(expected_filename)?;
    while start.elapsed() < timeout {
        let response = send_command_to_server(
            port,
            WsCommand::EvalJs {
                expression: format!(
                    r#"(function() {{
                        if (!window.boonPlayground || typeof window.boonPlayground.getCurrentFile !== 'function') {{
                            return {{ ready: false, currentFile: null }};
                        }}
                        return {{
                            ready: true,
                            currentFile: window.boonPlayground.getCurrentFile()
                        }};
                    }})()"#
                ),
            },
        )
        .await?;

        let value = match response {
            WsResponse::Success {
                data: Some(serde_json::Value::Object(value)),
            } => serde_json::Value::Object(value),
            WsResponse::Success {
                data: Some(serde_json::Value::String(json)),
            } => serde_json::from_str(&json)?,
            WsResponse::Error { message } => {
                if message.to_ascii_lowercase().contains("timeout") {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                anyhow::bail!("EvalJs current-file probe failed: {}", message)
            }
            _ => serde_json::Value::Null,
        };

        let current_file = value.get("currentFile").and_then(|v| v.as_str());
        if current_file == Some(expected_filename) {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    anyhow::bail!(
        "Current file did not become {} within {} ms",
        expected_json,
        timeout.as_millis()
    );
}

async fn wait_for_editor_contents(
    port: u16,
    expected_filename: &str,
    expected_code: &str,
    timeout: Duration,
) -> Result<()> {
    let start = Instant::now();
    let expected_filename_json = serde_json::to_string(expected_filename)?;
    let expected_code_json = serde_json::to_string(expected_code)?;

    while start.elapsed() < timeout {
        let response = send_command_to_server(
            port,
            WsCommand::EvalJs {
                expression: format!(
                    r#"(function() {{
                        if (!window.boonPlayground ||
                            typeof window.boonPlayground.getCurrentFile !== 'function' ||
                            typeof window.boonPlayground.getCode !== 'function') {{
                            return {{ ready: false, matches: false }};
                        }}
                        const currentFile = window.boonPlayground.getCurrentFile();
                        const code = window.boonPlayground.getCode();
                        return {{
                            ready: true,
                            currentFile,
                            codeLength: code.length,
                            matches: currentFile === {expected_filename_json} && code === {expected_code_json}
                        }};
                    }})()"#
                ),
            },
        )
        .await?;

        let value = match response {
            WsResponse::Success {
                data: Some(serde_json::Value::Object(value)),
            } => serde_json::Value::Object(value),
            WsResponse::Success {
                data: Some(serde_json::Value::String(json)),
            } => serde_json::from_str(&json)?,
            WsResponse::Error { message } => {
                if message.to_ascii_lowercase().contains("timeout") {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                anyhow::bail!("EvalJs editor-content probe failed: {}", message);
            }
            _ => serde_json::Value::Null,
        };

        if value
            .get("matches")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    anyhow::bail!(
        "Editor contents did not become {} within {} ms",
        expected_filename_json,
        timeout.as_millis()
    );
}

async fn refresh_to_example(
    port: u16,
    example_name: &str,
    select_name: &str,
    editor_filename: &str,
    example_path: &Path,
    engine: Option<&str>,
    min_delay_ms: u64,
    capture_stable_initial_preview: bool,
) -> Result<Option<String>> {
    if engine == Some("ActorsLite") {
        println!(
            "[ActorsLite harness] prepare example '{}' with ready-first bootstrap",
            example_name
        );
    }
    let response = send_command_to_server(
        port,
        WsCommand::NavigateTo {
            path: example_route_path(example_name, engine),
        },
    )
    .await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("NavigateTo failed: {}", message);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = send_command_to_server(port, WsCommand::Refresh).await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("Refresh-to-example failed: {}", message);
    }

    tokio::time::sleep(Duration::from_millis(min_delay_ms)).await;

    clear_preview_input_memory(port).await?;

    let response = send_command_to_server(port, WsCommand::ClearStates).await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("ClearStates failed: {}", message);
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Clear ActorsLite persistence data to ensure a fresh state
    if engine == Some("ActorsLite") {
        let clear_expression = r#"(function() {
            var keys = [];
            for (var i = 0; i < localStorage.length; i++) {
                var key = localStorage.key(i);
                if (key && key.startsWith("boon.actorslite")) {
                    keys.push(key);
                }
            }
            keys.forEach(function(k) { localStorage.removeItem(k); });
            return keys.length;
        })()"#;
        let _ = send_command_to_server(
            port,
            WsCommand::EvalJs {
                expression: clear_expression.to_string(),
            },
        )
        .await;
    }

    if engine == Some("ActorsLite") {
        println!(
            "[ActorsLite harness] skip redundant refresh after ClearStates for '{}'",
            example_name
        );
        tokio::time::sleep(Duration::from_millis(min_delay_ms.min(250))).await;
        wait_for_playground_api_ready(port, Duration::from_secs(10)).await?;
    } else {
        let response = send_command_to_server(port, WsCommand::Refresh).await?;
        if let WsResponse::Error { message } = response {
            anyhow::bail!("Refresh after ClearStates failed: {}", message);
        }

        tokio::time::sleep(Duration::from_millis(min_delay_ms)).await;
        wait_for_playground_api_ready(port, Duration::from_secs(10)).await?;
    }

    let response =
        send_command_to_server(port, WsCommand::SetPersistence { enabled: false }).await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("Disable persistence failed: {}", message);
    }

    clear_preview_input_memory(port).await?;

    let target_file_already_loaded =
        wait_for_current_file(port, editor_filename, Duration::from_secs(3))
            .await
            .is_ok();

    if should_reselect_example_after_refresh(engine, target_file_already_loaded) {
        let response = send_command_to_server(
            port,
            WsCommand::SelectExample {
                name: select_name.to_string(),
            },
        )
        .await?;
        if let WsResponse::Error { message } = response {
            anyhow::bail!("SelectExample failed for '{}': {}", select_name, message);
        }
        wait_for_current_file(port, editor_filename, Duration::from_secs(10)).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
    } else {
        wait_for_current_file(port, editor_filename, Duration::from_secs(10)).await?;
    }

    if engine == Some("ActorsLite") {
        inject_example_code(port, editor_filename, example_path).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    if wait_for_current_file(port, editor_filename, Duration::from_secs(10))
        .await
        .is_ok()
    {
        tokio::time::sleep(Duration::from_millis(min_delay_ms.max(300))).await;

        if engine == Some("ActorsLite") {
            println!(
                "[ActorsLite harness] run initial preview for '{}' via TriggerRun + {} capture",
                example_name,
                if capture_stable_initial_preview {
                    "settled"
                } else {
                    "immediate"
                }
            );
            let initial_preview = if capture_stable_initial_preview {
                trigger_run_until_preview_ready(port, engine, 3).await?
            } else {
                trigger_run_then_capture_immediate(port, engine).await?
            };
            let summary = initial_preview
                .as_deref()
                .map(|text| truncate_for_error(text, 80))
                .unwrap_or_else(|| "<none>".to_string());
            println!(
                "[ActorsLite harness] initial preview for '{}' => {}",
                example_name, summary
            );
            if capture_stable_initial_preview && preview_needs_retry(initial_preview.as_deref()) {
                match get_actors_lite_debug(port).await {
                    Ok(Some(debug)) => {
                        println!(
                            "[ActorsLite harness] actors-lite debug for '{}' => {}",
                            example_name, debug
                        );
                    }
                    Ok(None) => {
                        println!(
                            "[ActorsLite harness] actors-lite debug for '{}' => <none>",
                            example_name
                        );
                    }
                    Err(error) => {
                        println!(
                            "[ActorsLite harness] actors-lite debug for '{}' failed: {}",
                            example_name, error
                        );
                    }
                }
            }
            if capture_stable_initial_preview {
                wait_for_initial_interaction_settle(port).await;
            }
            return Ok(initial_preview);
        }

        if use_factory_fabric_timing_bootstrap(engine, example_name, capture_stable_initial_preview)
        {
            return trigger_run_then_capture_immediate(port, engine).await;
        }

        let initial_preview = if capture_stable_initial_preview {
            trigger_run_then_capture(port, engine).await?
        } else {
            normalize_immediate_initial_preview(
                trigger_run_then_capture_immediate(port, engine).await?,
            )
        };
        if capture_stable_initial_preview && preview_needs_retry(initial_preview.as_deref()) {
            return trigger_run_then_capture(port, engine).await;
        }
        return Ok(initial_preview);
    }

    let reset_example = if example_name == "minimal" {
        "hello_world.bn"
    } else {
        "minimal.bn"
    };
    let response = send_command_to_server(
        port,
        WsCommand::SelectExample {
            name: reset_example.to_string(),
        },
    )
    .await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("Reset SelectExample failed: {}", message);
    }
    wait_for_current_file(port, reset_example, Duration::from_secs(10)).await?;

    let response = send_command_to_server(
        port,
        WsCommand::SelectExample {
            name: select_name.to_string(),
        },
    )
    .await?;
    if let WsResponse::Error { message } = response {
        anyhow::bail!("SelectExample after refresh failed: {}", message);
    }
    wait_for_current_file(port, editor_filename, Duration::from_secs(10)).await?;
    if engine == Some("ActorsLite") {
        inject_example_code(port, editor_filename, example_path).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        wait_for_current_file(port, editor_filename, Duration::from_secs(10)).await?;
    }
    tokio::time::sleep(Duration::from_millis(min_delay_ms.max(300))).await;

    if engine == Some("ActorsLite") {
        println!(
            "[ActorsLite harness] run initial preview for '{}' via TriggerRun + {} capture",
            example_name,
            if capture_stable_initial_preview {
                "settled"
            } else {
                "immediate"
            }
        );
        let initial_preview = if capture_stable_initial_preview {
            trigger_run_until_preview_ready(port, engine, 3).await?
        } else {
            trigger_run_then_capture_immediate(port, engine).await?
        };
        let summary = initial_preview
            .as_deref()
            .map(|text| truncate_for_error(text, 80))
            .unwrap_or_else(|| "<none>".to_string());
        println!(
            "[ActorsLite harness] initial preview for '{}' => {}",
            example_name, summary
        );
        if capture_stable_initial_preview && preview_needs_retry(initial_preview.as_deref()) {
            match get_actors_lite_debug(port).await {
                Ok(Some(debug)) => {
                    println!(
                        "[ActorsLite harness] actors-lite debug for '{}' => {}",
                        example_name, debug
                    );
                }
                Ok(None) => {
                    println!(
                        "[ActorsLite harness] actors-lite debug for '{}' => <none>",
                        example_name
                    );
                }
                Err(error) => {
                    println!(
                        "[ActorsLite harness] actors-lite debug for '{}' failed: {}",
                        example_name, error
                    );
                }
            }
        }
        if capture_stable_initial_preview {
            wait_for_initial_interaction_settle(port).await;
        }
        return Ok(initial_preview);
    }
    if use_factory_fabric_timing_bootstrap(engine, example_name, capture_stable_initial_preview) {
        return trigger_run_then_capture_immediate(port, engine).await;
    }

    match run_and_capture_initial(port, true).await {
        Ok(initial_preview) => {
            if engine == Some("ActorsLite") {
                let summary = initial_preview
                    .as_deref()
                    .map(|text| truncate_for_error(text, 80))
                    .unwrap_or_else(|| "<none>".to_string());
                println!(
                    "[ActorsLite harness] initial preview for '{}' => {}",
                    example_name, summary
                );
            }
            wait_for_initial_interaction_settle(port).await;
            Ok(initial_preview)
        }
        Err(error) => {
            if engine == Some("ActorsLite") {
                println!(
                    "[ActorsLite harness] initial preview for '{}' failed: {}",
                    example_name, error
                );
            }
            let is_timeout = error.to_string().to_ascii_lowercase().contains("timeout");
            if !is_timeout {
                return Err(error);
            }
            if engine == Some("ActorsLite") {
                println!(
                    "[ActorsLite harness] falling back to TriggerRun + settled preview capture for '{}'",
                    example_name
                );
                let initial_preview = trigger_run_then_capture(port, engine).await?;
                wait_for_initial_interaction_settle(port).await;
                Ok(initial_preview)
            } else {
                tokio::time::sleep(Duration::from_millis(750)).await;
                let initial_preview = run_and_capture_initial(port, true).await?;
                wait_for_initial_interaction_settle(port).await;
                Ok(initial_preview)
            }
        }
    }
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
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let bn_path = path.with_extension("bn");

            if bn_path.exists() {
                let name = stem.to_string();
                examples.push(DiscoveredExample {
                    name,
                    select_name: stem.to_string(),
                    editor_filename: format!("{stem}.bn"),
                    bn_path,
                    expected_path: path.to_path_buf(),
                });
                continue;
            }

            let Some(parent) = path.parent() else {
                continue;
            };
            let run_path = parent.join("RUN.bn");
            if run_path.exists() {
                let name = stem.to_string();
                examples.push(DiscoveredExample {
                    name,
                    select_name: stem.to_string(),
                    editor_filename: "RUN.bn".to_string(),
                    bn_path: run_path,
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

/// Check if the playground dev server (mzoon) is running
async fn is_playground_running(playground_port: u16) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    match client
        .get(format!("http://localhost:{}", playground_port))
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Start the playground dev server (mzoon) in background
async fn start_playground_server(playground_port: u16) -> Result<()> {
    use std::process::Command as StdCommand;

    let boon_root = find_boon_root().context("Could not find boon repository root")?;

    let playground_dir = boon_root.join("playground");

    if !playground_dir.exists() {
        anyhow::bail!(
            "Playground directory not found: {}",
            playground_dir.display()
        );
    }

    println!("Starting mzoon server in {}...", playground_dir.display());

    // Start mzoon in background using nohup
    let result = StdCommand::new("sh")
        .args([
            "-c",
            &format!(
                "cd {} && nohup makers mzoon start > /tmp/mzoon.log 2>&1 &",
                playground_dir.display()
            ),
        ])
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
                if is_playground_running(playground_port).await {
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

async fn get_server_status(port: u16) -> Option<ServerStatus> {
    check_server_connection(port).await.ok()
}

fn setup_launch_engine(initial_engine: Option<&str>) -> &'static str {
    match initial_engine {
        Some("ActorsLite") => "ActorsLite",
        Some("Actors") => "Actors",
        Some("DD") => "DD",
        Some("Wasm") => "Wasm",
        Some(_) | None => "Actors",
    }
}

async fn relaunch_clean_automation_browser(
    port: u16,
    playground_port: u16,
    initial_engine: Option<&str>,
) -> Result<()> {
    println!("[ActorsLite harness] relaunching automation browser from a clean visible page");
    let _ = browser::kill_browser_instances();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let opts = browser::LaunchOptions {
        playground_port,
        ws_port: port,
        headless: false,
        keep_open: true,
        browser_path: None,
        initial_engine: Some(setup_launch_engine(initial_engine).to_string()),
        initial_example: Some("counter".to_string()),
    };

    let child = browser::launch_browser(opts)?;
    println!(
        "[ActorsLite harness] relaunched Chromium (PID: {}) for clean ready state",
        child.id()
    );
    browser::wait_for_extension_connection(port, Duration::from_secs(30)).await?;

    if let Some(server_status) = get_server_status(port).await {
        println!(
            "[ActorsLite harness] post-relaunch url={:?} api_ready={}",
            server_status.page_url, server_status.api_ready
        );
    }

    Ok(())
}

/// Result of setup - tracks what we started so we can clean up
pub struct SetupState {
    /// Did we start mzoon ourselves? If so, we should kill it when done.
    pub started_mzoon: bool,
}

/// Ensure WebSocket server is running and browser extension is connected.
/// Will start server and launch browser if needed.
/// Returns SetupState indicating what was started (for cleanup).
async fn ensure_browser_connection(
    port: u16,
    playground_port: u16,
    initial_engine: Option<&str>,
) -> Result<SetupState> {
    let mut setup = SetupState {
        started_mzoon: false,
    };

    // Step 0: Ensure playground (mzoon) is running
    if !is_playground_running(playground_port).await {
        println!("Playground server not running on port {}.", playground_port);
        start_playground_server(playground_port).await?;
        setup.started_mzoon = true;
    } else {
        println!("Playground server running on port {}.", playground_port);
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
                    if !e.to_string().contains("address in use") && !e.to_string().contains("bind")
                    {
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
    if matches!(
        status,
        ConnectionStatus::NoExtension | ConnectionStatus::ServerNotRunning
    ) {
        if browser::has_running_automation_browser() {
            println!("Automation Chromium already running, waiting for extension reconnect...");
            let reconnect_timeout = Duration::from_secs(25);
            match browser::wait_for_extension_connection(port, reconnect_timeout).await {
                Ok(()) => {
                    println!("Reused existing Chromium session.");
                }
                Err(error) => {
                    let pids = browser::running_automation_pids();
                    anyhow::bail!(
                        "Existing shared Chromium session {:?} did not reconnect: {}.\n\
                        Refusing to reset the shared profile or kill the browser automatically.",
                        pids,
                        error
                    );
                }
            }
        }
    }

    let status = get_connection_status(port).await;
    if matches!(
        status,
        ConnectionStatus::NoExtension | ConnectionStatus::ServerNotRunning
    ) {
        println!("Browser extension not connected, launching Chromium...");

        let opts = browser::LaunchOptions {
            playground_port,
            ws_port: port,
            headless: false,
            keep_open: true, // Don't block waiting
            browser_path: None,
            initial_engine: Some(setup_launch_engine(initial_engine).to_string()),
            initial_example: Some("counter".to_string()),
        };

        match browser::launch_browser(opts) {
            Ok(child) => {
                println!(
                    "Chromium launched (PID: {}), waiting for extension to connect...",
                    child.id()
                );

                // Wait for extension to connect
                let timeout = Duration::from_secs(30);
                match browser::wait_for_extension_connection(port, timeout).await {
                    Ok(()) => {
                        println!("Extension connected!");
                    }
                    Err(e) => {
                        println!(
                            "Browser launched but extension connection timed out during initial wait: {}\n\
                            Continuing with readiness polling in case the extension reconnects.",
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
        let max_retries = 6;
        let mut actors_lite_stuck_retries = 0u8;

        println!("Waiting for playground API to be ready...");

        'readiness: for retry in 0..max_retries {
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
                    println!(
                        "  WASM not ready after {}s, refreshing page (attempt {}/{})...",
                        initial_wait.as_secs(),
                        retry + 1,
                        max_retries
                    );

                    if initial_engine == Some("ActorsLite") {
                        if let Some(server_status) = get_server_status(port).await {
                            println!(
                                "[ActorsLite harness] retry reset url={:?} api_ready={}",
                                server_status.page_url, server_status.api_ready
                            );
                            let page_is_stuck = !server_status.api_ready
                                && server_status
                                    .page_url
                                    .as_deref()
                                    .is_some_and(|url| url.contains("engine=actorslite"));
                            if page_is_stuck {
                                actors_lite_stuck_retries =
                                    actors_lite_stuck_retries.saturating_add(1);
                                if actors_lite_stuck_retries >= 2 {
                                    println!(
                                        "[ActorsLite harness] repeated stuck ActorsLite route detected, fast-tracking clean relaunch"
                                    );
                                    break 'readiness;
                                }
                            } else {
                                actors_lite_stuck_retries = 0;
                            }
                        }
                    }

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

        if initial_engine == Some("ActorsLite") {
            println!(
                "[ActorsLite harness] API still not ready after {} retries, attempting one clean browser relaunch",
                max_retries
            );

            match relaunch_clean_automation_browser(port, playground_port, initial_engine).await {
                Ok(()) => {
                    let recovery_wait = Duration::from_secs(15);
                    let recovery_retries = 3;

                    for retry in 0..recovery_retries {
                        let start = Instant::now();
                        while start.elapsed() < recovery_wait {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            if matches!(get_connection_status(port).await, ConnectionStatus::Ready)
                            {
                                println!("Playground API ready after clean relaunch!");
                                return Ok(setup);
                            }
                        }

                        if retry < recovery_retries - 1
                            && matches!(
                                get_connection_status(port).await,
                                ConnectionStatus::ExtensionConnectedNotReady
                            )
                        {
                            println!(
                                "[ActorsLite harness] post-relaunch API still not ready, refreshing page (attempt {}/{})...",
                                retry + 1,
                                recovery_retries
                            );
                            let _ = send_command_to_server(port, WsCommand::Refresh).await;
                            tokio::time::sleep(Duration::from_secs(3)).await;
                        }
                    }
                }
                Err(error) => {
                    println!(
                        "[ActorsLite harness] clean relaunch failed after readiness retries: {}",
                        error
                    );
                }
            }
        }

        anyhow::bail!(
            "Playground API not ready after {} retries. \
            Make sure the playground is running at localhost:{}",
            max_retries,
            playground_port
        );
    }

    Ok(setup)
}

/// Kill mzoon server we started (port-based, like `makers kill`)
fn kill_mzoon_server(playground_port: u16) {
    use std::process::Command as StdCommand;

    println!("Stopping mzoon server we started...");

    // Find the process LISTENING on the playground port (not browsers connecting to it)
    // This matches the approach in playground/Makefile.toml [tasks.kill]
    let pid_output = StdCommand::new("lsof")
        .args([&format!("-ti:{}", playground_port), "-sTCP:LISTEN"])
        .output();

    if let Ok(output) = pid_output {
        let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !pid_str.is_empty() {
            // Send TERM signal first (graceful shutdown)
            if let Ok(pid) = pid_str.parse::<i32>() {
                let _ = StdCommand::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
                println!(
                    "Sent TERM signal to server on port {} (PID: {})",
                    playground_port, pid
                );

                // Wait for graceful shutdown
                std::thread::sleep(std::time::Duration::from_secs(2));

                // Check if still running and force kill if needed
                let still_running = StdCommand::new("lsof")
                    .args([&format!("-ti:{}", playground_port), "-sTCP:LISTEN"])
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
    let setup =
        ensure_browser_connection(opts.port, opts.playground_port, opts.engine.as_deref()).await?;

    // Run tests and ensure cleanup happens even on error
    let result = run_tests_inner(&opts).await;

    // Cleanup: if we started mzoon, kill it
    if setup.started_mzoon {
        kill_mzoon_server(opts.playground_port);
    }

    result
}

/// Inner test runner (separated for cleanup handling)
async fn run_tests_inner(opts: &TestOptions) -> Result<Vec<TestResult>> {
    // Switch engine if requested
    if let Some(ref engine) = opts.engine {
        let mut effective_engine = engine.clone();
        let already_on_requested_engine =
            match send_command_to_server(opts.port, WsCommand::GetEngine).await {
                Ok(WsResponse::EngineInfo {
                    available_engines,
                    engine: current,
                    ..
                }) => {
                    effective_engine = resolve_requested_engine(engine, &available_engines);
                    current == effective_engine
                }
                _ => false,
            };

        // Refresh to a lightweight example before switching engines so the next engine
        // does not inherit heavy in-memory state from the currently loaded page.
        if !already_on_requested_engine {
            let counter_path = find_examples_dir()?.join("counter").join("counter.bn");
            let _ = refresh_to_example(
                opts.port,
                "counter",
                "counter",
                "counter.bn",
                &counter_path,
                None,
                1500,
                true,
            )
            .await?;

            if effective_engine != *engine {
                println!(
                    "Requested engine '{}' is not available in this build; using '{}' instead.",
                    engine, effective_engine
                );
            }
            println!("Setting engine to: {}", effective_engine);
            let response = send_command_to_server(
                opts.port,
                WsCommand::SetEngine {
                    engine: effective_engine.clone(),
                },
            )
            .await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!(
                    "Failed to set engine to '{}': {}",
                    effective_engine,
                    message
                );
            }
            // Wait for engine switch and recompilation
            tokio::time::sleep(Duration::from_millis(500)).await;
        } else {
            println!("Engine already set to: {}", effective_engine);
        }
    }

    // Milestone 1 example parity should run from fresh in-memory state, not persistence.
    // Persistence is validated separately in dedicated sections and later milestones.
    let _ = send_command_to_server(opts.port, WsCommand::SetPersistence { enabled: false }).await;

    // Find examples directory
    let examples_dir = if let Some(ref dir) = opts.examples_dir {
        dir.clone()
    } else {
        find_examples_dir()?
    };

    // Discover examples
    let mut examples = discover_examples(&examples_dir)?;

    if examples.is_empty() {
        println!(
            "No examples with .expected files found in {}",
            examples_dir.display()
        );
        return Ok(vec![]);
    }

    // Apply filter
    if let Some(ref filter) = opts.filter {
        let prefer_exact = examples.iter().any(|example| example.name == *filter);
        examples.retain(|example| example_name_matches_filter(&example.name, filter, prefer_exact));
        if examples.is_empty() {
            println!("No examples match filter '{}'", filter);
            return Ok(vec![]);
        }
    }

    let engine_label = opts.engine.as_deref().unwrap_or("(current)");
    println!("Boon Example Tests [engine: {}]", engine_label);
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
    let passed = results
        .iter()
        .filter(|r| r.passed && r.skipped.is_none())
        .count();
    let skipped = results.iter().filter(|r| r.skipped.is_some()).count();
    let total = results.len();
    if skipped > 0 {
        println!(
            "{}/{} passed ({} skipped)",
            passed,
            total - skipped,
            skipped
        );
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
    let parsed_sequences = parse_interaction_sequences(&spec.sequence)?;
    validate_required_shared_sequences(&example.name, &parsed_sequences)?;
    let sequences = shared_example_parsed_sequences(&example.name).unwrap_or(parsed_sequences);
    let is_timer_category = matches!(spec.test.category.as_deref(), Some("timer"));
    let persistence_enabled = !opts.skip_persistence && !spec.persistence.is_empty();

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

    // Check engine-specific skip
    if let Some(ref engine) = opts.engine {
        if let Some(ref engines) = spec.test.engines {
            if !engines.iter().any(|e| e == engine) {
                return Ok(TestResult {
                    name: example.name.clone(),
                    passed: true,
                    skipped: Some(format!("not in engines list (requires {:?})", engines)),
                    duration: start.elapsed(),
                    error: None,
                    actual_output: None,
                    expected_output: None,
                    steps,
                });
            }
        }
        if let Some(ref skip_engines) = spec.test.skip_engines {
            if skip_engines.iter().any(|e| e == engine) {
                return Ok(TestResult {
                    name: example.name.clone(),
                    passed: true,
                    skipped: Some(format!("skipped for engine {}", engine)),
                    duration: start.elapsed(),
                    error: None,
                    actual_output: None,
                    expected_output: None,
                    steps,
                });
            }
        }
    }

    // Load each example from a fresh page instead of chaining in-page SelectExample
    // transitions. This keeps browser verification aligned with the plan's
    // "fresh Wasm example in the real browser" goal and avoids inherited
    // memory pressure from heavy examples like Cells.
    let refresh_delay = post_refresh_delay_ms(spec.timing.initial_delay);
    if opts.engine.as_deref() == Some("ActorsLite") {
        println!(
            "[ActorsLite harness] start example '{}' (initial_delay={}ms, refresh_delay={}ms, sequences={})",
            example.name,
            spec.timing.initial_delay,
            refresh_delay,
            sequences.len()
        );
    }
    let initial_preview = match refresh_to_example(
        opts.port,
        &example.name,
        &example.select_name,
        &example.editor_filename,
        &example.bn_path,
        opts.engine.as_deref(),
        refresh_delay,
        !is_timer_category,
    )
    .await
    {
        Ok(initial_preview) => initial_preview,
        Err(error) => {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(error.to_string()),
                actual_output: None,
                expected_output: None,
                steps,
            });
        }
    };

    // Persistence is captured by the running engine instance, not retrofitted after the fact.
    // If this example has persistence checks, enable persistence and trigger a fresh run before
    // any interaction sequences begin so the active preview is constructed with persistence on.
    if persistence_enabled {
        let response =
            send_command_to_server(opts.port, WsCommand::SetPersistence { enabled: true }).await?;
        if let WsResponse::Error { message } = response {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(format!("Failed to enable persistence: {}", message)),
                actual_output: None,
                expected_output: None,
                steps,
            });
        }

        let response = send_command_to_server(opts.port, WsCommand::TriggerRun).await?;
        if let WsResponse::Error { message } = response {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(format!(
                    "Failed to rerun with persistence enabled: {}",
                    message
                )),
                actual_output: None,
                expected_output: None,
                steps,
            });
        }

        tokio::time::sleep(Duration::from_millis(spec.timing.initial_delay)).await;
    }

    // Reset preview scroll so position-sensitive example checks like Cells start
    // from the canonical top-left viewport instead of inheriting scroll from a
    // previous run or browser session.
    let _ = send_command_to_server(
        opts.port,
        WsCommand::EvalJs {
            expression: r#"(function() {
                const root = document.querySelector('[data-boon-panel="preview"]');
                if (root) {
                    root.scrollTo({{ left: 0, top: 0 }});
                }
                return { ok: true };
            })()"#
                .to_string(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Wait for initial output with smart waiting
    let (initial_passed, actual_output) = if spec.output.is_configured() {
        if let Some(initial_preview) = initial_preview {
            match spec.output.matches(&initial_preview) {
                Ok(true) => (true, initial_preview),
                Ok(false)
                    if !is_timer_category
                        || !timer_category_requires_immediate_initial_match(&spec.output) =>
                {
                    let initial_result = wait_for_output(
                        opts.port,
                        &spec.output,
                        spec.timing.timeout,
                        spec.timing.poll_interval,
                    )
                    .await;

                    match initial_result {
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
                    }
                }
                Ok(false) => (false, initial_preview),
                Err(e) => {
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
            }
        } else {
            let initial_result = wait_for_output(
                opts.port,
                &spec.output,
                spec.timing.timeout,
                spec.timing.poll_interval,
            )
            .await;

            match initial_result {
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
            }
        }
    } else {
        (true, String::new())
    };

    if !initial_passed {
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
    let mut preferred_input_index = None;

    for seq in &sequences {
        if opts.engine.as_deref() == Some("ActorsLite") {
            println!(
                "[ActorsLite harness] sequence '{}' for '{}'",
                seq.description.as_deref().unwrap_or("<unnamed sequence>"),
                example.name
            );
        }
        let mut last_action = None;
        // Execute actions
        for parsed in &seq.actions {
            if opts.verbose {
                println!("  -> {:?}", parsed);
            }
            last_action = Some(parsed.clone());
            if let Err(e) = execute_action(
                opts.port,
                opts.engine.as_deref(),
                parsed,
                &mut preferred_input_index,
                opts.verbose,
            )
            .await
            {
                let mut error_text = e.to_string();
                if opts.engine.as_deref() == Some("ActorsLite") {
                    if let Ok(Some(debug)) = get_actors_lite_debug(opts.port).await {
                        println!("[ActorsLite harness] debug on failure => {}", debug);
                        error_text = format!("{error_text}\nActorsLite debug: {debug}");
                    }
                }
                // Action failed (including assertions) - record as test failure
                steps.push(StepResult {
                    description: seq
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", parsed)),
                    passed: false,
                    actual: Some(error_text.clone()),
                    expected: None,
                });
                return Ok(TestResult {
                    name: example.name.clone(),
                    passed: false,
                    skipped: None,
                    duration: start.elapsed(),
                    error: Some(error_text),
                    actual_output: None,
                    expected_output: None,
                    steps,
                });
            }
        }

        // Check expected output if specified
        if let Some(ref expected) = seq.expect {
            let step_result =
                if is_timer_category && matches!(last_action, Some(ParsedAction::Wait { .. })) {
                    wait_for_inline_output_after_explicit_wait(
                        opts.port,
                        expected,
                        &seq.expect_match,
                        500,
                    )
                    .await
                } else if matches!(last_action, Some(ParsedAction::Wait { .. })) {
                    wait_for_inline_output_after_explicit_wait(
                        opts.port,
                        expected,
                        &seq.expect_match,
                        1200,
                    )
                    .await
                } else {
                    wait_for_inline_output(
                        opts.port,
                        expected,
                        &seq.expect_match,
                        spec.timing.timeout,
                        spec.timing.poll_interval,
                    )
                    .await
                };

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
    if all_passed && !opts.skip_persistence && !spec.persistence.is_empty() {
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

        // Refresh already waits for API readiness; keep only a small settle window here.
        let refresh_delay = post_refresh_delay_ms(spec.timing.initial_delay);
        tokio::time::sleep(Duration::from_millis(refresh_delay)).await;

        // Refresh resets the playground toggle state, so enable persistence again before rerunning.
        let response =
            send_command_to_server(opts.port, WsCommand::SetPersistence { enabled: true }).await?;
        if let WsResponse::Error { message } = response {
            return Ok(TestResult {
                name: example.name.clone(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(format!(
                    "Failed to re-enable persistence after refresh: {}",
                    message
                )),
                actual_output: None,
                expected_output: None,
                steps,
            });
        }

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
        let mut preferred_input_index = None;
        for seq in &spec.persistence {
            // Execute actions
            for action in &seq.actions {
                let parsed = action.parse()?;
                if opts.verbose {
                    println!("  -> [PERSISTENCE] {:?}", parsed);
                }
                if let Err(e) = execute_action(
                    opts.port,
                    opts.engine.as_deref(),
                    &parsed,
                    &mut preferred_input_index,
                    opts.verbose,
                )
                .await
                {
                    steps.push(StepResult {
                        description: format!(
                            "[PERSISTENCE] {}",
                            seq.description
                                .clone()
                                .unwrap_or_else(|| format!("{:?}", parsed))
                        ),
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
                    description: format!(
                        "[PERSISTENCE] {}",
                        seq.description.clone().unwrap_or_default()
                    ),
                    passed,
                    actual: Some(actual),
                    expected: Some(expected.clone()),
                });

                if !passed {
                    all_passed = false;
                    // Capture debug info for persistence failures
                    if opts.engine.as_deref() == Some("ActorsLite") {
                        if let Ok(Some(debug)) = get_actors_lite_debug(opts.port).await {
                            println!("[ActorsLite harness] PERSISTENCE debug => {}", debug);
                        }
                    }
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
            return Err(WaitError::Timeout {
                actual: last_preview,
            });
        }

        // Get current preview
        let preview = get_preview(port).await.map_err(WaitError::Other)?;

        // Check match
        let matches = output_spec
            .matches(&preview)
            .map_err(|e| WaitError::Other(e))?;

        if matches {
            // Stability check - wait and verify again
            tokio::time::sleep(interval).await;
            let text = get_preview(port).await.map_err(WaitError::Other)?;
            let still_matches = output_spec
                .matches(&text)
                .map_err(|e| WaitError::Other(e))?;
            if still_matches {
                return Ok(text);
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
            return Err(WaitError::Timeout {
                actual: last_preview,
            });
        }

        let preview = get_preview(port).await.map_err(WaitError::Other)?;

        let matches = matches_inline(&preview, expected, mode).map_err(|e| WaitError::Other(e))?;

        if matches {
            // Stability check
            tokio::time::sleep(interval).await;
            let text = get_preview(port).await.map_err(WaitError::Other)?;
            let still_matches =
                matches_inline(&text, expected, mode).map_err(|e| WaitError::Other(e))?;
            if still_matches {
                return Ok(text);
            }
        }

        last_preview = preview;
        tokio::time::sleep(interval).await;
        interval = std::cmp::min(interval * 2, max_interval);
    }
}

async fn get_current_preview_text(port: u16) -> Result<String> {
    let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
    match response {
        WsResponse::PreviewText { text } => Ok(text),
        WsResponse::Error { message } => anyhow::bail!("GetPreview failed: {}", message),
        _ => anyhow::bail!("Unexpected response for GetPreviewText"),
    }
}

async fn wait_for_inline_output_after_explicit_wait(
    port: u16,
    expected: &str,
    mode: &MatchMode,
    timeout_ms: u64,
) -> Result<String, WaitError> {
    let timeout = Duration::from_millis(timeout_ms);
    let poll_interval = Duration::from_millis(50);
    let start = Instant::now();
    let mut last_preview = String::new();

    loop {
        if start.elapsed() > timeout {
            return Err(WaitError::Timeout {
                actual: last_preview,
            });
        }

        let preview = get_current_preview_text(port)
            .await
            .map_err(WaitError::Other)?;
        let matches = matches_inline(&preview, expected, mode).map_err(WaitError::Other)?;
        if matches {
            return Ok(preview);
        }

        last_preview = preview;
        tokio::time::sleep(poll_interval).await;
    }
}

async fn set_focused_input_value(
    port: u16,
    engine: Option<&str>,
    value: &str,
    verbose: bool,
) -> Result<()> {
    let focus_js = r#"(function() {
            const preview = document.querySelector('[data-boon-panel="preview"]');
            if (!preview) return 'ERROR: preview root not found';
            const isTextInput = (element) =>
                element
                && element !== document.body
                && (
                    element.tagName === 'TEXTAREA'
                    || (element.tagName === 'INPUT'
                        && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                );
            const inputs = Array.from(preview.querySelectorAll('input, textarea')).filter(isTextInput);
            const collectInputs = () => inputs.map((element) => ({
                nodeId: element.getAttribute('data-boon-node-id'),
                inputPort: element.getAttribute('data-boon-port-input'),
                keyDownPort: element.getAttribute('data-boon-port-key-down'),
                changePort: element.getAttribute('data-boon-port-change'),
                focused: element === document.activeElement,
                boonFocused: element.getAttribute('data-boon-focused'),
                autofocus: element.getAttribute('autofocus'),
                value: element.value || '',
                connected: element.isConnected,
            }));
            const remembered = window.__boonLastPreviewTextInput;
            const rememberedNodeId =
                window.__boonLastPreviewTextInputNodeId ||
                remembered?.getAttribute?.('data-boon-node-id') ||
                null;
            const rememberedIndex =
                typeof window.__boonLastPreviewTextInputIndex === 'number'
                    ? window.__boonLastPreviewTextInputIndex
                    : null;
            const preferredIndex =
                typeof window.__boonPreferredTextInputIndex === 'number'
                    ? window.__boonPreferredTextInputIndex
                    : null;
            const focusedCandidate = (() => {
                const previewFocused = preview.querySelector(':focus');
                if (isTextInput(previewFocused)) return previewFocused;
                if (isTextInput(document.activeElement) && preview.contains(document.activeElement)) {
                    return document.activeElement;
                }
                return preview.querySelector('[data-boon-focused="true"]')
                    || preview.querySelector('[focused="true"]')
                    || preview.querySelector('input[autofocus], textarea[autofocus]');
            })();
            let input = isTextInput(focusedCandidate) ? focusedCandidate : null;
            if (!isTextInput(input) && preferredIndex != null) {
                input = inputs[preferredIndex] || null;
            }
            if (!isTextInput(input) && rememberedNodeId) {
                input = preview.querySelector('[data-boon-node-id="' + rememberedNodeId + '"]');
            }
            if (!isTextInput(input) && rememberedIndex != null) {
                input = inputs[rememberedIndex] || input;
            }
            if (!isTextInput(input)) {
                input = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                    ? remembered
                    : null;
            }
            if (!input || input === document.body) return 'ERROR: no focused element';
            if (input.tagName !== 'INPUT' && input.tagName !== 'TEXTAREA') {
                return 'ERROR: focused element is ' + input.tagName;
            }
            if (typeof input.focus === 'function') {
                input.focus();
            }
            window.__boonLastPreviewTextInput = input;
            window.__boonLastPreviewTextInputNodeId = input.getAttribute('data-boon-node-id');
            window.__boonLastPreviewTextInputIndex = inputs.indexOf(input);
            if (typeof input.select === 'function') {
                input.select();
                window.__boonLastPreviewTextSelectionStart = 0;
                window.__boonLastPreviewTextSelectionEnd = (input.value || '').length;
            } else if (typeof input.setSelectionRange === 'function') {
                const end = input.value.length;
                input.setSelectionRange(0, end);
                window.__boonLastPreviewTextSelectionStart = 0;
                window.__boonLastPreviewTextSelectionEnd = end;
            }
            return {
                ok: true,
                path: 'focus-select-all',
                nodeId: input.getAttribute('data-boon-node-id'),
                inputPort: input.getAttribute('data-boon-port-input'),
                changePort: input.getAttribute('data-boon-port-change'),
                value: input.value || '',
                valueLength: (input.value || '').length,
                inputs: collectInputs(),
                wasmDebug: window.__boonWasmDebug || null
            };
        })()"#;
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: focus_js.to_string(),
        },
    )
    .await?;
    match response {
        WsResponse::Success { data } => {
            if verbose {
                println!("[set-focused-input-value] {:?}", data);
            }
            if let Some(serde_json::Value::String(ref d)) = data {
                if d.starts_with("ERROR") {
                    anyhow::bail!("Set focused input value failed: {}", d);
                }
            }
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Set focused input value failed: {}", message);
        }
        _ => {}
    }
    let response = if engine == Some("FactoryFabric") {
        let direct_set_js = format!(
            r#"(function() {{
                const preview = document.querySelector('[data-boon-panel="preview"]');
                if (!preview) return 'ERROR: preview root not found';
                const isTextInput = (element) =>
                    element
                    && element !== document.body
                    && (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA');
                const remembered = window.__boonLastPreviewTextInput;
                const previewFocused = preview.querySelector(':focus');
                let focused = isTextInput(previewFocused)
                    ? previewFocused
                    : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                        ? document.activeElement
                        : preview.querySelector('[data-boon-focused="true"]')
                            || preview.querySelector('[focused="true"]')
                            || preview.querySelector('input[autofocus], textarea[autofocus]')
                            || null);
                if (!isTextInput(focused)) {{
                    focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                        ? remembered
                        : null;
                }}
                if (!isTextInput(focused)) {{
                    return 'ERROR: no focused text input';
                }}
                const nextValue = {value_json};
                const nativeSetter =
                    Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set ||
                    Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set;
                if (nativeSetter) {{
                    nativeSetter.call(focused, nextValue);
                }} else {{
                    focused.value = nextValue;
                }}
                if (typeof focused.focus === 'function') {{
                    focused.focus();
                }}
                if (typeof focused.setSelectionRange === 'function') {{
                    focused.setSelectionRange(nextValue.length, nextValue.length);
                }}
                window.__boonLastPreviewTextInput = focused;
                window.__boonLastPreviewTextInputNodeId = focused.getAttribute('data-boon-node-id');
                window.__boonLastPreviewTextSelectionStart = nextValue.length;
                window.__boonLastPreviewTextSelectionEnd = nextValue.length;
                return {{
                    ok: true,
                    value: focused.value || '',
                    nodeId: focused.getAttribute('data-boon-node-id'),
                    changePort: focused.getAttribute('data-boon-port-change'),
                    keyDownPort: focused.getAttribute('data-boon-port-key-down')
                }};
            }})()"#,
            value_json = serde_json::to_string(value)?,
        );
        send_command_to_server(
            port,
            WsCommand::EvalJs {
                expression: direct_set_js,
            },
        )
        .await?
    } else if value.is_empty() {
        send_command_to_server(
            port,
            WsCommand::PressKey {
                key: "Backspace".to_string(),
            },
        )
        .await?
    } else {
        send_command_to_server(
            port,
            WsCommand::TypeText {
                text: value.to_string(),
            },
        )
        .await?
    };
    if verbose {
        println!("[set-focused-input-value-apply] {:?}", response);
    }
    match response {
        WsResponse::Success { data } => {
            if let Some(serde_json::Value::String(ref d)) = data {
                if d.starts_with("ERROR") {
                    anyhow::bail!("Set focused input value failed: {}", d);
                }
            }
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Set focused input value failed: {}", message);
        }
        _ => {}
    }
    wait_for_focused_text_input_suffix(port, value).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

async fn focused_preview_input_via_eval(port: u16) -> Result<(Option<String>, Option<u32>)> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: r#"(function() {
                const preview = document.querySelector('[data-boon-panel="preview"]');
                if (!preview) return { tagName: null, inputIndex: null };
                const isTextInput = (element) =>
                    element
                    && element !== document.body
                    && (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA');
                const remembered = window.__boonLastPreviewTextInput;
                const previewFocused = preview.querySelector(':focus');
                let focused = isTextInput(previewFocused)
                    ? previewFocused
                    : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                        ? document.activeElement
                        : preview.querySelector('[data-boon-focused="true"]')
                            || preview.querySelector('[focused="true"]')
                            || preview.querySelector('input[autofocus], textarea[autofocus]'));
                if (!isTextInput(focused)) {
                    focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                        ? remembered
                        : null;
                }
                if (!isTextInput(focused)) {
                    return { tagName: focused?.tagName ?? null, inputIndex: null };
                }
                const inputs = Array.from(preview.querySelectorAll('input, textarea, [contenteditable="true"]')).filter((element) =>
                    element.matches?.('textarea, [contenteditable="true"]')
                    || (element.matches?.('input')
                        && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                );
                const index = inputs.findIndex((input) => input === focused);
                return {
                    tagName: focused.tagName || null,
                    inputIndex: index >= 0 ? index : null,
                };
            })()"#
                .to_string(),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => {
            let tag_name = value
                .get("tagName")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let input_index = value
                .get("inputIndex")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok());
            Ok((tag_name, input_index))
        }
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            let tag_name = value
                .get("tagName")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let input_index = value
                .get("inputIndex")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok());
            Ok((tag_name, input_index))
        }
        WsResponse::Success { data: None } => Ok((None, None)),
        WsResponse::Error { message } => anyhow::bail!("Focused preview eval failed: {}", message),
        _ => anyhow::bail!("Unexpected response for focused preview eval"),
    }
}

async fn focused_preview_debug_via_eval(port: u16) -> Result<String> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: r#"(function() {
                const preview = document.querySelector('[data-boon-panel="preview"]');
                if (!preview) return { error: 'preview root not found' };
                const isTextInput = (element) =>
                    element
                    && element !== document.body
                    && (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA');
                const describe = (element) => element ? {
                    tag: element.tagName || null,
                    type: element.type || null,
                    value: element.value || '',
                    nodeId: element.getAttribute?.('data-boon-node-id') || null,
                    focusedAttr: element.getAttribute?.('focused') || null,
                    boonFocused: element.getAttribute?.('data-boon-focused') || null,
                    autofocus: element.getAttribute?.('autofocus') || null,
                    connected: !!element.isConnected,
                    active: element === document.activeElement,
                } : null;
                const inputs = Array.from(preview.querySelectorAll('input, textarea, [contenteditable=\"true\"]')).filter((element) =>
                    element.matches?.('textarea, [contenteditable=\"true\"]')
                    || (element.matches?.('input')
                        && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                );
                return {
                    activeElement: describe(document.activeElement),
                    previewFocus: describe(preview.querySelector(':focus')),
                    remembered: describe(window.__boonLastPreviewTextInput),
                    textInputs: inputs.map((element, index) => ({
                        index,
                        ...describe(element),
                    })),
                };
            })()"#
                .to_string(),
        },
    )
    .await?;

    match response {
        WsResponse::Success { data: Some(value) } => Ok(value.to_string()),
        WsResponse::Success { data: None } => Ok("null".to_string()),
        WsResponse::Error { message } => {
            anyhow::bail!("Focused preview debug eval failed: {}", message)
        }
        _ => anyhow::bail!("Unexpected response for focused preview debug eval"),
    }
}

async fn preview_text_input_properties_via_eval(
    port: u16,
    index: u32,
) -> Result<(bool, Option<String>, Option<String>)> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ found: false, placeholder: null, value: null }};
                    const inputs = Array.from(
                        preview.querySelectorAll('input, textarea, [contenteditable="true"]')
                    ).filter((element) =>
                        element.matches?.('textarea, [contenteditable="true"]')
                        || (element.matches?.('input')
                            && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                    );
                    const input = inputs[{index}];
                    if (!input) {{
                        return {{ found: false, placeholder: null, value: null }};
                    }}
                    return {{
                        found: true,
                        placeholder: input.placeholder || input.getAttribute('placeholder') || null,
                        value: input.value ?? input.textContent ?? ''
                    }};
                }})()"#,
                index = index,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok((
            value
                .get("found")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            value
                .get("placeholder")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            value
                .get("value")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        )),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok((
                value
                    .get("found")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                value
                    .get("placeholder")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                value
                    .get("value")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            ))
        }
        WsResponse::Error { message } => anyhow::bail!("Preview input eval failed: {}", message),
        _ => Ok((false, None, None)),
    }
}

async fn preview_button_disabled_via_eval(port: u16, index: u32) -> Result<(bool, bool, String)> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ found: false, disabled: false, text: '' }};
                    const isCheckboxInput = (el) =>
                        el
                        && el.tagName === 'INPUT'
                        && ((el.getAttribute('type') || '').toLowerCase() === 'checkbox');
                    const isVisible = (el) => {{
                        const rect = el.getBoundingClientRect();
                        if (rect.width === 0 || rect.height === 0) return false;
                        const style = window.getComputedStyle(el);
                        return style.display !== 'none' && style.visibility !== 'hidden';
                    }};
                    const buttons = Array.from(
                        preview.querySelectorAll('button, [role="button"], [data-boon-port-click]')
                    ).filter((element) => !isCheckboxInput(element) && isVisible(element));
                    const button = buttons[{index}];
                    if (!button) {{
                        return {{ found: false, disabled: false, text: '' }};
                    }}
                    return {{
                        found: true,
                        disabled: !!button.disabled || button.getAttribute('aria-disabled') === 'true',
                        text: (button.textContent || '').trim()
                    }};
                }})()"#,
                index = index,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok((
            value
                .get("found")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            value
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            value
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        )),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok((
                value
                    .get("found")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                value
                    .get("disabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                value
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ))
        }
        WsResponse::Error { message } => anyhow::bail!("Preview button eval failed: {}", message),
        _ => Ok((false, false, String::new())),
    }
}

async fn wait_for_focused_text_input_suffix(port: u16, expected_suffix: &str) -> Result<()> {
    if expected_suffix.is_empty() {
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    let js = r#"(function() {
        const preview = document.querySelector('[data-boon-panel="preview"]');
        if (!preview) return { found: false, value: null };
        const isTextInput = (element) =>
            element
            && element !== document.body
            && (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA');
        const remembered = window.__boonLastPreviewTextInput;
        const previewFocused = preview.querySelector(':focus');
        let focused = isTextInput(previewFocused)
            ? previewFocused
            : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                ? document.activeElement
                : preview.querySelector('[data-boon-focused="true"]')
                    || preview.querySelector('[focused="true"]')
                    || preview.querySelector('input[autofocus], textarea[autofocus]')
                    || null);
        if (!isTextInput(focused)) {
            focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                ? remembered
                : null;
        }
        if (!isTextInput(focused)) {
            return { found: false, value: null };
        }
        return { found: true, value: focused.value || '' };
    })()"#;

    let mut saw_input = false;
    let mut last_value = String::new();
    loop {
        let response = send_command_to_server(
            port,
            WsCommand::EvalJs {
                expression: js.to_string(),
            },
        )
        .await?;
        match response {
            WsResponse::Success {
                data: Some(serde_json::Value::Object(obj)),
            } => {
                let found = obj.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
                let value = obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if found {
                    saw_input = true;
                    last_value = value.clone();
                    if value.ends_with(expected_suffix) {
                        return Ok(());
                    }
                }
            }
            WsResponse::Error { .. } => return Ok(()),
            _ => return Ok(()),
        }

        if Instant::now() >= deadline {
            if saw_input {
                anyhow::bail!(
                    "Focused input value did not settle after typing {:?}; last value: {:?}",
                    expected_suffix,
                    last_value
                );
            }
            anyhow::bail!(
                "No focused preview text input was available while waiting for typed suffix {:?}",
                expected_suffix
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[allow(dead_code)]
async fn dispatch_preview_special_key_via_eval(port: u16, key: &str) -> Result<bool> {
    if !matches!(key, "Enter" | "Escape") {
        return Ok(false);
    }

    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ dispatched: false }};
                    const isTextInput = (element) =>
                        element
                        && element !== document.body
                        && (
                            element.tagName === 'TEXTAREA'
                            || (element.tagName === 'INPUT'
                                && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                        );
                    const remembered = window.__boonLastPreviewTextInput;
                    const rememberedNodeId =
                        window.__boonLastPreviewTextInputNodeId ||
                        remembered?.getAttribute?.('data-boon-node-id') ||
                        null;
                    const previewFocused = preview.querySelector(':focus');
                    let focused = isTextInput(previewFocused)
                        ? previewFocused
                        : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                            ? document.activeElement
                            : preview.querySelector('[data-boon-focused="true"]')
                                || preview.querySelector('[focused="true"]')
                                || preview.querySelector('input[autofocus], textarea[autofocus]')
                                || null);
                    if (!isTextInput(focused) && rememberedNodeId) {{
                        focused = preview.querySelector('[data-boon-node-id="' + rememberedNodeId + '"]');
                    }}
                    if (!isTextInput(focused)) {{
                        focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                            ? remembered
                            : null;
                    }}
                    if (!isTextInput(focused)) return {{ dispatched: false }};
                    const keyDownPort = focused.getAttribute('data-boon-port-key-down');
                    const dispatchEvent = window.__boonDispatchUiEvent;
                    if (!keyDownPort || typeof dispatchEvent !== 'function') {{
                        return {{ dispatched: false }};
                    }}
                    window.__boonLastPreviewTextInput = focused;
                    window.__boonLastPreviewTextInputNodeId =
                        focused.getAttribute('data-boon-node-id');
                    const inputs = Array.from(
                        preview.querySelectorAll('input, textarea, [contenteditable="true"]')
                    ).filter((element) =>
                        element.matches?.('textarea, [contenteditable="true"]')
                        || (element.matches?.('input')
                            && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                    );
                    window.__boonLastPreviewTextInputIndex = inputs.indexOf(focused);
                    const payload = {key_json} + '\u001f' + (focused.value || '');
                    dispatchEvent(keyDownPort, 'KeyDown', payload);
                    return {{
                        dispatched: true,
                        nodeId: focused.getAttribute('data-boon-node-id'),
                        keyDownPort
                    }};
                }})()"#,
                key_json = serde_json::to_string(key)?,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok(value
            .get("dispatched")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok(value
                .get("dispatched")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Preview special key eval failed: {}", message)
        }
        _ => Ok(false),
    }
}

async fn type_preview_text_via_eval(port: u16, text: &str) -> Result<bool> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ dispatched: false }};
                    const isTextInput = (element) =>
                        element
                        && element !== document.body
                        && (
                            element.tagName === 'TEXTAREA'
                            || (element.tagName === 'INPUT'
                                && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                        );
                    const inputs = Array.from(
                        preview.querySelectorAll('input, textarea, [contenteditable="true"]')
                    ).filter((element) =>
                        element.matches?.('textarea, [contenteditable="true"]')
                        || (element.matches?.('input')
                            && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                    );
                    const remembered = window.__boonLastPreviewTextInput;
                    const rememberedNodeId =
                        window.__boonLastPreviewTextInputNodeId ||
                        remembered?.getAttribute?.('data-boon-node-id') ||
                        null;
                    const rememberedIndex =
                        typeof window.__boonLastPreviewTextInputIndex === 'number'
                            ? window.__boonLastPreviewTextInputIndex
                            : null;
                    const previewFocused = preview.querySelector(':focus');
                    const hintedFocused =
                        preview.querySelector('[data-boon-focused="true"]')
                        || preview.querySelector('[focused="true"]')
                        || preview.querySelector('input[autofocus], textarea[autofocus]')
                        || null;
                    let focused = isTextInput(hintedFocused)
                        ? hintedFocused
                        : (isTextInput(previewFocused)
                            ? previewFocused
                            : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                                ? document.activeElement
                                : null));
                    if (!isTextInput(focused) && rememberedNodeId) {{
                        focused = preview.querySelector('[data-boon-node-id="' + rememberedNodeId + '"]');
                    }}
                    if (!isTextInput(focused) && rememberedIndex != null) {{
                        focused = inputs[rememberedIndex] || focused;
                    }}
                    if (!isTextInput(focused)) {{
                        focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                            ? remembered
                            : null;
                    }}
                    if (!isTextInput(focused)) return {{ dispatched: false }};
                    if (document.activeElement !== focused && typeof focused.focus === 'function') {{
                        focused.focus();
                    }}
                    const start0 =
                        typeof focused.selectionStart === 'number'
                            ? focused.selectionStart
                            : focused.value.length;
                    const end0 =
                        typeof focused.selectionEnd === 'number'
                            ? focused.selectionEnd
                            : start0;
                    let start = start0;
                    let end = end0;
                    if (
                        focused === hintedFocused
                        && focused.value
                        && start === 0
                        && end === 0
                    ) {{
                        start = focused.value.length;
                        end = start;
                    }}
                    const insertedText = {text_json};
                    if (
                        focused.value
                        && start === 0
                        && end === focused.value.length
                        && typeof insertedText === 'string'
                        && /^\\s/.test(insertedText)
                    ) {{
                        start = focused.value.length;
                        end = start;
                    }}
                    const nextValue =
                        focused.value.slice(0, start) +
                        insertedText +
                        focused.value.slice(end);
                    const nativeSetter =
                        Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set ||
                        Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set;
                    if (nativeSetter) {{
                        nativeSetter.call(focused, nextValue);
                    }} else {{
                        focused.value = nextValue;
                    }}
                    const caret = start + insertedText.length;
                    if (typeof focused.setSelectionRange === 'function') {{
                        focused.setSelectionRange(caret, caret);
                    }}
                    window.__boonLastPreviewTextInput = focused;
                    window.__boonLastPreviewTextInputNodeId =
                        focused.getAttribute('data-boon-node-id');
                    window.__boonLastPreviewTextInputIndex = inputs.indexOf(focused);
                    window.__boonLastPreviewTextSelectionStart = caret;
                    window.__boonLastPreviewTextSelectionEnd = caret;
                    focused.dispatchEvent(new InputEvent('input', {{
                        bubbles: true,
                        composed: true,
                        data: insertedText,
                        inputType: 'insertText'
                    }}));
                    const dispatchFact = window.__boonDispatchUiFact;
                    const dispatchEvent = window.__boonDispatchUiEvent;
                    const nodeId = focused.getAttribute('data-boon-node-id');
                    const inputPort = focused.getAttribute('data-boon-port-input');
                    if (typeof dispatchFact === 'function' && nodeId) {{
                        dispatchFact(nodeId, 'DraftText', focused.value || '');
                    }}
                    if (typeof dispatchEvent === 'function' && inputPort) {{
                        dispatchEvent(inputPort, 'Input', focused.value || '');
                    }}
                    return {{
                        dispatched: true,
                        value: focused.value || '',
                        nodeId,
                        inputPort
                    }};
                }})()"#,
                text_json = serde_json::to_string(text)?,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok(value
            .get("dispatched")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok(value
                .get("dispatched")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Preview type text eval failed: {}", message)
        }
        _ => Ok(false),
    }
}

async fn append_preview_text_via_eval(port: u16, text: &str) -> Result<bool> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ dispatched: false }};
                    const isTextInput = (element) =>
                        element
                        && element !== document.body
                        && (
                            element.tagName === 'TEXTAREA'
                            || (element.tagName === 'INPUT'
                                && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
                        );
                    const remembered = window.__boonLastPreviewTextInput;
                    const previewFocused = preview.querySelector(':focus');
                    let focused = isTextInput(previewFocused)
                        ? previewFocused
                        : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                            ? document.activeElement
                            : preview.querySelector('[data-boon-focused="true"]')
                                || preview.querySelector('[focused="true"]')
                                || preview.querySelector('input[autofocus], textarea[autofocus]'));
                    if (!isTextInput(focused)) {{
                        focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                            ? remembered
                            : null;
                    }}
                    if (!isTextInput(focused) || !focused.value) return {{ dispatched: false }};
                    const insertedText = {text_json};
                    const nextValue = (focused.value || '') + insertedText;
                    const nativeSetter =
                        Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set ||
                        Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set;
                    if (nativeSetter) {{
                        nativeSetter.call(focused, nextValue);
                    }} else {{
                        focused.value = nextValue;
                    }}
                    const caret = nextValue.length;
                    if (typeof focused.setSelectionRange === 'function') {{
                        focused.setSelectionRange(caret, caret);
                    }}
                    window.__boonLastPreviewTextInput = focused;
                    window.__boonLastPreviewTextInputNodeId =
                        focused.getAttribute('data-boon-node-id');
                    window.__boonLastPreviewTextSelectionStart = caret;
                    window.__boonLastPreviewTextSelectionEnd = caret;
                    focused.dispatchEvent(new InputEvent('input', {{
                        bubbles: true,
                        composed: true,
                        data: insertedText,
                        inputType: 'insertText'
                    }}));
                    const dispatchFact = window.__boonDispatchUiFact;
                    const dispatchEvent = window.__boonDispatchUiEvent;
                    const nodeId = focused.getAttribute('data-boon-node-id');
                    const inputPort = focused.getAttribute('data-boon-port-input');
                    if (typeof dispatchFact === 'function' && nodeId) {{
                        dispatchFact(nodeId, 'DraftText', focused.value || '');
                    }}
                    if (typeof dispatchEvent === 'function' && inputPort) {{
                        dispatchEvent(inputPort, 'Input', focused.value || '');
                    }}
                    return {{ dispatched: true, value: focused.value || '' }};
                }})()"#,
                text_json = serde_json::to_string(text)?,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok(value
            .get("dispatched")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok(value
                .get("dispatched")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Preview append text eval failed: {}", message)
        }
        _ => Ok(false),
    }
}

async fn click_button_near_text_via_eval(port: u16, text: &str, button_text: &str) -> Result<bool> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ clicked: false, reason: 'preview root not found' }};

                    const visible = (el) => {{
                        if (!el) return false;
                        const rect = el.getBoundingClientRect();
                        const style = window.getComputedStyle(el);
                        return rect.width > 0
                            && rect.height > 0
                            && style.display !== 'none'
                            && style.visibility !== 'hidden';
                    }};

                    const directTextOf = (el) =>
                        Array.from(el.childNodes)
                            .filter((node) => node.nodeType === Node.TEXT_NODE)
                            .map((node) => node.textContent || '')
                            .join('')
                            .trim();

                    const matchesText = (candidate) => {{
                        const direct = directTextOf(candidate);
                        const full = (candidate.textContent || '').trim();
                        return direct === {text_json}
                            || direct.includes({text_json})
                            || full === {text_json}
                            || full.includes({text_json});
                    }};

                    const candidates = Array.from(preview.querySelectorAll('*'))
                        .filter((el) => visible(el) && matchesText(el));
                    if (candidates.length === 0) {{
                        return {{ clicked: false, reason: 'target text not found' }};
                    }}

                    candidates.sort((a, b) => {{
                        const ar = a.getBoundingClientRect();
                        const br = b.getBoundingClientRect();
                        return (ar.width * ar.height) - (br.width * br.height);
                    }});

                    let target = candidates[0];
                    let container = target;
                    const buttonMatches = (button) => {{
                        const direct = directTextOf(button);
                        const full = (button.textContent || '').trim();
                        return direct === {button_json}
                            || full === {button_json}
                            || direct.includes({button_json})
                            || full.includes({button_json});
                    }};
                    for (let depth = 0; depth < 5 && container; depth += 1) {{
                        const buttons = Array.from(
                            container.querySelectorAll('button, [role="button"]')
                        ).filter((el) => visible(el));
                        const match = buttons.find((button) => buttonMatches(button));
                        if (match) {{
                            const port = match.getAttribute('data-boon-port-click');
                            const dispatchEvent = window.__boonDispatchUiEvent;
                            if (port && typeof dispatchEvent === 'function') {{
                                dispatchEvent(port, 'Click');
                            }} else if (typeof match.click === 'function') {{
                                match.click();
                            }} else {{
                                match.dispatchEvent(new MouseEvent('click', {{ bubbles: true, composed: true }}));
                            }}
                            return {{ clicked: true }};
                        }}
                        container = container.parentElement;
                    }}

                    const visibleMatches = Array.from(
                        preview.querySelectorAll('button, [role="button"]')
                    ).filter((button) => visible(button) && buttonMatches(button));
                    if (visibleMatches.length > 0) {{
                        const targetRect = target.getBoundingClientRect();
                        visibleMatches.sort((a, b) => {{
                            const ar = a.getBoundingClientRect();
                            const br = b.getBoundingClientRect();
                            const ax = ar.left + (ar.width / 2);
                            const ay = ar.top + (ar.height / 2);
                            const bx = br.left + (br.width / 2);
                            const by = br.top + (br.height / 2);
                            const tx = targetRect.left + (targetRect.width / 2);
                            const ty = targetRect.top + (targetRect.height / 2);
                            const ad = Math.hypot(ax - tx, ay - ty);
                            const bd = Math.hypot(bx - tx, by - ty);
                            return ad - bd;
                        }});
                        const match = visibleMatches[0];
                        const port = match.getAttribute('data-boon-port-click');
                        const dispatchEvent = window.__boonDispatchUiEvent;
                        if (port && typeof dispatchEvent === 'function') {{
                            dispatchEvent(port, 'Click');
                        }} else if (typeof match.click === 'function') {{
                            match.click();
                        }} else {{
                            match.dispatchEvent(new MouseEvent('click', {{ bubbles: true, composed: true }}));
                        }}
                        return {{ clicked: true }};
                    }}

                    return {{ clicked: false, reason: 'button not found near target' }};
                }})()"#,
                text_json = serde_json::to_string(text)?,
                button_json = serde_json::to_string(button_text)?,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok(value
            .get("clicked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok(value
                .get("clicked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Click button near text eval failed: {}", message)
        }
        _ => Ok(false),
    }
}

async fn click_button_by_index_via_eval(port: u16, index: u32) -> Result<bool> {
    let response = send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: format!(
                r#"(function() {{
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return {{ clicked: false, reason: 'preview root not found' }};

                    const isCheckboxInput = (el) =>
                        el
                        && el.tagName === 'INPUT'
                        && ((el.getAttribute('type') || '').toLowerCase() === 'checkbox');
                    const isVisible = (el) => {{
                        if (!el) return false;
                        const rect = el.getBoundingClientRect();
                        const style = window.getComputedStyle(el);
                        return rect.width > 0
                            && rect.height > 0
                            && style.display !== 'none'
                            && style.visibility !== 'hidden';
                    }};

                    const buttons = Array.from(
                        preview.querySelectorAll('button, [role="button"], [data-boon-port-click]')
                    ).filter((element) => !isCheckboxInput(element) && isVisible(element));
                    const button = buttons[{index}];
                    if (!button) {{
                        return {{ clicked: false, reason: 'button index out of range' }};
                    }}

                    const port = button.getAttribute('data-boon-port-click');
                    const dispatchEvent = window.__boonDispatchUiEvent;
                    if (port && typeof dispatchEvent === 'function') {{
                        dispatchEvent(port, 'Click');
                        return {{ clicked: true, mode: 'port' }};
                    }}

                    if (typeof button.click === 'function') {{
                        button.click();
                        return {{ clicked: true, mode: 'native' }};
                    }}

                    button.dispatchEvent(new MouseEvent('click', {{ bubbles: true, composed: true }}));
                    return {{ clicked: true, mode: 'mouse' }};
                }})()"#,
                index = index,
            ),
        },
    )
    .await?;

    match response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(value)),
        } => Ok(value
            .get("clicked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)),
        WsResponse::Success {
            data: Some(serde_json::Value::String(json)),
        } => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok(value
                .get("clicked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        }
        WsResponse::Error { message } => anyhow::bail!("Click button eval failed: {}", message),
        _ => Ok(false),
    }
}

async fn get_current_focused_input_index(port: u16) -> Result<Option<u32>> {
    let response = send_command_to_server(port, WsCommand::GetFocusedElement).await?;
    match response {
        WsResponse::FocusedElement {
            tag_name,
            input_index,
            ..
        } => {
            let (fallback_tag_name, fallback_input_index) =
                focused_preview_input_via_eval(port).await?;
            if fallback_tag_name.is_some() {
                Ok(fallback_input_index)
            } else if tag_name.is_some() {
                Ok(input_index)
            } else {
                Ok(None)
            }
        }
        WsResponse::Error { message } => anyhow::bail!("Get focused element failed: {}", message),
        _ => Ok(None),
    }
}

#[allow(dead_code)]
async fn set_input_value_by_index(port: u16, index: u32, value: &str) -> Result<()> {
    let js = format!(
        r#"(function() {{
            const preview = document.querySelector('[data-boon-panel="preview"]');
            if (!preview) return 'ERROR: preview root not found';
            const inputs = Array.from(preview.querySelectorAll('input, textarea')).filter((element) =>
                element.tagName === 'TEXTAREA'
                || (element.tagName === 'INPUT'
                    && !['checkbox', 'radio', 'button', 'submit', 'reset', 'hidden'].includes((element.type || '').toLowerCase()))
            );
            const input = inputs[{index}];
            if (!input) return 'ERROR: input index {index} not found (have ' + inputs.length + ')';
            if (typeof input.focus === 'function') {{
                input.focus();
            }}
            window.__boonLastPreviewTextInput = input;
            window.__boonLastPreviewTextInputNodeId = input.getAttribute('data-boon-node-id');
            window.__boonLastPreviewTextInputIndex = inputs.indexOf(input);
            window.__boonPreferredTextInputIndex = {index};
            if (typeof input.setSelectionRange === 'function') {{
                const caret = input.value.length;
                input.setSelectionRange(caret, caret);
                window.__boonLastPreviewTextSelectionStart = caret;
                window.__boonLastPreviewTextSelectionEnd = caret;
            }}
            return {{
                ok: true,
                path: 'focus-select-only-by-index',
                index: {index},
                nodeId: input.getAttribute('data-boon-node-id'),
                inputPort: input.getAttribute('data-boon-port-input'),
                value: input.value || '',
                valueLength: (input.value || '').length
            }};
        }})()"#,
        index = index,
    );
    let response = send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
    let existing_len = match &response {
        WsResponse::Success {
            data: Some(serde_json::Value::Object(obj)),
        } => obj
            .get("valueLength")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize,
        _ => 0,
    };
    match response {
        WsResponse::Success { data } => {
            if let Some(serde_json::Value::String(ref d)) = data {
                if d.starts_with("ERROR") {
                    anyhow::bail!("Set input value by index failed: {}", d);
                }
            }
        }
        WsResponse::Error { message } => {
            anyhow::bail!("Set input value by index failed: {}", message);
        }
        _ => {}
    }
    for _ in 0..existing_len {
        send_command_to_server(
            port,
            WsCommand::PressKey {
                key: "Backspace".to_string(),
            },
        )
        .await?;
    }
    send_command_to_server(
        port,
        WsCommand::TypeText {
            text: value.to_string(),
        },
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

async fn get_preview_stability_snapshot(port: u16) -> Result<(String, Option<String>)> {
    let preview = get_preview(port).await?;
    let elements_signature = match send_command_to_server(port, WsCommand::GetPreviewElements).await
    {
        Ok(WsResponse::PreviewElements { data }) => {
            let elements = preview_element_infos(&data);
            let mut signature = format!("count={}", elements.len());
            for element in elements.into_iter().take(16) {
                use std::fmt::Write as _;
                let _ = write!(
                    &mut signature,
                    "|{}@{:.0},{:.0},{:.0},{:.0}",
                    element.direct_text, element.x, element.y, element.width, element.height
                );
            }
            Some(signature)
        }
        _ => None,
    };

    Ok((preview, elements_signature))
}

async fn wait_for_preview_to_settle(port: u16) {
    let timeout = Duration::from_millis(1000);
    let poll_interval = Duration::from_millis(50);
    let start = Instant::now();
    let mut last_snapshot: Option<(String, Option<String>)> = None;
    let mut stable_reads = 0u8;

    while start.elapsed() <= timeout {
        let snapshot = match get_preview_stability_snapshot(port).await {
            Ok(snapshot) => snapshot,
            Err(_) => return,
        };

        if preview_needs_retry(Some(snapshot.0.as_str())) {
            stable_reads = 0;
            last_snapshot = Some(snapshot);
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        if last_snapshot.as_ref() == Some(&snapshot) {
            stable_reads += 1;
            if stable_reads >= 2 {
                return;
            }
        } else {
            stable_reads = 0;
            last_snapshot = Some(snapshot);
        }

        tokio::time::sleep(poll_interval).await;
    }
}

async fn wait_for_preview_change_then_settle(
    port: u16,
    before_snapshot: Option<(String, Option<String>)>,
) -> bool {
    let Some(before_snapshot) = before_snapshot else {
        wait_for_preview_to_settle(port).await;
        return false;
    };

    let timeout = Duration::from_millis(4000);
    let quiet_period = Duration::from_millis(300);
    let active_change_window = Duration::from_millis(400);
    let poll_interval = Duration::from_millis(50);
    let start = Instant::now();
    let mut last_snapshot = before_snapshot;
    let mut first_change_at: Option<Instant> = None;
    let mut last_change_at: Option<Instant> = None;
    let mut change_count = 0u8;

    while start.elapsed() <= timeout {
        let snapshot = match get_preview_stability_snapshot(port).await {
            Ok(snapshot) => snapshot,
            Err(_) => return false,
        };

        if snapshot != last_snapshot {
            last_snapshot = snapshot;
            let now = Instant::now();
            first_change_at.get_or_insert(now);
            last_change_at = Some(now);
            change_count = change_count.saturating_add(1);
        } else if let Some(last_change_at) = last_change_at {
            if last_change_at.elapsed() >= quiet_period {
                return true;
            }
        }

        if let Some(first_change_at) = first_change_at {
            if change_count >= 2 && first_change_at.elapsed() >= active_change_window {
                return true;
            }
        }

        tokio::time::sleep(poll_interval).await;
    }

    last_change_at.is_some()
}

/// Execute a parsed action
async fn execute_action(
    port: u16,
    engine: Option<&str>,
    action: &ParsedAction,
    preferred_input_index: &mut Option<u32>,
    verbose: bool,
) -> Result<()> {
    match action {
        ParsedAction::Click { selector } => {
            let response = send_command_to_server(
                port,
                WsCommand::Click {
                    selector: selector.clone(),
                },
            )
            .await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click failed: {}", message);
            }
            // Small delay after click for UI to update
            tokio::time::sleep(Duration::from_millis(100)).await;
            wait_for_preview_to_settle(port).await;
        }
        ParsedAction::Type { selector, text } => {
            let response = send_command_to_server(
                port,
                WsCommand::Type {
                    selector: selector.clone(),
                    text: text.clone(),
                },
            )
            .await?;
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
            // Also clear ActorsLite persistence data
            if engine.as_deref() == Some("ActorsLite") {
                let clear_expression = r#"(function() {
                    var keys = [];
                    for (var i = 0; i < localStorage.length; i++) {
                        var key = localStorage.key(i);
                        if (key && key.startsWith("boon.actorslite")) {
                            keys.push(key);
                        }
                    }
                    keys.forEach(function(k) { localStorage.removeItem(k); });
                    return keys.length;
                })()"#;
                let _ = send_command_to_server(
                    port,
                    WsCommand::EvalJs {
                        expression: clear_expression.to_string(),
                    },
                )
                .await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        ParsedAction::Run => {
            let response = send_command_to_server(port, WsCommand::TriggerRun).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Run failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        ParsedAction::Key { key } => {
            let before_snapshot = get_preview_stability_snapshot(port).await.ok();
            let response =
                send_command_to_server(port, WsCommand::PressKey { key: key.clone() }).await?;
            if verbose {
                println!("[press-key] {:?}", response);
            }
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Key press failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            if matches!(key.as_str(), "Enter" | "Escape" | "Backspace" | "Delete") {
                let changed =
                    wait_for_preview_change_then_settle(port, before_snapshot.clone()).await;
                if !changed {
                    wait_for_preview_to_settle(port).await;
                }
            } else {
                wait_for_preview_to_settle(port).await;
            }
        }
        ParsedAction::FocusInput { index } => {
            let already_focused = get_current_focused_input_index(port).await? == Some(*index);
            if !already_focused {
                let response =
                    send_command_to_server(port, WsCommand::FocusInput { index: *index }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Focus input failed: {}", message);
                }
            }
            *preferred_input_index = Some(*index);
            if engine != Some("FactoryFabric") {
                let js = format!(
                    r#"(function() {{
                        window.__boonPreferredTextInputIndex = {index};
                        return true;
                    }})()"#,
                    index = index,
                );
                let _ = send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::TypeText { text } => {
            let send_type_text = |port: u16, text: String| async move {
                let response = send_command_to_server(port, WsCommand::TypeText { text }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Type text failed: {}", message);
                }
                Result::<()>::Ok(())
            };

            if engine == Some("ActorsLite") {
                if text.starts_with(char::is_whitespace) {
                    let handled_in_preview = append_preview_text_via_eval(port, text).await?
                        || type_preview_text_via_eval(port, text).await?;
                    if !handled_in_preview {
                        send_type_text(port, text.clone()).await?;
                    }
                } else {
                    let target_index = (*preferred_input_index)
                        .or(get_current_focused_input_index(port).await?)
                        .or(Some(0));
                    if let Some(index) = target_index {
                        *preferred_input_index = Some(index);
                        let response =
                            send_command_to_server(port, WsCommand::FocusInput { index }).await?;
                        if let WsResponse::Error { message } = response {
                            if index != 0 {
                                let fallback_index = 0;
                                *preferred_input_index = Some(fallback_index);
                                let fallback_response = send_command_to_server(
                                    port,
                                    WsCommand::FocusInput {
                                        index: fallback_index,
                                    },
                                )
                                .await?;
                                if let WsResponse::Error {
                                    message: fallback_message,
                                } = fallback_response
                                {
                                    anyhow::bail!(
                                        "Type text failed: focus retry before typing failed at index {}: {}; fallback focus {} failed: {}",
                                        index,
                                        message,
                                        fallback_index,
                                        fallback_message
                                    );
                                }
                            } else {
                                anyhow::bail!(
                                    "Type text failed: focus retry before typing failed: {}",
                                    message
                                );
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    send_type_text(port, text.clone()).await?;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                wait_for_preview_to_settle(port).await;
            } else {
                let prefer_command_path = engine == Some("FactoryFabric");
                let handled_in_preview = if prefer_command_path {
                    false
                } else if text.starts_with(char::is_whitespace) {
                    append_preview_text_via_eval(port, text).await?
                        || type_preview_text_via_eval(port, text).await?
                } else {
                    type_preview_text_via_eval(port, text).await?
                };
                if prefer_command_path || !handled_in_preview {
                    send_type_text(port, text.clone()).await?;
                }
                if let Err(first_err) = wait_for_focused_text_input_suffix(port, text).await {
                    let focused_index = get_current_focused_input_index(port).await?;
                    let retry_index = focused_index.or(*preferred_input_index).or(Some(0));
                    if let Some(index) = retry_index {
                        *preferred_input_index = Some(index);
                        let response =
                            send_command_to_server(port, WsCommand::FocusInput { index }).await?;
                        if let WsResponse::Error { message } = response {
                            if index != 0 {
                                let fallback_index = 0;
                                *preferred_input_index = Some(fallback_index);
                                let fallback_response = send_command_to_server(
                                    port,
                                    WsCommand::FocusInput {
                                        index: fallback_index,
                                    },
                                )
                                .await?;
                                if let WsResponse::Error {
                                    message: fallback_message,
                                } = fallback_response
                                {
                                    anyhow::bail!(
                                        "Type text failed after retry focus attempt: {}; focus retry failed at index {}: {}; fallback focus {} failed: {}",
                                        first_err,
                                        index,
                                        message,
                                        fallback_index,
                                        fallback_message
                                    );
                                }
                            } else {
                                anyhow::bail!(
                                    "Type text failed after retry focus attempt: {}; focus retry failed: {}",
                                    first_err,
                                    message
                                );
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        send_type_text(port, text.clone()).await?;
                        wait_for_focused_text_input_suffix(port, text).await?;
                    } else {
                        return Err(first_err);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        ParsedAction::ClickText { text } => {
            let before_snapshot = get_preview_stability_snapshot(port).await.ok();
            let clicked_in_page = click_preview_text_element(port, text, false).await?;
            if !clicked_in_page {
                let bounds = wait_for_preview_element_bounds_by_text(port, text, false).await?;
                let x = bounds.x + bounds.width / 2;
                let y = bounds.y + bounds.height / 2;
                let response = send_command_to_server(port, WsCommand::ClickAt { x, y }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Click text failed: {}", message);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let changed = wait_for_preview_change_then_settle(port, before_snapshot.clone()).await;
            if !changed {
                let bounds = wait_for_preview_element_bounds_by_text(port, text, false).await?;
                let x = bounds.x + bounds.width / 2;
                let y = bounds.y + bounds.height / 2;
                let response = send_command_to_server(port, WsCommand::ClickAt { x, y }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Click text failed: {}", message);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = wait_for_preview_change_then_settle(port, before_snapshot).await;
            }
        }
        ParsedAction::ClickButton { index } => {
            let before_snapshot = get_preview_stability_snapshot(port).await.ok();
            let response =
                send_command_to_server(port, WsCommand::ClickButton { index: *index }).await?;
            let primary_error = if let WsResponse::Error { message } = response {
                Some(message)
            } else {
                None
            };

            let changed = if primary_error.is_none() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                wait_for_preview_change_then_settle(port, before_snapshot.clone()).await
            } else {
                false
            };

            if primary_error.is_some() || !changed {
                if !click_button_by_index_via_eval(port, *index).await? {
                    if let Some(message) = primary_error {
                        anyhow::bail!("Click button failed: {}; eval fallback failed", message);
                    }
                    anyhow::bail!(
                        "Click button failed: no preview change and eval fallback failed"
                    );
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = wait_for_preview_change_then_settle(port, before_snapshot).await;
            }
        }
        ParsedAction::ClickButtonNearText { text, button_text } => {
            let before_snapshot = get_preview_stability_snapshot(port).await.ok();
            let response = send_command_to_server(
                port,
                WsCommand::ClickButtonNearText {
                    text: text.clone(),
                    button_text: button_text.clone(),
                },
            )
            .await?;
            if let WsResponse::Error { message } = response {
                let label = button_text.as_deref().unwrap_or("×");
                if !click_button_near_text_via_eval(port, text, label).await? {
                    anyhow::bail!("Click button near text failed: {}", message);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = wait_for_preview_change_then_settle(port, before_snapshot).await;
        }
        ParsedAction::ClickCheckbox { index } => {
            let before_snapshot = get_preview_stability_snapshot(port).await.ok();
            let response =
                send_command_to_server(port, WsCommand::ClickCheckbox { index: *index }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click checkbox failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = wait_for_preview_change_then_settle(port, before_snapshot).await;
        }
        ParsedAction::ClickAt { x, y } => {
            let response =
                send_command_to_server(port, WsCommand::ClickAt { x: *x, y: *y }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Click at failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            wait_for_preview_to_settle(port).await;
        }
        ParsedAction::DblClickText { text } => {
            let response = send_command_to_server(
                port,
                WsCommand::DoubleClickByText {
                    text: text.clone(),
                    exact: false,
                },
            )
            .await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Double-click text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::DblClickTextNth { text, index } => {
            let js = format!(
                r#"(function() {{
                    const root = document.querySelector('[data-boon-panel="preview"]');
                    if (!root) return {{ error: 'preview root not found' }};
                    const targetText = {text_json};
                    const wantedIndex = {wanted_index};
                    const matches = [];
                    const directText = (el) => Array.from(el.childNodes)
                        .filter((n) => n.nodeType === Node.TEXT_NODE)
                        .map((n) => n.textContent || '')
                        .join('')
                        .trim();
                    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                    let node = walker.currentNode;
                    while (node) {{
                        const direct = directText(node);
                        const full = (node.innerText || node.textContent || '').trim();
                        if ((direct && direct === targetText) || (!direct && full === targetText)) {{
                            matches.push(node);
                        }}
                        node = walker.nextNode();
                    }}
                    if (wantedIndex >= matches.length) {{
                        return {{ error: `exact text '${{targetText}}' match ${{wantedIndex}} not found (found ${{matches.length}})` }};
                    }}
                    const rect = matches[wantedIndex].getBoundingClientRect();
                    return {{
                        x: Math.round(rect.left + rect.width / 2),
                        y: Math.round(rect.top + rect.height / 2),
                        count: matches.length
                    }};
                }})()"#,
                text_json = serde_json::to_string(text)?,
                wanted_index = index,
            );
            let response =
                send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
            let (x, y) = match response {
                WsResponse::Success { data } => {
                    let Some(serde_json::Value::Object(obj)) = data else {
                        anyhow::bail!(
                            "Double-click nth text failed: JS did not return coordinates"
                        );
                    };
                    if let Some(error) = obj.get("error").and_then(|v| v.as_str()) {
                        anyhow::bail!("Double-click nth text failed: {}", error);
                    }
                    let x = obj
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .context("Double-click nth text failed: missing x")?;
                    let y = obj
                        .get("y")
                        .and_then(|v| v.as_i64())
                        .context("Double-click nth text failed: missing y")?;
                    (x as i32, y as i32)
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Double-click nth text failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for EvalJs"),
            };

            let response = send_command_to_server(port, WsCommand::DoubleClickAt { x, y }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Double-click nth text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::DblClickAt { x, y } => {
            let response =
                send_command_to_server(port, WsCommand::DoubleClickAt { x: *x, y: *y }).await?;
            if let WsResponse::Error { message } = response {
                anyhow::bail!("Double-click at failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::DblClickCellsCell { row, column } => {
            let direct_response = send_command_to_server(
                port,
                WsCommand::DoubleClickCellsCell {
                    row: *row,
                    column: *column,
                },
            )
            .await?;
            match direct_response {
                WsResponse::Error { .. } => {}
                _ => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(());
                }
            }

            let lookup_deadline = Instant::now() + Duration::from_secs(10);
            let (x, y) = loop {
                let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
                match response {
                    WsResponse::PreviewElements { data } => {
                        match locate_cells_cell(&data, *row, *column) {
                            Ok(lookup) => {
                                let x = (lookup.cell.x + (lookup.cell.width / 2.0)).round() as i32;
                                let y = (lookup.cell.y + (lookup.cell.height / 2.0)).round() as i32;
                                break (x, y);
                            }
                            Err(error) => {
                                let retryable =
                                    error.contains("row label") || error.contains("cell (");
                                if retryable && Instant::now() < lookup_deadline {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                    continue;
                                }
                                anyhow::bail!("Double-click cells cell failed: {}", error);
                            }
                        }
                    }
                    WsResponse::Error { message } => {
                        anyhow::bail!("Double-click cells cell failed: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response for GetPreviewElements"),
                }
            };
            for _ in 0..2 {
                let response = send_command_to_server(port, WsCommand::ClickAt { x, y }).await?;
                if let WsResponse::Error { message } = response {
                    anyhow::bail!("Double-click cells cell failed: {}", message);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        ParsedAction::HoverText { text } => {
            let mut last_error = None;
            for attempt in 0..3 {
                let response = send_command_to_server(
                    port,
                    WsCommand::HoverByText {
                        text: text.clone(),
                        exact: false,
                    },
                )
                .await?;
                match response {
                    WsResponse::Error { message } => {
                        if is_retryable_browser_error(&message) && attempt < 2 {
                            last_error = Some(message);
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            continue;
                        }
                        anyhow::bail!("Hover text failed: {}", message);
                    }
                    _ => {
                        last_error = None;
                        break;
                    }
                }
            }
            if let Some(message) = last_error {
                anyhow::bail!("Hover text failed: {}", message);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ParsedAction::AssertCellsCellText {
            row,
            column,
            expected,
        } => {
            let lookup_deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
                match response {
                    WsResponse::PreviewElements { data } => {
                        let lookup = match locate_cells_cell(&data, *row, *column) {
                            Ok(lookup) => lookup,
                            Err(error) => {
                                let retryable =
                                    error.contains("row label") || error.contains("cell (");
                                if retryable && Instant::now() < lookup_deadline {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                    continue;
                                }
                                anyhow::bail!("Assert cells cell text failed: {}", error);
                            }
                        };
                        let actual = lookup.cell.direct_text.as_str();
                        if actual != expected {
                            anyhow::bail!(
                                "Assert cells cell text failed: expected cell ({}, {}) to be '{}', got '{}' (row matches: {}, viable rows: {}, first row cells: {})",
                                row,
                                column,
                                expected,
                                actual,
                                lookup.row_match_count,
                                lookup.viable_row_count,
                                serde_json::json!(lookup.first_row_cells)
                            );
                        }
                        break;
                    }
                    WsResponse::Error { message } => {
                        anyhow::bail!("Assert cells cell text failed: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response for GetPreviewElements"),
                }
            }
        }
        ParsedAction::AssertCellsRowVisible { row } => {
            let js = format!(
                r#"(function() {{
                    const root = document.querySelector('[data-boon-panel="preview"]');
                    if (!root) return {{ error: 'preview root not found' }};
                    const viewport = {{
                        left: 0,
                        top: 0,
                        right: window.innerWidth,
                        bottom: window.innerHeight,
                    }};
                    const targetRow = {target_row};
                    const directText = (el) => Array.from(el.childNodes)
                        .filter((n) => n.nodeType === Node.TEXT_NODE)
                        .map((n) => n.textContent || '')
                        .join('')
                        .trim();
                    const styleVisible = (el) => {{
                        const style = window.getComputedStyle(el);
                        return style.visibility !== 'hidden' && style.display !== 'none';
                    }};
                    const isVisibleInViewport = (rect) =>
                        rect.width > 0 &&
                        rect.height > 0 &&
                        rect.right > viewport.left &&
                        rect.left < viewport.right &&
                        rect.bottom > viewport.top &&
                        rect.top < viewport.bottom;
                    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                    let node = walker.currentNode;
                    let targetNode = null;
                    while (node) {{
                        if (styleVisible(node) && directText(node) === String(targetRow)) {{
                            targetNode = node;
                            break;
                        }}
                        node = walker.nextNode();
                    }}
                    if (!targetNode) {{
                        return {{ error: `row label ${{targetRow}} not found` }};
                    }}
                    targetNode.scrollIntoView({{ block: 'nearest', inline: 'nearest' }});
                    const rect = targetNode.getBoundingClientRect();
                    if (isVisibleInViewport(rect)) {{
                        return {{ ok: true }};
                    }}
                    return {{ error: `row label ${{targetRow}} not found` }};
                }})()"#,
                target_row = row,
            );
            let lookup_deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let response = send_command_to_server(
                    port,
                    WsCommand::EvalJs {
                        expression: js.clone(),
                    },
                )
                .await?;
                match response {
                    WsResponse::Success { data } => {
                        let Some(value) = data else {
                            anyhow::bail!("Assert cells row visible failed: JS returned no data");
                        };
                        if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                            if (error.contains("row label")
                                || error.contains("preview root not found"))
                                && Instant::now() < lookup_deadline
                            {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                continue;
                            }
                            anyhow::bail!("Assert cells row visible failed: {}", error);
                        }
                        break;
                    }
                    WsResponse::Error { message } => {
                        anyhow::bail!("Assert cells row visible failed: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response for assert_cells_row_visible"),
                }
            }
        }
        ParsedAction::AssertPreviewDirectTextVisible { text } => {
            let js = format!(
                r#"(function() {{
                    const root = document.querySelector('[data-boon-panel="preview"]');
                    if (!root) return {{ error: 'preview root not found' }};
                    const target = {target:?};
                    const directText = (el) => Array.from(el.childNodes)
                        .filter((n) => n.nodeType === Node.TEXT_NODE)
                        .map((n) => n.textContent || '')
                        .join('')
                        .trim();
                    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                    let node = walker.currentNode;
                    while (node) {{
                        const rect = node.getBoundingClientRect();
                        if (
                            rect.width > 0 &&
                            rect.height > 0 &&
                            directText(node) === target
                        ) {{
                            return {{ ok: true }};
                        }}
                        node = walker.nextNode();
                    }}
                    return {{ error: `visible direct text '${{target}}' not found` }};
                }})()"#,
                target = text,
            );
            let response =
                send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
            match response {
                WsResponse::Success { data } => {
                    let Some(value) = data else {
                        anyhow::bail!(
                            "Assert preview direct text visible failed: JS returned no data"
                        );
                    };
                    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                        anyhow::bail!("Assert preview direct text visible failed: {}", error);
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert preview direct text visible failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for assert_preview_direct_text_visible"),
            }
        }
        ParsedAction::AssertFocused { input_index } => {
            let _ = wait_for_non_retry_preview_text(port, Duration::from_millis(1500)).await?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let response = send_command_to_server(port, WsCommand::GetFocusedElement).await?;
                match response {
                    WsResponse::FocusedElement {
                        tag_name,
                        input_index: actual_index,
                        ..
                    } => {
                        let mut last_tag_name = tag_name.clone();
                        let mut last_input_index = actual_index;
                        if actual_index.is_none()
                            || last_tag_name.is_none()
                            || input_index
                                .is_some_and(|expected_idx| actual_index != Some(expected_idx))
                        {
                            let (fallback_tag_name, fallback_input_index) =
                                focused_preview_input_via_eval(port).await?;
                            if fallback_tag_name.is_some() {
                                last_tag_name = fallback_tag_name;
                                last_input_index = fallback_input_index;
                            }
                        }
                        if last_tag_name.is_some()
                            && input_index
                                .is_none_or(|expected_idx| last_input_index == Some(expected_idx))
                        {
                            break;
                        }
                        if Instant::now() >= deadline {
                            let debug = focused_preview_debug_via_eval(port)
                                .await
                                .unwrap_or_else(|error| format!("debug failed: {error}"));
                            if last_tag_name.is_none() {
                                anyhow::bail!(
                                    "Assert focused failed: no element is focused\nFocus debug: {}",
                                    debug
                                );
                            }
                            if let Some(expected_idx) = input_index {
                                anyhow::bail!(
                                    "Assert focused failed: expected input index {}, got {:?}\nFocus debug: {}",
                                    expected_idx,
                                    last_input_index,
                                    debug
                                );
                            }
                            break;
                        }
                    }
                    WsResponse::Error { message } => {
                        anyhow::bail!("Assert focused failed: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response for GetFocusedElement"),
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        ParsedAction::AssertFocusedInputValue { expected } => {
            let _ = wait_for_non_retry_preview_text(port, Duration::from_millis(1500)).await?;
            let response = send_command_to_server(port, WsCommand::EvalJs {
                expression: r#"(function() {
                    const preview = document.querySelector('[data-boon-panel="preview"]');
                    if (!preview) return { error: 'preview root not found' };
                    const isTextInput = (element) =>
                        element
                        && element !== document.body
                        && (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA');
                    const remembered = window.__boonLastPreviewTextInput;
                    const previewFocused = preview.querySelector(':focus');
                    let focused = isTextInput(previewFocused)
                        ? previewFocused
                        : (isTextInput(document.activeElement) && preview.contains(document.activeElement)
                            ? document.activeElement
                            : preview.querySelector('[data-boon-focused="true"]')
                                || preview.querySelector('[focused="true"]')
                                || preview.querySelector('input[autofocus], textarea[autofocus]'));
                    if (!isTextInput(focused)) {
                        focused = isTextInput(remembered) && remembered.isConnected && preview.contains(remembered)
                            ? remembered
                            : null;
                    }
                    if (!focused || focused === document.body) {
                        return { error: 'no element is focused' };
                    }
                    if (focused.tagName !== 'INPUT' && focused.tagName !== 'TEXTAREA') {
                        return { error: `focused element is ${focused.tagName}, not input/textarea` };
                    }
                    return { value: focused.value ?? '' };
                })()"#.to_string(),
            }).await?;
            match response {
                WsResponse::Success { data } => {
                    let Some(value) = data else {
                        anyhow::bail!("Assert focused input value failed: JS returned no data");
                    };
                    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                        anyhow::bail!("Assert focused input value failed: {}", error);
                    }
                    let actual = value
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if actual != expected {
                        anyhow::bail!(
                            "Assert focused input value failed: expected '{}', got '{}'",
                            expected,
                            actual
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert focused input value failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for focused input value assertion"),
            }
        }
        ParsedAction::AssertInputPlaceholder { index, expected } => {
            let (found, placeholder, _) =
                preview_text_input_properties_via_eval(port, *index).await?;
            if !found {
                anyhow::bail!("Assert input placeholder failed: input {} not found", index);
            }
            let actual = placeholder.unwrap_or_default();
            if !actual.contains(expected) {
                anyhow::bail!(
                    "Assert input placeholder failed: expected '{}' in placeholder, got '{}'",
                    expected,
                    actual
                );
            }
        }
        ParsedAction::AssertUrl { pattern } => {
            let response = send_command_to_server(port, WsCommand::GetCurrentUrl).await?;
            match response {
                WsResponse::CurrentUrl { url } => {
                    if !url.contains(pattern) {
                        anyhow::bail!(
                            "Assert URL failed: expected '{}' in URL, got '{}'",
                            pattern,
                            url
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
            let response =
                send_command_to_server(port, WsCommand::VerifyInputTypeable { index: *index })
                    .await?;
            match response {
                WsResponse::InputTypeableStatus {
                    typeable,
                    disabled,
                    readonly,
                    hidden,
                    reason,
                } => {
                    if !typeable {
                        let reason_str = reason.unwrap_or_else(|| {
                            let mut reasons = vec![];
                            if disabled {
                                reasons.push("disabled");
                            }
                            if readonly {
                                reasons.push("readonly");
                            }
                            if hidden {
                                reasons.push("hidden");
                            }
                            reasons.join(", ")
                        });
                        anyhow::bail!("Input {} is NOT typeable: {}", index, reason_str);
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert input typeable failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for VerifyInputTypeable"),
            }
        }
        ParsedAction::AssertInputNotTypeable { index } => {
            let response =
                send_command_to_server(port, WsCommand::VerifyInputTypeable { index: *index })
                    .await?;
            match response {
                WsResponse::InputTypeableStatus {
                    typeable, reason, ..
                } => {
                    if typeable {
                        anyhow::bail!("Input {} is unexpectedly typeable", index);
                    }
                    if let Some(reason) = reason {
                        if reason.is_empty() {
                            return Ok(());
                        }
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert input not typeable failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for VerifyInputTypeable"),
            }
        }
        ParsedAction::AssertButtonDisabled { index } => {
            let (found, disabled, text) = preview_button_disabled_via_eval(port, *index).await?;
            if !found {
                anyhow::bail!("Assert button disabled failed: button {} not found", index);
            }
            if !disabled {
                anyhow::bail!(
                    "Assert button disabled failed: button {} ('{}') is enabled",
                    index,
                    text
                );
            }
        }
        ParsedAction::AssertButtonEnabled { index } => {
            let (found, disabled, text) = preview_button_disabled_via_eval(port, *index).await?;
            if !found {
                anyhow::bail!("Assert button enabled failed: button {} not found", index);
            }
            if disabled {
                anyhow::bail!(
                    "Assert button enabled failed: button {} ('{}') is disabled",
                    index,
                    text
                );
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
                            expected,
                            button_count
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
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let js = r#"
(() => {
  const preview = document.querySelector('[data-boon-panel="preview"]');
  if (!preview) return { error: 'Preview panel not found' };

  const roleCheckboxes = Array.from(preview.querySelectorAll('[role="checkbox"]'));
  const nativeCheckboxes = Array.from(preview.querySelectorAll('input[type="checkbox"]'));
  const idCheckboxes = Array.from(preview.querySelectorAll('[id^="cb-"]'));
  const seen = new Set();
  const allCheckboxes = [];

  roleCheckboxes.forEach((el) => {
    if (!seen.has(el)) {
      seen.add(el);
      allCheckboxes.push(el);
    }
  });

  nativeCheckboxes.forEach((el) => {
    if (!seen.has(el)) {
      seen.add(el);
      allCheckboxes.push(el);
    }
  });

  idCheckboxes.forEach((el) => {
    if (!seen.has(el)) {
      seen.add(el);
      allCheckboxes.push(el);
    }
  });

  return { count: allCheckboxes.length };
})()
"#
                .to_string();
                let response =
                    send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
                match response {
                    WsResponse::Success { data } => {
                        let checkbox_count =
                            data.as_ref()
                                .and_then(|value| value.get("count"))
                                .and_then(|value| value.as_u64())
                                .unwrap_or_default() as u32;
                        if checkbox_count == *expected {
                            break;
                        }
                        if Instant::now() >= deadline {
                            anyhow::bail!(
                                "Assert checkbox count failed: expected {} checkboxes, found {}",
                                expected,
                                checkbox_count
                            );
                        }
                    }
                    WsResponse::Error { message } => {
                        anyhow::bail!("Assert checkbox count failed: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response for GetPreviewElements"),
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        ParsedAction::AssertNotContains { text } => {
            // Get preview text and verify it does NOT contain the specified text
            let preview = get_preview(port).await?;
            if preview.contains(text) {
                anyhow::bail!(
                    "Assert not contains failed: preview should NOT contain '{}' but it does.\nPreview: {}",
                    text,
                    truncate_for_error(&preview, 200)
                );
            }
        }
        ParsedAction::AssertNotFocused { input_index } => {
            let _ = wait_for_non_retry_preview_text(port, Duration::from_millis(1500)).await?;
            let response = send_command_to_server(port, WsCommand::GetFocusedElement).await?;
            match response {
                WsResponse::FocusedElement {
                    tag_name,
                    input_index: actual_index,
                    ..
                } => {
                    let (fallback_tag_name, fallback_input_index) =
                        focused_preview_input_via_eval(port).await?;
                    let resolved_tag_name = fallback_tag_name.or(tag_name);
                    let resolved_input_index = fallback_input_index.or(actual_index);
                    let focused_text_input = matches!(
                        resolved_tag_name.as_deref(),
                        Some("INPUT") | Some("TEXTAREA")
                    );
                    if let Some(expected_index) = input_index {
                        if resolved_input_index == Some(*expected_index) {
                            anyhow::bail!(
                                "Assert not focused failed: expected input {} to NOT be focused, but it is",
                                expected_index
                            );
                        }
                    } else if focused_text_input || resolved_input_index.is_some() {
                        anyhow::bail!(
                            "Assert not focused failed: expected no focused input, got {:?}",
                            resolved_input_index
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
            let response =
                send_command_to_server(port, WsCommand::GetCheckboxState { index: *index }).await?;
            match response {
                WsResponse::CheckboxState { found, checked } => {
                    if !found {
                        anyhow::bail!(
                            "Assert checkbox unchecked failed: checkbox {} not found",
                            index
                        );
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
            let response =
                send_command_to_server(port, WsCommand::GetCheckboxState { index: *index }).await?;
            match response {
                WsResponse::CheckboxState { found, checked } => {
                    if !found {
                        anyhow::bail!(
                            "Assert checkbox checked failed: checkbox {} not found",
                            index
                        );
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
            let response = send_command_to_server(
                port,
                WsCommand::AssertButtonHasOutline { text: text.clone() },
            )
            .await?;
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
            let (found, _, value) = preview_text_input_properties_via_eval(port, *index).await?;
            if !found {
                anyhow::bail!("Assert input empty failed: input {} not found", index);
            }
            let actual_value = value.unwrap_or_default();
            if !actual_value.is_empty() {
                anyhow::bail!(
                    "Assert input empty failed: expected input {} to be empty, but got '{}'",
                    index,
                    actual_value
                );
            }
        }
        ParsedAction::AssertContains { text } => {
            // Verify that the preview contains the specified text
            let preview = get_preview(port).await?;
            if !preview.contains(text) {
                anyhow::bail!(
                    "Assert contains failed: preview should contain '{}' but it doesn't.\nPreview: {}",
                    text,
                    truncate_for_error(&preview, 200)
                );
            }
        }
        ParsedAction::AssertCheckboxClickable { index } => {
            // Verify that a checkbox is ACTUALLY clickable by real user (not obscured)
            let response =
                send_command_to_server(port, WsCommand::AssertCheckboxClickable { index: *index })
                    .await?;
            match response {
                WsResponse::Success { .. } => {}
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert checkbox clickable failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for AssertCheckboxClickable"),
            }
        }
        ParsedAction::AssertElementStyle {
            target,
            property,
            expected,
        } => {
            // Verify computed CSS style on an element found by text content
            let response = send_command_to_server(
                port,
                WsCommand::GetElementStyle {
                    text: target.clone(),
                    properties: vec![property.clone()],
                },
            )
            .await?;
            match response {
                WsResponse::ElementStyle {
                    found,
                    styles,
                    error,
                } => {
                    if !found {
                        anyhow::bail!(
                            "Assert element style failed: element with text '{}' not found. {}",
                            target,
                            error.unwrap_or_default()
                        );
                    }
                    let actual = styles
                        .as_ref()
                        .and_then(|s| s.get(property.as_str()))
                        .cloned()
                        .unwrap_or_default();
                    if !actual.contains(expected.as_str()) {
                        anyhow::bail!(
                            "Assert element style failed: for element '{}', CSS '{}' = '{}' does not contain '{}'",
                            target,
                            property,
                            actual,
                            expected
                        );
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Assert element style failed: {}", message);
                }
                _ => anyhow::bail!("Unexpected response for GetElementStyle"),
            }
        }
        ParsedAction::AssertInputValue { index, expected } => {
            let deadline = Instant::now() + Duration::from_secs(5);

            loop {
                let (found, _, value) =
                    preview_text_input_properties_via_eval(port, *index).await?;
                let last_error = if !found {
                    format!("Assert input value failed: input {} not found", index)
                } else {
                    let actual = value.unwrap_or_default();
                    if actual == *expected {
                        break;
                    }
                    format!(
                        "Assert input value failed: expected '{}' in input {}, got '{}'",
                        expected, index, actual
                    )
                };

                if Instant::now() >= deadline {
                    anyhow::bail!("{}", last_error);
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        ParsedAction::SetSliderValue { index, value } => {
            let before_snapshot = get_preview_stability_snapshot(port).await.ok();
            // Use EvalJs to set the slider value and dispatch input+change events,
            // plus the Boon hook path when available.
            let js = format!(
                r#"(function() {{
                    var sliders = document.querySelectorAll('[data-boon-panel="preview"] input[type="range"]');
                    if ({index} >= sliders.length) return 'ERROR: slider index {index} not found (have ' + sliders.length + ')';
                    var slider = sliders[{index}];
                    var nativeInputValueSetter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
                    nativeInputValueSetter.call(slider, '{value}');
                    slider.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    slider.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    var dispatchEvent = window.__boonDispatchUiEvent;
                    var inputPort = slider.getAttribute('data-boon-port-input');
                    if (typeof dispatchEvent === 'function' && inputPort) {{
                        dispatchEvent(inputPort, 'Input', slider.value || '');
                    }}
                    return {{
                        ok: true,
                        value: slider.value || '',
                        inputPort: inputPort,
                        nodeId: slider.getAttribute('data-boon-node-id')
                    }};
                }})()"#,
                index = index,
                value = value,
            );
            let response =
                send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
            match response {
                WsResponse::Success { data } => {
                    if let Some(serde_json::Value::String(ref d)) = data {
                        if d.starts_with("ERROR") {
                            anyhow::bail!("Set slider value failed: {}", d);
                        }
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Set slider value failed: {}", message);
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = wait_for_preview_change_then_settle(port, before_snapshot).await;
        }
        ParsedAction::SetInputValue { index, value } => {
            let js = format!(
                r#"(function() {{
                    var inputs = document.querySelectorAll('[data-boon-panel="preview"] input[type="text"], [data-boon-panel="preview"] input:not([type]), [data-boon-panel="preview"] textarea');
                    if ({index} >= inputs.length) return 'ERROR: input index {index} not found (have ' + inputs.length + ')';
                    var input = inputs[{index}];
                    var nativeSetter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set
                        || Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set;
                    if (!nativeSetter) return 'ERROR: native value setter not found';
                    nativeSetter.call(input, {value_json});
                    input.dispatchEvent(new InputEvent('input', {{
                        bubbles: true,
                        composed: true,
                        data: {value_json},
                        inputType: 'insertReplacementText'
                    }}));
                    input.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return 'OK';
                }})()"#,
                index = index,
                value_json = serde_json::to_string(value)?,
            );
            let response =
                send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
            match response {
                WsResponse::Success { data } => {
                    if let Some(serde_json::Value::String(ref d)) = data {
                        if d.starts_with("ERROR") {
                            anyhow::bail!("Set input value failed: {}", d);
                        }
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Set input value failed: {}", message);
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        ParsedAction::SetFocusedInputValue { value } => {
            set_focused_input_value(port, engine, value, verbose).await?
        }
        ParsedAction::SelectOption { index, value } => {
            // Use EvalJs to change select value and drive both DOM and Boon hooks.
            let js = format!(
                r#"(function() {{
                    var selects = document.querySelectorAll('[data-boon-panel="preview"] select');
                    if ({index} >= selects.length) return 'ERROR: select index {index} not found (have ' + selects.length + ')';
                    var sel = selects[{index}];
                    sel.value = '{value}';
                    sel.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    sel.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    var dispatchEvent = window.__boonDispatchUiEvent;
                    var inputPort = sel.getAttribute('data-boon-port-input');
                    if (typeof dispatchEvent === 'function' && inputPort) {{
                        dispatchEvent(inputPort, 'Input', sel.value || '');
                    }}
                    return 'OK';
                }})()"#,
                index = index,
                value = value,
            );
            let response =
                send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
            match response {
                WsResponse::Success { data } => {
                    if let Some(serde_json::Value::String(ref d)) = data {
                        if d.starts_with("ERROR") {
                            anyhow::bail!("Select option failed: {}", d);
                        }
                    }
                }
                WsResponse::Error { message } => {
                    anyhow::bail!("Select option failed: {}", message);
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
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

#[derive(Clone, Debug)]
struct PreviewElementInfo {
    direct_text: String,
    link_path: Option<String>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

struct ElementBounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl PreviewElementInfo {
    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn center_y(&self) -> f64 {
        self.y + (self.height / 2.0)
    }
}

#[derive(Clone, Debug)]
struct CellsLookup {
    cell: PreviewElementInfo,
    row_match_count: usize,
    viable_row_count: usize,
    first_row_cells: Vec<String>,
}

fn preview_element_infos(data: &serde_json::Value) -> Vec<PreviewElementInfo> {
    data.get("elements")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let obj = value.as_object()?;
            Some(PreviewElementInfo {
                direct_text: obj.get("directText")?.as_str()?.trim().to_string(),
                link_path: obj
                    .get("linkPath")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                x: obj.get("x")?.as_f64()?,
                y: obj.get("y")?.as_f64()?,
                width: obj.get("width")?.as_f64()?,
                height: obj.get("height")?.as_f64()?,
            })
        })
        .filter(|element| {
            !element.direct_text.is_empty() && element.width > 0.0 && element.height > 0.0
        })
        .collect()
}

fn find_preview_element_bounds_by_text(
    data: &serde_json::Value,
    text: &str,
    exact: bool,
) -> Option<ElementBounds> {
    fn match_priority(
        obj: &serde_json::Map<String, serde_json::Value>,
        text: &str,
        exact: bool,
    ) -> Option<u8> {
        let direct_text = obj.get("directText").and_then(|t| t.as_str());
        let full_text = obj.get("fullText").and_then(|t| t.as_str());
        let legacy_text = obj.get("text").and_then(|t| t.as_str());

        let matches = |candidate: &str| {
            if exact {
                candidate.trim() == text
            } else {
                candidate.contains(text)
            }
        };

        if direct_text.is_some_and(matches) {
            Some(0)
        } else if full_text.is_some_and(matches) {
            Some(1)
        } else if legacy_text.is_some_and(matches) {
            Some(2)
        } else {
            None
        }
    }

    fn candidate_bounds(obj: &serde_json::Map<String, serde_json::Value>) -> Option<ElementBounds> {
        let (Some(x), Some(y), Some(width), Some(height)) = (
            obj.get("x").and_then(|v| v.as_f64()),
            obj.get("y").and_then(|v| v.as_f64()),
            obj.get("width").and_then(|v| v.as_f64()),
            obj.get("height").and_then(|v| v.as_f64()),
        ) else {
            return None;
        };

        Some(ElementBounds {
            x: x as i32,
            y: y as i32,
            width: width as i32,
            height: height as i32,
        })
    }

    fn collect_recursive(
        value: &serde_json::Value,
        text: &str,
        exact: bool,
        candidates: &mut Vec<(u8, i64, ElementBounds)>,
    ) {
        match value {
            serde_json::Value::Object(obj) => {
                if let Some(priority) = match_priority(obj, text, exact) {
                    if let Some(bounds) = candidate_bounds(obj) {
                        let area = i64::from(bounds.width.max(0)) * i64::from(bounds.height.max(0));
                        candidates.push((priority, area, bounds));
                    }
                }

                if let Some(children) = obj.get("children") {
                    collect_recursive(children, text, exact, candidates);
                }

                for (key, val) in obj {
                    if key != "text" && key != "children" {
                        collect_recursive(val, text, exact, candidates);
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    collect_recursive(item, text, exact, candidates);
                }
            }
            _ => {}
        }
    }

    let mut candidates = Vec::new();
    collect_recursive(data, text, exact, &mut candidates);
    candidates
        .into_iter()
        .min_by(|(priority_a, area_a, _), (priority_b, area_b, _)| {
            priority_a.cmp(priority_b).then_with(|| area_a.cmp(area_b))
        })
        .map(|(_, _, bounds)| bounds)
}

fn same_element_bounds(a: &ElementBounds, b: &ElementBounds) -> bool {
    (a.x - b.x).abs() <= 1
        && (a.y - b.y).abs() <= 1
        && (a.width - b.width).abs() <= 1
        && (a.height - b.height).abs() <= 1
}

async fn wait_for_preview_element_bounds_by_text(
    port: u16,
    text: &str,
    exact: bool,
) -> Result<ElementBounds> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_match: Option<ElementBounds> = None;

    loop {
        let response = send_command_to_server(port, WsCommand::GetPreviewElements).await?;
        let bounds = match response {
            WsResponse::PreviewElements { data } => {
                find_preview_element_bounds_by_text(&data, text, exact)
            }
            WsResponse::Error { message } => anyhow::bail!("Click text failed: {}", message),
            _ => anyhow::bail!("Unexpected response for GetPreviewElements"),
        };

        if let Some(bounds) = bounds {
            if last_match
                .as_ref()
                .is_some_and(|previous| same_element_bounds(previous, &bounds))
            {
                return Ok(bounds);
            }
            last_match = Some(bounds);
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "Click text failed: no stable element found containing '{}'",
                text
            );
        }

        tokio::time::sleep(Duration::from_millis(75)).await;
    }
}

async fn click_preview_text_element(port: u16, text: &str, exact: bool) -> Result<bool> {
    let js = format!(
        r#"(function() {{
            const preview = document.querySelector('[data-boon-panel="preview"]');
            if (!preview) return {{ found: false, error: 'preview root not found' }};

            const wanted = {text_json};
            const exact = {exact_json};
            const matches = (candidate) => exact ? candidate.trim() === wanted : candidate.includes(wanted);
            const directText = (el) => {{
                let text = '';
                for (const node of el.childNodes) {{
                    if (node.nodeType === Node.TEXT_NODE) {{
                        text += node.textContent || '';
                    }}
                }}
                return text.trim();
            }};
            const tagScore = (tag) => {{
                switch (tag) {{
                    case 'BUTTON': return 0;
                    case 'A': return 1;
                    case 'LABEL': return 2;
                    case 'SPAN': return 3;
                    case 'DIV': return 4;
                    case 'P': return 5;
                    default: return 6;
                }}
            }};

            const candidates = Array.from(preview.querySelectorAll('*'))
                .map((el) => {{
                    const text = (el.innerText || el.textContent || '').trim();
                    const direct = directText(el);
                    const rect = el.getBoundingClientRect();
                    if (!text || rect.width === 0 || rect.height === 0) return null;
                    const style = window.getComputedStyle(el);
                    if (style.display === 'none' || style.visibility === 'hidden') return null;
                    const directMatch = direct && matches(direct);
                    const textMatch = matches(text);
                    if (!directMatch && !textMatch) return null;
                    return {{
                        el,
                        text,
                        direct,
                        area: rect.width * rect.height,
                        directMatch,
                        tagScore: tagScore(el.tagName),
                    }};
                }})
                .filter(Boolean)
                .sort((a, b) =>
                    Number(b.directMatch) - Number(a.directMatch) ||
                    a.tagScore - b.tagScore ||
                    a.area - b.area
                );

            const best = candidates[0];
            if (!best) return {{ found: false }};

            if (typeof best.el.focus === 'function') {{
                best.el.focus();
            }}
            if (typeof best.el.click === 'function') {{
                best.el.click();
            }} else {{
                best.el.dispatchEvent(new MouseEvent('click', {{
                    bubbles: true,
                    cancelable: true,
                    composed: true,
                }}));
            }}

            return {{
                found: true,
                tag: best.el.tagName,
                text: best.text,
                direct: best.direct,
            }};
        }})()"#,
        text_json = serde_json::to_string(text)?,
        exact_json = if exact { "true" } else { "false" },
    );

    let response = send_command_to_server(port, WsCommand::EvalJs { expression: js }).await?;
    match response {
        WsResponse::Success { data } => {
            let Some(serde_json::Value::Object(obj)) = data else {
                anyhow::bail!("Click text fallback failed: preview lookup returned no data");
            };
            if let Some(error) = obj.get("error").and_then(|value| value.as_str()) {
                anyhow::bail!("Click text fallback failed: {}", error);
            }
            Ok(obj.get("found").and_then(|value| value.as_bool()) == Some(true))
        }
        WsResponse::Error { message } => {
            Err(anyhow::anyhow!("Click text fallback failed: {}", message))
        }
        _ => Err(anyhow::anyhow!("Unexpected response for EvalJs")),
    }
}

fn locate_cells_cell(
    data: &serde_json::Value,
    target_row: u32,
    target_column: u32,
) -> Result<CellsLookup, String> {
    let elements = preview_element_infos(data);
    let target_path = format!(
        "all_row_cells.{:04}.cells.{:04}.display_element",
        target_row.saturating_sub(1),
        target_column.saturating_sub(1)
    );
    if let Some(cell) = elements
        .iter()
        .find(|element| element.link_path.as_deref() == Some(target_path.as_str()))
        .cloned()
    {
        return Ok(CellsLookup {
            cell,
            row_match_count: 1,
            viable_row_count: 1,
            first_row_cells: vec![target_path],
        });
    }

    let mut row_matches: Vec<_> = elements
        .iter()
        .filter(|element| {
            element.direct_text == target_row.to_string()
                && element.width <= 60.0
                && element.x < 1200.0
        })
        .cloned()
        .collect();
    row_matches.sort_by(|a, b| a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x)));

    if row_matches.is_empty() {
        return Err(format!("row label {} not found", target_row));
    }

    let mut viable_rows = Vec::new();
    for row_label in &row_matches {
        let mut cell_matches: Vec<_> = elements
            .iter()
            .filter(|element| {
                element.x >= row_label.right() - 0.5
                    && (element.center_y() - row_label.center_y()).abs() <= 3.0
            })
            .cloned()
            .collect();
        cell_matches.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
        let mut deduped = Vec::new();
        for candidate in cell_matches {
            let duplicate = deduped.last().is_some_and(|prev: &PreviewElementInfo| {
                (prev.x - candidate.x).abs() <= 1.0
                    && (prev.y - candidate.y).abs() <= 1.0
                    && prev.direct_text == candidate.direct_text
            });
            if !duplicate {
                deduped.push(candidate);
            }
        }
        if deduped.len() >= target_column as usize {
            viable_rows.push((row_label.clone(), deduped));
        }
    }

    viable_rows.sort_by(|(row_a, _), (row_b, _)| {
        row_a
            .y
            .total_cmp(&row_b.y)
            .then_with(|| row_a.x.total_cmp(&row_b.x))
    });

    let Some((_, cells)) = viable_rows.first() else {
        return Err(format!("row label {} not found", target_row));
    };
    let Some(cell) = cells.get(target_column as usize - 1) else {
        return Err(format!(
            "cell ({}, {}) not found",
            target_row, target_column
        ));
    };

    Ok(CellsLookup {
        cell: cell.clone(),
        row_match_count: row_matches.len(),
        viable_row_count: viable_rows.len(),
        first_row_cells: cells.iter().map(|cell| cell.direct_text.clone()).collect(),
    })
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
            "p" => match get_preview(port).await {
                Ok(text) => println!("    Preview:\n{}", text),
                Err(e) => println!("    Failed: {}", e),
            },
            "c" => match get_console(port).await {
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
            },
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
            let data = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &base64)?;
            std::fs::write(path, data)?;
            Ok(())
        }
        WsResponse::Error { message } => anyhow::bail!("{}", message),
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn get_preview(port: u16) -> Result<String> {
    Ok(
        wait_for_non_retry_preview_text(port, Duration::from_millis(1500))
            .await?
            .unwrap_or_default(),
    )
}

async fn get_console(port: u16) -> Result<Vec<String>> {
    let response = send_command_to_server(port, WsCommand::GetConsole).await?;
    match response {
        WsResponse::Console { messages } => Ok(messages
            .into_iter()
            .map(|m| format!("[{}] {}", m.level, m.text))
            .collect()),
        WsResponse::Error { message } => anyhow::bail!("{}", message),
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Server connection status
struct ServerStatus {
    connected: bool,
    api_ready: bool,
    page_url: Option<String>,
}

/// Check if WebSocket server is running and browser extension is connected
async fn check_server_connection(port: u16) -> Result<ServerStatus> {
    let response = send_command_to_server(port, WsCommand::GetStatus).await?;
    match response {
        WsResponse::Status {
            connected,
            api_ready,
            page_url,
            ..
        } => Ok(ServerStatus {
            connected,
            api_ready,
            page_url,
        }),
        WsResponse::Error { message } => {
            anyhow::bail!("Status check failed: {}", message)
        }
        _ => {
            anyhow::bail!("Unexpected response from status check")
        }
    }
}

/// Built-in playground examples to smoke-test
const BUILTIN_EXAMPLES: &[&str] = &[
    "counter",
    "interval",
    "todo_mvc",
    "shopping_list",
    "hello_world",
    "fibonacci",
    "latest",
    "then",
    "when",
    "while",
    "counter_hold",
    "interval_hold",
    "complex_counter",
    "text_interpolation_update",
    "list_map_block",
    "list_retain_count",
    "list_retain_reactive",
    "list_retain_remove",
    "list_object_state",
    "list_map_external_dep",
    "minimal",
];

/// Smoke-run built-in examples: select, run, wait, check for panics
pub async fn run_builtin_smoke(opts: SmokeOptions) -> Result<Vec<TestResult>> {
    let setup = ensure_browser_connection(opts.port, opts.playground_port, None).await?;
    let result = run_smoke_inner(&opts).await;
    if setup.started_mzoon {
        kill_mzoon_server(opts.playground_port);
    }
    result
}

async fn run_smoke_inner(opts: &SmokeOptions) -> Result<Vec<TestResult>> {
    let mut examples: Vec<&str> = BUILTIN_EXAMPLES.to_vec();

    if let Some(ref filter) = opts.filter {
        let prefer_exact = examples.contains(&filter.as_str());
        examples.retain(|name| example_name_matches_filter(name, filter, prefer_exact));
        if examples.is_empty() {
            println!("No built-in examples match filter '{}'", filter);
            return Ok(vec![]);
        }
    }

    println!("Boon Smoke Tests");
    println!("================\n");
    println!("Running {} example(s)...\n", examples.len());

    let mut results = Vec::new();

    for name in &examples {
        let start = Instant::now();

        // Select example
        let select_response = send_command_to_server(
            opts.port,
            WsCommand::SelectExample {
                name: format!("{}.bn", name),
            },
        )
        .await?;

        if let WsResponse::Error { message } = select_response {
            results.push(TestResult {
                name: name.to_string(),
                passed: false,
                skipped: None,
                duration: start.elapsed(),
                error: Some(format!("SelectExample failed: {}", message)),
                actual_output: None,
                expected_output: None,
                steps: vec![],
            });
            println!(
                "  [FAIL] {} ({:.0?}) - select failed",
                name,
                start.elapsed()
            );
            continue;
        }

        // Wait for rendering
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // Check console for panics/errors
        let console_response = send_command_to_server(opts.port, WsCommand::GetConsole).await?;
        let has_panic = match &console_response {
            WsResponse::Console { messages } => messages.iter().any(|m| {
                m.level == "error"
                    && (m.text.contains("panicked") || m.text.contains("unreachable"))
            }),
            _ => false,
        };

        let passed = !has_panic;
        let error = if has_panic {
            Some("Runtime panic detected in console".to_string())
        } else {
            None
        };

        let status = if passed { "[PASS]" } else { "[FAIL]" };
        println!("  {} {} ({:.0?})", status, name, start.elapsed());

        results.push(TestResult {
            name: name.to_string(),
            passed,
            skipped: None,
            duration: start.elapsed(),
            error,
            actual_output: None,
            expected_output: None,
            steps: vec![],
        });
    }

    println!("\n================");
    let passed = results.iter().filter(|r| r.passed).count();
    println!("{}/{} passed", passed, results.len());

    Ok(results)
}

fn example_name_matches_filter(name: &str, filter: &str, prefer_exact: bool) -> bool {
    if prefer_exact {
        name == filter
    } else {
        name.contains(filter)
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

    anyhow::bail!("Could not find examples directory. Run from project root or use --examples-dir")
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_immediate_initial_preview, should_reselect_example_after_refresh,
        timer_category_requires_immediate_initial_match,
    };
    use crate::commands::expected::{MatchMode, OutputSpec};

    #[test]
    fn actors_harness_skips_reselect_when_target_file_is_already_loaded() {
        assert!(!should_reselect_example_after_refresh(Some("Actors"), true));
        assert!(!should_reselect_example_after_refresh(Some("DD"), true));
        assert!(!should_reselect_example_after_refresh(None, true));
    }

    #[test]
    fn harness_reselects_when_target_file_is_not_loaded_or_engine_needs_injection() {
        assert!(should_reselect_example_after_refresh(Some("Actors"), false));
        assert!(should_reselect_example_after_refresh(
            Some("ActorsLite"),
            true
        ));
        assert!(should_reselect_example_after_refresh(
            Some("ActorsLite"),
            false
        ));
    }

    #[test]
    fn immediate_capture_treats_placeholder_as_empty_preview() {
        assert_eq!(
            normalize_immediate_initial_preview(None),
            Some(String::new())
        );
        assert_eq!(
            normalize_immediate_initial_preview(Some("Run to see preview".to_string())),
            Some(String::new())
        );
        assert_eq!(
            normalize_immediate_initial_preview(Some("1".to_string())),
            Some("1".to_string())
        );
    }

    #[test]
    fn timer_category_initial_wait_is_reserved_for_exact_empty_output() {
        let exact_empty = OutputSpec {
            r#match: MatchMode::Exact,
            text: Some(String::new()),
            pattern: None,
        };
        assert!(timer_category_requires_immediate_initial_match(
            &exact_empty
        ));

        let exact_non_empty = OutputSpec {
            r#match: MatchMode::Exact,
            text: Some("Timer".to_string()),
            pattern: None,
        };
        assert!(!timer_category_requires_immediate_initial_match(
            &exact_non_empty
        ));

        let contains_output = OutputSpec {
            r#match: MatchMode::Contains,
            text: Some("Timer".to_string()),
            pattern: None,
        };
        assert!(!timer_category_requires_immediate_initial_match(
            &contains_output
        ));
    }
}
