use crate::cells_preview::{CellsPreview, try_lower_cells_program};
use crate::cells_runtime::CellsSheetState;

pub const ORACLE_SUBSET: &[(u32, u32)] = &[(1, 1), (2, 1), (1, 2), (1, 3)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellsAcceptanceAction {
    AssertCellsCellText {
        row: u32,
        column: u32,
        expected: &'static str,
    },
    DblClickCellsCell {
        row: u32,
        column: u32,
    },
    AssertFocused,
    AssertFocusedInputValue {
        expected: &'static str,
    },
    SetFocusedInputValue {
        value: &'static str,
    },
    Key {
        key: &'static str,
    },
    AssertNotFocused,
    ClickText {
        text: &'static str,
    },
    AssertCellsRowVisible {
        row: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellsAcceptanceSequence {
    pub description: &'static str,
    pub actions: Vec<CellsAcceptanceAction>,
}

#[derive(Clone)]
pub struct CellsTraceRng(u64);

impl CellsTraceRng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 32) as u32
    }

    pub fn next_range(&mut self, upper: u32) -> u32 {
        self.next_u32() % upper
    }
}

pub fn cells_acceptance_sequences(title_text: &'static str) -> Vec<CellsAcceptanceSequence> {
    vec![
        CellsAcceptanceSequence {
            description: "Default A1=5 visible",
            actions: vec![CellsAcceptanceAction::AssertCellsCellText {
                row: 1,
                column: 1,
                expected: "5",
            }],
        },
        CellsAcceptanceSequence {
            description: "Default A2=10 visible",
            actions: vec![CellsAcceptanceAction::AssertCellsCellText {
                row: 2,
                column: 1,
                expected: "10",
            }],
        },
        CellsAcceptanceSequence {
            description: "Default B1=15 visible (add(A1,A2) = 5+10)",
            actions: vec![CellsAcceptanceAction::AssertCellsCellText {
                row: 1,
                column: 2,
                expected: "15",
            }],
        },
        CellsAcceptanceSequence {
            description: "Default C1=30 visible (sum(A1:A3) = 5+10+15)",
            actions: vec![CellsAcceptanceAction::AssertCellsCellText {
                row: 1,
                column: 3,
                expected: "30",
            }],
        },
        CellsAcceptanceSequence {
            description: "Double-click A1 enters edit mode with current value",
            actions: vec![
                CellsAcceptanceAction::DblClickCellsCell { row: 1, column: 1 },
                CellsAcceptanceAction::AssertFocused,
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "5" },
            ],
        },
        CellsAcceptanceSequence {
            description: "Enter commits edited A1 and recomputes dependent cells",
            actions: vec![
                CellsAcceptanceAction::SetFocusedInputValue { value: "7" },
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "7" },
                CellsAcceptanceAction::Key { key: "Enter" },
                CellsAcceptanceAction::AssertNotFocused,
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 1,
                    expected: "7",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 2,
                    expected: "17",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 3,
                    expected: "32",
                },
            ],
        },
        CellsAcceptanceSequence {
            description: "Escape cancels in-progress edit",
            actions: vec![
                CellsAcceptanceAction::DblClickCellsCell { row: 1, column: 1 },
                CellsAcceptanceAction::AssertFocused,
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "7" },
                CellsAcceptanceAction::SetFocusedInputValue { value: "9" },
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "9" },
                CellsAcceptanceAction::Key { key: "Escape" },
                CellsAcceptanceAction::AssertNotFocused,
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 1,
                    expected: "7",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 2,
                    expected: "17",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 3,
                    expected: "32",
                },
            ],
        },
        CellsAcceptanceSequence {
            description: "Blur exits edit mode without corrupting committed values",
            actions: vec![
                CellsAcceptanceAction::DblClickCellsCell { row: 1, column: 1 },
                CellsAcceptanceAction::AssertFocused,
                CellsAcceptanceAction::SetFocusedInputValue { value: "8" },
                CellsAcceptanceAction::ClickText { text: title_text },
                CellsAcceptanceAction::AssertNotFocused,
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 1,
                    expected: "7",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 2,
                    expected: "17",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 3,
                    expected: "32",
                },
            ],
        },
        CellsAcceptanceSequence {
            description: "Repeated reopen and commit recomputes dependent cells",
            actions: vec![
                CellsAcceptanceAction::DblClickCellsCell { row: 1, column: 1 },
                CellsAcceptanceAction::AssertFocused,
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "7" },
                CellsAcceptanceAction::SetFocusedInputValue { value: "11" },
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "11" },
                CellsAcceptanceAction::Key { key: "Enter" },
                CellsAcceptanceAction::AssertNotFocused,
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 1,
                    expected: "11",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 2,
                    expected: "21",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 3,
                    expected: "36",
                },
            ],
        },
        CellsAcceptanceSequence {
            description: "Repeated reopen escape keeps committed dependency values",
            actions: vec![
                CellsAcceptanceAction::DblClickCellsCell { row: 1, column: 1 },
                CellsAcceptanceAction::AssertFocused,
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "11" },
                CellsAcceptanceAction::SetFocusedInputValue { value: "12" },
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "12" },
                CellsAcceptanceAction::Key { key: "Escape" },
                CellsAcceptanceAction::AssertNotFocused,
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 1,
                    expected: "11",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 2,
                    expected: "21",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 3,
                    expected: "36",
                },
            ],
        },
        CellsAcceptanceSequence {
            description: "Editing A2 recomputes the dependent closure",
            actions: vec![
                CellsAcceptanceAction::DblClickCellsCell { row: 2, column: 1 },
                CellsAcceptanceAction::AssertFocused,
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "10" },
                CellsAcceptanceAction::SetFocusedInputValue { value: "20" },
                CellsAcceptanceAction::AssertFocusedInputValue { expected: "20" },
                CellsAcceptanceAction::Key { key: "Enter" },
                CellsAcceptanceAction::AssertNotFocused,
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 2,
                    column: 1,
                    expected: "20",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 2,
                    expected: "31",
                },
                CellsAcceptanceAction::AssertCellsCellText {
                    row: 1,
                    column: 3,
                    expected: "46",
                },
            ],
        },
        CellsAcceptanceSequence {
            description: "Official row 100 becomes visible",
            actions: vec![CellsAcceptanceAction::AssertCellsRowVisible { row: 100 }],
        },
    ]
}

pub fn cells_static_acceptance_sequences() -> Vec<CellsAcceptanceSequence> {
    cells_acceptance_sequences("Cells")
}

pub fn cells_dynamic_acceptance_sequences() -> Vec<CellsAcceptanceSequence> {
    cells_acceptance_sequences("Cells Dynamic")
}

pub fn oracle_sheet_for_source(source: &str) -> CellsSheetState {
    let program = try_lower_cells_program(source).expect("cells program lowers");
    CellsSheetState::new_lowered(program.default_formulas.clone(), program.baseline_state)
}

pub fn assert_preview_matches_oracle_subset(
    preview: &CellsPreview,
    oracle_sheet: &CellsSheetState,
    subset: &[(u32, u32)],
    context: &str,
) {
    for &(row, column) in subset {
        assert_eq!(
            preview.display_text(row, column),
            oracle_sheet.display_text(row, column),
            "{context} for ({row},{column})"
        );
    }
}
