pub mod backend_metrics;
pub mod browser;
pub mod expected;
pub mod pixel_diff;
pub mod test_examples;
pub mod verify_integrity;

pub fn is_valid_engine_name(engine: &str) -> bool {
    matches!(engine, "Actors" | "DD" | "Wasm")
}

pub fn resolve_requested_engine(requested: &str, available_engines: &[String]) -> String {
    if available_engines.iter().any(|engine| engine == requested) {
        return requested.to_string();
    }

    match requested {
        "Wasm" => available_engines
            .iter()
            .find(|engine| engine.as_str() == "Wasm")
            .cloned()
            .unwrap_or_else(|| requested.to_string()),
        _ => requested.to_string(),
    }
}
