use anyhow::{Context, Result, bail};
use boon_engine_factory_fabric::{
    FactoryFabricMetricsComparison, SUPPORTED_PLAYGROUND_EXAMPLES, factory_fabric_metrics_snapshot,
};
use serde::Serialize;
use serde_json::Value;

use crate::commands::test_examples::{TestOptions, TestResult, run_tests};
use crate::port_config::detect_ports;
use crate::ws_server::{Command as WsCommand, Response as WsResponse, send_command_to_server};

#[derive(Debug, Clone, Serialize)]
pub struct FactoryFabricExampleVerification {
    pub name: String,
    pub passed: bool,
    pub duration_ms: u128,
    pub skipped: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactoryFabricVerificationReport {
    pub engine_exposed: bool,
    pub engine_status_ok: bool,
    pub unsupported_error_smoke_ok: bool,
    pub metrics_gate_ok: bool,
    pub examples: Vec<FactoryFabricExampleVerification>,
}

impl FactoryFabricVerificationReport {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.engine_exposed
            && self.engine_status_ok
            && self.unsupported_error_smoke_ok
            && self.metrics_gate_ok
            && self.examples.iter().all(|example| example.passed)
    }
}

pub async fn run_verify_factory_fabric(json: bool, check: bool) -> Result<()> {
    let ports = detect_ports();
    let report = factory_fabric_verification_report(ports.ws_port, ports.playground_port).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("FactoryFabric Verification");
        println!(
            "  Engine exposed: {}",
            if report.engine_exposed { "PASS" } else { "FAIL" }
        );
        println!(
            "  Engine status contract: {}",
            if report.engine_status_ok { "PASS" } else { "FAIL" }
        );
        println!(
            "  Unsupported example smoke: {}",
            if report.unsupported_error_smoke_ok {
                "PASS"
            } else {
                "FAIL"
            }
        );
        println!(
            "  Metrics gate: {}",
            if report.metrics_gate_ok { "PASS" } else { "FAIL" }
        );
        for example in &report.examples {
            let status = if example.passed { "[PASS]" } else { "[FAIL]" };
            match &example.error {
                Some(error) => {
                    println!("  {status} {} ({} ms): {}", example.name, example.duration_ms, error)
                }
                None => println!("  {status} {} ({} ms)", example.name, example.duration_ms),
            }
        }
    }

    if check && !report.all_pass() {
        bail!(
            "FactoryFabric verification failed:\n{}",
            serde_json::to_string_pretty(&report)?
        );
    }

    Ok(())
}

pub async fn factory_fabric_verification_report(
    port: u16,
    playground_port: u16,
) -> Result<FactoryFabricVerificationReport> {
    send_command_to_server(
        port,
        WsCommand::SetEngine {
            engine: "FactoryFabric".to_string(),
        },
    )
    .await
    .context("failed to select FactoryFabric engine for verification")?;
    send_command_to_server(
        port,
        WsCommand::SelectExample {
            name: "counter.bn".to_string(),
        },
    )
    .await
    .context("failed to select counter example for FactoryFabric verification")?;
    send_command_to_server(port, WsCommand::TriggerRun)
        .await
        .context("failed to trigger counter run for FactoryFabric verification")?;
    let _ = wait_for_preview_text(port)
        .await
        .context("failed to confirm counter preview before FactoryFabric verification")?;

    let engine_info = eval_page_api(port, "window.boonPlayground.getEngine()")
        .await
        .context("failed to fetch page engine info during FactoryFabric verification")?;
    let engine_status = eval_page_api(port, "window.boonPlayground.getEngineStatus()")
        .await
        .context("failed to fetch page engine status during FactoryFabric verification")?;

    let engine_exposed = engine_info
        .get("availableEngines")
        .and_then(Value::as_array)
        .is_some_and(|engines| {
            engines
                .iter()
                .filter_map(Value::as_str)
                .any(|engine| engine == "FactoryFabric")
        })
        && engine_info
            .get("displayAvailableEngines")
            .and_then(Value::as_array)
            .is_some_and(|engines| {
                engines
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|engine| engine == "FactoryFabric")
            });
    let engine_status_ok = engine_status
        .get("engine")
        .and_then(Value::as_str)
        .is_some_and(|engine| engine == "FactoryFabric")
        && engine_status
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && engine_status
            .get("quiescent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let unsupported_error_smoke_ok =
        verify_unsupported_example_smoke(port).await.context(
            "failed to verify FactoryFabric unsupported-example explicit error contract",
        )?;
    let metrics_report = factory_fabric_metrics_snapshot()
        .map_err(anyhow::Error::msg)
        .context("failed to compute FactoryFabric metrics during verification")?;
    let metrics_gate_ok = FactoryFabricMetricsComparison::from_report(&metrics_report).all_pass();

    let mut examples = Vec::new();
    for filter in SUPPORTED_PLAYGROUND_EXAMPLES {
        let results = run_tests(TestOptions {
            port,
            playground_port,
            filter: Some((*filter).to_string()),
            interactive: false,
            screenshot_on_fail: true,
            verbose: false,
            examples_dir: None,
            no_launch: false,
            engine: Some("FactoryFabric".to_string()),
            skip_persistence: true,
        })
        .await
        .with_context(|| format!("failed to run FactoryFabric example filter '{filter}'"))?;
        if results.len() != 1 || results[0].name != *filter {
            let discovered = results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "FactoryFabric verification filter '{}' resolved to unexpected example set: [{}]",
                filter,
                discovered
            );
        }
        examples.extend(results.into_iter().map(into_example_verification));
    }

    Ok(FactoryFabricVerificationReport {
        engine_exposed,
        engine_status_ok,
        unsupported_error_smoke_ok,
        metrics_gate_ok,
        examples,
    })
}

