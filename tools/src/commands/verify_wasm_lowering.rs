use anyhow::{bail, Context, Result};
use boon_engine_wasm::{WasmLoweringReport, official_7guis_wasm_lowering_report};

pub fn run_verify_wasm_lowering(json: bool, check: bool) -> Result<()> {
    let report: WasmLoweringReport = official_7guis_wasm_lowering_report();

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Official 7GUIs Wasm Lowering");
        for example in &report.examples {
            if example.passed {
                println!("  [PASS] {}", example.example_name);
            } else {
                let error = example
                    .error
                    .as_deref()
                    .context("failed example should include an error")?;
                println!("  [FAIL] {}: {}", example.example_name, error);
            }
        }
    }

    if check && !report.all_pass() {
        bail!(
            "official 7GUIs Wasm lowering failed:\n{}",
            serde_json::to_string_pretty(&report)?
        );
    }

    Ok(())
}
