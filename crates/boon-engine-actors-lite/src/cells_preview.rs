use crate::cells_lower::{LoweredCellsFormula, parse_lowered_cells_formula};
use crate::cells_runtime::{CellsFormulaState, CellsSheetState};
use crate::edit_session::{EditSession, EditSessionStateExt, apply_edit_session_key_down};
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::{FunctionInstanceId, MirrorCellId, RetainedNodeKey, SourcePortId, ViewSiteId};
use crate::metrics::{CellsMetricsReport, LatencySummary};
use crate::parse::{
    StaticExpression, StaticSpannedExpression, parse_static_expressions, top_level_bindings,
};
use crate::runtime_backed_domain::RuntimeBackedDomain;
use crate::runtime_backed_preview::RuntimeBackedPreviewState;
use crate::text_input::KEYDOWN_TEXT_SEPARATOR;
use boon::parser::static_expression::{Comparator, Literal, Pattern, TextPart};
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    NodeId, RenderOp, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
    UiNodeKind,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct CellsProgram {
    pub title: &'static str,
    pub display_title: String,
    pub row_count: u32,
    pub col_count: u32,
    pub column_headers: Vec<String>,
    pub default_formulas: BTreeMap<(u32, u32), LoweredCellsFormula>,
    pub baseline_state: CellsFormulaState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CellsUiAction {
    HeadingClick,
    CellClick { row: u32, column: u32 },
    CellDoubleClick { row: u32, column: u32 },
    CellEditInput { row: u32, column: u32 },
    CellEditKeyDown { row: u32, column: u32 },
    CellEditBlur { row: u32, column: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CellsFactTarget {
    CellEditInput { row: u32, column: u32 },
}

pub struct CellsPreview {
    program: CellsProgram,
    sheet: CellsSheetState,
    editing: Option<EditSession<(u32, u32)>>,
    pending_click: Option<(u32, u32)>,
    ui: RuntimeBackedPreviewState<CellsUiAction, CellsFactTarget>,
}

impl InteractivePreview for CellsPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        RuntimeBackedDomain::dispatch_ui_events(self, batch)
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        RuntimeBackedDomain::dispatch_ui_facts(self, batch)
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        RuntimeBackedDomain::render_snapshot(self)
    }
}

impl RuntimeBackedDomain for CellsPreview {
    type Action = CellsUiAction;
    type FactTarget = CellsFactTarget;

    fn preview_state(&mut self) -> &mut RuntimeBackedPreviewState<Self::Action, Self::FactTarget> {
        &mut self.ui
    }

    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        CellsPreview::render_document(self, ops)
    }

    fn handle_event(
        &mut self,
        action: Self::Action,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        CellsPreview::apply_event(self, action, kind, payload)
    }

    fn handle_fact(&mut self, target: Self::FactTarget, kind: UiFactKind) -> bool {
        CellsPreview::apply_fact(self, target, kind)
    }

    fn fact_cell(target: &Self::FactTarget) -> MirrorCellId {
        cells_fact_cell(target)
    }

    fn fact_target(cell: MirrorCellId) -> Option<Self::FactTarget> {
        cells_fact_target(cell)
    }
}

