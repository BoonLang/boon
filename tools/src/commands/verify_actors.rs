use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::commands::test_examples::{run_tests, TestOptions, TestResult};
use crate::port_config::detect_ports;
use crate::ws_server::{send_command_to_server, Command as WsCommand, Response as WsResponse};

#[derive(Debug, Clone, Serialize)]
pub struct ActorsExampleVerification {
    pub name: String,
    pub passed: bool,
    pub duration_ms: u128,
    pub skipped: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorsVerificationReport {
    pub engine_exposed: bool,
    pub engine_status_ok: bool,
    pub examples: Vec<ActorsExampleVerification>,
}

impl ActorsVerificationReport {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.engine_exposed
            && self.engine_status_ok
            && self.examples.iter().all(|example| example.passed)
    }
}

pub async fn run_verify_actors(json: bool, check: bool) -> Result<()> {
    let ports = detect_ports();
    let report = actors_verification_report(ports.ws_port, ports.playground_port).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Actors Verification");
        println!(
            "  Engine exposed: {}",
            if report.engine_exposed {
                "PASS"
            } else {
                "FAIL"
            }
        );
        println!(
            "  Engine status contract: {}",
            if report.engine_status_ok {
                "PASS"
            } else {
                "FAIL"
            }
        );
        for example in &report.examples {
            let status = if example.passed { "[PASS]" } else { "[FAIL]" };
            match &example.error {
                Some(error) => {
                    println!(
                        "  {status} {} ({} ms): {}",
                        example.name, example.duration_ms, error
                    )
                }
                None => println!("  {status} {} ({} ms)", example.name, example.duration_ms),
            }
        }
    }

    if check && !report.all_pass() {
        bail!(
            "Actors verification failed:\n{}",
            serde_json::to_string_pretty(&report)?
        );
    }

    Ok(())
}

pub async fn actors_verification_report(
    port: u16,
    playground_port: u16,
) -> Result<ActorsVerificationReport> {
    send_command_to_server(
        port,
        WsCommand::SetEngine {
            engine: "Actors".to_string(),
        },
    )
    .await
    .context("failed to select Actors engine for verification")?;
    send_command_to_server(
        port,
        WsCommand::SelectExample {
            name: "counter.bn".to_string(),
        },
    )
    .await
    .context("failed to select counter example for Actors verification")?;
    send_command_to_server(port, WsCommand::TriggerRun)
        .await
        .context("failed to trigger counter run for Actors verification")?;
    let _ = wait_for_preview_text(port)
        .await
        .context("failed to confirm counter preview before Actors verification")?;

    let engine_info = eval_page_api(port, "window.boonPlayground.getEngine()")
        .await
        .context("failed to fetch page engine info during Actors verification")?;
    let engine_status = eval_page_api(port, "window.boonPlayground.getEngineStatus()")
        .await
        .context("failed to fetch page engine status during Actors verification")?;

    let engine_exposed = engine_info
        .get("availableEngines")
        .and_then(Value::as_array)
        .is_some_and(|engines| {
            engines
                .iter()
                .filter_map(Value::as_str)
                .any(|engine| engine == "Actors")
        })
        && engine_info
            .get("displayAvailableEngines")
            .and_then(Value::as_array)
            .is_some_and(|engines| {
                engines
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|engine| engine == "Actors")
            });
    let engine_status_ok = engine_status
        .get("engine")
        .and_then(Value::as_str)
        .is_some_and(|engine| engine == "Actors")
        && engine_status
            .get("supported")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        && engine_status
            .get("quiescent")
            .and_then(Value::as_bool)
            .unwrap_or(false);

    let mut examples = Vec::new();
    for filter in verification_example_filters() {
        let results = run_tests(TestOptions {
            port,
            playground_port,
            filter: Some((*filter).to_string()),
            interactive: false,
            screenshot_on_fail: true,
            verbose: false,
            examples_dir: None,
            no_launch: false,
            engine: Some("Actors".to_string()),
            skip_persistence: false,
        })
        .await
        .with_context(|| format!("failed to run Actors example filter '{filter}'"))?;
        if results.is_empty() {
            bail!(
                "Actors verification filter '{}' produced no matching browser test example",
                filter
            );
        }
        if results.len() != 1 || results[0].name != *filter {
            let discovered = results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Actors verification filter '{}' resolved to unexpected example set: [{}]",
                filter,
                discovered
            );
        }
        examples.extend(results.into_iter().map(into_example_verification));
    }

    Ok(ActorsVerificationReport {
        engine_exposed,
        engine_status_ok,
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

fn into_example_verification(result: TestResult) -> ActorsExampleVerification {
    ActorsExampleVerification {
        name: result.name,
        passed: result.passed,
        duration_ms: result.duration.as_millis(),
        skipped: result.skipped,
        error: result.error,
    }
}

fn verification_example_filters() -> &'static [&'static str] {
    &[
        "interval",
        "interval_hold",
        "timer",
        "then",
        "when",
        "while",
        "todo_mvc",
        "cells",
        "cells_dynamic",
    ]
}

#[cfg(test)]
mod tests {
    use super::verification_example_filters;

    #[test]
    fn verification_filters_cover_async_and_acceptance_examples() {
        assert_eq!(
            verification_example_filters(),
            &[
                "interval",
                "interval_hold",
                "timer",
                "then",
                "when",
                "while",
                "todo_mvc",
                "cells",
                "cells_dynamic",
            ]
        );
    }
}
