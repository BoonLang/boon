//! Parser for .expected test specification files (TOML format)

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Parsed .expected file specification
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedSpec {
    /// Test metadata
    #[serde(default)]
    pub test: TestMeta,

    /// Expected output specification
    pub output: OutputSpec,

    /// Interaction sequences for interactive examples
    #[serde(default)]
    pub sequence: Vec<InteractionSequence>,

    /// Timing configuration
    #[serde(default)]
    pub timing: TimingConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TestMeta {
    /// Category: static, interactive, timer
    #[serde(default)]
    pub category: Option<String>,

    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,

    /// Skip reason (if set, test will be skipped)
    #[serde(default)]
    pub skip: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputSpec {
    /// Match mode: contains (default), exact, regex
    #[serde(default)]
    pub r#match: MatchMode,

    /// Expected text (for contains/exact modes)
    #[serde(default)]
    pub text: Option<String>,

    /// Regex pattern (for regex mode)
    #[serde(default)]
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    #[default]
    Contains,
    Exact,
    Regex,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InteractionSequence {
    /// Description of this step
    #[serde(default)]
    pub description: Option<String>,

    /// Actions to perform
    #[serde(default)]
    pub actions: Vec<Action>,

    /// Expected output after actions
    #[serde(default)]
    pub expect: Option<String>,

    /// Match mode for this step's expectation
    #[serde(default)]
    pub expect_match: MatchMode,
}

/// Action to perform in an interaction sequence
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Action {
    /// Action as array: ["click", "selector"] or ["type", "selector", "text"]
    Array(Vec<serde_json::Value>),
}

impl Action {
    /// Parse action into command type and arguments
    pub fn parse(&self) -> Result<ParsedAction> {
        match self {
            Action::Array(arr) => {
                let cmd = arr
                    .first()
                    .and_then(|v| v.as_str())
                    .context("Action must have command type as first element")?;

                match cmd {
                    "click" => {
                        let selector = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("click requires selector")?
                            .to_string();
                        Ok(ParsedAction::Click { selector })
                    }
                    "type" => {
                        // Two forms:
                        // ["type", "text"] - type into currently focused element (use after focus_input)
                        // ["type", "selector", "text"] - focus element by selector, then type
                        if arr.len() == 2 {
                            // ["type", "text"] - type into focused element
                            let text = arr
                                .get(1)
                                .and_then(|v| v.as_str())
                                .context("type requires text")?
                                .to_string();
                            Ok(ParsedAction::TypeText { text })
                        } else {
                            // ["type", "selector", "text"] - focus by selector first
                            let selector = arr
                                .get(1)
                                .and_then(|v| v.as_str())
                                .context("type requires selector")?
                                .to_string();
                            let text = arr
                                .get(2)
                                .and_then(|v| v.as_str())
                                .context("type requires text")?
                                .to_string();
                            Ok(ParsedAction::Type { selector, text })
                        }
                    }
                    "wait" => {
                        let ms = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("wait requires milliseconds")?;
                        Ok(ParsedAction::Wait { ms })
                    }
                    "clear_states" => Ok(ParsedAction::ClearStates),
                    "key" => {
                        let key = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("key requires key name (Enter, Tab, Escape, Backspace, Delete)")?
                            .to_string();
                        Ok(ParsedAction::Key { key })
                    }
                    "focus_input" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("focus_input requires index (0-indexed)")?;
                        Ok(ParsedAction::FocusInput { index: index as u32 })
                    }
                    _ => anyhow::bail!("Unknown action type: {}", cmd),
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum ParsedAction {
    Click { selector: String },
    Type { selector: String, text: String },
    TypeText { text: String },  // Type into currently focused element
    Wait { ms: u64 },
    ClearStates,
    Key { key: String },
    FocusInput { index: u32 },
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimingConfig {
    /// Maximum time to wait for output stabilization (ms)
    #[serde(default = "default_timeout")]
    pub timeout: u64,

    /// Poll interval for smart waiting (ms)
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,

    /// Initial delay after run before first check (ms)
    #[serde(default = "default_initial_delay")]
    pub initial_delay: u64,
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            poll_interval: default_poll_interval(),
            initial_delay: default_initial_delay(),
        }
    }
}

fn default_timeout() -> u64 {
    5000
}

fn default_poll_interval() -> u64 {
    100
}

fn default_initial_delay() -> u64 {
    200
}

impl ExpectedSpec {
    /// Parse an .expected file from path
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        Self::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Parse from TOML string
    pub fn from_str(content: &str) -> Result<Self> {
        toml::from_str(content).context("Invalid TOML format")
    }

    /// Check if the given text matches the expected output
    pub fn matches(&self, text: &str) -> Result<bool> {
        self.output.matches(text)
    }
}

impl OutputSpec {
    /// Check if the given text matches this output specification
    pub fn matches(&self, text: &str) -> Result<bool> {
        let text = text.trim();

        match self.r#match {
            MatchMode::Contains => {
                let expected = self
                    .text
                    .as_ref()
                    .context("'text' required for contains match")?;
                Ok(text.contains(expected.trim()))
            }
            MatchMode::Exact => {
                let expected = self
                    .text
                    .as_ref()
                    .context("'text' required for exact match")?;
                Ok(text == expected.trim())
            }
            MatchMode::Regex => {
                let pattern = self
                    .pattern
                    .as_ref()
                    .context("'pattern' required for regex match")?;
                let re = regex::Regex::new(pattern)
                    .with_context(|| format!("Invalid regex: {}", pattern))?;
                Ok(re.is_match(text))
            }
        }
    }
}

/// Check if text matches an inline expectation with optional match mode
pub fn matches_inline(text: &str, expected: &str, mode: &MatchMode) -> Result<bool> {
    let text = text.trim();
    let expected = expected.trim();

    match mode {
        MatchMode::Contains => Ok(text.contains(expected)),
        MatchMode::Exact => Ok(text == expected),
        MatchMode::Regex => {
            let re = regex::Regex::new(expected)
                .with_context(|| format!("Invalid regex: {}", expected))?;
            Ok(re.is_match(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal() {
        let toml = r#"
[output]
text = "123"
"#;
        let spec = ExpectedSpec::from_str(toml).unwrap();
        assert!(spec.matches("123").unwrap());
        assert!(spec.matches("  123  ").unwrap());
        assert!(spec.matches("abc 123 def").unwrap());
    }

    #[test]
    fn test_parse_exact() {
        let toml = r#"
[output]
match = "exact"
text = "Hello world!"
"#;
        let spec = ExpectedSpec::from_str(toml).unwrap();
        assert!(spec.matches("Hello world!").unwrap());
        assert!(!spec.matches("Hello world! extra").unwrap());
    }

    #[test]
    fn test_parse_regex() {
        let toml = r#"
[output]
match = "regex"
pattern = "^\\d+$"
"#;
        let spec = ExpectedSpec::from_str(toml).unwrap();
        assert!(spec.matches("123").unwrap());
        assert!(spec.matches("0").unwrap());
        assert!(!spec.matches("abc").unwrap());
    }

    #[test]
    fn test_parse_with_sequences() {
        let toml = r#"
[output]
text = "0"

[[sequence]]
description = "Click increment"
actions = [["click", "[role='button']"]]
expect = "1"
"#;
        let spec = ExpectedSpec::from_str(toml).unwrap();
        assert_eq!(spec.sequence.len(), 1);
        assert_eq!(spec.sequence[0].description, Some("Click increment".to_string()));
        assert_eq!(spec.sequence[0].expect, Some("1".to_string()));
    }
}