impl CellsPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_cells_program(source)?;
        Ok(Self {
            sheet: CellsSheetState::new_lowered(
                program.default_formulas.clone(),
                program.baseline_state.clone(),
            ),
            program,
            editing: None,
            pending_click: None,
            ui: RuntimeBackedPreviewState::default(),
        })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        RuntimeBackedDomain::dispatch_ui_events(self, batch)
    }

    pub fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        RuntimeBackedDomain::dispatch_ui_facts(self, batch)
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        RuntimeBackedDomain::preview_text(self)
    }

    fn apply_event(
        &mut self,
        action: CellsUiAction,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        match action {
            CellsUiAction::HeadingClick if kind == UiEventKind::Click => {
                let changed = self.editing.is_some() || self.pending_click.is_some();
                self.editing = None;
                self.pending_click = None;
                changed
            }
            CellsUiAction::CellClick { row, column } if kind == UiEventKind::Click => {
                if self.pending_click == Some((row, column)) {
                    self.pending_click = None;
                    return self.enter_edit_mode(row, column);
                }
                self.pending_click = Some((row, column));
                false
            }
            CellsUiAction::CellDoubleClick { row, column } if kind == UiEventKind::DoubleClick => {
                self.pending_click = None;
                self.enter_edit_mode(row, column)
            }
            CellsUiAction::CellEditInput { row, column } => {
                if matches!(kind, UiEventKind::Input | UiEventKind::Change) {
                    return self
                        .editing
                        .set_edit_draft(&(row, column), payload.unwrap_or_default());
                }
                false
            }
            CellsUiAction::CellEditKeyDown { row, column } if kind == UiEventKind::KeyDown => {
                let outcome =
                    apply_edit_session_key_down(&mut self.editing, &(row, column), payload);
                if !outcome.matched {
                    return false;
                }
                if let Some(committed) = outcome.committed_draft {
                    let before = self.cell_formula(row, column);
                    self.commit_override(row, column, committed);
                    return outcome.input_changed || self.cell_formula(row, column) != before;
                }
                if outcome.cancelled {
                    return true;
                }
                outcome.input_changed
            }
            CellsUiAction::CellEditBlur { row, column } if kind == UiEventKind::Blur => {
                self.editing.clear_edit_session(&(row, column))
            }
            _ => false,
        }
    }

    fn apply_fact(&mut self, target: CellsFactTarget, kind: UiFactKind) -> bool {
        match (target, kind) {
            (CellsFactTarget::CellEditInput { row, column }, UiFactKind::DraftText(text)) => {
                self.editing.set_edit_draft(&(row, column), text)
            }
            (CellsFactTarget::CellEditInput { row, column }, UiFactKind::Focused(focused)) => {
                if focused {
                    self.editing.apply_edit_focus(&(row, column), true)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn enter_edit_mode(&mut self, row: u32, column: u32) -> bool {
        let draft = self.sheet.formula_text(row, column);
        self.editing.begin_edit_session((row, column), draft)
    }

    fn commit_override(&mut self, row: u32, column: u32, text: String) {
        self.sheet.commit_override(row, column, text);
    }

    fn cell_formula(&self, row: u32, column: u32) -> String {
        self.sheet.formula_text(row, column)
    }

    pub(crate) fn display_text(&self, row: u32, column: u32) -> String {
        self.sheet.display_text(row, column)
    }

    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let header = self.render_heading(ops);
        let column_header_row = self.render_column_header_row(ops);
        let rows = (1..=self.program.row_count)
            .map(|row| self.render_row(ops, row))
            .collect::<Vec<_>>();
        let body = self.element_node(ViewSiteId(400), "div", None, rows);
        ops.push(RenderOp::SetStyle {
            id: body.id,
            name: "height".to_string(),
            value: Some("500px".to_string()),
        });
        ops.push(RenderOp::SetStyle {
            id: body.id,
            name: "overflow".to_string(),
            value: Some("auto".to_string()),
        });

        self.element_node(
            ViewSiteId(401),
            "div",
            None,
            vec![header, column_header_row, body],
        )
    }

    fn render_heading(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let node = self.element_node(
            ViewSiteId(402),
            "div",
            Some(self.program.display_title.clone()),
            Vec::new(),
        );
        ops.push(RenderOp::SetProperty {
            id: node.id,
            name: "tabindex".to_string(),
            value: Some("0".to_string()),
        });
        self.attach_port(
            ops,
            node.id,
            SourcePortId(4_000),
            UiEventKind::Click,
            CellsUiAction::HeadingClick,
        );
        node
    }

    fn render_column_header_row(&mut self, _ops: &mut Vec<RenderOp>) -> UiNode {
        let headers = self.program.column_headers.clone();
        let mut children = vec![self.sized_label(ViewSiteId(403), "", 40, 26)];
        for (index, header) in headers.iter().enumerate() {
            children.push(self.sized_label(ViewSiteId(405 + index as u32), header, 80, 26));
        }
        self.element_node(ViewSiteId(430), "div", None, children)
    }

    fn render_row(&mut self, ops: &mut Vec<RenderOp>, row: u32) -> UiNode {
        let mut children =
            vec![self.sized_label_with_item(ViewSiteId(431), &row.to_string(), row as u64, 40, 26)];
        for column in 1..=self.program.col_count {
            children.push(self.render_cell(ops, row, column));
        }
        let node = self.element_node_with_item(ViewSiteId(432), "div", None, row as u64, children);
        ops.push(RenderOp::SetProperty {
            id: node.id,
            name: "data-boon-link-path".to_string(),
            value: Some(row_link_path(row)),
        });
        node
    }

    fn render_cell(&mut self, ops: &mut Vec<RenderOp>, row: u32, column: u32) -> UiNode {
        let mapped_id = cell_identity(row, column);
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.matches(&(row, column)))
        {
            let (draft, focus_hint) = {
                let editing = self.editing.as_ref().expect("editing exists");
                (editing.input.draft.clone(), editing.input.focus_hint)
            };
            let node =
                self.element_node_with_item(ViewSiteId(433), "input", None, mapped_id, Vec::new());
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "data-boon-link-path".to_string(),
                value: Some(cell_edit_link_path(row, column)),
            });
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "type".to_string(),
                value: Some("text".to_string()),
            });
            ops.push(RenderOp::SetInputValue {
                id: node.id,
                value: draft,
            });
            if focus_hint {
                ops.push(RenderOp::SetProperty {
                    id: node.id,
                    name: "autofocus".to_string(),
                    value: Some("true".to_string()),
                });
                ops.push(RenderOp::SetProperty {
                    id: node.id,
                    name: "focused".to_string(),
                    value: Some("true".to_string()),
                });
            }
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "width".to_string(),
                value: Some("80px".to_string()),
            });
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "height".to_string(),
                value: Some("26px".to_string()),
            });
            self.ui
                .bind_fact_target(node.id, CellsFactTarget::CellEditInput { row, column });
            self.attach_port(
                ops,
                node.id,
                cell_edit_port(row, column, 1),
                UiEventKind::Input,
                CellsUiAction::CellEditInput { row, column },
            );
            self.attach_port(
                ops,
                node.id,
                cell_edit_port(row, column, 1),
                UiEventKind::Change,
                CellsUiAction::CellEditInput { row, column },
            );
            self.attach_port(
                ops,
                node.id,
                cell_edit_port(row, column, 2),
                UiEventKind::KeyDown,
                CellsUiAction::CellEditKeyDown { row, column },
            );
            self.attach_port(
                ops,
                node.id,
                cell_edit_port(row, column, 3),
                UiEventKind::Blur,
                CellsUiAction::CellEditBlur { row, column },
            );
            node
        } else {
            let node = self.element_node_with_item(
                ViewSiteId(434),
                "span",
                Some(self.display_text(row, column)),
                mapped_id,
                Vec::new(),
            );
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "data-boon-link-path".to_string(),
                value: Some(cell_display_link_path(row, column)),
            });
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "display".to_string(),
                value: Some("inline-block".to_string()),
            });
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "width".to_string(),
                value: Some("80px".to_string()),
            });
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "height".to_string(),
                value: Some("26px".to_string()),
            });
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "padding-left".to_string(),
                value: Some("8px".to_string()),
            });
            self.attach_port(
                ops,
                node.id,
                cell_click_port(row, column),
                UiEventKind::Click,
                CellsUiAction::CellClick { row, column },
            );
            self.attach_port(
                ops,
                node.id,
                cell_display_port(row, column),
                UiEventKind::DoubleClick,
                CellsUiAction::CellDoubleClick { row, column },
            );
            node
        }
    }

    fn attach_port(
        &mut self,
        ops: &mut Vec<RenderOp>,
        node_id: NodeId,
        source_port: SourcePortId,
        kind: UiEventKind,
        action: CellsUiAction,
    ) {
        self.ui.attach_port(ops, node_id, source_port, kind, action);
    }

    fn sized_label(
        &mut self,
        view_site: ViewSiteId,
        text: &str,
        width: u32,
        height: u32,
    ) -> UiNode {
        self.sized_label_with_item(view_site, text, 0, width, height)
    }

    fn sized_label_with_item(
        &mut self,
        view_site: ViewSiteId,
        text: &str,
        mapped_item_identity: u64,
        _width: u32,
        _height: u32,
    ) -> UiNode {
        let node = if mapped_item_identity == 0 {
            self.element_node(view_site, "span", Some(text.to_string()), Vec::new())
        } else {
            self.element_node_with_item(
                view_site,
                "span",
                Some(text.to_string()),
                mapped_item_identity,
                Vec::new(),
            )
        };
        node
    }

    fn element_node(
        &mut self,
        view_site: ViewSiteId,
        tag: &str,
        text: Option<String>,
        children: Vec<UiNode>,
    ) -> UiNode {
        self.make_node(
            RetainedNodeKey {
                view_site,
                function_instance: Some(FunctionInstanceId(10)),
                mapped_item_identity: None,
            },
            tag,
            text,
            children,
        )
    }

    fn element_node_with_item(
        &mut self,
        view_site: ViewSiteId,
        tag: &str,
        text: Option<String>,
        mapped_item_identity: u64,
        children: Vec<UiNode>,
    ) -> UiNode {
        self.make_node(
            RetainedNodeKey {
                view_site,
                function_instance: Some(FunctionInstanceId(11)),
                mapped_item_identity: Some(mapped_item_identity),
            },
            tag,
            text,
            children,
        )
    }

    fn make_node(
        &mut self,
        retained_key: RetainedNodeKey,
        tag: &str,
        text: Option<String>,
        children: Vec<UiNode>,
    ) -> UiNode {
        self.ui.element_node(retained_key, tag, text, children)
    }
}

