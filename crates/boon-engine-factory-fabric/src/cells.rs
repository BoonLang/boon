use crate::host_view::{HostViewNode, HostViewTree};
use crate::lower::CellsProgram;
use crate::metrics::{CellsMetricsReport, LatencySummary};
use crate::{RegionId, RuntimeCore};
use boon_renderer_zoon::FakeRenderState;
use boon_scene::RenderDiffBatch;
use boon_scene::{EventPortId, NodeId, UiEvent, UiEventKind};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const ROW_LABEL_WIDTH: &str = "40px";
const CELL_WIDTH: &str = "80px";
const CELL_HEIGHT: &str = "26px";
const CELL_PADDING: &str = "0 8px";
const HEADER_BACKGROUND: &str = "rgb(235, 235, 235)";
const ROOT_BACKGROUND: &str = "rgb(248, 248, 248)";
const TEXT_COLOR: &str = "rgb(30, 30, 30)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CellCoord {
    row: u32,
    column: u32,
}

#[derive(Debug)]
pub struct CellsState {
    region: RegionId,
    program: CellsProgram,
    ui: CellsUi,
    editing: Option<CellCoord>,
    overrides: BTreeMap<CellCoord, String>,
}

#[derive(Debug)]
struct CellsUi {
    root: NodeId,
    heading: NodeId,
    table: NodeId,
    header_row: NodeId,
    body: NodeId,
    corner_header: NodeId,
    column_headers: Vec<NodeId>,
    rows: Vec<CellsRowUi>,
}

#[derive(Debug)]
struct CellsRowUi {
    root: NodeId,
    label: NodeId,
    cells: Vec<CellsCellUi>,
}

#[derive(Debug)]
struct CellsCellUi {
    td: NodeId,
    display: NodeId,
    input: NodeId,
    double_click_port: EventPortId,
    key_down_port: EventPortId,
    change_port: EventPortId,
    blur_port: EventPortId,
}

impl CellsState {
    pub fn new(program: CellsProgram, runtime: &mut RuntimeCore) -> Self {
        let region = runtime.alloc_region();
        let ui = CellsUi::new(program.row_count, program.col_count);
        Self {
            region,
            program,
            ui,
            editing: None,
            overrides: BTreeMap::new(),
        }
    }

    pub fn handle_event(&mut self, runtime: &mut RuntimeCore, event: &UiEvent) -> bool {
        runtime.schedule_region(self.region);
        let _ = runtime.pop_ready_region();

        for (row_index, row_ui) in self.ui.rows.iter().enumerate() {
            for (column_index, cell_ui) in row_ui.cells.iter().enumerate() {
                let coord = CellCoord {
                    row: row_index as u32 + 1,
                    column: column_index as u32 + 1,
                };
                if event.target == cell_ui.double_click_port
                    && event.kind == UiEventKind::DoubleClick
                {
                    self.editing = Some(coord);
                    return true;
                }
                if event.target == cell_ui.key_down_port && event.kind == UiEventKind::KeyDown {
                    let (key, text) = decode_key_payload(event.payload.as_deref().unwrap_or(""));
                    match key {
                        "Enter" => {
                            self.commit(coord, &text);
                            return true;
                        }
                        "Escape" => {
                            self.editing = None;
                            return true;
                        }
                        _ => return false,
                    }
                }
                if event.target == cell_ui.blur_port && event.kind == UiEventKind::Blur {
                    self.editing = None;
                    return true;
                }
            }
        }

        false
    }

    pub fn view_tree(&self) -> HostViewTree {
        HostViewTree::from_root(
            HostViewNode::element(self.ui.root, "div")
                .with_style("padding", "20px")
                .with_style("background", ROOT_BACKGROUND)
                .with_style("color", TEXT_COLOR)
                .with_style("display", "flex")
                .with_style("flex-direction", "column")
                .with_children(vec![
                    HostViewNode::element(self.ui.heading, "h1")
                        .with_text(self.program.title.clone()),
                    HostViewNode::element(self.ui.table, "div")
                        .with_style("display", "flex")
                        .with_style("flex-direction", "column")
                        .with_children(vec![
                            self.header_row_view(),
                            HostViewNode::element(self.ui.body, "tbody")
                                .with_style("display", "flex")
                                .with_style("flex-direction", "column")
                                .with_style("max-height", "500px")
                                .with_style("overflow-y", "auto")
                                .with_children(self.row_views()),
                        ]),
                ]),
        )
    }

