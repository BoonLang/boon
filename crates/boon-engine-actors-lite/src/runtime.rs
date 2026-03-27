use crate::clock::MonotonicInstant;
use crate::ids::{ActorId, GenerationalId, ScopeId};
use crate::ir::{IrProgram, MirrorCellId, NodeId, SinkPortId, SourcePortId};
use crate::ir_executor::IrExecutor;
use crate::semantics::CausalSeq;
use boon::platform::browser::kernel::KernelValue;
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::marker::PhantomData;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    ValueCell,
    Pulse,
    Queue,
    ListStore,
    SwitchGate,
    SourcePort,
    SinkPort,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    SourcePulse {
        port: SourcePortId,
        value: KernelValue,
        seq: CausalSeq,
    },
    MirrorWrite {
        cell: MirrorCellId,
        value: KernelValue,
        seq: CausalSeq,
    },
    Recompute,
    BindLink {
        target: NodeId,
        edge_epoch: u32,
    },
    UnbindLink {
        target: NodeId,
        edge_epoch: u32,
    },
}

#[derive(Debug, Clone)]
pub struct SubscriptionEdge {
    pub source: ActorId,
    pub cutoff_seq: CausalSeq,
    pub edge_epoch: u32,
}

impl SubscriptionEdge {
    #[must_use]
    pub fn accepts(&self, current_edge_epoch: u32, source_seq: CausalSeq) -> bool {
        self.edge_epoch == current_edge_epoch && source_seq > self.cutoff_seq
    }
}

#[derive(Debug, Clone)]
pub struct ActorSlot {
    pub kind: ActorKind,
    pub mailbox: VecDeque<Msg>,
    pub scheduled: bool,
    pub scope_id: ScopeId,
    pub subscriptions: Vec<SubscriptionEdge>,
}

#[derive(Debug, Clone, Default)]
pub struct ScopeSlot {
    pub parent: Option<ScopeId>,
    pub children: Vec<ScopeId>,
    pub actors: Vec<ActorId>,
}

#[derive(Debug, Clone)]
struct ArenaSlot<T> {
    generation: u32,
    value: Option<T>,
}

#[derive(Debug, Clone)]
pub struct SlotArena<Id, T> {
    slots: Vec<ArenaSlot<T>>,
    free: Vec<u32>,
    live: usize,
    marker: PhantomData<Id>,
}

impl<Id, T> Default for SlotArena<Id, T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            marker: PhantomData,
        }
    }
}

impl<Id, T> SlotArena<Id, T>
where
    Id: GenerationalId,
{
    pub fn alloc(&mut self, value: T) -> Id {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            self.live += 1;
            return Id::new(index, slot.generation);
        }

        let index = self.slots.len() as u32;
        self.slots.push(ArenaSlot {
            generation: 0,
            value: Some(value),
        });
        self.live += 1;
        Id::new(index, 0)
    }

    #[must_use]
    pub fn live_len(&self) -> usize {
        self.live
    }

    pub fn contains(&self, id: Id) -> bool {
        self.slots
            .get(id.index())
            .is_some_and(|slot| slot.generation == id.generation() && slot.value.is_some())
    }

    pub fn get(&self, id: Id) -> Option<&T> {
        let slot = self.slots.get(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.value.as_ref()
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        let slot = self.slots.get_mut(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.value.as_mut()
    }

    pub fn remove(&mut self, id: Id) -> Option<T> {
        let slot = self.slots.get_mut(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }

        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.index() as u32);
        self.live = self.live.saturating_sub(1);
        Some(value)
    }
}

#[derive(Debug, Clone, Default)]
struct RuntimeTelemetry {
    actor_creation_samples: Vec<Duration>,
    send_samples: Vec<Duration>,
    send_count: usize,
    peak_actor_count: usize,
    peak_ready_queue_depth: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeTelemetrySnapshot {
    pub actor_creation_samples: Vec<Duration>,
    pub send_samples: Vec<Duration>,
    pub send_count: usize,
    pub peak_actor_count: usize,
    pub peak_ready_queue_depth: usize,
}

#[derive(Debug, Default)]
pub struct RuntimeCore {
    pub actors: SlotArena<ActorId, ActorSlot>,
    pub scopes: SlotArena<ScopeId, ScopeSlot>,
    pub ready: VecDeque<ActorId>,
    telemetry: RuntimeTelemetry,
}

impl RuntimeCore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let scope_id = self.scopes.alloc(ScopeSlot {
            parent,
            ..ScopeSlot::default()
        });

        if let Some(parent_id) = parent {
            if let Some(parent_slot) = self.scopes.get_mut(parent_id) {
                parent_slot.children.push(scope_id);
            }
        }

        scope_id
    }

    pub fn alloc_actor(&mut self, kind: ActorKind, scope_id: ScopeId) -> ActorId {
        let started = MonotonicInstant::now();
        let actor_id = self.actors.alloc(ActorSlot {
            kind,
            mailbox: VecDeque::new(),
            scheduled: false,
            scope_id,
            subscriptions: Vec::new(),
        });

        if let Some(scope) = self.scopes.get_mut(scope_id) {
            scope.actors.push(actor_id);
        }

        self.telemetry
            .actor_creation_samples
            .push(started.elapsed());
        self.telemetry.peak_actor_count =
            self.telemetry.peak_actor_count.max(self.actors.live_len());

        actor_id
    }

    pub fn push_message(&mut self, actor_id: ActorId, msg: Msg) -> bool {
        let started = MonotonicInstant::now();
        let Some(actor) = self.actors.get_mut(actor_id) else {
            return false;
        };

        actor.mailbox.push_back(msg);
        if !actor.scheduled {
            actor.scheduled = true;
            self.ready.push_back(actor_id);
        }
        self.telemetry.send_count += 1;
        self.telemetry.send_samples.push(started.elapsed());
        self.telemetry.peak_ready_queue_depth =
            self.telemetry.peak_ready_queue_depth.max(self.ready.len());
        true
    }

    pub fn pop_ready(&mut self) -> Option<ActorId> {
        self.ready.pop_front()
    }

    pub fn mark_unscheduled_if_idle(&mut self, actor_id: ActorId) {
        if let Some(actor) = self.actors.get_mut(actor_id) {
            if actor.mailbox.is_empty() {
                actor.scheduled = false;
            } else {
                self.ready.push_back(actor_id);
                self.telemetry.peak_ready_queue_depth =
                    self.telemetry.peak_ready_queue_depth.max(self.ready.len());
            }
        }
    }

    #[must_use]
    pub fn telemetry_snapshot(&self) -> RuntimeTelemetrySnapshot {
        RuntimeTelemetrySnapshot {
            actor_creation_samples: self.telemetry.actor_creation_samples.clone(),
            send_samples: self.telemetry.send_samples.clone(),
            send_count: self.telemetry.send_count,
            peak_actor_count: self.telemetry.peak_actor_count,
            peak_ready_queue_depth: self.telemetry.peak_ready_queue_depth,
        }
    }
}

pub fn dependency_closure<K>(roots: &[K], dependents: &BTreeMap<K, Vec<K>>) -> Vec<K>
where
    K: Copy + Ord + std::hash::Hash,
{
    let mut seen = HashSet::new();
    let mut stack = roots.to_vec();
    let mut closure = Vec::new();
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        closure.push(node);
        if let Some(next) = dependents.get(&node) {
            stack.extend(next.iter().copied());
        }
    }
    closure
}