pub(crate) fn try_lower_cells_program(source: &str) -> Result<CellsProgram, String> {
    let expressions = parse_static_expressions(source)
        .map_err(|error| format!("cells subset requires parseable source: {error}"))?;
    let bindings = top_level_bindings(&expressions);

    for function_name in ["matching_overrides", "cell_formula", "compute_value"] {
        ensure_top_level_function(&expressions, function_name)?;
    }
    ensure_top_level_binding(&bindings, "document")?;
    ensure_document_new_binding(
        bindings
            .get("document")
            .copied()
            .expect("document binding checked above"),
    )?;
    for binding_name in [
        "event_ports",
        "editing_row",
        "editing_column",
        "editing_text",
        "editing_active",
        "overrides",
    ] {
        ensure_top_level_binding(&bindings, binding_name)?;
    }

    let row_count = bindings
        .get("row_count")
        .and_then(|expression| extract_u32_literal(expression))
        .unwrap_or(100);
    let col_count = bindings
        .get("col_count")
        .and_then(|expression| extract_u32_literal(expression))
        .unwrap_or(26);
    let title = if bindings.contains_key("row_count") || bindings.contains_key("col_count") {
        "Cells Dynamic"
    } else {
        "Cells"
    };
    let display_title = bindings
        .get("document")
        .and_then(|document| extract_cells_document_title(document))
        .unwrap_or_else(|| title.to_string());
    let column_headers = extract_cells_column_headers(&expressions, col_count)?;
    let default_formulas = extract_default_formulas(&expressions, row_count, col_count)?;
    let baseline_state = CellsFormulaState::from_lowered_formulas(
        default_formulas
            .iter()
            .map(|(coords, formula)| (*coords, formula.clone())),
    );

    Ok(CellsProgram {
        title,
        display_title,
        row_count,
        col_count,
        column_headers,
        default_formulas,
        baseline_state,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CellsStaticValue {
    Number(i64),
    Bool(bool),
    Text(String),
}

fn extract_default_formulas(
    expressions: &[StaticSpannedExpression],
    row_count: u32,
    col_count: u32,
) -> Result<BTreeMap<(u32, u32), LoweredCellsFormula>, String> {
    let default_formula = expressions
        .iter()
        .find_map(|expression| match &expression.node {
            StaticExpression::Function { name, body, .. } if name.as_str() == "default_formula" => {
                Some(body.as_ref())
            }
            _ => None,
        })
        .ok_or_else(|| "cells subset requires top-level function `default_formula`".to_string())?;

    let mut formulas = BTreeMap::new();
    for row in 1..=row_count {
        for column in 1..=col_count {
            let text = eval_cells_default_formula(default_formula, row, column)?;
            if !text.is_empty() {
                formulas.insert((row, column), parse_lowered_cells_formula(text));
            }
        }
    }
    Ok(formulas)
}

fn extract_cells_column_headers(
    expressions: &[StaticSpannedExpression],
    col_count: u32,
) -> Result<Vec<String>, String> {
    let column_header = expressions
        .iter()
        .find_map(|expression| match &expression.node {
            StaticExpression::Function { name, body, .. } if name.as_str() == "column_header" => {
                Some(body.as_ref())
            }
            _ => None,
        })
        .ok_or_else(|| "cells subset requires top-level function `column_header`".to_string())?;

    let mut headers = Vec::with_capacity(col_count as usize);
    for column in 1..=col_count {
        match eval_cells_expression(column_header, 0, column, None)? {
            CellsStaticValue::Text(text) => headers.push(text),
            CellsStaticValue::Number(number) => headers.push(number.to_string()),
            CellsStaticValue::Bool(_) => {
                return Err("cells column_header must resolve to text".to_string());
            }
        }
    }
    Ok(headers)
}

fn eval_cells_default_formula(
    expression: &StaticSpannedExpression,
    row: u32,
    column: u32,
) -> Result<String, String> {
    match eval_cells_expression(expression, row, column, None)? {
        CellsStaticValue::Text(text) => Ok(text),
        CellsStaticValue::Number(number) => Ok(number.to_string()),
        CellsStaticValue::Bool(_) => Err("cells default_formula must resolve to text".to_string()),
    }
}

fn eval_cells_expression(
    expression: &StaticSpannedExpression,
    row: u32,
    column: u32,
    piped: Option<CellsStaticValue>,
) -> Result<CellsStaticValue, String> {
    match &expression.node {
        StaticExpression::Literal(Literal::Number(number)) => {
            Ok(CellsStaticValue::Number(*number as i64))
        }
        StaticExpression::Literal(Literal::Text(text))
        | StaticExpression::Literal(Literal::Tag(text)) => {
            Ok(CellsStaticValue::Text(text.as_str().to_string()))
        }
        StaticExpression::TextLiteral { parts, .. } => {
            let mut out = String::new();
            for part in parts {
                match part {
                    TextPart::Text(text) => out.push_str(text.as_str()),
                    TextPart::Interpolation { .. } => {
                        return Err(
                            "cells default_formula subset does not support interpolated text"
                                .to_string(),
                        );
                    }
                }
            }
            Ok(CellsStaticValue::Text(out))
        }
        StaticExpression::Alias(alias) => match alias {
            boon::parser::static_expression::Alias::WithoutPassed { parts, .. }
                if parts.len() == 1 && parts[0].as_str() == "row" =>
            {
                Ok(CellsStaticValue::Number(row as i64))
            }
            boon::parser::static_expression::Alias::WithoutPassed { parts, .. }
                if parts.len() == 1 && parts[0].as_str() == "column" =>
            {
                Ok(CellsStaticValue::Number(column as i64))
            }
            boon::parser::static_expression::Alias::WithoutPassed { parts, .. }
                if parts.len() == 1 && parts[0].as_str() == "column_index" =>
            {
                Ok(CellsStaticValue::Number(column as i64))
            }
            _ => Err("cells default_formula subset uses unsupported alias".to_string()),
        },
        StaticExpression::Comparator(Comparator::Equal {
            operand_a,
            operand_b,
        }) => Ok(CellsStaticValue::Bool(
            eval_cells_expression(operand_a, row, column, None)?
                == eval_cells_expression(operand_b, row, column, None)?,
        )),
        StaticExpression::Pipe { from, to } => {
            let input = eval_cells_expression(from, row, column, None)?;
            eval_cells_expression(to, row, column, Some(input))
        }
        StaticExpression::When { arms } => {
            let source = piped.ok_or_else(|| {
                "cells default_formula subset requires WHEN to have pipe input".to_string()
            })?;
            for arm in arms {
                if cells_pattern_matches(&arm.pattern, &source)? {
                    return eval_cells_expression(&arm.body, row, column, None);
                }
            }
            Err("cells default_formula subset found no matching WHEN arm".to_string())
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 2
                && path[0].as_str() == "Text"
                && path[1].as_str() == "empty"
                && arguments.is_empty() =>
        {
            Ok(CellsStaticValue::Text(String::new()))
        }
        _ => Err("cells default_formula subset uses unsupported expression".to_string()),
    }
}

fn cells_pattern_matches(pattern: &Pattern, value: &CellsStaticValue) -> Result<bool, String> {
    Ok(match pattern {
        Pattern::WildCard => true,
        Pattern::Literal(Literal::Number(number)) => {
            matches!(value, CellsStaticValue::Number(current) if *current == *number as i64)
        }
        Pattern::Literal(Literal::Text(text)) | Pattern::Literal(Literal::Tag(text)) => {
            match text.as_str() {
                "True" => matches!(value, CellsStaticValue::Bool(true)),
                "False" => matches!(value, CellsStaticValue::Bool(false)),
                other => matches!(value, CellsStaticValue::Text(current) if current == other),
            }
        }
        _ => {
            return Err("cells default_formula subset uses unsupported WHEN pattern".to_string());
        }
    })
}

fn extract_cells_document_title(expression: &StaticSpannedExpression) -> Option<String> {
    let root = function_call_argument(expression, &["Document", "new"], "root")?;
    let items = function_call_argument(root, &["Element", "stripe"], "items")?;
    let StaticExpression::List { items } = &items.node else {
        return None;
    };
    let title_label = items.first()?;
    let label = function_call_argument(title_label, &["Element", "label"], "label")?;
    first_text_literal(label)
}

fn function_call_argument<'a>(
    expression: &'a StaticSpannedExpression,
    expected_path: &[&str],
    argument_name: &str,
) -> Option<&'a StaticSpannedExpression> {
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return None;
    };
    if path.len() != expected_path.len()
        || path
            .iter()
            .zip(expected_path.iter())
            .any(|(actual, expected)| actual.as_str() != *expected)
    {
        return None;
    }
    arguments
        .iter()
        .find(|argument| argument.node.name.as_str() == argument_name)
        .and_then(|argument| argument.node.value.as_ref())
}

fn first_text_literal(expression: &StaticSpannedExpression) -> Option<String> {
    match &expression.node {
        StaticExpression::Literal(Literal::Number(_)) => None,
        StaticExpression::Literal(Literal::Text(text))
        | StaticExpression::Literal(Literal::Tag(text)) => Some(text.as_str().to_string()),
        StaticExpression::TextLiteral { parts, .. } => {
            let mut out = String::new();
            for part in parts {
                match part {
                    TextPart::Text(text) => out.push_str(text.as_str()),
                    TextPart::Interpolation { .. } => return None,
                }
            }
            Some(out)
        }
        StaticExpression::Variable(variable) => first_text_literal(&variable.value),
        StaticExpression::List { items } | StaticExpression::Latest { inputs: items } => {
            items.iter().find_map(first_text_literal)
        }
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .find_map(|variable| first_text_literal(&variable.node.value)),
        StaticExpression::Map { entries } => entries
            .iter()
            .find_map(|entry| first_text_literal(&entry.value)),
        StaticExpression::Function { body, .. } => first_text_literal(body),
        StaticExpression::FunctionCall { arguments, .. } => arguments
            .iter()
            .filter_map(|argument| argument.node.value.as_ref())
            .find_map(first_text_literal),
        StaticExpression::Hold { body, .. } | StaticExpression::Then { body } => {
            first_text_literal(body)
        }
        StaticExpression::Flush { value } | StaticExpression::Spread { value } => {
            first_text_literal(value)
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => {
            arms.iter().find_map(|arm| first_text_literal(&arm.body))
        }
        StaticExpression::Pipe { from, to } => {
            first_text_literal(from).or_else(|| first_text_literal(to))
        }
        StaticExpression::Block { variables, output } => variables
            .iter()
            .find_map(|variable| first_text_literal(&variable.node.value))
            .or_else(|| first_text_literal(output)),
        StaticExpression::Comparator(comparator) => comparator_operands(comparator)
            .into_iter()
            .find_map(first_text_literal),
        StaticExpression::ArithmeticOperator(operator) => arithmetic_operands(operator)
            .into_iter()
            .find_map(first_text_literal),
        StaticExpression::Bits { size } | StaticExpression::Memory { address: size } => {
            first_text_literal(size)
        }
        StaticExpression::Bytes { data } => data.iter().find_map(first_text_literal),
        StaticExpression::PostfixFieldAccess { expr, .. } => first_text_literal(expr),
        StaticExpression::Alias(_)
        | StaticExpression::LinkSetter { .. }
        | StaticExpression::Link
        | StaticExpression::Skip
        | StaticExpression::FieldAccess { .. } => None,
    }
}

fn comparator_operands(comparator: &Comparator) -> Vec<&StaticSpannedExpression> {
    match comparator {
        Comparator::Equal {
            operand_a,
            operand_b,
        }
        | Comparator::NotEqual {
            operand_a,
            operand_b,
        }
        | Comparator::Greater {
            operand_a,
            operand_b,
        }
        | Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        }
        | Comparator::Less {
            operand_a,
            operand_b,
        }
        | Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => vec![operand_a, operand_b],
    }
}

fn arithmetic_operands(
    operator: &boon::parser::static_expression::ArithmeticOperator,
) -> Vec<&StaticSpannedExpression> {
    match operator {
        boon::parser::static_expression::ArithmeticOperator::Negate { operand } => vec![operand],
        boon::parser::static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }
        | boon::parser::static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }
        | boon::parser::static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        }
        | boon::parser::static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        } => vec![operand_a, operand_b],
    }
}