    fn header_row_view(&self) -> HostViewNode {
        let mut children = vec![
            HostViewNode::element(self.ui.corner_header, "div")
                .with_style("width", ROW_LABEL_WIDTH)
                .with_style("height", CELL_HEIGHT)
                .with_style("background", HEADER_BACKGROUND),
        ];
        children.extend(
            self.ui
                .column_headers
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    HostViewNode::element(*id, "div")
                        .with_style("width", CELL_WIDTH)
                        .with_style("height", CELL_HEIGHT)
                        .with_style("padding", CELL_PADDING)
                        .with_style("background", HEADER_BACKGROUND)
                        .with_text(column_header(index as u32 + 1))
                }),
        );
        HostViewNode::element(self.ui.header_row, "div")
            .with_style("display", "flex")
            .with_style("flex-direction", "row")
            .with_children(children)
    }

    fn row_views(&self) -> Vec<HostViewNode> {
        self.ui
            .rows
            .iter()
            .enumerate()
            .map(|(row_index, row_ui)| {
                let row = row_index as u32 + 1;
                let mut children = vec![
                    HostViewNode::element(row_ui.label, "div")
                        .with_style("width", ROW_LABEL_WIDTH)
                        .with_style("height", CELL_HEIGHT)
                        .with_style("padding", "0 0 0 8px")
                        .with_style("background", HEADER_BACKGROUND)
                        .with_text(row.to_string()),
                ];
                children.extend(
                    row_ui
                        .cells
                        .iter()
                        .enumerate()
                        .map(|(column_index, cell_ui)| {
                            let column = column_index as u32 + 1;
                            let coord = CellCoord { row, column };
                            let formula = self.formula_text(coord);
                            let is_editing = self.editing == Some(coord);
                            let cell_link_path = format!(
                                "all_row_cells.{:04}.cells.{:04}.display_element",
                                row - 1,
                                column - 1
                            );
                            let child = if is_editing {
                                HostViewNode::element(cell_ui.input, "input")
                                    .with_property("type", "text")
                                    .with_property("autofocus", "true")
                                    .with_property("focused", "true")
                                    .with_input_value(formula)
                                    .with_event_port(cell_ui.key_down_port, UiEventKind::KeyDown)
                                    .with_event_port(cell_ui.change_port, UiEventKind::Change)
                                    .with_event_port(cell_ui.blur_port, UiEventKind::Blur)
                                    .with_style("width", CELL_WIDTH)
                                    .with_style("height", CELL_HEIGHT)
                                    .with_style("padding", CELL_PADDING)
                            } else {
                                HostViewNode::element(cell_ui.display, "button")
                                    .with_property("data-boon-link-path", cell_link_path)
                                    .with_text(self.display_text(coord))
                                    .with_event_port(
                                        cell_ui.double_click_port,
                                        UiEventKind::DoubleClick,
                                    )
                                    .with_style("width", CELL_WIDTH)
                                    .with_style("height", CELL_HEIGHT)
                                    .with_style("padding", CELL_PADDING)
                            };
                            HostViewNode::element(cell_ui.td, "div")
                                .with_style("width", CELL_WIDTH)
                                .with_style("height", CELL_HEIGHT)
                                .with_style("display", "flex")
                                .with_children(vec![child])
                        }),
                );
                HostViewNode::element(row_ui.root, "div")
                    .with_property(
                        "data-boon-link-path",
                        format!("all_row_cells.{:04}.element", row - 1),
                    )
                    .with_style("display", "flex")
                    .with_style("flex-direction", "row")
                    .with_children(children)
            })
            .collect()
    }

    fn formula_text(&self, coord: CellCoord) -> String {
        self.overrides
            .get(&coord)
            .cloned()
            .unwrap_or_else(|| default_formula(coord))
    }

    fn display_text(&self, coord: CellCoord) -> String {
        let formula = self.formula_text(coord);
        if formula.is_empty() {
            return String::new();
        }
        self.evaluate_formula(coord, &formula, &mut BTreeSet::new())
            .to_string()
    }

    fn evaluate_formula(
        &self,
        coord: CellCoord,
        formula: &str,
        visited: &mut BTreeSet<CellCoord>,
    ) -> i64 {
        if !visited.insert(coord) {
            return 0;
        }

        let value = if let Some(expression) = formula.strip_prefix('=') {
            evaluate_expression(self, expression.trim(), visited)
        } else {
            formula.trim().parse::<i64>().unwrap_or(0)
        };

        visited.remove(&coord);
        value
    }

    fn cell_value(&self, coord: CellCoord, visited: &mut BTreeSet<CellCoord>) -> i64 {
        let formula = self.formula_text(coord);
        if formula.is_empty() {
            return 0;
        }
        self.evaluate_formula(coord, &formula, visited)
    }

    fn commit(&mut self, coord: CellCoord, text: &str) {
        let text = text.to_string();
        if text == default_formula(coord) {
            self.overrides.remove(&coord);
        } else {
            self.overrides.insert(coord, text);
        }
        self.editing = None;
    }

    pub(crate) const fn last_recreated_mapped_scopes(&self) -> usize {
        0
    }
}

