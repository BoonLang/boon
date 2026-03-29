#[cfg(test)]
use crate::cells_lower::lower_cells_formula;
use crate::cells_lower::{
    CellsFormula, CellsFormulaExpr, LoweredCellsFormula, parse_lowered_cells_formula,
};
#[cfg(test)]
use crate::ir::{IrNode, IrNodeKind, IrProgram, NodeId, SinkPortId};
#[cfg(test)]
use crate::ir_executor::IrExecutor;
use crate::list_semantics::get_one_based;
use crate::runtime::{
    AffectedComponentsTopoScratch, DependencyClosureScratch, RetainedMemberProgram,
    RetainedMemberRuntime, RetainedNumberFormula, affected_components_topo_order_into_with_scratch,
    build_pair_number_retained_member_program_from_descriptor,
    dependency_closure_into_with_scratch, patch_retained_input,
    rebuild_retained_runtime_from_owned_program_results, recompute_number_retained_members,
    replace_retained_member_program, retained_number_formula_input_value,
};
use boon::platform::browser::kernel::KernelValue;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct CellsFormulaState {
    pub(crate) formula_cells: BTreeMap<(u32, u32), LoweredCellsFormula>,
    pub(crate) formula_dependencies: BTreeMap<(u32, u32), Vec<(u32, u32)>>,
    pub(crate) formula_dependents: BTreeMap<(u32, u32), Vec<(u32, u32)>>,
    pub(crate) computed_values: BTreeMap<(u32, u32), i64>,
    ir_runtime: Option<CellsIrRuntime>,
    formula_closure_scratch: DependencyClosureScratch<(u32, u32)>,
    formula_closure_result_scratch: Vec<(u32, u32)>,
    affected_components_topo_scratch: AffectedComponentsTopoScratch<(u32, u32)>,
    affected_components_result_scratch: Vec<Vec<(u32, u32)>>,
    affected_formula_cells_scratch: Vec<(u32, u32)>,
    ordered_cells_scratch: Vec<(u32, u32)>,
    runtime_roots_scratch: BTreeSet<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CellsSheetState {
    baseline_formulas: BTreeMap<(u32, u32), LoweredCellsFormula>,
    override_entries: Vec<CellsOverrideEntry>,
    override_entry_indexes: BTreeMap<(u32, u32), Vec<usize>>,
    resolved_formulas: BTreeMap<(u32, u32), LoweredCellsFormula>,
    formula_state: CellsFormulaState,
}

#[derive(Debug, Clone, PartialEq)]
struct CellsOverrideEntry {
    row: u32,
    column: u32,
    formula: LoweredCellsFormula,
}

#[derive(Debug)]
struct CellsIrRuntime {
    retained: RetainedMemberRuntime<(u32, u32)>,
}

enum RuntimeInputUpdate {
    Applied,
    Unchanged,
    Failed,
}

impl Clone for CellsFormulaState {
    fn clone(&self) -> Self {
        Self::from_lowered_formulas(
            self.formula_cells
                .iter()
                .map(|(coords, formula)| (*coords, formula.clone())),
        )
    }
}

impl PartialEq for CellsFormulaState {
    fn eq(&self, other: &Self) -> bool {
        self.formula_cells == other.formula_cells
            && self.formula_dependencies == other.formula_dependencies
            && self.formula_dependents == other.formula_dependents
            && self.computed_values == other.computed_values
    }
}

impl Eq for CellsFormulaState {}

impl CellsFormulaState {
    pub(crate) fn from_lowered_formulas(
        formulas: impl IntoIterator<Item = ((u32, u32), LoweredCellsFormula)>,
    ) -> Self {
        let formula_cells = formulas
            .into_iter()
            .filter(|(_, formula)| !matches!(formula.formula().expr, CellsFormulaExpr::Empty))
            .collect::<BTreeMap<_, _>>();
        let (formula_dependencies, formula_dependents) = build_formula_graph(&formula_cells);
        let mut state = Self {
            formula_cells,
            formula_dependencies,
            formula_dependents,
            computed_values: BTreeMap::new(),
            ir_runtime: None,
            formula_closure_scratch: DependencyClosureScratch::default(),
            formula_closure_result_scratch: Vec::new(),
            affected_components_topo_scratch: AffectedComponentsTopoScratch::default(),
            affected_components_result_scratch: Vec::new(),
            affected_formula_cells_scratch: Vec::new(),
            ordered_cells_scratch: Vec::new(),
            runtime_roots_scratch: BTreeSet::new(),
        };
        state.rebuild_ir_runtime();
        state
    }

    #[cfg(test)]
    pub fn from_formulas(formulas: impl IntoIterator<Item = ((u32, u32), CellsFormula)>) -> Self {
        Self::from_lowered_formulas(
            formulas
                .into_iter()
                .map(|(cell, formula)| (cell, lower_cells_formula(formula))),
        )
    }

    fn set_lowered_formula(&mut self, cell: (u32, u32), lowered_formula: LoweredCellsFormula) {
        let old_formula = self.formula_cells.get(&cell).cloned();
        let old_formula_was_runtime_input = old_formula
            .as_ref()
            .and_then(|formula| {
                retained_number_formula_input_value(formula.retained_number_formula())
            })
            .is_some();
        let new_formula_is_runtime_input =
            retained_number_formula_input_value(lowered_formula.retained_number_formula())
                .is_some();
        let old_dependencies = self
            .formula_dependencies
            .get(&cell)
            .cloned()
            .unwrap_or_default();

        if matches!(lowered_formula.formula().expr, CellsFormulaExpr::Empty) {
            self.formula_cells.remove(&cell);
        } else {
            self.formula_cells.insert(cell, lowered_formula);
        }

        self.remove_formula_dependencies(cell, &old_dependencies);
        if let Some(formula) = self.formula_cells.get(&cell) {
            let new_dependencies = formula.formula().dependencies();
            self.insert_formula_dependencies(cell, &new_dependencies);
        } else {
            self.formula_dependencies.remove(&cell);
        }

        let mut affected = std::mem::take(&mut self.formula_closure_result_scratch);
        self.formula_closure_into(cell, &mut affected);
        let can_patch_runtime_input = self
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.mirror_cell(cell))
            .is_some()
            && new_formula_is_runtime_input
            && old_dependencies.is_empty();

        if can_patch_runtime_input {
            match self.apply_runtime_input_update(cell) {
                RuntimeInputUpdate::Applied => {
                    if self.recompute_affected_runtime_closure(cell, &affected) {
                        self.formula_closure_result_scratch = affected;
                        return;
                    }
                }
                RuntimeInputUpdate::Unchanged => {
                    self.formula_closure_result_scratch = affected;
                    return;
                }
                RuntimeInputUpdate::Failed => {}
            }
        }

        if !self.formula_cells.contains_key(&cell) && old_formula_was_runtime_input {
            let _ = self.clear_runtime_input(cell);
        }

        if self.replace_cell_runtime(cell)
            && self.recompute_affected_runtime_closure(cell, &affected)
        {
            self.formula_closure_result_scratch = affected;
            return;
        }

        self.rebuild_ir_runtime();
        self.formula_closure_result_scratch = affected;
    }

    #[cfg(test)]
    fn set_formula(&mut self, cell: (u32, u32), formula: CellsFormula) {
        self.set_lowered_formula(cell, lower_cells_formula(formula));
    }

    fn remove_formula_dependencies(&mut self, cell: (u32, u32), dependencies: &[(u32, u32)]) {
        self.formula_dependencies.remove(&cell);
        for dependency in dependencies {
            let should_remove =
                if let Some(dependents) = self.formula_dependents.get_mut(dependency) {
                    dependents.retain(|dependent| *dependent != cell);
                    dependents.is_empty()
                } else {
                    false
                };
            if should_remove {
                self.formula_dependents.remove(dependency);
            }
        }
    }

    fn insert_formula_dependencies(&mut self, cell: (u32, u32), dependencies: &[(u32, u32)]) {
        self.formula_dependencies
            .insert(cell, dependencies.to_vec());
        for dependency in dependencies {
            let dependents = self.formula_dependents.entry(*dependency).or_default();
            if !dependents.contains(&cell) {
                dependents.push(cell);
            }
        }
    }

    fn formula_closure_into(&mut self, root: (u32, u32), output: &mut Vec<(u32, u32)>) {
        dependency_closure_into_with_scratch(
            &[root],
            &self.formula_dependents,
            &mut self.formula_closure_scratch,
            output,
        );
    }

    #[cfg(test)]
    fn recompute_affected_closure(
        &mut self,
        affected: &[(u32, u32)],
        values: &mut BTreeMap<(u32, u32), i64>,
    ) {
        let mut affected_formula_cells = std::mem::take(&mut self.affected_formula_cells_scratch);
        affected_formula_cells.clear();
        for cell in affected.iter().copied() {
            if self.formula_cells.contains_key(&cell) {
                affected_formula_cells.push(cell);
            } else {
                values.remove(&cell);
            }
        }

        let mut ordered_components = std::mem::take(&mut self.affected_components_result_scratch);
        self.affected_components_topo_order_into(&affected_formula_cells, &mut ordered_components);
        let mut ordered_cells = std::mem::take(&mut self.ordered_cells_scratch);
        ordered_cells.clear();
        ordered_cells.extend(ordered_components.iter().flatten().copied());
        if !ordered_cells.is_empty() {
            self.recompute_ordered_cells_with_ir(&ordered_cells, values);
        }
        self.affected_components_result_scratch = ordered_components;
        self.ordered_cells_scratch = ordered_cells;
        self.affected_formula_cells_scratch = affected_formula_cells;
    }

    fn rebuild_ir_runtime(&mut self) {
        let previous_runtime = self.ir_runtime.take();
        let Some((runtime, computed_values)) = build_cells_ir_runtime(self, previous_runtime)
        else {
            self.ir_runtime = None;
            self.computed_values = BTreeMap::new();
            return;
        };
        self.ir_runtime = Some(runtime);
        self.computed_values = computed_values;
    }

    fn replace_cell_runtime(&mut self, cell: (u32, u32)) -> bool {
        let Some(runtime) = self.ir_runtime.as_mut() else {
            return false;
        };
        let program = self
            .formula_cells
            .get(&cell)
            .and_then(|formula| build_cell_ir_runtime(cell, formula.retained_number_formula()));
        replace_retained_member_program(&mut runtime.retained, cell, program).is_ok()
    }

    fn affected_components_topo_order_into(
        &mut self,
        affected: &[(u32, u32)],
        output: &mut Vec<Vec<(u32, u32)>>,
    ) {
        affected_components_topo_order_into_with_scratch(
            affected,
            &self.formula_dependencies,
            |dependency| self.formula_cells.contains_key(&dependency),
            &mut self.affected_components_topo_scratch,
            output,
        );
    }

    #[cfg(test)]
    fn affected_components_topo_order(&mut self, affected: &[(u32, u32)]) -> Vec<Vec<(u32, u32)>> {
        let mut ordered_components = Vec::new();
        self.affected_components_topo_order_into(affected, &mut ordered_components);
        ordered_components
    }

    #[cfg(test)]
    fn component_is_cycle_core(&self, component: &[(u32, u32)]) -> bool {
        component.len() > 1
            || component.iter().any(|cell| {
                self.formula_dependencies
                    .get(cell)
                    .into_iter()
                    .flat_map(|dependencies| dependencies.iter())
                    .any(|dependency| dependency == cell)
            })
    }

    #[cfg(test)]
    fn recompute_ordered_cells_with_ir(
        &self,
        ordered_cells: &[(u32, u32)],
        values: &mut BTreeMap<(u32, u32), i64>,
    ) {
        let mut next_node_id = 1u32;
        let mut next_sink_port = 1u32;
        let mut nodes = Vec::new();
        let mut output_nodes = BTreeMap::<(u32, u32), NodeId>::new();
        let mut sink_ports = BTreeMap::<(u32, u32), SinkPortId>::new();
        let push_literal =
            |value: i64, nodes: &mut Vec<IrNode>, next_node_id: &mut u32| -> NodeId {
                let node_id = NodeId(*next_node_id);
                *next_node_id += 1;
                nodes.push(IrNode {
                    id: node_id,
                    source_expr: None,
                    kind: IrNodeKind::Literal(KernelValue::from(value as f64)),
                });
                node_id
            };

        for &cell in ordered_cells {
            let Some(formula) = self.formula_cells.get(&cell).cloned() else {
                continue;
            };
            let dependency_nodes = formula
                .formula()
                .dependencies()
                .into_iter()
                .map(|dependency| {
                    output_nodes.get(&dependency).copied().unwrap_or_else(|| {
                        push_literal(
                            values.get(&dependency).copied().unwrap_or(0),
                            &mut nodes,
                            &mut next_node_id,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let output = append_formula_ir_node(
                formula.formula(),
                &dependency_nodes,
                &mut nodes,
                &mut next_node_id,
                &push_literal,
            );
            let sink_port = SinkPortId(next_sink_port);
            next_sink_port += 1;
            let sink = NodeId(next_node_id);
            next_node_id += 1;
            nodes.push(IrNode {
                id: sink,
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: sink_port,
                    input: output,
                },
            });
            output_nodes.insert(cell, output);
            sink_ports.insert(cell, sink_port);
        }

        let executor = match IrExecutor::new(IrProgram::from(nodes)) {
            Ok(executor) => executor,
            Err(_) => return,
        };
        for (cell, sink_port) in sink_ports {
            match executor.sink_value(sink_port) {
                Some(KernelValue::Number(value)) => {
                    values.insert(cell, *value as i64);
                }
                _ => {
                    values.insert(cell, 0);
                }
            }
        }
    }

    fn apply_runtime_input_value(
        &mut self,
        cell: (u32, u32),
        value: KernelValue,
    ) -> RuntimeInputUpdate {
        let Some(runtime) = self.ir_runtime.as_mut() else {
            return RuntimeInputUpdate::Failed;
        };
        match patch_retained_input(&mut runtime.retained, cell, value) {
            Ok(patch) if patch.changed() => RuntimeInputUpdate::Applied,
            Ok(_) => RuntimeInputUpdate::Unchanged,
            Err(_) => RuntimeInputUpdate::Failed,
        }
    }

    fn apply_runtime_input_update(&mut self, cell: (u32, u32)) -> RuntimeInputUpdate {
        let Some(formula) = self.formula_cells.get(&cell) else {
            return RuntimeInputUpdate::Failed;
        };
        let value = retained_number_formula_input_value(formula.retained_number_formula())
            .unwrap_or_else(|| KernelValue::from(0.0));
        self.apply_runtime_input_value(cell, value)
    }

    fn clear_runtime_input(&mut self, cell: (u32, u32)) -> bool {
        matches!(
            self.apply_runtime_input_value(cell, KernelValue::from(0.0)),
            RuntimeInputUpdate::Applied | RuntimeInputUpdate::Unchanged
        )
    }

    fn recompute_affected_runtime_closure(
        &mut self,
        root: (u32, u32),
        affected: &[(u32, u32)],
    ) -> bool {
        let mut affected_formula_cells = std::mem::take(&mut self.affected_formula_cells_scratch);
        affected_formula_cells.clear();
        for cell in affected.iter().copied() {
            if self.formula_cells.contains_key(&cell) {
                affected_formula_cells.push(cell);
            } else {
                self.computed_values.remove(&cell);
                if let Some(runtime) = self.ir_runtime.as_mut() {
                    let _ = runtime.retained.clear_output_seq(cell);
                }
            }
        }
        let mut ordered_components = std::mem::take(&mut self.affected_components_result_scratch);
        self.affected_components_topo_order_into(&affected_formula_cells, &mut ordered_components);
        let mut roots = std::mem::take(&mut self.runtime_roots_scratch);
        roots.clear();
        if self.formula_cells.contains_key(&root) {
            roots.insert(root);
        } else {
            roots.extend(affected_formula_cells.iter().copied().filter(|cell| {
                self.formula_dependencies
                    .get(cell)
                    .into_iter()
                    .flat_map(|dependencies| dependencies.iter())
                    .any(|dependency| *dependency == root)
            }));
        }

        let result = if let Some(runtime) = self.ir_runtime.as_mut() {
            recompute_number_retained_members(
                &roots,
                &ordered_components,
                &self.formula_dependencies,
                &mut runtime.retained,
                &mut self.computed_values,
            )
            .is_ok()
        } else {
            false
        };

        self.affected_components_result_scratch = ordered_components;
        self.runtime_roots_scratch = roots;
        self.affected_formula_cells_scratch = affected_formula_cells;
        result
    }
}

impl CellsSheetState {
    pub(crate) fn new_lowered(
        baseline_formulas: BTreeMap<(u32, u32), LoweredCellsFormula>,
        baseline_state: CellsFormulaState,
    ) -> Self {
        Self {
            resolved_formulas: baseline_formulas.clone(),
            baseline_formulas,
            override_entries: Vec::new(),
            override_entry_indexes: BTreeMap::new(),
            formula_state: baseline_state,
        }
    }

    #[cfg(test)]
    pub fn new(
        baseline_formulas: BTreeMap<(u32, u32), CellsFormula>,
        baseline_state: CellsFormulaState,
    ) -> Self {
        Self::new_lowered(
            baseline_formulas
                .into_iter()
                .map(|(cell, formula)| (cell, lower_cells_formula(formula)))
                .collect(),
            baseline_state,
        )
    }

    pub fn formula_text(&self, row: u32, column: u32) -> String {
        self.cell_formula(row, column).text.clone()
    }

    pub fn display_text(&self, row: u32, column: u32) -> String {
        let formula = self.cell_formula(row, column);
        if matches!(formula.expr, CellsFormulaExpr::Empty) {
            return String::new();
        }
        self.formula_state
            .computed_values
            .get(&(row, column))
            .copied()
            .unwrap_or(0)
            .to_string()
    }

    pub fn commit_override(&mut self, row: u32, column: u32, text: String) {
        let cell = (row, column);
        let formula = parse_lowered_cells_formula(text);
        let entry_index = self.override_entries.len();
        self.override_entries.push(CellsOverrideEntry {
            row,
            column,
            formula,
        });
        self.override_entry_indexes
            .entry(cell)
            .or_default()
            .push(entry_index);
        self.resolved_formulas
            .insert(cell, self.resolve_formula_from_history(row, column));
        self.refresh_formula_cell(cell);
    }

    fn cell_formula(&self, row: u32, column: u32) -> CellsFormula {
        self.cell_lowered_formula(row, column).formula().clone()
    }

    fn cell_lowered_formula(&self, row: u32, column: u32) -> LoweredCellsFormula {
        self.resolved_formulas
            .get(&(row, column))
            .cloned()
            .unwrap_or_else(|| parse_lowered_cells_formula(String::new()))
    }

    fn matching_overrides(&self, row: u32, column: u32) -> Vec<&CellsOverrideEntry> {
        self.override_entry_indexes
            .get(&(row, column))
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .filter_map(|index| self.override_entries.get(*index))
            .collect()
    }

    fn resolve_formula_from_history(&self, row: u32, column: u32) -> LoweredCellsFormula {
        let matches = self.matching_overrides(row, column);
        get_one_based(&matches, matches.len() as i64)
            .map(|entry| entry.formula.clone())
            .unwrap_or_else(|| self.default_formula(row, column))
    }

    fn default_formula(&self, row: u32, column: u32) -> LoweredCellsFormula {
        self.baseline_formulas
            .get(&(row, column))
            .cloned()
            .unwrap_or_else(|| parse_lowered_cells_formula(String::new()))
    }

    fn refresh_formula_cell(&mut self, cell: (u32, u32)) {
        let current = self.cell_lowered_formula(cell.0, cell.1);
        self.formula_state.set_lowered_formula(cell, current);
    }
}

fn build_formula_graph(
    formula_cells: &BTreeMap<(u32, u32), LoweredCellsFormula>,
) -> (
    BTreeMap<(u32, u32), Vec<(u32, u32)>>,
    BTreeMap<(u32, u32), Vec<(u32, u32)>>,
) {
    let mut formula_dependencies = BTreeMap::new();
    let mut formula_dependents = BTreeMap::new();
    for (cell, formula) in formula_cells {
        let dependencies = formula.formula().dependencies();
        formula_dependencies.insert(*cell, dependencies.clone());
        for dependency in dependencies {
            let dependents = formula_dependents
                .entry(dependency)
                .or_insert_with(Vec::<(u32, u32)>::new);
            if !dependents.contains(cell) {
                dependents.push(*cell);
            }
        }
    }
    (formula_dependencies, formula_dependents)
}

fn build_cells_ir_runtime(
    planner: &mut CellsFormulaState,
    previous_runtime: Option<CellsIrRuntime>,
) -> Option<(CellsIrRuntime, BTreeMap<(u32, u32), i64>)> {
    let mut affected_formula_cells = std::mem::take(&mut planner.affected_formula_cells_scratch);
    affected_formula_cells.clear();
    affected_formula_cells.extend(planner.formula_cells.keys().copied());

    let mut ordered_components = std::mem::take(&mut planner.affected_components_result_scratch);
    planner.affected_components_topo_order_into(&affected_formula_cells, &mut ordered_components);
    let mut ordered_cells = std::mem::take(&mut planner.ordered_cells_scratch);
    ordered_cells.clear();
    ordered_cells.extend(ordered_components.iter().flatten().copied());

    let retained = rebuild_retained_runtime_from_owned_program_results(
        previous_runtime.map(|runtime| runtime.retained),
        ordered_cells.iter().copied().map(|cell| {
            let formula = planner
                .formula_cells
                .get(&cell)
                .ok_or_else(|| format!("missing cells formula for ({}, {})", cell.0, cell.1))?;
            build_cell_ir_runtime(cell, formula.retained_number_formula()).ok_or_else(|| {
                format!("failed to build cells runtime for ({}, {})", cell.0, cell.1)
            })
        }),
    )
    .ok()?;

    let mut runtime = CellsIrRuntime { retained };
    let mut computed_values = BTreeMap::new();
    computed_values.extend(ordered_cells.iter().filter_map(|cell| {
        planner
            .computed_values
            .get(cell)
            .copied()
            .map(|value| (*cell, value))
    }));
    let mut roots = std::mem::take(&mut planner.runtime_roots_scratch);
    roots.clear();
    roots.extend(ordered_cells.iter().copied());
    // `affected_formula_cells` already covered the full formula set, so the
    // existing topo order still matches the retained rebuild inputs. Seed the
    // last computed snapshots too, so preserved retained output seqs can clamp
    // the first full-rebuild recompute instead of regressing slot freshness.
    let result = recompute_number_retained_members(
        &roots,
        &ordered_components,
        &planner.formula_dependencies,
        &mut runtime.retained,
        &mut computed_values,
    )
    .ok()
    .map(|_| (runtime, computed_values));

    planner.affected_components_result_scratch = ordered_components;
    planner.runtime_roots_scratch = roots;
    planner.ordered_cells_scratch = ordered_cells;
    planner.affected_formula_cells_scratch = affected_formula_cells;
    result
}

fn build_cell_ir_runtime(
    cell: (u32, u32),
    formula: &RetainedNumberFormula,
) -> Option<RetainedMemberProgram<(u32, u32)>> {
    build_pair_number_retained_member_program_from_descriptor(cell, formula).ok()
}

#[cfg(test)]
fn stable_runtime_cell_base(cell: (u32, u32)) -> u32 {
    3_000_000 + (cell.0 * 1_000 + cell.1) * 512
}

#[cfg(test)]
fn stable_runtime_mirror_node(cell: (u32, u32)) -> NodeId {
    NodeId(stable_runtime_cell_base(cell))
}

#[cfg(test)]
fn stable_runtime_output_node(cell: (u32, u32)) -> NodeId {
    NodeId(stable_runtime_cell_base(cell) + 1)
}

#[cfg(test)]
fn stable_runtime_list_node(cell: (u32, u32)) -> NodeId {
    NodeId(stable_runtime_cell_base(cell) + 2)
}

#[cfg(test)]
fn stable_runtime_sink_node(cell: (u32, u32)) -> NodeId {
    NodeId(stable_runtime_cell_base(cell) + 3)
}

#[cfg(test)]
fn stable_runtime_dependency_placeholder_node(cell: (u32, u32), dependency_index: usize) -> NodeId {
    NodeId(stable_runtime_cell_base(cell) + 4 + dependency_index as u32)
}

#[cfg(test)]
#[allow(dead_code)]
fn append_formula_ir_node_with_ids(
    cell: (u32, u32),
    formula: &CellsFormula,
    dependency_nodes: &[NodeId],
    nodes: &mut Vec<IrNode>,
    push_literal: &impl Fn(NodeId, i64, &mut Vec<IrNode>),
) -> NodeId {
    match formula.expr {
        CellsFormulaExpr::Empty => {
            let output = stable_runtime_output_node(cell);
            push_literal(output, 0, nodes);
            output
        }
        CellsFormulaExpr::Number(number) => {
            let output = stable_runtime_output_node(cell);
            push_literal(output, number, nodes);
            output
        }
        CellsFormulaExpr::Add(_, _) => {
            let output = stable_runtime_output_node(cell);
            nodes.push(IrNode {
                id: output,
                source_expr: None,
                kind: IrNodeKind::Add {
                    lhs: dependency_nodes[0],
                    rhs: dependency_nodes[1],
                },
            });
            output
        }
        CellsFormulaExpr::SumColumnRange { .. } => {
            let list = stable_runtime_list_node(cell);
            nodes.push(IrNode {
                id: list,
                source_expr: None,
                kind: IrNodeKind::ListLiteral {
                    items: dependency_nodes.to_vec(),
                },
            });
            let output = stable_runtime_output_node(cell);
            nodes.push(IrNode {
                id: output,
                source_expr: None,
                kind: IrNodeKind::ListSum { list },
            });
            output
        }
        CellsFormulaExpr::Invalid => {
            let output = stable_runtime_output_node(cell);
            push_literal(output, 0, nodes);
            output
        }
    }
}

#[cfg(test)]
fn append_formula_ir_node(
    formula: &CellsFormula,
    dependency_nodes: &[NodeId],
    nodes: &mut Vec<IrNode>,
    next_node_id: &mut u32,
    push_literal: &impl Fn(i64, &mut Vec<IrNode>, &mut u32) -> NodeId,
) -> NodeId {
    match formula.expr {
        CellsFormulaExpr::Empty => push_literal(0, nodes, next_node_id),
        CellsFormulaExpr::Number(number) => push_literal(number, nodes, next_node_id),
        CellsFormulaExpr::Add(_, _) => {
            let output = NodeId(*next_node_id);
            *next_node_id += 1;
            nodes.push(IrNode {
                id: output,
                source_expr: None,
                kind: IrNodeKind::Add {
                    lhs: dependency_nodes[0],
                    rhs: dependency_nodes[1],
                },
            });
            output
        }
        CellsFormulaExpr::SumColumnRange { .. } => {
            let list = NodeId(*next_node_id);
            *next_node_id += 1;
            nodes.push(IrNode {
                id: list,
                source_expr: None,
                kind: IrNodeKind::ListLiteral {
                    items: dependency_nodes.to_vec(),
                },
            });
            let output = NodeId(*next_node_id);
            *next_node_id += 1;
            nodes.push(IrNode {
                id: output,
                source_expr: None,
                kind: IrNodeKind::ListSum { list },
            });
            output
        }
        CellsFormulaExpr::Invalid => push_literal(0, nodes, next_node_id),
    }
}

#[cfg(test)]
fn evaluate_cells_formula_with_ir(formula: &CellsFormula, reference_values: &[i64]) -> i64 {
    if formula.dependencies().len() != reference_values.len() {
        return 0;
    }
    let mut next_node_id = 1u32;
    let mut nodes = Vec::new();
    let push_literal = |value: i64, nodes: &mut Vec<IrNode>, next_node_id: &mut u32| -> NodeId {
        let node_id = NodeId(*next_node_id);
        *next_node_id += 1;
        nodes.push(IrNode {
            id: node_id,
            source_expr: None,
            kind: IrNodeKind::Literal(KernelValue::from(value as f64)),
        });
        node_id
    };
    let dependency_nodes = reference_values
        .iter()
        .copied()
        .map(|value| push_literal(value, &mut nodes, &mut next_node_id))
        .collect::<Vec<_>>();
    let output = append_formula_ir_node(
        formula,
        &dependency_nodes,
        &mut nodes,
        &mut next_node_id,
        &push_literal,
    );
    let sink_port = SinkPortId(1);
    let sink = NodeId(next_node_id);
    nodes.push(IrNode {
        id: sink,
        source_expr: None,
        kind: IrNodeKind::SinkPort {
            port: sink_port,
            input: output,
        },
    });
    let executor = match IrExecutor::new_program(IrProgram::from(nodes)) {
        Ok(executor) => executor,
        Err(_) => return 0,
    };
    match executor.sink_value(sink_port) {
        Some(KernelValue::Number(value)) => *value as i64,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::CausalSeq;

    #[test]
    fn sheet_state_keeps_append_only_override_history_and_latest_formula() {
        let baseline_formulas = BTreeMap::from([((1, 1), CellsFormula::parse("5".to_string()))]);
        let baseline_state = CellsFormulaState::from_formulas(
            baseline_formulas
                .iter()
                .map(|(coords, formula)| (*coords, formula.clone())),
        );
        let mut sheet = CellsSheetState::new(baseline_formulas, baseline_state);

        sheet.commit_override(1, 1, "7".to_string());
        sheet.commit_override(1, 1, "8".to_string());

        assert_eq!(sheet.override_entries.len(), 2);
        assert_eq!(
            sheet
                .override_entries
                .last()
                .map(|entry| entry.formula.formula().text.as_str()),
            Some("8")
        );
        assert_eq!(sheet.formula_text(1, 1), "8");
        assert_eq!(sheet.display_text(1, 1), "8");
    }

    #[test]
    fn sheet_state_keeps_default_text_as_latest_committed_override() {
        let baseline_formulas = BTreeMap::from([((1, 1), CellsFormula::parse("5".to_string()))]);
        let baseline_state = CellsFormulaState::from_formulas(
            baseline_formulas
                .iter()
                .map(|(coords, formula)| (*coords, formula.clone())),
        );
        let mut sheet = CellsSheetState::new(baseline_formulas, baseline_state);

        sheet.commit_override(1, 1, "5".to_string());

        assert_eq!(sheet.override_entries.len(), 1);
        assert_eq!(sheet.formula_text(1, 1), "5");
        assert_eq!(
            sheet
                .resolved_formulas
                .get(&(1, 1))
                .map(|formula| formula.formula().text.as_str()),
            Some("5")
        );
        assert_eq!(
            sheet.matching_overrides(1, 1).last().map(|entry| entry
                .formula
                .formula()
                .text
                .as_str()),
            Some("5")
        );
    }

    #[test]
    fn sheet_state_keeps_empty_override_as_resolved_formula_over_default() {
        let baseline_formulas = BTreeMap::from([((1, 1), CellsFormula::parse("5".to_string()))]);
        let baseline_state = CellsFormulaState::from_formulas(
            baseline_formulas
                .iter()
                .map(|(coords, formula)| (*coords, formula.clone())),
        );
        let mut sheet = CellsSheetState::new(baseline_formulas, baseline_state);

        sheet.commit_override(1, 1, String::new());

        assert_eq!(sheet.formula_text(1, 1), "");
        assert_eq!(sheet.display_text(1, 1), "");
        assert_eq!(
            sheet
                .resolved_formulas
                .get(&(1, 1))
                .map(|formula| formula.formula().text.as_str()),
            Some("")
        );
        assert!(!sheet.formula_state.formula_cells.contains_key(&(1, 1)));
    }

    #[test]
    fn sheet_state_matching_overrides_filters_by_cell_and_picks_latest_match() {
        let baseline_formulas = BTreeMap::from([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((2, 1), CellsFormula::parse("10".to_string())),
        ]);
        let baseline_state = CellsFormulaState::from_formulas(
            baseline_formulas
                .iter()
                .map(|(coords, formula)| (*coords, formula.clone())),
        );
        let mut sheet = CellsSheetState::new(baseline_formulas, baseline_state);

        sheet.commit_override(1, 1, "7".to_string());
        sheet.commit_override(1, 2, "99".to_string());
        sheet.commit_override(1, 1, "8".to_string());

        let matching = sheet
            .matching_overrides(1, 1)
            .into_iter()
            .map(|entry| entry.formula.formula().text.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            sheet.override_entry_indexes.get(&(1, 1)).cloned(),
            Some(vec![0, 2])
        );
        assert_eq!(matching, vec!["7", "8"]);
        assert_eq!(sheet.formula_text(1, 1), "8");
        assert_eq!(sheet.formula_text(1, 2), "99");
    }

    #[test]
    fn list_get_one_based_matches_cells_source_indexing_shape() {
        let values = vec!["first", "second", "third"];

        assert_eq!(get_one_based(&values, 0), None);
        assert_eq!(get_one_based(&values, 1), Some(&"first"));
        assert_eq!(get_one_based(&values, 3), Some(&"third"));
        assert_eq!(get_one_based(&values, 4), None);
    }

    #[test]
    fn formula_state_set_formula_recomputes_only_affected_closure() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
            ((2, 1), CellsFormula::parse("99".to_string())),
        ]);

        state.set_formula((1, 1), CellsFormula::parse("5".to_string()));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&20));
        assert_eq!(state.computed_values.get(&(2, 1)), Some(&99));
        assert_eq!(
            state.formula_dependencies.get(&(1, 2)).cloned(),
            Some(vec![(1, 1), (1, 1)])
        );
    }

    #[test]
    fn set_formula_clears_reused_formula_closure_result_scratch_between_edits() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
            ((2, 1), CellsFormula::parse("99".to_string())),
        ]);

        state.formula_closure_result_scratch = vec![(9, 9), (8, 8)];

        state.set_formula((1, 1), CellsFormula::parse("5".to_string()));

        assert_eq!(
            state.formula_closure_result_scratch,
            vec![(1, 1), (1, 2), (1, 3)]
        );

        state.set_formula((2, 1), CellsFormula::parse("7".to_string()));

        assert_eq!(state.formula_closure_result_scratch, vec![(2, 1)]);
    }

    #[test]
    fn number_cell_updates_patch_persistent_runtime_without_rebuild() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");

        state.set_formula((1, 1), CellsFormula::parse("5".to_string()));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq + 1)
        );
    }

    #[test]
    fn same_number_cell_update_does_not_repatch_or_reexecute_downstream_runtime() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state.set_formula((1, 1), CellsFormula::parse("1".to_string()));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&1));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn same_number_cell_update_recovers_missing_retained_input_value_without_repatch() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_input_value_for_test((1, 1));

        state.set_formula((1, 1), CellsFormula::parse("1".to_string()));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&1));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 1)))
                .cloned(),
            Some(KernelValue::from(1.0))
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn same_number_cell_update_recovers_stale_retained_seq_floor_without_repatch() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        state.set_formula((1, 1), CellsFormula::parse("2".to_string()));

        let live_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_seq((1, 1)))
            .expect("live seq exists");
        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_seq_for_test((1, 1), crate::semantics::CausalSeq::new(1, 0));
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 1), crate::semantics::CausalSeq::new(1, 0));

        state.set_formula((1, 1), CellsFormula::parse("2".to_string()));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&2));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&4));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 1))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 1))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn retained_runtime_handles_clear_and_restore_of_value_cell() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");

        state.set_formula((1, 1), CellsFormula::parse(String::new()));
        assert_eq!(state.computed_values.get(&(1, 1)), None);
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&0));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq + 1)
        );

        state.set_formula((1, 1), CellsFormula::parse("5".to_string()));
        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq + 2)
        );
    }

    #[test]
    fn runtime_recompute_seeds_preserved_root_seq_without_revisiting_unchanged_downstream() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let root_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.output_seq((1, 1)))
            .expect("root seq exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_output_seq((1, 1));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 1))),
            Some(root_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_stale_preserved_root_value_without_revisiting_unchanged_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state.computed_values.insert((1, 1), 999);

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_missing_root_input_value_without_revisiting_unchanged_downstream()
    {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_input_value_for_test((1, 1));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 1)))
                .cloned(),
            Some(KernelValue::from(5.0))
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_prefers_live_sink_root_input_value_over_stale_cached_input_value_without_revisiting_unchanged_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_value_for_test((1, 1), KernelValue::from(999.0));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 1)))
                .cloned(),
            Some(KernelValue::from(5.0))
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_prefers_live_sink_non_root_input_value_over_stale_cached_input_value_without_revisiting_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("7".to_string())),
            ((1, 3), CellsFormula::parse("=add(A1,B1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_value_for_test((1, 2), KernelValue::from(999.0));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&7));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&12));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 2)))
                .cloned(),
            Some(KernelValue::from(7.0))
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_missing_non_root_input_value_without_revisiting_downstream() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("7".to_string())),
            ((1, 3), CellsFormula::parse("=add(A1,B1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_input_value_for_test((1, 2));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&7));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&12));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 2)))
                .cloned(),
            Some(KernelValue::from(7.0))
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_seeds_preserved_non_root_seq_without_revisiting_downstream() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);

        let downstream_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.output_seq((1, 3)))
            .expect("downstream seq exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_output_seq((1, 3));

        assert!(state.recompute_affected_runtime_closure((1, 2), &[(1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&20));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 3))),
            Some(downstream_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_missing_root_input_seq_without_revisiting_unchanged_downstream() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_input_seq_for_test((1, 1));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 1))),
            Some(CausalSeq::new(1, 0))
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_prefers_live_sink_root_input_seq_over_stale_cached_input_seq_without_revisiting_unchanged_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let live_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_seq((1, 1)))
            .expect("root seq exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_seq_for_test((1, 1), CausalSeq::new(0, 0));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 1))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_does_not_regress_preserved_root_input_seq_floor_without_revisiting_unchanged_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let preserved_seq = CausalSeq::new(25, 7);
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_seq_for_test((1, 1), preserved_seq);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 1), preserved_seq);

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 1))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 1))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_prefers_live_sink_root_output_seq_over_stale_cached_output_seq_without_revisiting_unchanged_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let live_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.output_seq((1, 1)))
            .expect("root seq exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 2)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 1), CausalSeq::new(0, 0));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 1))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 2))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_missing_non_root_input_seq_without_revisiting_downstream() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("7".to_string())),
            ((1, 3), CellsFormula::parse("=add(A1,B1)".to_string())),
        ]);

        let live_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_seq((1, 2)))
            .expect("preserved non-root seq exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_input_seq_for_test((1, 2));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&7));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&12));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 2))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_prefers_live_sink_non_root_input_seq_over_stale_cached_input_seq_without_revisiting_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("7".to_string())),
            ((1, 3), CellsFormula::parse("=add(A1,B1)".to_string())),
        ]);

        let live_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_seq((1, 2)))
            .expect("preserved non-root seq exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_seq_for_test((1, 2), CausalSeq::new(1, 0));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&7));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&12));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 2))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_stale_preserved_non_root_value_without_revisiting_downstream() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state.computed_values.insert((1, 3), 999);

        assert!(state.recompute_affected_runtime_closure((1, 2), &[(1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&20));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn runtime_recompute_recovers_stale_preserved_non_root_output_seq_without_revisiting_downstream()
     {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);

        state.set_formula((1, 1), CellsFormula::parse("6".to_string()));
        state.set_formula((1, 1), CellsFormula::parse("5".to_string()));

        let live_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.output_seq((1, 2)))
            .expect("preserved non-root seq exists");
        assert!(live_seq.turn > 0);
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 2), CausalSeq::new(live_seq.turn - 1, live_seq.seq));

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2), (1, 3)]));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&20));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 2))),
            Some(live_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn clearing_zero_value_cell_does_not_repatch_retained_runtime_input() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("0".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");

        state.set_formula((1, 1), CellsFormula::parse(String::new()));

        assert_eq!(state.computed_values.get(&(1, 1)), None);
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&0));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq()),
            Some(initial_seq)
        );
    }

    #[test]
    fn rebuilt_sheet_runtime_keeps_stable_ids_for_unchanged_cells() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("9".to_string())),
        ]);

        let initial_output = state
            .ir_runtime
            .as_ref()
            .map(|_| stable_runtime_output_node((1, 2)))
            .expect("unchanged formula cell has output id");
        let initial_mirror = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.mirror_cell((1, 3)))
            .expect("unchanged leaf has mirror id");
        let initial_sink = state
            .ir_runtime
            .as_ref()
            .map(|_| {
                crate::runtime::stable_retained_sink_port(
                    crate::runtime::stable_retained_pair_slot(1, 3),
                )
            })
            .expect("unchanged leaf has sink id");

        state.set_formula((1, 2), CellsFormula::parse("=sum(A1:A1)".to_string()));

        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .map(|_| stable_runtime_output_node((1, 2))),
            Some(initial_output)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.mirror_cell((1, 3))),
            Some(initial_mirror)
        );
        assert_eq!(
            state.ir_runtime.as_ref().map(|_| {
                crate::runtime::stable_retained_sink_port(
                    crate::runtime::stable_retained_pair_slot(1, 3),
                )
            }),
            Some(initial_sink)
        );
    }

    #[test]
    fn structural_rebuild_preserves_retained_input_state_progress() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("9".to_string())),
        ]);

        let initial_seq = state
            .ir_runtime
            .as_ref()
            .map(|runtime| runtime.retained.next_seq())
            .expect("persistent runtime exists");
        let initial_leaf_value = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_value((1, 3)).cloned())
            .expect("leaf value stored");
        let initial_leaf_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_seq((1, 3)))
            .expect("leaf seq stored");

        state.set_formula((1, 2), CellsFormula::parse("=sum(A1:A1)".to_string()));

        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 3)).cloned()),
            Some(initial_leaf_value)
        );
        assert!(
            state
                .ir_runtime
                .as_ref()
                .map(|runtime| runtime.retained.next_seq())
                .is_some_and(|next_seq| next_seq == initial_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 3))),
            Some(initial_leaf_seq)
        );
    }

    #[test]
    fn structural_rebuild_drops_stale_retained_input_state_for_non_leaf_cell() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("9".to_string())),
        ]);

        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 2)).cloned()),
            Some(KernelValue::from(9.0))
        );
        assert!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 2)))
                .is_some()
        );

        state.set_formula((1, 2), CellsFormula::parse("=add(A1,A1)".to_string()));

        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_value((1, 2)).cloned()),
            None
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 2))),
            None
        );
    }

    #[test]
    fn structural_formula_edit_preserves_changed_cell_runtime_identity() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
            ((1, 4), CellsFormula::parse("9".to_string())),
        ]);

        let changed_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 2)))
            .expect("changed cell runtime exists");
        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 3)))
            .expect("downstream runtime exists");
        let unaffected_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 4)))
            .expect("unaffected leaf runtime exists");

        state.set_formula((1, 2), CellsFormula::parse("=sum(A1:A1)".to_string()));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&1));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 2))),
            Some(changed_before)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 3))),
            Some(downstream_before)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 4))),
            Some(unaffected_before)
        );
    }

    #[test]
    fn structural_formula_edit_preserves_changed_cell_output_seq_floor() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);
        let preserved_seq = crate::semantics::CausalSeq::new(25, 7);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 2), preserved_seq);

        state.set_formula((1, 2), CellsFormula::parse("=sum(A1:A1)".to_string()));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&1));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 2))),
            Some(preserved_seq)
        );
    }

    #[test]
    fn structural_formula_edit_preserves_downstream_cell_output_seq_floor() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);
        let preserved_seq = crate::semantics::CausalSeq::new(25, 7);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 3), preserved_seq);

        state.set_formula((1, 2), CellsFormula::parse("=sum(A1:A1)".to_string()));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&1));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 3))),
            Some(preserved_seq)
        );
    }

    #[test]
    fn rebuild_ir_runtime_reuses_seeded_planner_scratch() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);

        state.affected_formula_cells_scratch = vec![(9, 9)];
        state.ordered_cells_scratch = vec![(8, 8), (8, 9)];
        state.runtime_roots_scratch = BTreeSet::from([(7, 7)]);

        state.rebuild_ir_runtime();

        assert_eq!(
            state.affected_formula_cells_scratch,
            vec![(1, 1), (1, 2), (1, 3)]
        );
        assert_eq!(
            state.affected_components_result_scratch,
            vec![vec![(1, 1)], vec![(1, 2)], vec![(1, 3)]]
        );
        assert_eq!(state.ordered_cells_scratch, vec![(1, 1), (1, 2), (1, 3)]);
        assert_eq!(
            state.runtime_roots_scratch,
            BTreeSet::from([(1, 1), (1, 2), (1, 3)])
        );
    }

    #[test]
    fn rebuild_ir_runtime_preserves_output_seq_floor_for_unchanged_cells() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);
        let preserved_seq = crate::semantics::CausalSeq::new(25, 7);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 2), preserved_seq);

        state.rebuild_ir_runtime();

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 2))),
            Some(preserved_seq)
        );
    }

    #[test]
    fn rebuild_ir_runtime_recovers_missing_output_seq_from_live_sink_state_for_unchanged_cell() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let preserved_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.output_seq((1, 2)))
            .expect("unchanged cell seq exists");
        let preserved_instance = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 2)))
            .expect("unchanged cell instance exists");
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_output_seq((1, 2));

        state.rebuild_ir_runtime();

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 2))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 2))),
            Some(preserved_instance)
        );
    }

    #[test]
    fn rebuild_ir_runtime_recovers_missing_input_seq_from_live_sink_state_for_unchanged_leaf() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let preserved_seq = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.input_seq((1, 1)))
            .expect("unchanged leaf seq exists");
        let preserved_instance = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 1)))
            .expect("unchanged leaf instance exists");
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .clear_input_seq_for_test((1, 1));

        state.rebuild_ir_runtime();

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 1))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 1))),
            Some(preserved_instance)
        );
    }

    #[test]
    fn rebuild_ir_runtime_preserves_input_seq_floor_for_unchanged_leaf() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
        ]);

        let preserved_seq = CausalSeq::new(25, 7);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_input_seq_for_test((1, 1), preserved_seq);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 1), preserved_seq);

        state.rebuild_ir_runtime();

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&10));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.input_seq((1, 1))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 1))),
            Some(preserved_seq)
        );
    }

    #[test]
    fn rebuild_ir_runtime_preserves_instance_ids_for_unchanged_cells() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("9".to_string())),
        ]);

        let formula_instance = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 2)))
            .expect("formula instance id exists");
        let leaf_instance = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 3)))
            .expect("leaf instance id exists");

        state.rebuild_ir_runtime();

        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 2))),
            Some(formula_instance)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 3))),
            Some(leaf_instance)
        );
    }

    #[test]
    fn rebuild_ir_runtime_preserves_downstream_output_seq_floor_and_instance_id() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);
        let downstream_instance = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 3)))
            .expect("downstream instance id exists");
        let preserved_seq = crate::semantics::CausalSeq::new(25, 7);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 3), preserved_seq);

        state.rebuild_ir_runtime();

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&2));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&4));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 3))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 3))),
            Some(downstream_instance)
        );
    }

    #[test]
    fn rebuild_ir_runtime_preserves_changed_cell_output_seq_floor_and_instance_id() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);
        let changed_instance = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.instance_id((1, 2)))
            .expect("changed instance id exists");
        let preserved_seq = crate::semantics::CausalSeq::new(25, 7);
        state
            .ir_runtime
            .as_mut()
            .expect("runtime exists")
            .retained
            .set_output_seq_for_test((1, 2), preserved_seq);

        state.formula_cells.insert(
            (1, 2),
            lower_cells_formula(CellsFormula::parse("=sum(A1:A1)".to_string())),
        );
        let (dependencies, dependents) = build_formula_graph(&state.formula_cells);
        state.formula_dependencies = dependencies;
        state.formula_dependents = dependents;

        state.rebuild_ir_runtime();

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&1));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&2));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.output_seq((1, 2))),
            Some(preserved_seq)
        );
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.instance_id((1, 2))),
            Some(changed_instance)
        );
    }

    #[test]
    fn unchanged_structural_edit_does_not_reexecute_downstream_cell_runtime() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("0".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);

        let downstream_before = state
            .ir_runtime
            .as_ref()
            .and_then(|runtime| runtime.retained.evaluation_count((1, 3)))
            .expect("downstream runtime exists");

        state.set_formula((1, 2), CellsFormula::parse("=sum(A1:A1)".to_string()));

        assert_eq!(state.computed_values.get(&(1, 2)), Some(&0));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&0));
        assert_eq!(
            state
                .ir_runtime
                .as_ref()
                .and_then(|runtime| runtime.retained.evaluation_count((1, 3))),
            Some(downstream_before)
        );
    }

    #[test]
    fn affected_runtime_recompute_clears_reused_cell_and_root_scratch_between_calls() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("1".to_string())),
            ((1, 2), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,B1)".to_string())),
        ]);
        let mut values = BTreeMap::new();

        state.recompute_affected_closure(&[(1, 1), (1, 2), (1, 3)], &mut values);
        assert_eq!(
            state.affected_formula_cells_scratch,
            vec![(1, 1), (1, 2), (1, 3)]
        );
        assert_eq!(
            state.affected_components_result_scratch,
            vec![vec![(1, 1)], vec![(1, 2)], vec![(1, 3)]]
        );
        assert_eq!(state.ordered_cells_scratch, vec![(1, 1), (1, 2), (1, 3)]);

        state.recompute_affected_closure(&[(1, 2), (1, 3)], &mut values);
        assert_eq!(state.affected_formula_cells_scratch, vec![(1, 2), (1, 3)]);
        assert_eq!(
            state.affected_components_result_scratch,
            vec![vec![(1, 2)], vec![(1, 3)]]
        );
        assert_eq!(state.ordered_cells_scratch, vec![(1, 2), (1, 3)]);

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2), (1, 3)]));
        assert_eq!(
            state.affected_formula_cells_scratch,
            vec![(1, 1), (1, 2), (1, 3)]
        );
        assert_eq!(
            state.affected_components_result_scratch,
            vec![vec![(1, 1)], vec![(1, 2)], vec![(1, 3)]]
        );
        assert_eq!(state.runtime_roots_scratch, BTreeSet::from([(1, 1)]));

        assert!(state.recompute_affected_runtime_closure((1, 2), &[(1, 2), (1, 3)]));
        assert_eq!(state.affected_formula_cells_scratch, vec![(1, 2), (1, 3)]);
        assert_eq!(
            state.affected_components_result_scratch,
            vec![vec![(1, 2)], vec![(1, 3)]]
        );
        assert_eq!(state.runtime_roots_scratch, BTreeSet::from([(1, 2)]));
    }

    #[test]
    fn stable_runtime_node_stride_leaves_room_for_long_sum_placeholders() {
        assert_ne!(
            stable_runtime_dependency_placeholder_node((1, 1), 100),
            stable_runtime_mirror_node((1, 2))
        );
        assert_ne!(
            stable_runtime_dependency_placeholder_node((1, 1), 100),
            stable_runtime_output_node((1, 2))
        );
        assert_ne!(
            stable_runtime_dependency_placeholder_node((1, 1), 100),
            stable_runtime_sink_node((1, 2))
        );
    }

    #[test]
    fn closure_recompute_preserves_back_edge_zero_semantics() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("=add(A1,B1)".to_string())),
            ((1, 2), CellsFormula::parse("2".to_string())),
        ]);
        let mut values = BTreeMap::new();

        state.recompute_affected_closure(&[(1, 1), (1, 2)], &mut values);

        assert_eq!(values.get(&(1, 1)), Some(&2));
        assert_eq!(values.get(&(1, 2)), Some(&2));
    }

    #[test]
    fn runtime_closure_recompute_preserves_back_edge_zero_semantics() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("=add(A1,B1)".to_string())),
            ((1, 2), CellsFormula::parse("2".to_string())),
        ]);

        assert!(state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (1, 2)]));

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&2));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&2));
    }

    #[test]
    fn ir_closure_recompute_handles_simple_acyclic_chain() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((2, 1), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((3, 1), CellsFormula::parse("=sum(A1:A2)".to_string())),
        ]);
        let mut values = BTreeMap::new();

        let components = state.affected_components_topo_order(&[(1, 1), (2, 1), (3, 1)]);
        assert_eq!(components, vec![vec![(1, 1)], vec![(2, 1)], vec![(3, 1)]]);
        assert!(
            components
                .iter()
                .all(|component| !state.component_is_cycle_core(component))
        );
        state.recompute_ordered_cells_with_ir(
            &components.into_iter().flatten().collect::<Vec<_>>(),
            &mut values,
        );
        assert_eq!(values.get(&(1, 1)), Some(&5));
        assert_eq!(values.get(&(2, 1)), Some(&10));
        assert_eq!(values.get(&(3, 1)), Some(&15));
    }

    #[test]
    fn affected_components_topo_order_keeps_acyclic_prefix_before_cycle_core() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((2, 1), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 2), CellsFormula::parse("=add(B1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,A1)".to_string())),
        ]);

        let components = state.affected_components_topo_order(&[(1, 1), (2, 1), (1, 2), (1, 3)]);

        assert_eq!(
            components,
            vec![vec![(1, 1)], vec![(2, 1)], vec![(1, 2)], vec![(1, 3)]]
        );
        assert!(!state.component_is_cycle_core(&components[0]));
        assert!(!state.component_is_cycle_core(&components[1]));
        assert!(state.component_is_cycle_core(&components[2]));
        assert!(!state.component_is_cycle_core(&components[3]));
    }

    #[test]
    fn component_cycle_detection_excludes_downstream_tail() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((2, 1), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 2), CellsFormula::parse("=add(B1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,A1)".to_string())),
        ]);

        let components = state.affected_components_topo_order(&[(1, 2), (1, 3)]);

        assert_eq!(components, vec![vec![(1, 2)], vec![(1, 3)]]);
        assert!(state.component_is_cycle_core(&components[0]));
        assert!(!state.component_is_cycle_core(&components[1]));
    }

    #[test]
    fn scc_ordered_recompute_handles_cycle_members_and_downstream_tail() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((2, 1), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 2), CellsFormula::parse("=add(B1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,A1)".to_string())),
        ]);
        let mut values = BTreeMap::new();

        state.recompute_affected_closure(&[(1, 1), (2, 1), (1, 2), (1, 3)], &mut values);

        assert_eq!(values.get(&(1, 1)), Some(&5));
        assert_eq!(values.get(&(2, 1)), Some(&10));
        assert_eq!(values.get(&(1, 2)), Some(&5));
        assert_eq!(values.get(&(1, 3)), Some(&10));
    }

    #[test]
    fn runtime_scc_ordered_recompute_handles_cycle_members_and_downstream_tail() {
        let mut state = CellsFormulaState::from_formulas([
            ((1, 1), CellsFormula::parse("5".to_string())),
            ((2, 1), CellsFormula::parse("=add(A1,A1)".to_string())),
            ((1, 2), CellsFormula::parse("=add(B1,A1)".to_string())),
            ((1, 3), CellsFormula::parse("=add(B1,A1)".to_string())),
        ]);

        assert!(
            state.recompute_affected_runtime_closure((1, 1), &[(1, 1), (2, 1), (1, 2), (1, 3)],)
        );

        assert_eq!(state.computed_values.get(&(1, 1)), Some(&5));
        assert_eq!(state.computed_values.get(&(2, 1)), Some(&10));
        assert_eq!(state.computed_values.get(&(1, 2)), Some(&5));
        assert_eq!(state.computed_values.get(&(1, 3)), Some(&10));
    }

    #[test]
    fn formula_execution_uses_runtime_ops_for_add_and_sum() {
        assert_eq!(
            evaluate_cells_formula_with_ir(
                &CellsFormula::parse("=add(A1,B1)".to_string()),
                &[7, 8]
            ),
            15
        );
        assert_eq!(
            evaluate_cells_formula_with_ir(
                &CellsFormula::parse("=sum(A1:A3)".to_string()),
                &[5, 6, 7]
            ),
            18
        );
        assert_eq!(
            evaluate_cells_formula_with_ir(&CellsFormula::parse("=sum(A1:A0)".to_string()), &[]),
            0
        );
    }

    #[test]
    fn parsed_formula_keeps_dependency_order_for_compiled_inputs() {
        assert_eq!(
            CellsFormula::parse("=add(C2,A1)".to_string()).dependencies(),
            vec![(2, 3), (1, 1)]
        );
        assert_eq!(
            CellsFormula::parse("=sum(B3:B5)".to_string()).dependencies(),
            vec![(3, 2), (4, 2), (5, 2)]
        );
    }
}