fn ensure_top_level_function(
    expressions: &[StaticSpannedExpression],
    expected_name: &str,
) -> Result<(), String> {
    if expressions.iter().any(|expression| {
        matches!(
            &expression.node,
            StaticExpression::Function { name, .. } if name.as_str() == expected_name
        )
    }) {
        Ok(())
    } else {
        Err(format!(
            "cells subset requires top-level function `{expected_name}`"
        ))
    }
}

fn ensure_top_level_binding(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    expected_name: &str,
) -> Result<(), String> {
    if bindings.contains_key(expected_name) {
        Ok(())
    } else {
        Err(format!("cells subset requires top-level `{expected_name}`"))
    }
}

fn ensure_document_new_binding(expression: &StaticSpannedExpression) -> Result<(), String> {
    match &expression.node {
        StaticExpression::FunctionCall { path, .. }
            if path.len() == 2 && path[0].as_str() == "Document" && path[1].as_str() == "new" =>
        {
            Ok(())
        }
        _ => Err("cells subset requires top-level `document: Document/new(...)`".to_string()),
    }
}

fn extract_u32_literal(expression: &StaticSpannedExpression) -> Option<u32> {
    match &expression.node {
        StaticExpression::Literal(boon::parser::static_expression::Literal::Number(number))
            if number.fract() == 0.0 && *number >= 0.0 =>
        {
            Some(*number as u32)
        }
        _ => None,
    }
}

