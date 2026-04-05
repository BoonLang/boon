use crate::cells_lower::LoweredCellsFormula;
use crate::cells_runtime::{CellsFormulaState, CellsSheetState};
use crate::clock::MonotonicInstant;
use crate::edit_session::{EditSession, EditSessionStateExt, apply_edit_session_key_down};
use crate::host_view_preview::{
    HostViewPreviewApp, InteractiveHostViewModel, render_interactive_host_view,
};
use crate::ir::{IrProgram, NodeId as IrNodeId, RetainedNodeKey, ViewSiteId};
use crate::lower::{CellsEditingView, lower_cells_display_typed_program};
use crate::metrics::{CellsMetricsReport, LatencySummary};
use crate::parse::{StaticSpannedExpression, parse_static_expressions};
use crate::text_input::KEYDOWN_TEXT_SEPARATOR;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    NodeId, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode, UiNodeKind,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CellsProgram {
    pub ir: IrProgram,
    pub title: &'static str,
    pub display_title: String,
    pub row_count: u32,
    pub col_count: u32,
    pub column_headers: Vec<String>,
    pub(crate) default_formulas: BTreeMap<(u32, u32), LoweredCellsFormula>,
    pub baseline_state: CellsFormulaState,
}

impl CellsProgram {
    pub const OVERRIDES_LIST_HOLD_NODE: IrNodeId = IrNodeId(19_005);
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
    app: HostViewPreviewApp,
}

impl InteractiveHostViewModel for CellsPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        self.dispatch_host_ui_events(batch)
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        self.dispatch_host_ui_facts(batch)
    }

    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        &mut self.app
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        self.refresh_host_view();
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

impl CellsPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        Ok(Self::from_program(try_lower_cells_program(source)?))
    }

    pub fn from_program(program: CellsProgram) -> Self {
        let sheet = CellsSheetState::new_lowered(
            program.default_formulas.clone(),
            program.baseline_state.clone(),
        );
        let app =
            HostViewPreviewApp::new(program.materialize_host_view(&sheet, None), BTreeMap::new());
        Self {
            sheet,
            program,
            editing: None,
            pending_click: None,
            app,
        }
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        self.dispatch_host_ui_events(batch)
    }

    /// Enable persistence on this preview.
    /// TODO: Wire persistence collection into cells event dispatch.
    pub fn enable_persistence(&mut self) {
        // Persistence metadata exists in program.ir.persistence
        // Collection and commit needs to happen after dispatch_host_ui_events
    }

    pub fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        self.dispatch_host_ui_facts(batch)
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.refresh_host_view();
        self.app.preview_text()
    }

    #[cfg(test)]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn retained_nodes(&self) -> &BTreeMap<RetainedNodeKey, NodeId> {
        self.app.retained_nodes()
    }

    fn editing_view(&self) -> Option<CellsEditingView<'_>> {
        self.editing.as_ref().map(|editing| CellsEditingView {
            row: editing.target.0,
            column: editing.target.1,
            draft: &editing.input.draft,
            focus_hint: editing.input.focus_hint,
        })
    }

    fn refresh_host_view(&mut self) {
        let host_view = self
            .program
            .materialize_host_view(&self.sheet, self.editing_view());
        self.app.set_host_view(host_view);
    }

    fn dispatch_host_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let mut changed = false;
        for event in batch.events {
            let Some(binding) = self.app.event_binding_for_port(event.target) else {
                continue;
            };
            changed |= match (binding.source_port, binding.mapped_item_identity) {
                (CellsProgram::HEADING_CLICK_PORT, None) => self.apply_event(
                    CellsUiAction::HeadingClick,
                    event.kind,
                    event.payload.as_deref(),
                ),
                (source_port, Some(identity))
                    if source_port.0 >= 100_000 && source_port.0 < 200_000 =>
                {
                    decode_cell_identity(identity).is_some_and(|(row, column)| {
                        self.apply_event(
                            CellsUiAction::CellClick { row, column },
                            event.kind,
                            event.payload.as_deref(),
                        )
                    })
                }
                (source_port, Some(identity))
                    if source_port.0 >= 10_000 && source_port.0 < 100_000 =>
                {
                    decode_cell_identity(identity).is_some_and(|(row, column)| {
                        self.apply_event(
                            CellsUiAction::CellDoubleClick { row, column },
                            event.kind,
                            event.payload.as_deref(),
                        )
                    })
                }
                (source_port, Some(identity)) if source_port.0 >= 200_000 => {
                    decode_cell_identity(identity).is_some_and(|(row, column)| {
                        match source_port.0 % 10 {
                            1 => self.apply_event(
                                CellsUiAction::CellEditInput { row, column },
                                event.kind,
                                event.payload.as_deref(),
                            ),
                            2 => self.apply_event(
                                CellsUiAction::CellEditKeyDown { row, column },
                                event.kind,
                                event.payload.as_deref(),
                            ),
                            3 => self.apply_event(
                                CellsUiAction::CellEditBlur { row, column },
                                event.kind,
                                event.payload.as_deref(),
                            ),
                            _ => false,
                        }
                    })
                }
                _ => false,
            };
        }
        if changed {
            self.refresh_host_view();
        }
        changed
    }

    fn dispatch_host_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        let mut changed = false;
        for fact in batch.facts {
            let Some(key) = self.app.retained_key_for_node(fact.id) else {
                continue;
            };
            changed |= match (key.view_site, key.mapped_item_identity) {
                (ViewSiteId(433), Some(identity)) => {
                    decode_cell_identity(identity).is_some_and(|(row, column)| {
                        self.apply_fact(CellsFactTarget::CellEditInput { row, column }, fact.kind)
                    })
                }
                _ => false,
            };
        }
        if changed {
            self.refresh_host_view();
        }
        changed
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
}