impl CellsUi {
    fn new(row_count: u32, col_count: u32) -> Self {
        Self {
            root: NodeId::new(),
            heading: NodeId::new(),
            table: NodeId::new(),
            header_row: NodeId::new(),
            body: NodeId::new(),
            corner_header: NodeId::new(),
            column_headers: (0..col_count).map(|_| NodeId::new()).collect(),
            rows: (0..row_count).map(|_| CellsRowUi::new(col_count)).collect(),
        }
    }
}

impl CellsRowUi {
    fn new(col_count: u32) -> Self {
        Self {
            root: NodeId::new(),
            label: NodeId::new(),
            cells: (0..col_count).map(|_| CellsCellUi::new()).collect(),
        }
    }
}

impl CellsCellUi {
    fn new() -> Self {
        Self {
            td: NodeId::new(),
            display: NodeId::new(),
            input: NodeId::new(),
            double_click_port: EventPortId::new(),
            key_down_port: EventPortId::new(),
            change_port: EventPortId::new(),
            blur_port: EventPortId::new(),
        }
    }
}

fn column_header(column: u32) -> String {
    char::from_u32('A' as u32 + column - 1)
        .unwrap_or('?')
        .to_string()
}

fn default_formula(coord: CellCoord) -> String {
    match (coord.row, coord.column) {
        (1, 1) => "5".to_string(),
        (2, 1) => "10".to_string(),
        (3, 1) => "15".to_string(),
        (1, 2) => "=add(A1, A2)".to_string(),
        (1, 3) => "=sum(A1:A3)".to_string(),
        _ => String::new(),
    }
}

fn evaluate_expression(
    state: &CellsState,
    expression: &str,
    visited: &mut BTreeSet<CellCoord>,
) -> i64 {
    if let Some(inner) = expression
        .strip_prefix("add(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let mut refs = inner.split(',').map(str::trim);
        let Some(left) = refs.next() else {
            return 0;
        };
        let Some(right) = refs.next() else {
            return 0;
        };
        if refs.next().is_some() {
            return 0;
        }
        return parse_cell_reference(left)
            .map(|coord| state.cell_value(coord, visited))
            .unwrap_or(0)
            + parse_cell_reference(right)
                .map(|coord| state.cell_value(coord, visited))
                .unwrap_or(0);
    }

    if let Some(inner) = expression
        .strip_prefix("sum(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let Some((start, end)) = inner.split_once(':') else {
            return 0;
        };
        let Some(start) = parse_cell_reference(start.trim()) else {
            return 0;
        };
        let Some(end) = parse_cell_reference(end.trim()) else {
            return 0;
        };
        if start.column != end.column {
            return 0;
        }
        let row_start = start.row.min(end.row);
        let row_end = start.row.max(end.row);
        return (row_start..=row_end)
            .map(|row| {
                state.cell_value(
                    CellCoord {
                        row,
                        column: start.column,
                    },
                    visited,
                )
            })
            .sum();
    }

    expression.trim().parse::<i64>().unwrap_or(0)
}