fn collect_node_ids(node: &UiNode, ids: &mut HashSet<NodeId>) {
    ids.insert(node.id);
    for child in &node.children {
        collect_node_ids(child, ids);
    }
}

fn collect_node_texts(node: &UiNode, texts: &mut HashMap<NodeId, String>) {
    let text = match &node.kind {
        UiNodeKind::Element { text, .. } => text.clone().unwrap_or_default(),
        UiNodeKind::Text { text } => text.clone(),
    };
    texts.insert(node.id, text);
    for child in &node.children {
        collect_node_texts(child, texts);
    }
}

fn snapshot_ids_and_texts(
    preview: &mut CellsPreview,
) -> (HashSet<NodeId>, HashMap<NodeId, String>) {
    let (root, _) = RuntimeBackedDomain::render_snapshot(preview);
    let RenderRoot::UiTree(root) = root else {
        panic!("cells preview must render a ui tree");
    };
    let mut ids = HashSet::new();
    let mut texts = HashMap::new();
    collect_node_ids(&root, &mut ids);
    collect_node_texts(&root, &mut texts);
    (ids, texts)
}

fn changed_text_count(before: &HashMap<NodeId, String>, after: &HashMap<NodeId, String>) -> usize {
    let all_ids = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<HashSet<_>>();
    all_ids
        .into_iter()
        .filter(|id| before.get(id) != after.get(id))
        .count()
}

fn function_instance_reuse_rate(
    before: &BTreeMap<RetainedNodeKey, NodeId>,
    after: &BTreeMap<RetainedNodeKey, NodeId>,
) -> f64 {
    let after_with_function = after
        .keys()
        .filter(|key| key.function_instance.is_some())
        .collect::<Vec<_>>();
    if after_with_function.is_empty() {
        return 1.0;
    }
    let reused = after_with_function
        .iter()
        .filter(|key| before.contains_key(key))
        .count();
    reused as f64 / after_with_function.len() as f64
}

fn recreated_mapped_scope_count(
    before: &BTreeMap<RetainedNodeKey, NodeId>,
    after: &BTreeMap<RetainedNodeKey, NodeId>,
) -> usize {
    before
        .iter()
        .filter(|(key, _)| key.mapped_item_identity.is_some())
        .filter(|(key, before_id)| {
            after
                .get(key)
                .is_some_and(|after_id| after_id != *before_id)
        })
        .count()
}

pub fn cells_metrics_snapshot() -> Result<CellsMetricsReport, String> {
    let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");

    let startup_started = Instant::now();
    let mut preview = CellsPreview::new(source)?;
    let _ = preview.preview_text();
    let cold_mount_to_stable_first_paint_millis = startup_started.elapsed().as_secs_f64() * 1000.0;

    let mut edit_samples = Vec::<Duration>::new();
    let mut retained_node_creations_per_edit_max = 0usize;
    let mut retained_node_deletions_per_edit_max = 0usize;
    let mut dirty_sink_or_export_count_per_edit_max = 0usize;
    let mut function_instance_reuse_hit_rate_min = 1.0f64;
    let mut recreated_mapped_scope_count_max = 0usize;

    for (_index, (row, column)) in [(1, 1), (2, 1)].into_iter().cycle().take(24).enumerate() {
        let before_keys = preview.ui.retained_nodes().clone();
        let (_before_ids, before_texts) = snapshot_ids_and_texts(&mut preview);
        let current_formula = preview.cell_formula(row, column);
        let next_text = if row == 1 {
            if current_formula == "11" {
                "12".to_string()
            } else {
                "11".to_string()
            }
        } else {
            if current_formula == "20" {
                "21".to_string()
            } else {
                "20".to_string()
            }
        };

        let started = Instant::now();
        assert!(preview.apply_event(
            CellsUiAction::CellDoubleClick { row, column },
            UiEventKind::DoubleClick,
            None,
        ));
        assert!(preview.apply_fact(
            CellsFactTarget::CellEditInput { row, column },
            UiFactKind::DraftText(next_text.clone()),
        ));
        assert!(preview.apply_event(
            CellsUiAction::CellEditKeyDown { row, column },
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}{next_text}")),
        ));
        let _ = preview.preview_text();
        edit_samples.push(started.elapsed());

        let (after_ids, after_texts) = snapshot_ids_and_texts(&mut preview);
        let created = after_ids
            .difference(&before_keys.values().copied().collect::<HashSet<_>>())
            .count();
        let deleted = before_keys
            .values()
            .copied()
            .collect::<HashSet<_>>()
            .difference(&after_ids)
            .count();
        retained_node_creations_per_edit_max = retained_node_creations_per_edit_max.max(created);
        retained_node_deletions_per_edit_max = retained_node_deletions_per_edit_max.max(deleted);
        dirty_sink_or_export_count_per_edit_max = dirty_sink_or_export_count_per_edit_max
            .max(changed_text_count(&before_texts, &after_texts));
        function_instance_reuse_hit_rate_min = function_instance_reuse_hit_rate_min.min(
            function_instance_reuse_rate(&before_keys, preview.ui.retained_nodes()),
        );
        recreated_mapped_scope_count_max = recreated_mapped_scope_count_max.max(
            recreated_mapped_scope_count(&before_keys, preview.ui.retained_nodes()),
        );
    }

    Ok(CellsMetricsReport {
        cold_mount_to_stable_first_paint_millis,
        steady_state_single_cell_edit_to_paint: LatencySummary::from_durations(&edit_samples),
        retained_node_creations_per_edit_max,
        retained_node_deletions_per_edit_max,
        dirty_sink_or_export_count_per_edit_max,
        function_instance_reuse_hit_rate_min,
        recreated_mapped_scope_count_max,
    })
}