pub fn affected_components_topo_order<K>(
    affected: &[K],
    dependencies: &BTreeMap<K, Vec<K>>,
    include_dependency: impl Fn(K) -> bool,
) -> Vec<Vec<K>>
where
    K: Copy + Ord + std::hash::Hash,
{
    fn strong_connect<K>(
        cell: K,
        affected_set: &HashSet<K>,
        dependencies: &BTreeMap<K, Vec<K>>,
        include_dependency: &impl Fn(K) -> bool,
        next_index: &mut usize,
        indices: &mut BTreeMap<K, usize>,
        lowlinks: &mut BTreeMap<K, usize>,
        stack: &mut Vec<K>,
        on_stack: &mut HashSet<K>,
        components: &mut Vec<Vec<K>>,
    ) where
        K: Copy + Ord + std::hash::Hash,
    {
        let current_index = *next_index;
        *next_index += 1;
        indices.insert(cell, current_index);
        lowlinks.insert(cell, current_index);
        stack.push(cell);
        on_stack.insert(cell);

        let deps = dependencies
            .get(&cell)
            .into_iter()
            .flat_map(|items| items.iter().copied())
            .filter(|dependency| {
                affected_set.contains(dependency) && include_dependency(*dependency)
            })
            .collect::<BTreeSet<_>>();

        for dependency in &deps {
            if !indices.contains_key(dependency) {
                strong_connect(
                    *dependency,
                    affected_set,
                    dependencies,
                    include_dependency,
                    next_index,
                    indices,
                    lowlinks,
                    stack,
                    on_stack,
                    components,
                );
                let dependency_lowlink = *lowlinks.get(dependency).unwrap_or(&current_index);
                if let Some(lowlink) = lowlinks.get_mut(&cell) {
                    *lowlink = (*lowlink).min(dependency_lowlink);
                }
            } else if on_stack.contains(dependency) {
                let dependency_index = *indices.get(dependency).unwrap_or(&current_index);
                if let Some(lowlink) = lowlinks.get_mut(&cell) {
                    *lowlink = (*lowlink).min(dependency_index);
                }
            }
        }

        if lowlinks.get(&cell) == indices.get(&cell) {
            let mut component = Vec::new();
            while let Some(member) = stack.pop() {
                on_stack.remove(&member);
                component.push(member);
                if member == cell {
                    break;
                }
            }
            components.push(component);
        }
    }

    let affected_set = affected.iter().copied().collect::<HashSet<_>>();
    let order = affected
        .iter()
        .copied()
        .enumerate()
        .map(|(index, cell)| (cell, index))
        .collect::<BTreeMap<_, _>>();
    let mut next_index = 0usize;
    let mut indices = BTreeMap::new();
    let mut lowlinks = BTreeMap::new();
    let mut stack = Vec::new();
    let mut on_stack = HashSet::new();
    let mut components = Vec::new();

    for &cell in affected {
        if !indices.contains_key(&cell) {
            strong_connect(
                cell,
                &affected_set,
                dependencies,
                &include_dependency,
                &mut next_index,
                &mut indices,
                &mut lowlinks,
                &mut stack,
                &mut on_stack,
                &mut components,
            );
        }
    }

    for component in &mut components {
        component.sort_by_key(|cell| order.get(cell).copied().unwrap_or(usize::MAX));
    }

    let mut component_index_by_cell = BTreeMap::new();
    for (component_index, component) in components.iter().enumerate() {
        for &cell in component {
            component_index_by_cell.insert(cell, component_index);
        }
    }

    let mut component_edges = vec![BTreeSet::<usize>::new(); components.len()];
    let mut indegrees = vec![0usize; components.len()];
    for (component_index, component) in components.iter().enumerate() {
        for &cell in component {
            for dependency in dependencies
                .get(&cell)
                .into_iter()
                .flat_map(|items| items.iter().copied())
                .filter(|dependency| {
                    affected_set.contains(dependency) && include_dependency(*dependency)
                })
            {
                let dependency_component = component_index_by_cell[&dependency];
                if dependency_component != component_index
                    && component_edges[dependency_component].insert(component_index)
                {
                    indegrees[component_index] += 1;
                }
            }
        }
    }

    let mut ready = indegrees
        .iter()
        .enumerate()
        .filter_map(|(index, indegree)| (*indegree == 0).then_some(index))
        .collect::<Vec<_>>();
    ready.sort_by_key(|component_index| {
        std::cmp::Reverse(
            components[*component_index]
                .first()
                .and_then(|cell| order.get(cell))
                .copied()
                .unwrap_or(usize::MAX),
        )
    });

    let mut ordered_components = Vec::new();
    while let Some(component_index) = ready.pop() {
        ordered_components.push(components[component_index].clone());
        for &dependent in &component_edges[component_index] {
            indegrees[dependent] -= 1;
            if indegrees[dependent] == 0 {
                ready.push(dependent);
            }
        }
        ready.sort_by_key(|component_index| {
            std::cmp::Reverse(
                components[*component_index]
                    .first()
                    .and_then(|cell| order.get(cell))
                    .copied()
                    .unwrap_or(usize::MAX),
            )
        });
    }

    ordered_components
}

pub fn propagate_dirty_components<K, E, Eval>(
    roots: &BTreeSet<K>,
    ordered_components: &[Vec<K>],
    dependencies: &BTreeMap<K, Vec<K>>,
    mut evaluate_component: Eval,
) -> Result<BTreeSet<K>, E>
where
    K: Copy + Ord + std::hash::Hash,
    Eval: FnMut(&[K], &HashSet<K>) -> Result<BTreeSet<K>, E>,
{
    let mut changed_nodes = BTreeSet::new();

    for component in ordered_components {
        let component_set = component.iter().copied().collect::<HashSet<_>>();
        let should_evaluate_component = component.iter().any(|cell| roots.contains(cell))
            || component.iter().any(|cell| {
                dependencies
                    .get(cell)
                    .into_iter()
                    .flat_map(|items| items.iter().copied())
                    .any(|dependency| {
                        !component_set.contains(&dependency) && changed_nodes.contains(&dependency)
                    })
            });
        if !should_evaluate_component {
            continue;
        }

        changed_nodes.extend(evaluate_component(component, &component_set)?);
    }

    Ok(changed_nodes)
}

