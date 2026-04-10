//! Parser for .expected test specification files (TOML format)

use anyhow::{Context, Result};
use boon_engine_actors_lite::cells_acceptance::{
    cells_dynamic_acceptance_sequences, cells_static_acceptance_sequences, CellsAcceptanceAction,
    CellsAcceptanceSequence,
};
use boon_engine_actors_lite::counter_acceptance::{
    counter_acceptance_sequences, CounterAcceptanceAction, CounterAcceptanceSequence,
};
use boon_engine_actors_lite::todo_acceptance::{
    todo_edit_save_acceptance_sequences, TodoAcceptanceAction, TodoAcceptanceSequence,
};
use serde::Deserialize;
use std::path::Path;

/// Parsed .expected file specification
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedSpec {
    /// Test metadata
    #[serde(default)]
    pub test: TestMeta,

    /// Expected output specification
    #[serde(default)]
    pub output: OutputSpec,

    /// Interaction sequences for interactive examples
    #[serde(default)]
    pub sequence: Vec<InteractionSequence>,

    /// Persistence test: sequences to run AFTER re-running example (without clearing state)
    /// This tests that state survives across re-runs (simulates page reload)
    #[serde(default)]
    pub persistence: Vec<InteractionSequence>,

    /// Timing configuration
    #[serde(default)]
    pub timing: TimingConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
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

    /// Only run on these engines (e.g., ["Actors", "DD"])
    #[serde(default)]
    pub engines: Option<Vec<String>>,

    /// Skip on these engines (e.g., ["Wasm"] for the WebAssembly backend)
    #[serde(default)]
    pub skip_engines: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
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
                    "run" => Ok(ParsedAction::Run),
                    "key" => {
                        let key = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context(
                                "key requires key name (Enter, Tab, Escape, Backspace, Delete)",
                            )?
                            .to_string();
                        Ok(ParsedAction::Key { key })
                    }
                    "focus_input" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("focus_input requires index (0-indexed)")?;
                        Ok(ParsedAction::FocusInput {
                            index: index as u32,
                        })
                    }
                    "click_text" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("click_text requires text to click")?
                            .to_string();
                        Ok(ParsedAction::ClickText { text })
                    }
                    "click_button" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("click_button requires index (0-indexed)")?;
                        Ok(ParsedAction::ClickButton {
                            index: index as u32,
                        })
                    }
                    "click_button_near_text" => {
                        // ["click_button_near_text", "Walk the dog"] or ["click_button_near_text", "Walk the dog", "×"]
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("click_button_near_text requires target text")?
                            .to_string();
                        let button_text =
                            arr.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
                        Ok(ParsedAction::ClickButtonNearText { text, button_text })
                    }
                    "click_checkbox" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("click_checkbox requires index (0-indexed)")?;
                        Ok(ParsedAction::ClickCheckbox {
                            index: index as u32,
                        })
                    }
                    "click_at" => {
                        let x = arr
                            .get(1)
                            .and_then(|v| v.as_i64())
                            .context("click_at requires x coordinate")?;
                        let y = arr
                            .get(2)
                            .and_then(|v| v.as_i64())
                            .context("click_at requires y coordinate")?;
                        Ok(ParsedAction::ClickAt {
                            x: x as i32,
                            y: y as i32,
                        })
                    }
                    "dblclick_text" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("dblclick_text requires text to double-click")?
                            .to_string();
                        Ok(ParsedAction::DblClickText { text })
                    }
                    "dblclick_text_nth" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("dblclick_text_nth requires text to double-click")?
                            .to_string();
                        let index = arr
                            .get(2)
                            .and_then(|v| v.as_u64())
                            .context("dblclick_text_nth requires match index (0-indexed)")?;
                        Ok(ParsedAction::DblClickTextNth {
                            text,
                            index: index as usize,
                        })
                    }
                    "dblclick_at" => {
                        let x = arr
                            .get(1)
                            .and_then(|v| v.as_i64())
                            .context("dblclick_at requires x coordinate")?;
                        let y = arr
                            .get(2)
                            .and_then(|v| v.as_i64())
                            .context("dblclick_at requires y coordinate")?;
                        Ok(ParsedAction::DblClickAt {
                            x: x as i32,
                            y: y as i32,
                        })
                    }
                    "dblclick_cells_cell" => {
                        let row = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("dblclick_cells_cell requires 1-based row")?;
                        let column = arr
                            .get(2)
                            .and_then(|v| v.as_u64())
                            .context("dblclick_cells_cell requires 1-based column")?;
                        Ok(ParsedAction::DblClickCellsCell {
                            row: row as u32,
                            column: column as u32,
                        })
                    }
                    "assert_cells_cell_text" => {
                        let row = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_cells_cell_text requires 1-based row")?;
                        let column = arr
                            .get(2)
                            .and_then(|v| v.as_u64())
                            .context("assert_cells_cell_text requires 1-based column")?;
                        let expected = arr
                            .get(3)
                            .and_then(|v| v.as_str())
                            .context("assert_cells_cell_text requires expected text")?
                            .to_string();
                        Ok(ParsedAction::AssertCellsCellText {
                            row: row as u32,
                            column: column as u32,
                            expected,
                        })
                    }
                    "assert_cells_row_visible" => {
                        let row = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_cells_row_visible requires 1-based row")?;
                        Ok(ParsedAction::AssertCellsRowVisible { row: row as u32 })
                    }
                    "assert_preview_direct_text_visible" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context(
                                "assert_preview_direct_text_visible requires visible direct text",
                            )?
                            .to_string();
                        Ok(ParsedAction::AssertPreviewDirectTextVisible { text })
                    }
                    "hover_text" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("hover_text requires text to hover over")?
                            .to_string();
                        Ok(ParsedAction::HoverText { text })
                    }
                    "assert_focused" => {
                        let index = arr.get(1).and_then(|v| v.as_u64()).map(|i| i as u32);
                        Ok(ParsedAction::AssertFocused { input_index: index })
                    }
                    "assert_focused_input_value" => {
                        let expected = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("assert_focused_input_value requires expected value")?
                            .to_string();
                        Ok(ParsedAction::AssertFocusedInputValue { expected })
                    }
                    "assert_input_placeholder" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_input_placeholder requires index (0-indexed)")?;
                        let expected = arr
                            .get(2)
                            .and_then(|v| v.as_str())
                            .context("assert_input_placeholder requires expected placeholder text")?
                            .to_string();
                        Ok(ParsedAction::AssertInputPlaceholder {
                            index: index as u32,
                            expected,
                        })
                    }
                    "assert_url" => {
                        let pattern = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("assert_url requires URL pattern")?
                            .to_string();
                        Ok(ParsedAction::AssertUrl { pattern })
                    }
                    "assert_input_typeable" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_input_typeable requires index (0-indexed)")?;
                        Ok(ParsedAction::AssertInputTypeable {
                            index: index as u32,
                        })
                    }
                    "assert_input_not_typeable" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_input_not_typeable requires index (0-indexed)")?;
                        Ok(ParsedAction::AssertInputNotTypeable {
                            index: index as u32,
                        })
                    }
                    "assert_button_disabled" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_button_disabled requires index (0-indexed)")?;
                        Ok(ParsedAction::AssertButtonDisabled {
                            index: index as u32,
                        })
                    }
                    "assert_button_enabled" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_button_enabled requires index (0-indexed)")?;
                        Ok(ParsedAction::AssertButtonEnabled {
                            index: index as u32,
                        })
                    }
                    "assert_button_count" => {
                        let expected_count = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_button_count requires expected count")?;
                        Ok(ParsedAction::AssertButtonCount {
                            expected: expected_count as u32,
                        })
                    }
                    "assert_checkbox_count" => {
                        let expected_count = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_checkbox_count requires expected count")?;
                        Ok(ParsedAction::AssertCheckboxCount {
                            expected: expected_count as u32,
                        })
                    }
                    "assert_not_contains" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context(
                                "assert_not_contains requires text that should NOT be present",
                            )?
                            .to_string();
                        Ok(ParsedAction::AssertNotContains { text })
                    }
                    "assert_not_focused" => {
                        let index = arr.get(1).and_then(|v| v.as_u64()).map(|i| i as u32);
                        Ok(ParsedAction::AssertNotFocused { input_index: index })
                    }
                    "assert_checkbox_unchecked" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_checkbox_unchecked requires checkbox index")?;
                        Ok(ParsedAction::AssertCheckboxUnchecked {
                            index: index as u32,
                        })
                    }
                    "assert_checkbox_checked" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_checkbox_checked requires checkbox index")?;
                        Ok(ParsedAction::AssertCheckboxChecked {
                            index: index as u32,
                        })
                    }
                    "assert_button_has_outline" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("assert_button_has_outline requires button text")?
                            .to_string();
                        Ok(ParsedAction::AssertButtonHasOutline { text })
                    }
                    "assert_toggle_all_darker" => Ok(ParsedAction::AssertToggleAllDarker),
                    "assert_input_empty" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_input_empty requires input index")?;
                        Ok(ParsedAction::AssertInputEmpty {
                            index: index as u32,
                        })
                    }
                    "assert_contains" => {
                        let text = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("assert_contains requires text that should be present")?
                            .to_string();
                        Ok(ParsedAction::AssertContains { text })
                    }
                    "assert_checkbox_clickable" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_checkbox_clickable requires checkbox index")?;
                        Ok(ParsedAction::AssertCheckboxClickable {
                            index: index as u32,
                        })
                    }
                    "assert_element_style" => {
                        let target = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("assert_element_style requires target text")?
                            .to_string();
                        let property = arr
                            .get(2)
                            .and_then(|v| v.as_str())
                            .context("assert_element_style requires CSS property name")?
                            .to_string();
                        let expected = arr
                            .get(3)
                            .and_then(|v| v.as_str())
                            .context("assert_element_style requires expected value substring")?
                            .to_string();
                        Ok(ParsedAction::AssertElementStyle {
                            target,
                            property,
                            expected,
                        })
                    }
                    "assert_input_value" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("assert_input_value requires index (0-indexed)")?;
                        let expected = arr
                            .get(2)
                            .and_then(|v| v.as_str())
                            .context("assert_input_value requires expected value")?
                            .to_string();
                        Ok(ParsedAction::AssertInputValue {
                            index: index as u32,
                            expected,
                        })
                    }
                    "set_slider_value" => {
                        let index = arr.get(1).and_then(|v| v.as_u64()).context(
                            "set_slider_value requires slider index (0-indexed among range inputs)",
                        )?;
                        let raw = arr.get(2).context("set_slider_value requires value")?;
                        // Handle both string and number values from TOML
                        let value_str = if let Some(s) = raw.as_str() {
                            s.to_string()
                        } else if let Some(n) = raw.as_f64() {
                            format!("{}", n)
                        } else {
                            raw.to_string()
                        };
                        Ok(ParsedAction::SetSliderValue {
                            index: index as u32,
                            value: value_str,
                        })
                    }
                    "select_option" => {
                        // ["select_option", index, "value"] - select dropdown option by value
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("select_option requires select index (0-indexed)")?;
                        let value = arr
                            .get(2)
                            .and_then(|v| v.as_str())
                            .context("select_option requires option value")?
                            .to_string();
                        Ok(ParsedAction::SelectOption {
                            index: index as u32,
                            value,
                        })
                    }
                    "set_input_value" => {
                        let index = arr
                            .get(1)
                            .and_then(|v| v.as_u64())
                            .context("set_input_value requires input index (0-indexed)")?;
                        let value = arr
                            .get(2)
                            .and_then(|v| v.as_str())
                            .context("set_input_value requires value")?
                            .to_string();
                        Ok(ParsedAction::SetInputValue {
                            index: index as u32,
                            value,
                        })
                    }
                    "set_focused_input_value" => {
                        let value = arr
                            .get(1)
                            .and_then(|v| v.as_str())
                            .context("set_focused_input_value requires value")?
                            .to_string();
                        Ok(ParsedAction::SetFocusedInputValue { value })
                    }
                    _ => anyhow::bail!("Unknown action type: {}", cmd),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedAction {
    Click {
        selector: String,
    },
    Type {
        selector: String,
        text: String,
    },
    TypeText {
        text: String,
    }, // Type into currently focused element
    Wait {
        ms: u64,
    },
    ClearStates,
    Run, // Trigger code execution
    Key {
        key: String,
    },
    FocusInput {
        index: u32,
    },
    ClickText {
        text: String,
    }, // Click by text content
    ClickButton {
        index: u32,
    }, // Click button by index
    ClickButtonNearText {
        text: String,
        button_text: Option<String>,
    }, // Click button near text (e.g., × button for a todo)
    ClickCheckbox {
        index: u32,
    }, // Click checkbox by index
    ClickAt {
        x: i32,
        y: i32,
    }, // Click preview coordinates
    DblClickText {
        text: String,
    }, // Double-click by text content
    DblClickTextNth {
        text: String,
        index: usize,
    }, // Double-click nth exact match by text
    DblClickAt {
        x: i32,
        y: i32,
    }, // Double-click preview coordinates
    DblClickCellsCell {
        row: u32,
        column: u32,
    }, // Double-click a 7GUIs Cells grid cell by 1-based row/column
    AssertCellsCellText {
        row: u32,
        column: u32,
        expected: String,
    }, // Assert a 7GUIs Cells grid cell text by 1-based row/column
    AssertCellsRowVisible {
        row: u32,
    }, // Assert a 7GUIs Cells row label exists in the rendered sheet
    AssertPreviewDirectTextVisible {
        text: String,
    }, // Assert visible direct text exists in preview without serializing whole preview
    HoverText {
        text: String,
    }, // Hover over element by text content
    AssertFocused {
        input_index: Option<u32>,
    }, // Assert input has focus
    AssertFocusedInputValue {
        expected: String,
    }, // Assert currently focused input value
    AssertInputPlaceholder {
        index: u32,
        expected: String,
    }, // Assert input placeholder
    AssertUrl {
        pattern: String,
    }, // Assert current URL contains pattern
    AssertInputTypeable {
        index: u32,
    }, // Assert input is actually typeable (not disabled/readonly/hidden)
    AssertInputNotTypeable {
        index: u32,
    }, // Assert input is disabled/readonly/hidden and cannot be typed into
    AssertButtonDisabled {
        index: u32,
    }, // Assert button is disabled
    AssertButtonEnabled {
        index: u32,
    }, // Assert button is enabled
    AssertButtonCount {
        expected: u32,
    }, // Assert number of visible buttons in preview
    AssertCheckboxCount {
        expected: u32,
    }, // Assert number of visible checkboxes in preview
    AssertNotContains {
        text: String,
    }, // Assert preview does NOT contain text
    AssertNotFocused {
        input_index: Option<u32>,
    }, // Assert input does NOT have focus
    AssertCheckboxUnchecked {
        index: u32,
    }, // Assert checkbox is NOT checked
    AssertCheckboxChecked {
        index: u32,
    }, // Assert checkbox IS checked
    AssertButtonHasOutline {
        text: String,
    }, // Assert button has visible outline
    AssertToggleAllDarker, // Assert toggle all icon is dark (all todos completed)
    AssertInputEmpty {
        index: u32,
    }, // Assert input value is empty
    AssertContains {
        text: String,
    }, // Assert preview contains text
    AssertCheckboxClickable {
        index: u32,
    }, // Assert checkbox is clickable by real user (not obscured)
    AssertElementStyle {
        target: String,
        property: String,
        expected: String,
    }, // Assert computed CSS style on element found by text
    AssertInputValue {
        index: u32,
        expected: String,
    }, // Assert input's current value
    SetInputValue {
        index: u32,
        value: String,
    }, // Set text input value and dispatch input/change
    SetFocusedInputValue {
        value: String,
    }, // Set currently focused text input value
    SetSliderValue {
        index: u32,
        value: String,
    }, // Set range input value
    SelectOption {
        index: u32,
        value: String,
    }, // Select dropdown option by value
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInteractionSequence {
    pub description: Option<String>,
    pub actions: Vec<ParsedAction>,
    pub expect: Option<String>,
    pub expect_match: MatchMode,
}

pub fn parsed_action_from_cells_acceptance_action(action: &CellsAcceptanceAction) -> ParsedAction {
    match action {
        CellsAcceptanceAction::AssertCellsCellText {
            row,
            column,
            expected,
        } => ParsedAction::AssertCellsCellText {
            row: *row,
            column: *column,
            expected: (*expected).to_string(),
        },
        CellsAcceptanceAction::DblClickCellsCell { row, column } => {
            ParsedAction::DblClickCellsCell {
                row: *row,
                column: *column,
            }
        }
        CellsAcceptanceAction::AssertFocused => ParsedAction::AssertFocused { input_index: None },
        CellsAcceptanceAction::AssertFocusedInputValue { expected } => {
            ParsedAction::AssertFocusedInputValue {
                expected: (*expected).to_string(),
            }
        }
        CellsAcceptanceAction::SetFocusedInputValue { value } => {
            ParsedAction::SetFocusedInputValue {
                value: (*value).to_string(),
            }
        }
        CellsAcceptanceAction::Key { key } => ParsedAction::Key {
            key: (*key).to_string(),
        },
        CellsAcceptanceAction::AssertNotFocused => {
            ParsedAction::AssertNotFocused { input_index: None }
        }
        CellsAcceptanceAction::ClickText { text } => ParsedAction::ClickText {
            text: (*text).to_string(),
        },
        CellsAcceptanceAction::AssertCellsRowVisible { row } => {
            ParsedAction::AssertCellsRowVisible { row: *row }
        }
    }
}

pub fn parsed_action_from_counter_acceptance_action(
    action: &CounterAcceptanceAction,
) -> ParsedAction {
    match action {
        CounterAcceptanceAction::ClickButton { index } => {
            ParsedAction::ClickButton { index: *index }
        }
    }
}

pub fn parsed_action_from_todo_acceptance_action(action: &TodoAcceptanceAction) -> ParsedAction {
    match action {
        TodoAcceptanceAction::DblClickText { text } => ParsedAction::DblClickText {
            text: (*text).to_string(),
        },
        TodoAcceptanceAction::AssertFocused { index } => ParsedAction::AssertFocused {
            input_index: Some(*index),
        },
        TodoAcceptanceAction::AssertInputTypeable { index } => {
            ParsedAction::AssertInputTypeable { index: *index }
        }
        TodoAcceptanceAction::TypeText { text } => ParsedAction::TypeText {
            text: (*text).to_string(),
        },
        TodoAcceptanceAction::FocusInput { index } => ParsedAction::FocusInput { index: *index },
        TodoAcceptanceAction::Key { key } => ParsedAction::Key {
            key: (*key).to_string(),
        },
    }
}

fn parsed_interaction_sequence_from_cells_acceptance_sequence(
    sequence: CellsAcceptanceSequence,
) -> ParsedInteractionSequence {
    ParsedInteractionSequence {
        description: Some(sequence.description.to_string()),
        actions: sequence
            .actions
            .iter()
            .map(parsed_action_from_cells_acceptance_action)
            .collect(),
        expect: None,
        expect_match: MatchMode::default(),
    }
}

fn parsed_interaction_sequence_from_counter_acceptance_sequence(
    sequence: CounterAcceptanceSequence,
) -> ParsedInteractionSequence {
    ParsedInteractionSequence {
        description: Some(sequence.description.to_string()),
        actions: sequence
            .actions
            .iter()
            .map(parsed_action_from_counter_acceptance_action)
            .collect(),
        expect: Some(sequence.expect.to_string()),
        expect_match: MatchMode::default(),
    }
}

fn parsed_interaction_sequence_from_todo_acceptance_sequence(
    sequence: TodoAcceptanceSequence,
) -> ParsedInteractionSequence {
    ParsedInteractionSequence {
        description: Some(sequence.description.to_string()),
        actions: sequence
            .actions
            .iter()
            .map(parsed_action_from_todo_acceptance_action)
            .collect(),
        expect: Some(sequence.expect.to_string()),
        expect_match: MatchMode::default(),
    }
}

pub fn parse_interaction_sequences(
    sequences: &[InteractionSequence],
) -> Result<Vec<ParsedInteractionSequence>> {
    sequences
        .iter()
        .map(|sequence| {
            let actions = sequence
                .actions
                .iter()
                .map(Action::parse)
                .collect::<Result<Vec<_>>>()?;
            Ok(ParsedInteractionSequence {
                description: sequence.description.clone(),
                actions,
                expect: sequence.expect.clone(),
                expect_match: sequence.expect_match.clone(),
            })
        })
        .collect()
}

pub fn shared_example_parsed_sequences(
    example_name: &str,
) -> Option<Vec<ParsedInteractionSequence>> {
    let sequences = match example_name {
        "counter" => {
            return Some(
                counter_acceptance_sequences()
                    .into_iter()
                    .map(parsed_interaction_sequence_from_counter_acceptance_sequence)
                    .collect(),
            );
        }
        "cells" => cells_static_acceptance_sequences(),
        "cells_dynamic" => cells_dynamic_acceptance_sequences(),
        _ => return None,
    };
    Some(
        sequences
            .into_iter()
            .map(parsed_interaction_sequence_from_cells_acceptance_sequence)
            .collect(),
    )
}

fn contains_sequence_window(
    actual: &[ParsedInteractionSequence],
    expected: &[ParsedInteractionSequence],
) -> bool {
    if expected.is_empty() {
        return true;
    }

    actual.windows(expected.len()).any(|window| {
        window
            .iter()
            .zip(expected.iter())
            .all(|(left, right)| left == right)
    })
}

pub fn validate_required_shared_sequences(
    example_name: &str,
    parsed_sequences: &[ParsedInteractionSequence],
) -> Result<()> {
    match example_name {
        "todo_mvc" => {
            let expected = todo_edit_save_acceptance_sequences()
                .into_iter()
                .map(parsed_interaction_sequence_from_todo_acceptance_sequence)
                .collect::<Vec<_>>();
            if !contains_sequence_window(parsed_sequences, &expected) {
                anyhow::bail!(
                    "todo_mvc expected sequences drifted from shared edit-save acceptance trace"
                );
            }
        }
        "counter" => {
            let expected = counter_acceptance_sequences()
                .into_iter()
                .map(parsed_interaction_sequence_from_counter_acceptance_sequence)
                .collect::<Vec<_>>();
            if parsed_sequences != expected {
                anyhow::bail!("counter expected sequences drifted from shared burst-click trace");
            }
        }
        _ => {}
    }

    Ok(())
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
        Self::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Parse from TOML string
    pub fn from_str(content: &str) -> Result<Self> {
        toml::from_str(content).context("Invalid TOML format")
    }

    /// Check if the given text matches the expected output
    #[allow(dead_code)]
    pub fn matches(&self, text: &str) -> Result<bool> {
        self.output.matches(text)
    }
}

impl OutputSpec {
    pub fn is_configured(&self) -> bool {
        self.text.is_some() || self.pattern.is_some()
    }

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
    use std::path::{Path, PathBuf};

    fn repo_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(relative)
    }

    fn assert_expected_file_matches_shared_cells_sequences(
        path: &Path,
        shared: &[CellsAcceptanceSequence],
    ) {
        let spec = ExpectedSpec::from_file(path).expect("expected file parses");
        assert_eq!(
            spec.sequence.len(),
            shared.len(),
            "sequence count mismatch for {}",
            path.display()
        );

        for (index, (actual, expected)) in spec.sequence.iter().zip(shared.iter()).enumerate() {
            assert_eq!(
                actual.description.as_deref(),
                Some(expected.description),
                "description mismatch in {} sequence {index}",
                path.display()
            );
            let parsed_actions = actual
                .actions
                .iter()
                .map(|action| action.parse().expect("action parses"))
                .collect::<Vec<_>>();
            let expected_actions = expected
                .actions
                .iter()
                .map(parsed_action_from_cells_acceptance_action)
                .collect::<Vec<_>>();
            assert_eq!(
                parsed_actions,
                expected_actions,
                "actions mismatch in {} sequence {index}",
                path.display()
            );
        }
    }

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
        assert_eq!(
            spec.sequence[0].description,
            Some("Click increment".to_string())
        );
        assert_eq!(spec.sequence[0].expect, Some("1".to_string()));
    }

    #[test]
    fn counter_expected_matches_shared_acceptance_sequences() {
        let spec = ExpectedSpec::from_file(&repo_path(
            "playground/frontend/src/examples/counter/counter.expected",
        ))
        .expect("expected file parses");
        let actual = parse_interaction_sequences(&spec.sequence).expect("sequence parses");
        let expected = counter_acceptance_sequences()
            .into_iter()
            .map(parsed_interaction_sequence_from_counter_acceptance_sequence)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn cells_expected_matches_shared_acceptance_sequences() {
        assert_expected_file_matches_shared_cells_sequences(
            &repo_path("playground/frontend/src/examples/cells/cells.expected"),
            &cells_static_acceptance_sequences(),
        );
    }

    #[test]
    fn cells_dynamic_expected_matches_shared_acceptance_sequences() {
        assert_expected_file_matches_shared_cells_sequences(
            &repo_path("playground/frontend/src/examples/cells_dynamic/cells_dynamic.expected"),
            &cells_dynamic_acceptance_sequences(),
        );
    }

    #[test]
    fn todo_mvc_expected_contains_shared_edit_save_acceptance_sequences() {
        let spec = ExpectedSpec::from_file(&repo_path(
            "playground/frontend/src/examples/todo_mvc/todo_mvc.expected",
        ))
        .expect("expected file parses");
        let actual = parse_interaction_sequences(&spec.sequence).expect("sequence parses");
        let expected = todo_edit_save_acceptance_sequences()
            .into_iter()
            .map(parsed_interaction_sequence_from_todo_acceptance_sequence)
            .collect::<Vec<_>>();
        assert!(
            contains_sequence_window(&actual, &expected),
            "todo_mvc shared edit-save acceptance trace should appear in expected file",
        );
    }
}