fn cell_identity(row: u32, column: u32) -> u64 {
    (row as u64) * 1_000 + column as u64
}

fn row_link_path(row: u32) -> String {
    format!("all_row_cells.{:04}.element", row.saturating_sub(1))
}

fn cell_display_link_path(row: u32, column: u32) -> String {
    format!(
        "all_row_cells.{:04}.cells.{:04}.display_element",
        row.saturating_sub(1),
        column.saturating_sub(1)
    )
}

fn cell_edit_link_path(row: u32, column: u32) -> String {
    format!(
        "all_row_cells.{:04}.cells.{:04}.editing_element",
        row.saturating_sub(1),
        column.saturating_sub(1)
    )
}

fn cell_display_port(row: u32, column: u32) -> SourcePortId {
    SourcePortId(10_000 + row * 100 + column)
}

fn cell_click_port(row: u32, column: u32) -> SourcePortId {
    SourcePortId(100_000 + row * 100 + column)
}

fn cell_edit_port(row: u32, column: u32, suffix: u32) -> SourcePortId {
    SourcePortId(200_000 + row * 1000 + column * 10 + suffix)
}

fn cells_fact_cell(target: &CellsFactTarget) -> MirrorCellId {
    match target {
        CellsFactTarget::CellEditInput { row, column } => {
            MirrorCellId(300_000 + row * 1_000 + column)
        }
    }
}

fn cells_fact_target(cell: MirrorCellId) -> Option<CellsFactTarget> {
    if !(300_000..400_000).contains(&cell.0) {
        return None;
    }
    let encoded = cell.0 - 300_000;
    let row = encoded / 1_000;
    let column = encoded % 1_000;
    Some(CellsFactTarget::CellEditInput { row, column })
}