pub fn propagate_dirty_component_members<K, E, Eval>(
    roots: &BTreeSet<K>,
    ordered_components: &[Vec<K>],
    dependencies: &BTreeMap<K, Vec<K>>,
    mut evaluate_member: Eval,
) -> Result<BTreeSet<K>, E>
where
    K: Copy + Ord + std::hash::Hash,
    Eval: FnMut(K, &HashSet<K>, &HashSet<K>) -> Result<bool, E>,
{
    propagate_dirty_components(
        roots,
        ordered_components,
        dependencies,
        |component, component_set| {
            let mut changed_members = BTreeSet::new();
            let mut resolved_in_component = HashSet::new();
            for &member in component {
                if evaluate_member(member, component_set, &resolved_in_component)? {
                    changed_members.insert(member);
                }
                resolved_in_component.insert(member);
            }
            Ok(changed_members)
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropagatedValue<V> {
    pub value: V,
    pub seq: CausalSeq,
}

#[derive(Debug)]
pub struct RetainedMirrorEvaluator {
    executor: IrExecutor,
    sink_port: SinkPortId,
    mirror_cells: Vec<MirrorCellId>,
}

impl RetainedMirrorEvaluator {
    pub fn new(
        program: IrProgram,
        sink_port: SinkPortId,
        mirror_cells: Vec<MirrorCellId>,
    ) -> Result<Self, String> {
        Ok(Self {
            executor: IrExecutor::new(program)?,
            sink_port,
            mirror_cells,
        })
    }

    pub fn mirror_cells(&self) -> &[MirrorCellId] {
        &self.mirror_cells
    }

    pub fn apply_messages(&mut self, messages: &[(ActorId, Msg)]) -> Result<(), String> {
        self.executor.apply_messages(messages)
    }

    pub fn sink_value(&self) -> Option<&KernelValue> {
        self.executor.sink_value(self.sink_port)
    }
}

#[derive(Debug, Default)]
pub struct RetainedMemberRuntime<K>
where
    K: Copy + Ord,
{
    pub evaluators: BTreeMap<K, RetainedMirrorEvaluator>,
    pub mirror_cells: BTreeMap<K, MirrorCellId>,
    pub input_values: BTreeMap<K, KernelValue>,
    pub input_seqs: BTreeMap<K, CausalSeq>,
    pub output_seqs: BTreeMap<K, CausalSeq>,
    pub instance_ids: BTreeMap<K, u64>,
    #[cfg(test)]
    pub evaluation_counts: BTreeMap<K, u64>,
    pub next_seq: u64,
    pub next_instance_id: u64,
}

#[derive(Debug)]
pub struct RetainedMemberInstall<K>
where
    K: Copy + Ord,
{
    pub member: K,
    pub evaluator: RetainedMirrorEvaluator,
    pub mirror_cell: Option<MirrorCellId>,
    pub fallback_input_value: Option<KernelValue>,
}

#[derive(Debug)]
pub struct RetainedMemberProgram<K>
where
    K: Copy + Ord,
{
    pub member: K,
    pub program: IrProgram,
    pub sink_port: SinkPortId,
    pub dependency_mirrors: Vec<MirrorCellId>,
    pub mirror_cell: Option<MirrorCellId>,
    pub fallback_input_value: Option<KernelValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedNumberProgramKind {
    InputLeaf,
    Add2,
    SumList,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RetainedNumberMemberSpec {
    InputLeaf { value: KernelValue },
    Add2,
    SumList,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetainedNumberFormula {
    pub dependency_count: usize,
    pub spec: RetainedNumberMemberSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoweredRetainedNumberFormula<F> {
    formula: F,
    retained_number_formula: RetainedNumberFormula,
}

impl<F> LoweredRetainedNumberFormula<F> {
    pub fn new(formula: F, retained_number_formula: RetainedNumberFormula) -> Self {
        Self {
            formula,
            retained_number_formula,
        }
    }

    pub fn formula(&self) -> &F {
        &self.formula
    }

    pub fn retained_number_formula(&self) -> &RetainedNumberFormula {
        &self.retained_number_formula
    }
}

pub trait RetainedNumberMemberDescriptor {
    fn retained_number_member_dependency_count(&self) -> usize;
    fn retained_number_member_spec(&self) -> RetainedNumberMemberSpec;
}

impl RetainedNumberMemberDescriptor for RetainedNumberFormula {
    fn retained_number_member_dependency_count(&self) -> usize {
        self.dependency_count
    }

    fn retained_number_member_spec(&self) -> RetainedNumberMemberSpec {
        self.spec.clone()
    }
}

#[derive(Debug)]
pub struct RetainedNumberMemberPlan<K>
where
    K: Copy + Ord,
{
    pub member: K,
    pub stable_slot: u32,
    pub dependency_count: usize,
    pub kind: RetainedNumberProgramKind,
    pub fallback_input_value: Option<KernelValue>,
}

pub fn max_propagated_seq<V>(inputs: &[PropagatedValue<V>]) -> CausalSeq {
    inputs
        .iter()
        .map(|input| input.seq)
        .max()
        .unwrap_or_else(|| CausalSeq::new(0, 0))
}

pub fn stable_retained_mirror_cell(slot: u32) -> MirrorCellId {
    MirrorCellId(1_000_000 + slot)
}

pub fn stable_retained_pair_slot(primary: u32, secondary: u32) -> u32 {
    primary * 1_000 + secondary
}

pub fn stable_retained_sink_port(slot: u32) -> SinkPortId {
    SinkPortId(2_000_000 + slot)
}

pub fn stable_retained_dependency_mirror_cell(slot: u32, dependency_index: usize) -> MirrorCellId {
    MirrorCellId(4_000_000 + slot * 512 + dependency_index as u32)
}

pub fn retained_number_member_input_value(spec: &RetainedNumberMemberSpec) -> Option<KernelValue> {
    match spec {
        RetainedNumberMemberSpec::InputLeaf { value } => Some(value.clone()),
        RetainedNumberMemberSpec::Add2 | RetainedNumberMemberSpec::SumList => None,
    }
}

pub fn retained_number_formula_input_value(formula: &RetainedNumberFormula) -> Option<KernelValue> {
    retained_number_member_input_value(&formula.spec)
}

pub fn retained_number_member_kind(spec: &RetainedNumberMemberSpec) -> RetainedNumberProgramKind {
    match spec {
        RetainedNumberMemberSpec::InputLeaf { .. } => RetainedNumberProgramKind::InputLeaf,
        RetainedNumberMemberSpec::Add2 => RetainedNumberProgramKind::Add2,
        RetainedNumberMemberSpec::SumList => RetainedNumberProgramKind::SumList,
    }
}

pub fn retained_number_formula_kind(formula: &RetainedNumberFormula) -> RetainedNumberProgramKind {
    retained_number_member_kind(&formula.spec)
}

pub fn build_pair_number_retained_member_plan(
    member: (u32, u32),
    dependency_count: usize,
    spec: RetainedNumberMemberSpec,
) -> RetainedNumberMemberPlan<(u32, u32)> {
    RetainedNumberMemberPlan {
        member,
        stable_slot: stable_retained_pair_slot(member.0, member.1),
        dependency_count,
        kind: retained_number_member_kind(&spec),
        fallback_input_value: retained_number_member_input_value(&spec),
    }
}

pub fn build_number_retained_member_program<K>(
    member: K,
    stable_slot: u32,
    dependency_count: usize,
    kind: RetainedNumberProgramKind,
    fallback_input_value: Option<KernelValue>,
) -> Result<RetainedMemberProgram<K>, String>
where
    K: Copy + Ord,
{
    let sink_port = stable_retained_sink_port(stable_slot);
    let mut nodes = Vec::new();

    if matches!(kind, RetainedNumberProgramKind::InputLeaf) {
        let mirror_cell = stable_retained_mirror_cell(stable_slot);
        let mirror_node = NodeId(1);
        nodes.push(crate::ir::IrNode {
            id: mirror_node,
            source_expr: None,
            kind: crate::ir::IrNodeKind::MirrorCell(mirror_cell),
        });
        nodes.push(crate::ir::IrNode {
            id: NodeId(2),
            source_expr: None,
            kind: crate::ir::IrNodeKind::SinkPort {
                port: sink_port,
                input: mirror_node,
            },
        });
        return Ok(RetainedMemberProgram {
            member,
            program: IrProgram::from(nodes),
            sink_port,
            dependency_mirrors: vec![mirror_cell],
            mirror_cell: Some(mirror_cell),
            fallback_input_value,
        });
    }

    let dependency_mirrors = (0..dependency_count)
        .map(|dependency_index| {
            stable_retained_dependency_mirror_cell(stable_slot, dependency_index)
        })
        .collect::<Vec<_>>();
    let dependency_nodes = dependency_mirrors
        .iter()
        .enumerate()
        .map(|(dependency_index, mirror_cell)| {
            let node_id = NodeId((dependency_index + 1) as u32);
            nodes.push(crate::ir::IrNode {
                id: node_id,
                source_expr: None,
                kind: crate::ir::IrNodeKind::MirrorCell(*mirror_cell),
            });
            node_id
        })
        .collect::<Vec<_>>();
    let mut next_node_id = dependency_nodes.len() as u32 + 1;
    let push_literal = |value: i64, nodes: &mut Vec<crate::ir::IrNode>, next_node_id: &mut u32| {
        let node_id = NodeId(*next_node_id);
        *next_node_id += 1;
        nodes.push(crate::ir::IrNode {
            id: node_id,
            source_expr: None,
            kind: crate::ir::IrNodeKind::Literal(KernelValue::from(value as f64)),
        });
        node_id
    };
    let output = match kind {
        RetainedNumberProgramKind::InputLeaf => unreachable!(),
        RetainedNumberProgramKind::Add2 => {
            if dependency_nodes.len() < 2 {
                return Err("Add2 retained program requires two dependencies".to_string());
            }
            let output = NodeId(next_node_id);
            next_node_id += 1;
            nodes.push(crate::ir::IrNode {
                id: output,
                source_expr: None,
                kind: crate::ir::IrNodeKind::Add {
                    lhs: dependency_nodes[0],
                    rhs: dependency_nodes[1],
                },
            });
            output
        }
        RetainedNumberProgramKind::SumList => {
            let list = NodeId(next_node_id);
            next_node_id += 1;
            nodes.push(crate::ir::IrNode {
                id: list,
                source_expr: None,
                kind: crate::ir::IrNodeKind::ListLiteral {
                    items: dependency_nodes,
                },
            });
            let output = NodeId(next_node_id);
            next_node_id += 1;
            nodes.push(crate::ir::IrNode {
                id: output,
                source_expr: None,
                kind: crate::ir::IrNodeKind::ListSum { list },
            });
            output
        }
    };
    let sink = NodeId(next_node_id);
    nodes.push(crate::ir::IrNode {
        id: sink,
        source_expr: None,
        kind: crate::ir::IrNodeKind::SinkPort {
            port: sink_port,
            input: output,
        },
    });
    let _ = push_literal; // keep local builder shape symmetric with leaf path extensions
    Ok(RetainedMemberProgram {
        member,
        program: IrProgram::from(nodes),
        sink_port,
        dependency_mirrors,
        mirror_cell: None,
        fallback_input_value: None,
    })
}

pub fn build_number_retained_member_program_from_plan<K>(
    plan: RetainedNumberMemberPlan<K>,
) -> Result<RetainedMemberProgram<K>, String>
where
    K: Copy + Ord,
{
    build_number_retained_member_program(
        plan.member,
        plan.stable_slot,
        plan.dependency_count,
        plan.kind,
        plan.fallback_input_value,
    )
}

pub fn build_pair_number_retained_member_program(
    member: (u32, u32),
    dependency_count: usize,
    spec: RetainedNumberMemberSpec,
) -> Result<RetainedMemberProgram<(u32, u32)>, String> {
    build_number_retained_member_program_from_plan(build_pair_number_retained_member_plan(
        member,
        dependency_count,
        spec,
    ))
}

pub fn build_pair_number_retained_member_program_from_descriptor<D>(
    member: (u32, u32),
    descriptor: &D,
) -> Result<RetainedMemberProgram<(u32, u32)>, String>
where
    D: RetainedNumberMemberDescriptor,
{
    build_pair_number_retained_member_program(
        member,
        descriptor.retained_number_member_dependency_count(),
        descriptor.retained_number_member_spec(),
    )
}

pub fn mirror_write_messages_from_inputs<V>(
    actor: ActorId,
    mirror_cells: &[MirrorCellId],
    inputs: &[PropagatedValue<V>],
    encode_value: impl Fn(&V) -> KernelValue,
) -> Vec<(ActorId, Msg)> {
    mirror_cells
        .iter()
        .copied()
        .zip(inputs.iter())
        .map(|(cell, input)| {
            (
                actor,
                Msg::MirrorWrite {
                    cell,
                    value: encode_value(&input.value),
                    seq: input.seq,
                },
            )
        })
        .collect()
}

pub fn evaluate_mirror_driven_member<V, O, Encode, Read>(
    actor: ActorId,
    evaluator: &mut RetainedMirrorEvaluator,
    inputs: &[PropagatedValue<V>],
    encode_value: Encode,
    read_output: Read,
) -> Result<PropagatedValue<O>, String>
where
    Encode: Fn(&V) -> KernelValue,
    Read: FnOnce(&RetainedMirrorEvaluator) -> O,
{
    let messages =
        mirror_write_messages_from_inputs(actor, evaluator.mirror_cells(), inputs, encode_value);
    Ok(PropagatedValue {
        value: {
            if !messages.is_empty() {
                evaluator.apply_messages(&messages)?;
            }
            read_output(evaluator)
        },
        seq: max_propagated_seq(inputs),
    })
}

pub fn evaluate_retained_member<V, O, Encode, Read>(
    actor: ActorId,
    evaluator: &mut RetainedMirrorEvaluator,
    inputs: &[PropagatedValue<V>],
    input_seq: Option<CausalSeq>,
    encode_value: Encode,
    read_output: Read,
) -> Result<PropagatedValue<O>, String>
where
    Encode: Fn(&V) -> KernelValue,
    Read: FnOnce(&RetainedMirrorEvaluator) -> O,
{
    if inputs.is_empty() {
        return Ok(PropagatedValue {
            value: read_output(evaluator),
            seq: input_seq.unwrap_or_else(|| CausalSeq::new(0, 0)),
        });
    }

    evaluate_mirror_driven_member(actor, evaluator, inputs, encode_value, read_output)
}

pub fn retained_sink_number_or_zero(evaluator: &RetainedMirrorEvaluator) -> i64 {
    match evaluator.sink_value() {
        Some(KernelValue::Number(value)) => *value as i64,
        _ => 0,
    }
}

pub fn retained_runtime_with_preserved_inputs<K>(
    previous: Option<&RetainedMemberRuntime<K>>,
) -> RetainedMemberRuntime<K>
where
    K: Copy + Ord,
{
    RetainedMemberRuntime {
        evaluators: BTreeMap::new(),
        mirror_cells: BTreeMap::new(),
        input_values: previous
            .map(|runtime| runtime.input_values.clone())
            .unwrap_or_default(),
        input_seqs: previous
            .map(|runtime| runtime.input_seqs.clone())
            .unwrap_or_default(),
        output_seqs: BTreeMap::new(),
        instance_ids: BTreeMap::new(),
        #[cfg(test)]
        evaluation_counts: BTreeMap::new(),
        next_seq: previous.map_or(0, |runtime| runtime.next_seq),
        next_instance_id: previous.map_or(1, |runtime| runtime.next_instance_id),
    }
}

pub fn rebuild_retained_runtime<K>(
    previous: Option<&RetainedMemberRuntime<K>>,
    actor: ActorId,
    programs: impl IntoIterator<Item = RetainedMemberProgram<K>>,
) -> Result<RetainedMemberRuntime<K>, String>
where
    K: Copy + Ord,
{
    let mut retained = retained_runtime_with_preserved_inputs(previous);
    let installs = build_retained_member_installs(programs)?;
    install_retained_members(&mut retained, actor, installs)?;
    Ok(retained)
}

pub fn build_retained_member_install<K>(
    program: RetainedMemberProgram<K>,
) -> Result<RetainedMemberInstall<K>, String>
where
    K: Copy + Ord,
{
    Ok(RetainedMemberInstall {
        member: program.member,
        evaluator: RetainedMirrorEvaluator::new(
            program.program,
            program.sink_port,
            program.dependency_mirrors,
        )?,
        mirror_cell: program.mirror_cell,
        fallback_input_value: program.fallback_input_value,
    })
}

pub fn build_retained_member_installs<K>(
    programs: impl IntoIterator<Item = RetainedMemberProgram<K>>,
) -> Result<Vec<RetainedMemberInstall<K>>, String>
where
    K: Copy + Ord,
{
    programs
        .into_iter()
        .map(build_retained_member_install)
        .collect()
}

pub fn remove_retained_member<K>(retained: &mut RetainedMemberRuntime<K>, member: K)
where
    K: Copy + Ord,
{
    retained.evaluators.remove(&member);
    retained.mirror_cells.remove(&member);
    retained.input_values.remove(&member);
    retained.input_seqs.remove(&member);
    retained.output_seqs.remove(&member);
    retained.instance_ids.remove(&member);
    #[cfg(test)]
    retained.evaluation_counts.remove(&member);
}

pub fn install_retained_member<K>(
    retained: &mut RetainedMemberRuntime<K>,
    actor: ActorId,
    member: K,
    evaluator: RetainedMirrorEvaluator,
    mirror_cell: Option<MirrorCellId>,
    fallback_input_value: Option<KernelValue>,
) -> Result<u64, String>
where
    K: Copy + Ord,
{
    let instance_id = retained.next_instance_id;
    retained.next_instance_id += 1;
    retained.evaluators.insert(member, evaluator);
    retained.instance_ids.insert(member, instance_id);
    #[cfg(test)]
    retained.evaluation_counts.insert(member, 0);

    if let Some(mirror_cell) = mirror_cell {
        retained.mirror_cells.insert(member, mirror_cell);
        let value = retained
            .input_values
            .get(&member)
            .cloned()
            .or(fallback_input_value)
            .ok_or_else(|| "missing retained input value".to_string())?;
        retained.input_values.insert(member, value.clone());
        let seq = retained
            .input_seqs
            .get(&member)
            .copied()
            .unwrap_or_else(|| {
                retained.next_seq += 1;
                let seq = CausalSeq::new(retained.next_seq, 0);
                retained.input_seqs.insert(member, seq);
                seq
            });
        retained
            .evaluators
            .get_mut(&member)
            .expect("retained evaluator exists after install")
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: mirror_cell,
                    value,
                    seq,
                },
            )])?;
    } else {
        retained.mirror_cells.remove(&member);
        retained.input_values.remove(&member);
        retained.input_seqs.remove(&member);
    }

    Ok(instance_id)
}

pub fn install_retained_members<K>(
    retained: &mut RetainedMemberRuntime<K>,
    actor: ActorId,
    installs: impl IntoIterator<Item = RetainedMemberInstall<K>>,
) -> Result<(), String>
where
    K: Copy + Ord,
{
    for install in installs {
        install_retained_member(
            retained,
            actor,
            install.member,
            install.evaluator,
            install.mirror_cell,
            install.fallback_input_value,
        )?;
    }
    Ok(())
}

pub fn replace_retained_member<K>(
    retained: &mut RetainedMemberRuntime<K>,
    actor: ActorId,
    member: K,
    install: Option<RetainedMemberInstall<K>>,
) -> Result<(), String>
where
    K: Copy + Ord,
{
    let preserved_input_value = retained.input_values.get(&member).cloned();
    let preserved_input_seq = retained.input_seqs.get(&member).copied();
    let preserved_output_seq = retained.output_seqs.get(&member).copied();
    remove_retained_member(retained, member);
    if let Some(install) = install {
        if install.mirror_cell.is_some() {
            if let Some(value) = preserved_input_value {
                retained.input_values.insert(member, value);
            }
            if let Some(seq) = preserved_input_seq {
                retained.input_seqs.insert(member, seq);
            }
        }
        if let Some(seq) = preserved_output_seq {
            retained.output_seqs.insert(member, seq);
        }
        install_retained_member(
            retained,
            actor,
            install.member,
            install.evaluator,
            install.mirror_cell,
            install.fallback_input_value,
        )?;
    }
    Ok(())
}

pub fn replace_retained_member_program<K>(
    retained: &mut RetainedMemberRuntime<K>,
    actor: ActorId,
    member: K,
    program: Option<RetainedMemberProgram<K>>,
) -> Result<(), String>
where
    K: Copy + Ord,
{
    let install = match program {
        Some(program) => Some(build_retained_member_install(program)?),
        None => None,
    };
    replace_retained_member(retained, actor, member, install)
}

pub fn patch_retained_input<K>(
    retained: &mut RetainedMemberRuntime<K>,
    actor: ActorId,
    member: K,
    value: KernelValue,
) -> Result<CausalSeq, String>
where
    K: Copy + Ord,
{
    let mirror_cell = *retained
        .mirror_cells
        .get(&member)
        .ok_or_else(|| "missing retained mirror cell".to_string())?;
    retained.next_seq += 1;
    let seq = CausalSeq::new(retained.next_seq, 0);
    retained.input_values.insert(member, value.clone());
    retained.input_seqs.insert(member, seq);
    retained
        .evaluators
        .get_mut(&member)
        .ok_or_else(|| "missing retained evaluator".to_string())?
        .apply_messages(&[(
            actor,
            Msg::MirrorWrite {
                cell: mirror_cell,
                value,
                seq,
            },
        )])?;
    Ok(seq)
}

pub fn apply_dependency_member<K, V, E, Eval>(
    member: K,
    member_dependencies: &[K],
    component_set: &HashSet<K>,
    resolved_in_component: &HashSet<K>,
    output_states: &mut BTreeMap<K, PropagatedValue<V>>,
    unresolved: &PropagatedValue<V>,
    mut evaluate_member: Eval,
) -> Result<bool, E>
where
    K: Copy + Ord + std::hash::Hash,
    V: Clone + PartialEq,
    Eval: FnMut(&[PropagatedValue<V>]) -> Result<PropagatedValue<V>, E>,
{
    let dependency_inputs = member_dependencies
        .iter()
        .copied()
        .map(|dependency| {
            if component_set.contains(&dependency) && !resolved_in_component.contains(&dependency) {
                unresolved.clone()
            } else {
                output_states
                    .get(&dependency)
                    .cloned()
                    .unwrap_or_else(|| unresolved.clone())
            }
        })
        .collect::<Vec<_>>();
    let next_state = evaluate_member(&dependency_inputs)?;
    let previous_state = output_states.get(&member).cloned();
    output_states.insert(member, next_state.clone());
    Ok(previous_state.as_ref() != Some(&next_state))
}

pub fn recompute_retained_members<K, V, E, Eval>(
    roots: &BTreeSet<K>,
    ordered_components: &[Vec<K>],
    dependencies: &BTreeMap<K, Vec<K>>,
    retained: &mut RetainedMemberRuntime<K>,
    values: &mut BTreeMap<K, V>,
    unresolved: PropagatedValue<V>,
    mut evaluate_member: Eval,
) -> Result<(), E>
where
    K: Copy + Ord + std::hash::Hash,
    V: Clone + PartialEq,
    Eval: FnMut(
        K,
        &mut RetainedMirrorEvaluator,
        &[PropagatedValue<V>],
        Option<CausalSeq>,
    ) -> Result<PropagatedValue<V>, E>,
{
    let mut output_states = values
        .iter()
        .map(|(member, value)| {
            (
                *member,
                PropagatedValue {
                    value: value.clone(),
                    seq: retained
                        .output_seqs
                        .get(member)
                        .copied()
                        .unwrap_or_else(|| CausalSeq::new(0, 0)),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    propagate_dirty_component_members(
        roots,
        ordered_components,
        dependencies,
        |member, component_set, resolved_in_component| {
            let input_seq = retained.input_seqs.get(&member).copied();
            #[cfg(test)]
            {
                *retained.evaluation_counts.entry(member).or_insert(0) += 1;
            }
            let evaluator = retained
                .evaluators
                .get_mut(&member)
                .expect("retained evaluator exists for dependency member");
            let changed = apply_dependency_member(
                member,
                dependencies.get(&member).map(Vec::as_slice).unwrap_or(&[]),
                component_set,
                resolved_in_component,
                &mut output_states,
                &unresolved,
                |dependency_inputs| {
                    evaluate_member(member, evaluator, dependency_inputs, input_seq)
                },
            )?;
            if let Some(state) = output_states.get(&member) {
                values.insert(member, state.value.clone());
                retained.output_seqs.insert(member, state.seq);
            }
            Ok(changed)
        },
    )
    .map(|_| ())
}

pub fn recompute_number_retained_members<K>(
    actor: ActorId,
    roots: &BTreeSet<K>,
    ordered_components: &[Vec<K>],
    dependencies: &BTreeMap<K, Vec<K>>,
    retained: &mut RetainedMemberRuntime<K>,
    values: &mut BTreeMap<K, i64>,
) -> Result<(), String>
where
    K: Copy + Ord + std::hash::Hash,
{
    recompute_retained_members(
        roots,
        ordered_components,
        dependencies,
        retained,
        values,
        PropagatedValue {
            value: 0i64,
            seq: CausalSeq::new(0, 0),
        },
        |_, evaluator, dependency_inputs, input_seq| {
            evaluate_retained_member(
                actor,
                evaluator,
                dependency_inputs,
                input_seq,
                |value| KernelValue::from(*value as f64),
                retained_sink_number_or_zero,
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_actor_id_is_rejected_after_slot_reuse() {
        let mut arena = SlotArena::<ActorId, i32>::default();
        let first = arena.alloc(10);
        assert_eq!(arena.remove(first), Some(10));

        let second = arena.alloc(20);
        assert_ne!(first, second);
        assert_eq!(arena.get(first), None);
        assert_eq!(arena.get(second), Some(&20));
    }

    #[test]
    fn stale_scope_id_is_rejected_after_slot_reuse() {
        let mut arena = SlotArena::<ScopeId, i32>::default();
        let first = arena.alloc(1);
        assert_eq!(arena.remove(first), Some(1));

        let second = arena.alloc(2);
        assert_ne!(first, second);
        assert_eq!(arena.get(first), None);
        assert_eq!(arena.get(second), Some(&2));
    }

    #[test]
    fn ready_queue_schedules_actor_only_once_while_mailbox_is_non_empty() {
        let mut runtime = RuntimeCore::new();
        let root = runtime.alloc_scope(None);
        let actor = runtime.alloc_actor(ActorKind::Pulse, root);

        assert!(runtime.push_message(
            actor,
            Msg::SourcePulse {
                port: SourcePortId(1),
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 0),
            }
        ));
        assert!(runtime.push_message(
            actor,
            Msg::SourcePulse {
                port: SourcePortId(1),
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 1),
            }
        ));

        assert_eq!(runtime.ready.len(), 1);
        assert_eq!(runtime.pop_ready(), Some(actor));
    }

    #[test]
    fn subscription_edge_accepts_only_newer_messages_for_same_epoch() {
        let edge = SubscriptionEdge {
            source: ActorId {
                index: 0,
                generation: 0,
            },
            cutoff_seq: CausalSeq::new(8, 2),
            edge_epoch: 5,
        };

        assert!(!edge.accepts(4, CausalSeq::new(8, 3)));
        assert!(!edge.accepts(5, CausalSeq::new(8, 2)));
        assert!(edge.accepts(5, CausalSeq::new(8, 3)));
    }

    #[test]
    fn dependency_closure_walks_only_transitive_dependents() {
        let dependents = BTreeMap::from([(1, vec![2, 3]), (2, vec![4]), (5, vec![6])]);

        let closure = dependency_closure(&[1], &dependents);

        assert_eq!(closure, vec![1, 3, 2, 4]);
    }

    #[test]
    fn affected_components_topo_order_orders_sccs_by_dependency() {
        let dependencies =
            BTreeMap::from([(1, vec![]), (2, vec![1]), (3, vec![2, 3]), (4, vec![3])]);

        let components = affected_components_topo_order(&[1, 2, 3, 4], &dependencies, |_| true);

        assert_eq!(components, vec![vec![1], vec![2], vec![3], vec![4]]);
    }

    #[test]
    fn propagate_dirty_components_skips_downstream_when_no_dependency_changed() {
        let dependencies = BTreeMap::from([(1, vec![]), (2, vec![1]), (3, vec![2])]);
        let ordered_components = vec![vec![1], vec![2], vec![3]];
        let roots = BTreeSet::from([1]);
        let mut visited = Vec::new();

        let changed = propagate_dirty_components(
            &roots,
            &ordered_components,
            &dependencies,
            |component, _component_set| {
                visited.push(component[0]);
                Ok::<_, ()>(if component[0] == 1 {
                    BTreeSet::from([1])
                } else {
                    BTreeSet::new()
                })
            },
        )
        .expect("propagation succeeds");

        assert_eq!(visited, vec![1, 2]);
        assert_eq!(changed, BTreeSet::from([1]));
    }

    #[test]
    fn propagate_dirty_component_members_tracks_resolved_members_within_component() {
        let dependencies = BTreeMap::from([(1, vec![]), (2, vec![1]), (3, vec![2])]);
        let ordered_components = vec![vec![1, 2], vec![3]];
        let roots = BTreeSet::from([1]);
        let mut seen_resolved = Vec::new();

        let changed = propagate_dirty_component_members(
            &roots,
            &ordered_components,
            &dependencies,
            |member, _component_set, resolved_in_component| {
                seen_resolved.push((
                    member,
                    resolved_in_component.iter().copied().collect::<Vec<_>>(),
                ));
                Ok::<_, ()>(member == 1)
            },
        )
        .expect("propagation succeeds");

        assert_eq!(seen_resolved, vec![(1, vec![]), (2, vec![1])]);
        assert_eq!(changed, BTreeSet::from([1]));
    }

    #[test]
    fn apply_dependency_member_uses_resolved_component_values_and_tracks_change() {
        let mut output_states = BTreeMap::from([(
            1,
            PropagatedValue {
                value: 5i64,
                seq: CausalSeq::new(1, 0),
            },
        )]);
        let unresolved = PropagatedValue {
            value: 0i64,
            seq: CausalSeq::new(0, 0),
        };

        let changed = apply_dependency_member(
            2,
            &[1, 2],
            &HashSet::from([1, 2]),
            &HashSet::from([1]),
            &mut output_states,
            &unresolved,
            |inputs| {
                assert_eq!(
                    inputs,
                    &[
                        PropagatedValue {
                            value: 5,
                            seq: CausalSeq::new(1, 0),
                        },
                        PropagatedValue {
                            value: 0,
                            seq: CausalSeq::new(0, 0),
                        },
                    ]
                );
                Ok::<_, ()>(PropagatedValue {
                    value: 5,
                    seq: CausalSeq::new(1, 0),
                })
            },
        )
        .expect("evaluation succeeds");

        assert!(changed);
        assert_eq!(
            output_states.get(&2),
            Some(&PropagatedValue {
                value: 5,
                seq: CausalSeq::new(1, 0),
            })
        );
    }

    #[test]
    fn mirror_write_messages_from_inputs_preserves_actor_value_and_seq() {
        let actor = ActorId {
            index: 7,
            generation: 3,
        };
        let messages = mirror_write_messages_from_inputs(
            actor,
            &[MirrorCellId(10), MirrorCellId(11)],
            &[
                PropagatedValue {
                    value: 5i64,
                    seq: CausalSeq::new(1, 0),
                },
                PropagatedValue {
                    value: 8i64,
                    seq: CausalSeq::new(2, 0),
                },
            ],
            |value| KernelValue::from(*value as f64),
        );

        assert_eq!(
            messages,
            vec![
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: MirrorCellId(10),
                        value: KernelValue::from(5.0),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: MirrorCellId(11),
                        value: KernelValue::from(8.0),
                        seq: CausalSeq::new(2, 0),
                    },
                ),
            ]
        );
        assert_eq!(
            max_propagated_seq(&[
                PropagatedValue {
                    value: 5i64,
                    seq: CausalSeq::new(1, 0),
                },
                PropagatedValue {
                    value: 8i64,
                    seq: CausalSeq::new(2, 0),
                },
            ]),
            CausalSeq::new(2, 0)
        );
    }

    #[test]
    fn evaluate_mirror_driven_member_applies_messages_and_reads_output() {
        let actor = ActorId {
            index: 7,
            generation: 3,
        };
        let program = IrProgram::from(vec![
            crate::ir::IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(10)),
            },
            crate::ir::IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(11)),
            },
            crate::ir::IrNode {
                id: NodeId(3),
                source_expr: None,
                kind: crate::ir::IrNodeKind::Add {
                    lhs: NodeId(1),
                    rhs: NodeId(2),
                },
            },
            crate::ir::IrNode {
                id: NodeId(4),
                source_expr: None,
                kind: crate::ir::IrNodeKind::SinkPort {
                    port: SinkPortId(1),
                    input: NodeId(3),
                },
            },
        ]);
        let mut evaluator = RetainedMirrorEvaluator::new(
            program,
            SinkPortId(1),
            vec![MirrorCellId(10), MirrorCellId(11)],
        )
        .expect("retained evaluator");

        let state = evaluate_mirror_driven_member(
            actor,
            &mut evaluator,
            &[
                PropagatedValue {
                    value: 5i64,
                    seq: CausalSeq::new(1, 0),
                },
                PropagatedValue {
                    value: 8i64,
                    seq: CausalSeq::new(2, 0),
                },
            ],
            |value| KernelValue::from(*value as f64),
            |evaluator| match evaluator.sink_value() {
                Some(KernelValue::Number(value)) => *value as i64,
                _ => 0,
            },
        )
        .expect("evaluation succeeds");

        assert_eq!(state.value, 13);
        assert_eq!(state.seq, CausalSeq::new(2, 0));
    }

    #[test]
    fn evaluate_retained_member_uses_input_seq_for_leaf_member() {
        let actor = ActorId {
            index: 7,
            generation: 3,
        };
        let mut evaluator = RetainedMirrorEvaluator::new(
            IrProgram::from(vec![
                crate::ir::IrNode {
                    id: NodeId(1),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(10)),
                },
                crate::ir::IrNode {
                    id: NodeId(2),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::SinkPort {
                        port: SinkPortId(1),
                        input: NodeId(1),
                    },
                },
            ]),
            SinkPortId(1),
            vec![MirrorCellId(10)],
        )
        .expect("retained evaluator");
        evaluator
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: MirrorCellId(10),
                    value: KernelValue::from(5.0),
                    seq: CausalSeq::new(3, 0),
                },
            )])
            .expect("seed evaluator");

        let state = evaluate_retained_member(
            actor,
            &mut evaluator,
            &[],
            Some(CausalSeq::new(3, 0)),
            |value: &i64| KernelValue::from(*value as f64),
            |retained_evaluator| match retained_evaluator.sink_value() {
                Some(KernelValue::Number(value)) => *value as i64,
                _ => 0,
            },
        )
        .expect("evaluation succeeds");

        assert_eq!(state.value, 5);
        assert_eq!(state.seq, CausalSeq::new(3, 0));
    }

    #[test]
    fn rebuild_retained_runtime_reuses_preserved_leaf_input_state() {
        let actor = ActorId {
            index: 7,
            generation: 3,
        };
        let previous = RetainedMemberRuntime {
            evaluators: BTreeMap::new(),
            mirror_cells: BTreeMap::new(),
            input_values: BTreeMap::from([(1, KernelValue::from(5.0))]),
            input_seqs: BTreeMap::from([(1, CausalSeq::new(3, 0))]),
            output_seqs: BTreeMap::new(),
            instance_ids: BTreeMap::new(),
            #[cfg(test)]
            evaluation_counts: BTreeMap::new(),
            next_seq: 3,
            next_instance_id: 1,
        };
        let retained = rebuild_retained_runtime(
            Some(&previous),
            actor,
            [RetainedMemberProgram {
                member: 1,
                program: IrProgram::from(vec![
                    crate::ir::IrNode {
                        id: NodeId(1),
                        source_expr: None,
                        kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(10)),
                    },
                    crate::ir::IrNode {
                        id: NodeId(2),
                        source_expr: None,
                        kind: crate::ir::IrNodeKind::SinkPort {
                            port: SinkPortId(1),
                            input: NodeId(1),
                        },
                    },
                ]),
                sink_port: SinkPortId(1),
                dependency_mirrors: vec![MirrorCellId(10)],
                mirror_cell: Some(MirrorCellId(10)),
                fallback_input_value: Some(KernelValue::from(9.0)),
            }],
        )
        .expect("rebuild succeeds");

        assert_eq!(retained.input_values.get(&1), Some(&KernelValue::from(5.0)));
        assert_eq!(retained.input_seqs.get(&1), Some(&CausalSeq::new(3, 0)));
        assert_eq!(
            retained
                .evaluators
                .get(&1)
                .and_then(RetainedMirrorEvaluator::sink_value),
            Some(&KernelValue::from(5.0))
        );
    }

    #[test]
    fn build_pair_number_retained_member_program_uses_stable_pair_slot_ids_and_leaf_spec() {
        let program = build_pair_number_retained_member_program(
            (1u32, 3u32),
            0,
            RetainedNumberMemberSpec::InputLeaf {
                value: KernelValue::from(9.0),
            },
        )
        .expect("plan builds");

        assert_eq!(
            program.mirror_cell,
            Some(stable_retained_mirror_cell(stable_retained_pair_slot(1, 3)))
        );
        assert_eq!(
            program.sink_port,
            stable_retained_sink_port(stable_retained_pair_slot(1, 3))
        );
        assert_eq!(program.fallback_input_value, Some(KernelValue::from(9.0)));
    }

    #[test]
    fn build_pair_number_retained_member_program_from_descriptor_uses_runtime_descriptor_trait() {
        let program = build_pair_number_retained_member_program_from_descriptor(
            (2u32, 4u32),
            &RetainedNumberFormula {
                dependency_count: 2,
                spec: RetainedNumberMemberSpec::Add2,
            },
        )
        .expect("descriptor builds");

        assert_eq!(program.mirror_cell, None);
        assert_eq!(
            program.dependency_mirrors,
            vec![
                stable_retained_dependency_mirror_cell(stable_retained_pair_slot(2, 4), 0),
                stable_retained_dependency_mirror_cell(stable_retained_pair_slot(2, 4), 1),
            ]
        );
    }

    #[test]
    fn replace_retained_member_program_builds_install_and_seeds_leaf_input() {
        let actor = ActorId {
            index: 7,
            generation: 3,
        };
        let mut retained = RetainedMemberRuntime::<u32> {
            evaluators: BTreeMap::new(),
            mirror_cells: BTreeMap::new(),
            input_values: BTreeMap::from([(1, KernelValue::from(5.0))]),
            input_seqs: BTreeMap::from([(1, CausalSeq::new(3, 0))]),
            output_seqs: BTreeMap::new(),
            instance_ids: BTreeMap::new(),
            #[cfg(test)]
            evaluation_counts: BTreeMap::new(),
            next_seq: 3,
            next_instance_id: 1,
        };

        replace_retained_member_program(
            &mut retained,
            actor,
            1,
            Some(RetainedMemberProgram {
                member: 1,
                program: IrProgram::from(vec![
                    crate::ir::IrNode {
                        id: NodeId(1),
                        source_expr: None,
                        kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(10)),
                    },
                    crate::ir::IrNode {
                        id: NodeId(2),
                        source_expr: None,
                        kind: crate::ir::IrNodeKind::SinkPort {
                            port: SinkPortId(1),
                            input: NodeId(1),
                        },
                    },
                ]),
                sink_port: SinkPortId(1),
                dependency_mirrors: vec![MirrorCellId(10)],
                mirror_cell: Some(MirrorCellId(10)),
                fallback_input_value: Some(KernelValue::from(9.0)),
            }),
        )
        .expect("replace succeeds");

        assert_eq!(retained.input_values.get(&1), Some(&KernelValue::from(5.0)));
        assert_eq!(retained.input_seqs.get(&1), Some(&CausalSeq::new(3, 0)));
        assert_eq!(
            retained
                .evaluators
                .get(&1)
                .and_then(RetainedMirrorEvaluator::sink_value),
            Some(&KernelValue::from(5.0))
        );
    }

    #[test]
    fn recompute_retained_members_propagates_values_and_output_seqs() {
        let actor = ActorId {
            index: 7,
            generation: 3,
        };

        let mut leaf = RetainedMirrorEvaluator::new(
            IrProgram::from(vec![
                crate::ir::IrNode {
                    id: NodeId(1),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(10)),
                },
                crate::ir::IrNode {
                    id: NodeId(2),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::SinkPort {
                        port: SinkPortId(1),
                        input: NodeId(1),
                    },
                },
            ]),
            SinkPortId(1),
            vec![MirrorCellId(10)],
        )
        .expect("leaf evaluator");
        leaf.apply_messages(&[(
            actor,
            Msg::MirrorWrite {
                cell: MirrorCellId(10),
                value: KernelValue::from(5.0),
                seq: CausalSeq::new(3, 0),
            },
        )])
        .expect("seed leaf");

        let add = RetainedMirrorEvaluator::new(
            IrProgram::from(vec![
                crate::ir::IrNode {
                    id: NodeId(1),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(20)),
                },
                crate::ir::IrNode {
                    id: NodeId(2),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::MirrorCell(MirrorCellId(21)),
                },
                crate::ir::IrNode {
                    id: NodeId(3),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::Add {
                        lhs: NodeId(1),
                        rhs: NodeId(2),
                    },
                },
                crate::ir::IrNode {
                    id: NodeId(4),
                    source_expr: None,
                    kind: crate::ir::IrNodeKind::SinkPort {
                        port: SinkPortId(2),
                        input: NodeId(3),
                    },
                },
            ]),
            SinkPortId(2),
            vec![MirrorCellId(20), MirrorCellId(21)],
        )
        .expect("add evaluator");

        let mut retained = RetainedMemberRuntime {
            evaluators: BTreeMap::from([(1, leaf), (2, add)]),
            mirror_cells: BTreeMap::from([(1, MirrorCellId(10))]),
            input_values: BTreeMap::from([(1, KernelValue::from(5.0))]),
            input_seqs: BTreeMap::from([(1, CausalSeq::new(3, 0))]),
            output_seqs: BTreeMap::new(),
            instance_ids: BTreeMap::from([(1, 1), (2, 2)]),
            #[cfg(test)]
            evaluation_counts: BTreeMap::new(),
            next_seq: 3,
            next_instance_id: 3,
        };
        let dependencies = BTreeMap::from([(1, vec![]), (2, vec![1, 1])]);
        let ordered_components = vec![vec![1], vec![2]];
        let roots = BTreeSet::from([1]);
        let mut values = BTreeMap::new();

        recompute_retained_members(
            &roots,
            &ordered_components,
            &dependencies,
            &mut retained,
            &mut values,
            PropagatedValue {
                value: 0i64,
                seq: CausalSeq::new(0, 0),
            },
            |member, evaluator, dependency_inputs, input_seq| {
                let _ = member;
                evaluate_retained_member(
                    actor,
                    evaluator,
                    dependency_inputs,
                    input_seq,
                    |value| KernelValue::from(*value as f64),
                    |retained_evaluator| match retained_evaluator.sink_value() {
                        Some(KernelValue::Number(value)) => *value as i64,
                        _ => 0,
                    },
                )
            },
        )
        .expect("recompute succeeds");

        assert_eq!(values.get(&1), Some(&5));
        assert_eq!(values.get(&2), Some(&10));
        assert_eq!(retained.output_seqs.get(&1), Some(&CausalSeq::new(3, 0)));
        assert_eq!(retained.output_seqs.get(&2), Some(&CausalSeq::new(3, 0)));
    }

    #[test]
    fn telemetry_tracks_actor_creation_and_message_enqueue() {
        let mut runtime = RuntimeCore::new();
        let root = runtime.alloc_scope(None);
        let actor = runtime.alloc_actor(ActorKind::Pulse, root);

        assert!(runtime.push_message(actor, Msg::Recompute));

        let telemetry = runtime.telemetry_snapshot();
        assert_eq!(telemetry.actor_creation_samples.len(), 1);
        assert_eq!(telemetry.send_samples.len(), 1);
        assert_eq!(telemetry.send_count, 1);
        assert_eq!(telemetry.peak_actor_count, 1);
        assert_eq!(telemetry.peak_ready_queue_depth, 1);
    }
}
