use anyhow::{Context, Result, bail};
use boon_engine_actors_lite::{
    ActorsLiteMetricsComparison, ActorsLiteMetricsReport, ActorsLitePhase4AcceptanceRecord,
    MILESTONE_PLAYGROUND_EXAMPLES, actors_lite_phase4_acceptance_is_green,
    actors_lite_phase4_acceptance_record, actors_lite_public_exposure_enabled,
};
use serde::Serialize;

use crate::commands::backend_metrics::{
    ActorsLitePinnedEnvironmentComparison, ActorsLitePinnedEnvironmentReport,
    detect_actors_lite_pinned_environment, run_actors_lite_metrics_capture,
};
use crate::commands::test_examples::{TestOptions, TestResult, run_tests};
use crate::port_config::detect_ports;

#[derive(Debug, Clone, Serialize)]
pub struct ActorsLiteExampleVerification {
    pub name: String,
    pub passed: bool,
    pub duration_ms: u128,
    pub skipped: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActorsLiteVerificationReport {
    pub public_exposure_enabled: bool,
    pub phase4_acceptance_green: bool,
    pub phase4_acceptance: ActorsLitePhase4AcceptanceRecord,
    pub examples: Vec<ActorsLiteExampleVerification>,
    pub metrics: ActorsLiteMetricsReport,
    pub metrics_comparison: ActorsLiteMetricsComparison,
    pub pinned_environment_required: bool,
    pub pinned_environment: ActorsLitePinnedEnvironmentReport,
    pub pinned_environment_comparison: ActorsLitePinnedEnvironmentComparison,
}

impl ActorsLiteVerificationReport {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        self.public_exposure_enabled
            && self.phase4_acceptance_green
            && self.examples.iter().all(|example| example.passed)
            && self.metrics_comparison.all_pass()
            && (!self.pinned_environment_required || self.pinned_environment_comparison.all_pass())
    }
}

pub async fn run_verify_actors_lite(
    json: bool,
    check: bool,
    pinned_env: bool,
    warmed_session: bool,
    single_visible_tab: bool,
    no_devtools: bool,
) -> Result<()> {
    let ports = detect_ports();
    let report = actors_lite_verification_report(
        ports.ws_port,
        ports.playground_port,
        pinned_env,
        warmed_session,
        single_visible_tab,
        no_devtools,
    )
    .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("ActorsLite Verification");
        println!(
            "  Public exposure gate: {}",
            if report.public_exposure_enabled {
                "PASS"
            } else {
                "FAIL"
            }
        );
        println!(
            "  Phase 4 acceptance record: {}",
            if report.phase4_acceptance_green {
                "PASS"
            } else {
                "FAIL"
            }
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
        println!(
            "  Metrics gate: {}",
            if report.metrics_comparison.all_pass() {
                "PASS"
            } else {
                "FAIL"
            }
        );
        if report.pinned_environment_required {
            println!(
                "  Pinned environment gate: {}",
                if report.pinned_environment_comparison.all_pass() {
                    "PASS"
                } else {
                    "FAIL"
                }
            );
        }
    }

    if check && !report.all_pass() {
        bail!(
            "ActorsLite verification failed:\n{}",
            serde_json::to_string_pretty(&report)?
        );
    }

    Ok(())
}

pub async fn actors_lite_verification_report(
    port: u16,
    playground_port: u16,
    pinned_environment_required: bool,
    warmed_session: bool,
    single_visible_tab: bool,
    no_devtools: bool,
) -> Result<ActorsLiteVerificationReport> {
    let phase4_acceptance = actors_lite_phase4_acceptance_record()
        .map_err(anyhow::Error::msg)
        .context("failed to load ActorsLite phase 4 acceptance record")?
        .clone();
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
            engine: Some("ActorsLite".to_string()),
            skip_persistence: true,
        })
        .await
        .with_context(|| format!("failed to run ActorsLite example filter '{filter}'"))?;
        if results.is_empty() {
            bail!(
                "ActorsLite verification filter '{}' produced no matching browser test example",
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
                "ActorsLite verification filter '{}' resolved to unexpected example set: [{}]",
                filter,
                discovered
            );
        }
        examples.extend(results.into_iter().map(into_example_verification));
    }

    let (metrics, metrics_comparison) = run_actors_lite_metrics_capture()
        .context("failed to collect ActorsLite metrics during verification")?;
    let (pinned_environment, pinned_environment_comparison) = detect_actors_lite_pinned_environment(
        warmed_session,
        single_visible_tab,
        no_devtools,
    );

    Ok(ActorsLiteVerificationReport {
        public_exposure_enabled: actors_lite_public_exposure_enabled(),
        phase4_acceptance_green: actors_lite_phase4_acceptance_is_green(),
        phase4_acceptance,
        examples,
        metrics,
        metrics_comparison,
        pinned_environment_required,
        pinned_environment,
        pinned_environment_comparison,
    })
}

fn into_example_verification(result: TestResult) -> ActorsLiteExampleVerification {
    ActorsLiteExampleVerification {
        name: result.name,
        passed: result.passed,
        duration_ms: result.duration.as_millis(),
        skipped: result.skipped,
        error: result.error,
    }
}

fn verification_example_filters() -> &'static [&'static str] {
    MILESTONE_PLAYGROUND_EXAMPLES
}

#[cfg(test)]
mod tests {
    use super::verification_example_filters;

    #[test]
    fn verification_filters_match_phase_milestone_subset() {
        assert_eq!(
            verification_example_filters(),
            &["counter", "todo_mvc", "cells", "cells_dynamic"]
        );
    }
}
