use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const PHASE4_ACCEPTANCE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/verification/actors_lite_phase4_acceptance.json"
));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorsLitePhase4AcceptanceRecord {
    pub schema_version: u32,
    pub phase: String,
    pub status: String,
    pub verified_at_utc: String,
    pub milestone_examples: Vec<String>,
    pub fast_harness_examples: Vec<String>,
    pub required_commands: Vec<String>,
    pub verification_command: String,
    pub notes: Vec<String>,
}

impl ActorsLitePhase4AcceptanceRecord {
    #[must_use]
    pub fn is_green(&self) -> bool {
        self.schema_version == 1
            && self.phase == "phase4"
            && self.status == "green"
            && self.milestone_examples.iter().map(String::as_str).eq([
                "counter",
                "todo_mvc",
                "cells",
                "cells_dynamic",
            ])
            && self
                .fast_harness_examples
                .iter()
                .map(String::as_str)
                .eq(["cells", "cells_dynamic"])
    }
}

static PHASE4_ACCEPTANCE_RECORD: OnceLock<Result<ActorsLitePhase4AcceptanceRecord, String>> =
    OnceLock::new();

pub fn actors_lite_phase4_acceptance_record()
-> Result<&'static ActorsLitePhase4AcceptanceRecord, &'static str> {
    match PHASE4_ACCEPTANCE_RECORD.get_or_init(|| {
        serde_json::from_str(PHASE4_ACCEPTANCE_JSON)
            .map_err(|error| format!("invalid ActorsLite phase 4 acceptance record: {error}"))
    }) {
        Ok(record) => Ok(record),
        Err(error) => Err(error.as_str()),
    }
}

#[must_use]
pub fn actors_lite_phase4_acceptance_is_green() -> bool {
    actors_lite_phase4_acceptance_record().is_ok_and(|record| record.is_green())
}

#[must_use]
pub fn actors_lite_public_exposure_enabled() -> bool {
    actors_lite_phase4_acceptance_is_green()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase4_acceptance_record_is_present_and_green() {
        let record =
            actors_lite_phase4_acceptance_record().expect("phase 4 acceptance record should parse");
        assert!(record.is_green());
    }
}