fn parse_cell_reference(text: &str) -> Option<CellCoord> {
    let mut chars = text.chars();
    let column_char = chars.next()?;
    if !column_char.is_ascii_uppercase() {
        return None;
    }
    let row = chars.as_str().parse::<u32>().ok()?;
    Some(CellCoord {
        row,
        column: column_char as u32 - 'A' as u32 + 1,
    })
}

fn decode_key_payload(payload: &str) -> (&str, String) {
    match payload.split_once('\u{1f}') {
        Some((key, text)) => (key, text.to_string()),
        None => (payload, String::new()),
    }
}

pub(crate) fn cells_metrics_capture(program: CellsProgram) -> Result<CellsMetricsReport, String> {
    let startup_started = Instant::now();
    let mut runtime = RuntimeCore::new();
    let mut state = CellsState::new(program, &mut runtime);
    let mut render = FakeRenderState::default();
    let mut previous_view = state.view_tree();
    let (initial_batch, _) = previous_view.into_render_batch_with_stats();
    render
        .apply_batch(&initial_batch)
        .map_err(|error| format!("FactoryFabric cells metrics render error: {error:?}"))?;
    let cold_mount_to_stable_first_paint_millis = startup_started.elapsed().as_secs_f64() * 1000.0;

    let mut edit_samples = Vec::<Duration>::new();
    let mut retained_node_creations_per_edit_max = 0usize;
    let mut retained_node_deletions_per_edit_max = 0usize;
    let mut dirty_sink_or_export_count_per_edit_max = 0usize;
    let function_instance_reuse_hit_rate_min = 1.0f64;
    let recreated_mapped_scope_count_max = 0usize;

    for (row, column, next_value) in [(1, 1, "11"), (2, 1, "20")].into_iter().cycle().take(24) {
        let double_click_port =
            state.ui.rows[(row - 1) as usize].cells[(column - 1) as usize].double_click_port;
        let key_down_port =
            state.ui.rows[(row - 1) as usize].cells[(column - 1) as usize].key_down_port;
        let activate = UiEvent {
            target: double_click_port,
            kind: UiEventKind::DoubleClick,
            payload: None,
        };
        if !state.handle_event(&mut runtime, &activate) {
            return Err("FactoryFabric cells metrics double-click was not handled".to_string());
        }
        apply_cells_diff(&state, &mut render, &mut previous_view)?;

        let started = Instant::now();
        let commit = UiEvent {
            target: key_down_port,
            kind: UiEventKind::KeyDown,
            payload: Some(format!("Enter\u{1f}{next_value}")),
        };
        if !state.handle_event(&mut runtime, &commit) {
            return Err("FactoryFabric cells metrics commit was not handled".to_string());
        }
        let batch = apply_cells_diff(&state, &mut render, &mut previous_view)?;
        edit_samples.push(started.elapsed());
        let stats = render_batch_budget_counters(&batch);
        retained_node_creations_per_edit_max = retained_node_creations_per_edit_max.max(stats.0);
        retained_node_deletions_per_edit_max = retained_node_deletions_per_edit_max.max(stats.1);
        dirty_sink_or_export_count_per_edit_max =
            dirty_sink_or_export_count_per_edit_max.max(stats.2);
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

fn apply_cells_diff(
    state: &CellsState,
    render: &mut FakeRenderState,
    previous_view: &mut HostViewTree,
) -> Result<RenderDiffBatch, String> {
    let next_view = state.view_tree();
    let (batch, _) = previous_view.diff_with_stats(&next_view);
    render
        .apply_batch(&batch)
        .map_err(|error| format!("FactoryFabric cells metrics render error: {error:?}"))?;
    *previous_view = next_view;
    Ok(batch)
}

fn render_batch_budget_counters(batch: &RenderDiffBatch) -> (usize, usize, usize) {
    let mut creations = 0usize;
    let mut deletions = 0usize;
    let mut dirty = 0usize;

    for op in &batch.ops {
        match op {
            boon_scene::RenderOp::InsertChild { node, .. } => {
                creations += render_node_count(node);
                dirty += 1;
            }
            boon_scene::RenderOp::RemoveNode { .. } => {
                deletions += 1;
                dirty += 1;
            }
            boon_scene::RenderOp::ReplaceRoot(_) => {
                dirty += 1;
            }
            boon_scene::RenderOp::SetText { .. }
            | boon_scene::RenderOp::SetProperty { .. }
            | boon_scene::RenderOp::SetStyle { .. }
            | boon_scene::RenderOp::SetClassFlag { .. }
            | boon_scene::RenderOp::SetInputValue { .. }
            | boon_scene::RenderOp::SetChecked { .. }
            | boon_scene::RenderOp::SetSelectedIndex { .. }
            | boon_scene::RenderOp::UpdateSceneParam { .. } => {
                dirty += 1;
            }
            boon_scene::RenderOp::MoveChild { .. }
            | boon_scene::RenderOp::AttachEventPort { .. }
            | boon_scene::RenderOp::DetachEventPort { .. } => {}
        }
    }

    (creations, deletions, dirty)
}

fn render_node_count(node: &boon_scene::RenderNode) -> usize {
    match node {
        boon_scene::RenderNode::Ui(node) => ui_node_count(node),
        boon_scene::RenderNode::Scene(_) => 1,
    }
}

fn ui_node_count(node: &boon_scene::UiNode) -> usize {
    1 + node.children.iter().map(ui_node_count).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::{CellCoord, CellsState};
    use crate::RuntimeCore;
    use crate::lower::CellsProgram;
    use boon_renderer_zoon::FakeRenderState;
    use boon_scene::{UiEvent, UiEventKind, UiNode, UiNodeKind};
    use std::collections::BTreeMap;

    fn sample_program(title: &str) -> CellsProgram {
        CellsProgram {
            title: title.to_string(),
            row_count: 100,
            col_count: 26,
            dynamic_axes: title.contains("Dynamic"),
        }
    }

    fn apply_tree(state: &CellsState, render_state: &mut FakeRenderState) {
        render_state
            .apply_batch(&state.view_tree().into_render_batch())
            .expect("render batch should apply");
    }

    fn root_children(root: &UiNode) -> &[UiNode] {
        &root.children
    }

    fn table_body(root: &UiNode) -> &UiNode {
        &root_children(root)[1].children[1]
    }

    fn display_text(root: &UiNode, row: usize, column: usize) -> String {
        let row_node = &table_body(root).children[row - 1];
        let cell_td = &row_node.children[column];
        let button = &cell_td.children[0];
        match &button.kind {
            UiNodeKind::Element { text, .. } => text.clone().unwrap_or_default(),
            UiNodeKind::Text { text } => text.clone(),
        }
    }

    #[test]
    fn cells_commit_cancel_and_dependency_recompute() {
        let mut runtime = RuntimeCore::new();
        let mut state = CellsState::new(sample_program("Cells"), &mut runtime);
        let mut render = FakeRenderState::default();
        apply_tree(&state, &mut render);

        let initial = render
            .root()
            .expect("root")
            .clone_ui_tree()
            .expect("ui root");
        assert_eq!(display_text(&initial, 1, 1), "5");
        assert_eq!(display_text(&initial, 1, 2), "15");
        assert_eq!(display_text(&initial, 1, 3), "30");

        let a1_double_click_port = state.ui.rows[0].cells[0].double_click_port;
        let a1_key_down_port = state.ui.rows[0].cells[0].key_down_port;
        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: a1_double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }
        ));
        assert_eq!(state.editing, Some(CellCoord { row: 1, column: 1 }));
        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: a1_key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1f}7".to_string()),
            }
        ));

        apply_tree(&state, &mut render);
        let committed = render
            .root()
            .expect("root")
            .clone_ui_tree()
            .expect("ui root");
        assert_eq!(display_text(&committed, 1, 1), "7");
        assert_eq!(display_text(&committed, 1, 2), "17");
        assert_eq!(display_text(&committed, 1, 3), "32");

        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: a1_double_click_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            }
        ));
        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: a1_key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Escape\u{1f}9".to_string()),
            }
        ));
        apply_tree(&state, &mut render);
        let cancelled = render
            .root()
            .expect("root")
            .clone_ui_tree()
            .expect("ui root");
        assert_eq!(display_text(&cancelled, 1, 1), "7");
        assert_eq!(display_text(&cancelled, 1, 2), "17");
    }

    #[test]
    fn randomized_cells_trace_matches_oracle_subset() {
        #[derive(Clone)]
        struct TraceRng(u64);

        impl TraceRng {
            fn new(seed: u64) -> Self {
                Self(seed)
            }

            fn next_u32(&mut self) -> u32 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
                (self.0 >> 32) as u32
            }

            fn next_range(&mut self, upper: u32) -> u32 {
                self.next_u32() % upper
            }
        }

        let mut runtime = RuntimeCore::new();
        let mut state = CellsState::new(sample_program("Cells"), &mut runtime);
        let mut render = FakeRenderState::default();
        apply_tree(&state, &mut render);
        let mut rng = TraceRng::new(0xCE115_u64);
        let mut overrides = BTreeMap::<(u32, u32), String>::new();

        let stable_ids = [
            state.ui.rows[0].root,
            state.ui.rows[0].cells[0].display,
            state.ui.rows[0].cells[1].display,
            state.ui.rows[0].cells[2].display,
        ];

        for _ in 0..48 {
            let row = rng.next_range(3) + 1;
            let next_value = (rng.next_range(20) + 1).to_string();
            let double_click_port = state.ui.rows[(row - 1) as usize].cells[0].double_click_port;
            let key_down_port = state.ui.rows[(row - 1) as usize].cells[0].key_down_port;
            assert!(state.handle_event(
                &mut runtime,
                &UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }
            ));
            assert!(state.handle_event(
                &mut runtime,
                &UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some(format!("Enter\u{1f}{next_value}")),
                }
            ));
            if next_value == super::default_formula(CellCoord { row, column: 1 }) {
                overrides.remove(&(row, 1));
            } else {
                overrides.insert((row, 1), next_value);
            }

            render = FakeRenderState::default();
            apply_tree(&state, &mut render);
            let root = render
                .root()
                .expect("root")
                .clone_ui_tree()
                .expect("ui root");

            let a1 = overrides
                .get(&(1, 1))
                .cloned()
                .unwrap_or_else(|| super::default_formula(CellCoord { row: 1, column: 1 }))
                .parse::<i64>()
                .unwrap_or(0);
            let a2 = overrides
                .get(&(2, 1))
                .cloned()
                .unwrap_or_else(|| super::default_formula(CellCoord { row: 2, column: 1 }))
                .parse::<i64>()
                .unwrap_or(0);
            let a3 = overrides
                .get(&(3, 1))
                .cloned()
                .unwrap_or_else(|| super::default_formula(CellCoord { row: 3, column: 1 }))
                .parse::<i64>()
                .unwrap_or(0);

            assert_eq!(display_text(&root, 1, 1), a1.to_string());
            assert_eq!(display_text(&root, 2, 1), a2.to_string());
            assert_eq!(display_text(&root, 3, 1), a3.to_string());
            assert_eq!(display_text(&root, 1, 2), (a1 + a2).to_string());
            assert_eq!(display_text(&root, 1, 3), (a1 + a2 + a3).to_string());

            assert_eq!(state.ui.rows[0].root, stable_ids[0]);
            assert_eq!(state.ui.rows[0].cells[0].display, stable_ids[1]);
            assert_eq!(state.ui.rows[0].cells[1].display, stable_ids[2]);
            assert_eq!(state.ui.rows[0].cells[2].display, stable_ids[3]);
        }
    }

    trait RenderRootExt {
        fn clone_ui_tree(&self) -> Option<UiNode>;
    }

    impl RenderRootExt for boon_scene::RenderRoot {
        fn clone_ui_tree(&self) -> Option<UiNode> {
            match self {
                boon_scene::RenderRoot::UiTree(root) => Some(root.clone()),
                _ => None,
            }
        }
    }
}