pub fn render_cells_preview(preview: CellsPreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells_acceptance::{
        CellsAcceptanceAction, CellsAcceptanceSequence, CellsTraceRng, ORACLE_SUBSET,
        assert_preview_matches_oracle_subset, cells_dynamic_acceptance_sequences,
        cells_static_acceptance_sequences, oracle_sheet_for_source,
    };

    #[test]
    fn lowers_real_cells_and_cells_dynamic_examples() {
        let cells = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let dynamic = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );
        let cells_program = try_lower_cells_program(cells).expect("cells lowers");
        let dynamic_program = try_lower_cells_program(dynamic).expect("cells_dynamic lowers");
        assert_eq!(cells_program.title, "Cells");
        assert_eq!(dynamic_program.title, "Cells Dynamic");
        assert_eq!(dynamic_program.row_count, 100);
        assert_eq!(dynamic_program.col_count, 26);
        assert!(
            cells_program
                .default_formulas
                .iter()
                .any(|(coords, formula)| *coords == (1, 1) && formula.formula().text == "5")
        );
        assert!(
            cells_program
                .default_formulas
                .iter()
                .any(|(coords, formula)| {
                    *coords == (1, 2) && formula.formula().text == "=add(A1, A2)"
                })
        );
        assert!(
            cells_program
                .default_formulas
                .iter()
                .any(|(coords, formula)| {
                    *coords == (1, 3) && formula.formula().text == "=sum(A1:A3)"
                })
        );
        assert_eq!(
            cells_program
                .baseline_state
                .computed_values
                .get(&(1, 1))
                .copied(),
            Some(5)
        );
        assert_eq!(
            cells_program
                .baseline_state
                .computed_values
                .get(&(1, 2))
                .copied(),
            Some(15)
        );
        assert_eq!(
            cells_program
                .baseline_state
                .formula_dependencies
                .get(&(1, 2))
                .cloned(),
            Some(vec![(1, 1), (2, 1)])
        );
    }

    #[test]
    fn cells_title_is_inferred_from_structure_not_label_text() {
        let dynamic = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        )
        .replace("TEXT { Cells Dynamic }", "TEXT { Renamed Dynamic Sheet }");
        let cells = include_str!("../../../playground/frontend/src/examples/cells/cells.bn")
            .replace("TEXT { Cells }", "TEXT { Renamed Static Sheet }");

        let dynamic_program = try_lower_cells_program(&dynamic).expect("dynamic still lowers");
        let cells_program = try_lower_cells_program(&cells).expect("cells still lowers");

        assert_eq!(dynamic_program.title, "Cells Dynamic");
        assert_eq!(cells_program.title, "Cells");
    }

    #[test]
    fn cells_visible_title_and_headers_are_lowered_from_source() {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn")
            .replace("TEXT { Cells }", "TEXT { Renamed Sheet }")
            .replace("1 => TEXT { A }", "1 => TEXT { Alpha }")
            .replace("2 => TEXT { B }", "2 => TEXT { Beta }");

        let program = try_lower_cells_program(&source).expect("cells lowers");

        assert_eq!(program.title, "Cells");
        assert_eq!(program.display_title, "Renamed Sheet");
        assert_eq!(
            program.column_headers.first().map(String::as_str),
            Some("Alpha")
        );
        assert_eq!(
            program.column_headers.get(1).map(String::as_str),
            Some("Beta")
        );
    }

    #[test]
    fn cells_preview_renders_default_values_and_row_100() {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let mut preview = CellsPreview::new(source).expect("cells preview");
        let text = preview.preview_text();
        assert!(text.contains("Cells"));
        assert!(text.contains("5"));
        assert!(text.contains("10"));
        assert!(text.contains("15"));
        assert!(text.contains("30"));
        assert!(text.contains("100"));
    }

    fn run_shared_acceptance_sequences(
        source: &str,
        sequences: &[CellsAcceptanceSequence],
        expected_heading: &str,
    ) {
        let mut preview = CellsPreview::new(source).expect("cells preview");

        for sequence in sequences {
            for action in &sequence.actions {
                match action {
                    CellsAcceptanceAction::AssertCellsCellText {
                        row,
                        column,
                        expected,
                    } => {
                        assert_eq!(
                            preview.display_text(*row, *column),
                            *expected,
                            "{}",
                            sequence.description
                        );
                    }
                    CellsAcceptanceAction::DblClickCellsCell { row, column } => {
                        preview.apply_event(
                            CellsUiAction::CellDoubleClick {
                                row: *row,
                                column: *column,
                            },
                            UiEventKind::DoubleClick,
                            None,
                        );
                    }
                    CellsAcceptanceAction::AssertFocused => {
                        assert!(preview.editing.is_some(), "{}", sequence.description);
                    }
                    CellsAcceptanceAction::AssertFocusedInputValue { expected } => {
                        assert_eq!(
                            preview.editing.as_ref().map(|e| e.input.draft.as_str()),
                            Some(*expected),
                            "{}",
                            sequence.description
                        );
                    }
                    CellsAcceptanceAction::SetFocusedInputValue { value } => {
                        let (row, column) = preview
                            .editing
                            .as_ref()
                            .map(|edit| edit.target)
                            .expect("editing active");
                        preview.apply_fact(
                            CellsFactTarget::CellEditInput { row, column },
                            UiFactKind::DraftText((*value).to_string()),
                        );
                    }
                    CellsAcceptanceAction::Key { key } => {
                        let (row, column, draft) = preview
                            .editing
                            .as_ref()
                            .map(|edit| (edit.target.0, edit.target.1, edit.input.draft.clone()))
                            .expect("editing active");
                        let payload = if *key == "Enter" {
                            Some(format!("Enter{KEYDOWN_TEXT_SEPARATOR}{draft}"))
                        } else {
                            Some((*key).to_string())
                        };
                        preview.apply_event(
                            CellsUiAction::CellEditKeyDown { row, column },
                            UiEventKind::KeyDown,
                            payload.as_deref(),
                        );
                    }
                    CellsAcceptanceAction::AssertNotFocused => {
                        assert!(preview.editing.is_none(), "{}", sequence.description);
                    }
                    CellsAcceptanceAction::ClickText { text } => {
                        assert_eq!(*text, expected_heading, "{}", sequence.description);
                        preview.apply_event(CellsUiAction::HeadingClick, UiEventKind::Click, None);
                    }
                    CellsAcceptanceAction::AssertCellsRowVisible { row } => {
                        assert!(
                            preview.preview_text().contains(&row.to_string()),
                            "{}",
                            sequence.description
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn cells_preview_shared_static_acceptance_sequences_behave_as_expected() {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        run_shared_acceptance_sequences(source, &cells_static_acceptance_sequences(), "Cells");
    }

    #[test]
    fn cells_preview_shared_dynamic_acceptance_sequences_behave_as_expected() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );
        run_shared_acceptance_sequences(
            source,
            &cells_dynamic_acceptance_sequences(),
            "Cells Dynamic",
        );
    }

    #[test]
    fn two_clicks_on_same_cell_enter_edit_mode() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );
        let mut preview = CellsPreview::new(source).expect("cells preview");

        assert!(!preview.apply_event(
            CellsUiAction::CellClick { row: 1, column: 1 },
            UiEventKind::Click,
            None,
        ));
        assert!(preview.editing.is_none());

        assert!(preview.apply_event(
            CellsUiAction::CellClick { row: 1, column: 1 },
            UiEventKind::Click,
            None,
        ));
        assert_eq!(
            preview
                .editing
                .as_ref()
                .map(|editing| editing.input.draft.as_str()),
            Some("5")
        );
    }

    #[test]
    fn cells_dependency_closure_updates_only_affected_formula_chain() {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let mut preview = CellsPreview::new(source).expect("cells preview");

        assert_eq!(preview.display_text(2, 1), "10");
        assert_eq!(preview.display_text(1, 2), "15");
        assert_eq!(preview.display_text(1, 3), "30");
        assert_eq!(preview.display_text(10, 10), "");

        preview.apply_event(
            CellsUiAction::CellDoubleClick { row: 2, column: 1 },
            UiEventKind::DoubleClick,
            None,
        );
        preview.apply_fact(
            CellsFactTarget::CellEditInput { row: 2, column: 1 },
            UiFactKind::DraftText("20".to_string()),
        );
        preview.apply_event(
            CellsUiAction::CellEditKeyDown { row: 2, column: 1 },
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}20")),
        );

        assert_eq!(preview.display_text(2, 1), "20");
        assert_eq!(preview.display_text(1, 2), "25");
        assert_eq!(preview.display_text(1, 3), "40");
        assert_eq!(preview.display_text(10, 10), "");

        preview.apply_event(
            CellsUiAction::CellDoubleClick { row: 2, column: 1 },
            UiEventKind::DoubleClick,
            None,
        );
        preview.apply_fact(
            CellsFactTarget::CellEditInput { row: 2, column: 1 },
            UiFactKind::DraftText("10".to_string()),
        );
        preview.apply_event(
            CellsUiAction::CellEditKeyDown { row: 2, column: 1 },
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}10")),
        );

        assert_eq!(preview.display_text(2, 1), "10");
        assert_eq!(preview.display_text(1, 2), "15");
        assert_eq!(preview.display_text(1, 3), "30");
    }

    #[test]
    fn cells_preview_edit_metrics_stay_within_plan_budget() {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let mut preview = CellsPreview::new(source).expect("cells preview");

        let unaffected_row_key = RetainedNodeKey {
            view_site: ViewSiteId(432),
            function_instance: Some(FunctionInstanceId(11)),
            mapped_item_identity: Some(50),
        };
        let unaffected_cell_key = RetainedNodeKey {
            view_site: ViewSiteId(434),
            function_instance: Some(FunctionInstanceId(11)),
            mapped_item_identity: Some(cell_identity(50, 5)),
        };

        let (initial_ids, initial_texts) = snapshot_ids_and_texts(&mut preview);
        let initial_row_id = *preview
            .ui
            .retained_nodes()
            .get(&unaffected_row_key)
            .expect("unaffected row retained");
        let initial_cell_id = *preview
            .ui
            .retained_nodes()
            .get(&unaffected_cell_key)
            .expect("unaffected cell retained");

        preview.apply_event(
            CellsUiAction::CellDoubleClick { row: 1, column: 1 },
            UiEventKind::DoubleClick,
            None,
        );
        let (editing_ids, _editing_texts) = snapshot_ids_and_texts(&mut preview);
        let created_on_edit = editing_ids.difference(&initial_ids).count();
        let deleted_on_edit = initial_ids.difference(&editing_ids).count();
        let reused_on_edit = editing_ids.intersection(&initial_ids).count();
        let reuse_rate_on_edit = reused_on_edit as f64 / editing_ids.len() as f64;
        assert!(created_on_edit <= 6, "created_on_edit={created_on_edit}");
        assert!(deleted_on_edit <= 6, "deleted_on_edit={deleted_on_edit}");
        assert!(
            reuse_rate_on_edit >= 0.95,
            "reuse_rate_on_edit={reuse_rate_on_edit}"
        );

        preview.apply_fact(
            CellsFactTarget::CellEditInput { row: 1, column: 1 },
            UiFactKind::DraftText("11".to_string()),
        );
        preview.apply_event(
            CellsUiAction::CellEditKeyDown { row: 1, column: 1 },
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}11")),
        );
        let (after_commit_ids, after_commit_texts) = snapshot_ids_and_texts(&mut preview);
        let created_on_commit = after_commit_ids.difference(&editing_ids).count();
        let deleted_on_commit = editing_ids.difference(&after_commit_ids).count();
        let reused_on_commit = after_commit_ids.intersection(&editing_ids).count();
        let reuse_rate_on_commit = reused_on_commit as f64 / after_commit_ids.len() as f64;
        let dirty_text_count = changed_text_count(&initial_texts, &after_commit_texts);
        assert!(
            created_on_commit <= 6,
            "created_on_commit={created_on_commit}"
        );
        assert!(
            deleted_on_commit <= 6,
            "deleted_on_commit={deleted_on_commit}"
        );
        assert!(
            reuse_rate_on_commit >= 0.95,
            "reuse_rate_on_commit={reuse_rate_on_commit}"
        );
        assert!(
            dirty_text_count <= 32,
            "dirty_text_count={dirty_text_count}"
        );
        assert_eq!(preview.display_text(1, 1), "11");
        assert_eq!(preview.display_text(1, 2), "21");
        assert_eq!(preview.display_text(1, 3), "36");

        preview.apply_event(
            CellsUiAction::CellDoubleClick { row: 2, column: 1 },
            UiEventKind::DoubleClick,
            None,
        );
        preview.apply_fact(
            CellsFactTarget::CellEditInput { row: 2, column: 1 },
            UiFactKind::DraftText("20".to_string()),
        );
        preview.apply_event(
            CellsUiAction::CellEditKeyDown { row: 2, column: 1 },
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}20")),
        );
        let (_after_a2_ids, after_a2_texts) = snapshot_ids_and_texts(&mut preview);
        let dirty_text_count_after_a2 = changed_text_count(&after_commit_texts, &after_a2_texts);
        assert!(
            dirty_text_count_after_a2 <= 32,
            "dirty_text_count_after_a2={dirty_text_count_after_a2}"
        );
        assert_eq!(preview.display_text(2, 1), "20");
        assert_eq!(preview.display_text(1, 2), "31");
        assert_eq!(preview.display_text(1, 3), "46");

        let final_row_id = *preview
            .ui
            .retained_nodes()
            .get(&unaffected_row_key)
            .expect("unaffected row retained after edits");
        let final_cell_id = *preview
            .ui
            .retained_nodes()
            .get(&unaffected_cell_key)
            .expect("unaffected cell retained after edits");
        assert_eq!(initial_row_id, final_row_id, "recreated mapped row scope");
        assert_eq!(
            initial_cell_id, final_cell_id,
            "recreated mapped cell scope"
        );
    }

    #[test]
    fn cells_preview_randomized_trace_matches_oracle_subset() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/cells_dynamic/cells_dynamic.bn"
        );
        let mut preview = CellsPreview::new(source).expect("cells preview");
        let mut oracle_sheet = oracle_sheet_for_source(source);
        let mut rng = CellsTraceRng::new(0xCE115);

        for step in 0..80u32 {
            let target_row = if rng.next_range(2) == 0 { 1 } else { 2 };
            let next_value = (1 + rng.next_range(40)).to_string();
            preview.apply_event(
                CellsUiAction::CellDoubleClick {
                    row: target_row,
                    column: 1,
                },
                UiEventKind::DoubleClick,
                None,
            );

            match rng.next_range(3) {
                0 => {
                    preview.apply_fact(
                        CellsFactTarget::CellEditInput {
                            row: target_row,
                            column: 1,
                        },
                        UiFactKind::DraftText(next_value.clone()),
                    );
                    preview.apply_event(
                        CellsUiAction::CellEditKeyDown {
                            row: target_row,
                            column: 1,
                        },
                        UiEventKind::KeyDown,
                        Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}{next_value}")),
                    );
                    oracle_sheet.commit_override(target_row, 1, next_value);
                }
                1 => {
                    preview.apply_fact(
                        CellsFactTarget::CellEditInput {
                            row: target_row,
                            column: 1,
                        },
                        UiFactKind::DraftText(next_value),
                    );
                    preview.apply_event(
                        CellsUiAction::CellEditKeyDown {
                            row: target_row,
                            column: 1,
                        },
                        UiEventKind::KeyDown,
                        Some("Escape"),
                    );
                }
                _ => {
                    preview.apply_fact(
                        CellsFactTarget::CellEditInput {
                            row: target_row,
                            column: 1,
                        },
                        UiFactKind::DraftText(next_value),
                    );
                    preview.apply_event(
                        CellsUiAction::CellEditBlur {
                            row: target_row,
                            column: 1,
                        },
                        UiEventKind::Blur,
                        None,
                    );
                }
            }

            assert_preview_matches_oracle_subset(
                &preview,
                &oracle_sheet,
                ORACLE_SUBSET,
                &format!("mismatch at step {step}"),
            );
        }
    }
}