async fn eval_page_api(port: u16, expression: &str) -> Result<Value> {
    match send_command_to_server(
        port,
        WsCommand::EvalJs {
            expression: expression.to_string(),
        },
    )
    .await?
    {
        WsResponse::Success { data: Some(value) } => Ok(value),
        WsResponse::Success { data: None } => Ok(Value::Null),
        WsResponse::Error { message } => bail!("page eval failed: {message}"),
        other => bail!("unexpected EvalJs response: {other:?}"),
    }
}

async fn verify_unsupported_example_smoke(port: u16) -> Result<bool> {
    send_command_to_server(
        port,
        WsCommand::SelectExample {
            name: "counter.bn".to_string(),
        },
    )
    .await
    .context("failed to select counter example for unsupported smoke")?;
    send_command_to_server(
        port,
        WsCommand::InjectCode {
            code: "broken: 1".to_string(),
            filename: Some("counter.bn".to_string()),
        },
    )
    .await
    .context("failed to inject unsupported custom code for FactoryFabric smoke")?;
    send_command_to_server(port, WsCommand::TriggerRun)
        .await
        .context("failed to trigger unsupported custom run")?;

    let preview = wait_for_preview_text(port)
        .await
        .context("failed to read preview text for unsupported FactoryFabric smoke")?;
    let status = eval_page_api(port, "window.boonPlayground.getEngineStatus()")
        .await
        .context("failed to fetch engine status after unsupported custom run")?;

    send_command_to_server(
        port,
        WsCommand::SelectExample {
            name: "counter.bn".to_string(),
        },
    )
    .await
    .context("failed to restore counter example after unsupported smoke")?;
    send_command_to_server(port, WsCommand::TriggerRun)
        .await
        .context("failed to rerun counter after unsupported smoke")?;

    Ok(preview.contains("FactoryFabric")
        && status
            .get("engine")
            .and_then(Value::as_str)
            .is_some_and(|engine| engine == "FactoryFabric")
        && !status
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(true))
}

async fn wait_for_preview_text(port: u16) -> Result<String> {
    for _ in 0..30 {
        let value = eval_page_api(
            port,
            r#"(function() {
                const preview = document.querySelector('[data-boon-panel="preview"]');
                return preview ? (preview.textContent || '') : '';
            })()"#,
        )
        .await?;
        if let Some(text) = value.as_str() {
            let trimmed = text.trim();
            if !trimmed.is_empty() && trimmed != "Run to see preview" {
                return Ok(text.to_string());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    bail!("preview text did not stabilize before timeout")
}

fn into_example_verification(result: TestResult) -> FactoryFabricExampleVerification {
    FactoryFabricExampleVerification {
        name: result.name,
        passed: result.passed,
        duration_ms: result.duration.as_millis(),
        skipped: result.skipped,
        error: result.error,
    }
}
