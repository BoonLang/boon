use crate::runtime::{
    LoweredRetainedNumberFormula, RetainedNumberFormula, RetainedNumberMemberSpec,
};
use boon::platform::browser::kernel::KernelValue;

#[derive(Debug, Clone)]
pub struct CellsFormula {
    pub(crate) text: String,
    pub(crate) expr: CellsFormulaExpr,
    dependencies: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CellsFormulaExpr {
    Empty,
    Number(i64),
    Add(CellsReference, CellsReference),
    SumColumnRange {
        column: u32,
        start_row: u32,
        end_row: u32,
    },
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellsReference {
    pub(crate) row: u32,
    pub(crate) column: u32,
}

pub(crate) type LoweredCellsFormula = LoweredRetainedNumberFormula<CellsFormula>;

impl CellsFormula {
    pub fn parse(text: String) -> Self {
        let trimmed = text.trim();
        let expr = if trimmed.is_empty() {
            CellsFormulaExpr::Empty
        } else if let Some(expression) = trimmed.strip_prefix('=') {
            parse_cells_formula_expression(expression)
        } else {
            trimmed
                .parse::<i64>()
                .map(CellsFormulaExpr::Number)
                .unwrap_or(CellsFormulaExpr::Invalid)
        };
        let dependencies = cells_formula_dependencies(&expr);
        Self {
            text,
            expr,
            dependencies,
        }
    }

    pub(crate) fn dependencies(&self) -> Vec<(u32, u32)> {
        self.dependencies.clone()
    }
}

impl PartialEq for CellsFormula {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.expr == other.expr
    }
}

impl Eq for CellsFormula {}

pub(crate) fn lower_cells_formula(formula: CellsFormula) -> LoweredCellsFormula {
    let retained_number_formula = RetainedNumberFormula {
        dependency_count: formula.dependencies().len(),
        spec: match &formula.expr {
            CellsFormulaExpr::Number(number) => RetainedNumberMemberSpec::InputLeaf {
                value: KernelValue::from(*number as f64),
            },
            CellsFormulaExpr::Invalid | CellsFormulaExpr::Empty => {
                RetainedNumberMemberSpec::InputLeaf {
                    value: KernelValue::from(0.0),
                }
            }
            CellsFormulaExpr::Add(_, _) => RetainedNumberMemberSpec::Add2,
            CellsFormulaExpr::SumColumnRange { .. } => RetainedNumberMemberSpec::SumList,
        },
    };
    LoweredRetainedNumberFormula::new(formula, retained_number_formula)
}

pub(crate) fn parse_lowered_cells_formula(text: String) -> LoweredCellsFormula {
    lower_cells_formula(CellsFormula::parse(text))
}

fn cells_formula_dependencies(expr: &CellsFormulaExpr) -> Vec<(u32, u32)> {
    match expr {
        CellsFormulaExpr::Empty | CellsFormulaExpr::Number(_) | CellsFormulaExpr::Invalid => {
            Vec::new()
        }
        CellsFormulaExpr::Add(left, right) => {
            vec![(left.row, left.column), (right.row, right.column)]
        }
        CellsFormulaExpr::SumColumnRange {
            column,
            start_row,
            end_row,
        } => (*start_row..=*end_row).map(|row| (row, *column)).collect(),
    }
}

fn parse_cells_formula_expression(expression: &str) -> CellsFormulaExpr {
    if let Some(inner) = expression
        .strip_prefix("add(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let mut parts = inner.split(',').map(|part| part.trim());
        let left = parts.next().and_then(parse_reference);
        let right = parts.next().and_then(parse_reference);
        if parts.next().is_none() {
            if let (Some((left_column, left_row)), Some((right_column, right_row))) = (left, right)
            {
                return CellsFormulaExpr::Add(
                    CellsReference {
                        row: left_row,
                        column: left_column,
                    },
                    CellsReference {
                        row: right_row,
                        column: right_column,
                    },
                );
            }
        }
        return CellsFormulaExpr::Invalid;
    }

    if let Some(inner) = expression
        .strip_prefix("sum(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        if let Some((start, end)) = inner.split_once(':') {
            if let (Some((start_col, start_row)), Some((end_col, end_row))) =
                (parse_reference(start.trim()), parse_reference(end.trim()))
            {
                if start_col == end_col {
                    return CellsFormulaExpr::SumColumnRange {
                        column: start_col,
                        start_row,
                        end_row,
                    };
                }
            }
        }
        return CellsFormulaExpr::Invalid;
    }

    CellsFormulaExpr::Invalid
}

pub(crate) fn parse_reference(reference: &str) -> Option<(u32, u32)> {
    let trimmed = reference.trim();
    let mut chars = trimmed.chars();
    let column_char = chars.next()?;
    if !column_char.is_ascii_uppercase() {
        return None;
    }
    let column = (column_char as u8 - b'A' + 1) as u32;
    let row = chars.as_str().parse::<u32>().ok()?;
    Some((column, row))
}