pub(crate) fn try_lower_cells_program(source: &str) -> Result<CellsProgram, String> {
    let expressions = parse_static_expressions(source)
        .map_err(|error| format!("cells subset requires parseable source: {error}"))?;
    try_lower_cells_program_from_expressions(&expressions)
}

pub(crate) fn try_lower_cells_program_from_expressions(
    expressions: &[StaticSpannedExpression],
) -> Result<CellsProgram, String> {
    lower_cells_display_typed_program(expressions)
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
    let (root, _) = <CellsPreview as InteractiveHostViewModel>::render_snapshot(preview);
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

    let startup_started = MonotonicInstant::now();
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
        let before_keys = preview.app.retained_nodes().clone();
        let (before_ids, before_texts) = snapshot_ids_and_texts(&mut preview);
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

        let started = MonotonicInstant::now();
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
        let created = after_ids.difference(&before_ids).count();
        let deleted = before_ids.difference(&after_ids).count();
        retained_node_creations_per_edit_max = retained_node_creations_per_edit_max.max(created);
        retained_node_deletions_per_edit_max = retained_node_deletions_per_edit_max.max(deleted);
        dirty_sink_or_export_count_per_edit_max = dirty_sink_or_export_count_per_edit_max
            .max(changed_text_count(&before_texts, &after_texts));
        function_instance_reuse_hit_rate_min = function_instance_reuse_hit_rate_min.min(
            function_instance_reuse_rate(&before_keys, preview.app.retained_nodes()),
        );
        recreated_mapped_scope_count_max = recreated_mapped_scope_count_max.max(
            recreated_mapped_scope_count(&before_keys, preview.app.retained_nodes()),
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

#[cfg(test)]
fn cell_identity(row: u32, column: u32) -> u64 {
    (row as u64) * 1_000 + column as u64
}

fn decode_cell_identity(identity: u64) -> Option<(u32, u32)> {
    let row = (identity / 1_000) as u32;
    let column = (identity % 1_000) as u32;
    if row == 0 || column == 0 {
        None
    } else {
        Some((row, column))
    }
}

pub fn render_cells_preview(preview: CellsPreview) -> impl Element {
    render_interactive_host_view(preview)
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
            function_instance: Some(crate::ir::FunctionInstanceId(11)),
            mapped_item_identity: Some(50),
        };
        let unaffected_cell_key = RetainedNodeKey {
            view_site: ViewSiteId(434),
            function_instance: Some(crate::ir::FunctionInstanceId(11)),
            mapped_item_identity: Some(cell_identity(50, 5)),
        };

        let (initial_ids, initial_texts) = snapshot_ids_and_texts(&mut preview);
        let initial_row_id = *preview
            .retained_nodes()
            .get(&unaffected_row_key)
            .expect("unaffected row retained");
        let initial_cell_id = *preview
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
            .retained_nodes()
            .get(&unaffected_row_key)
            .expect("unaffected row retained after edits");
        let final_cell_id = *preview
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
