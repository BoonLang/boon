use crate::ids::ActorId;
use crate::ids::ScopeId;
use crate::ir::{
    CallSiteId, FunctionId, FunctionInstanceKey, IrFunctionTemplate, IrNode, IrNodeKind, IrProgram,
    MirrorCellId, NodeId, SinkPortId, SourcePortId,
};
use crate::list_semantics::get_one_based;
use crate::runtime::Msg;
use crate::semantics::CausalSeq;
use crate::text_input::decode_key_down_payload;
use boon::platform::browser::kernel::KernelValue;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
struct ValueState {
    value: KernelValue,
    last_changed: CausalSeq,
}

impl ValueState {
    fn skip() -> Self {
        Self {
            value: KernelValue::Skip,
            last_changed: CausalSeq::new(0, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinkState {
    target: Option<NodeId>,
    edge_epoch: u32,
    last_changed: CausalSeq,
}

impl Default for LinkState {
    fn default() -> Self {
        Self {
            target: None,
            edge_epoch: 0,
            last_changed: CausalSeq::new(0, 0),
        }
    }
}

#[derive(Debug, Clone)]
struct FunctionInstanceState {
    parameter_values: Vec<ValueState>,
}

#[derive(Debug)]
pub struct IrExecutor {
    nodes: Vec<IrNode>,
    functions: BTreeMap<FunctionId, IrFunctionTemplate>,
    node_indices: BTreeMap<NodeId, usize>,
    dependents: BTreeMap<NodeId, Vec<NodeId>>,
    source_nodes: BTreeMap<SourcePortId, NodeId>,
    mirror_nodes: BTreeMap<MirrorCellId, NodeId>,
    values: BTreeMap<NodeId, ValueState>,
    links: BTreeMap<NodeId, LinkState>,
    math_sum_inputs: BTreeMap<NodeId, Option<ValueState>>,
    sink_values: BTreeMap<SinkPortId, KernelValue>,
    function_instances: BTreeMap<FunctionInstanceKey, FunctionInstanceState>,
    list_identities: BTreeMap<NodeId, Vec<u64>>,
    next_list_identity: u64,
}

impl IrExecutor {
    pub fn new(program: impl Into<IrProgram>) -> Result<Self, String> {
        Self::new_program(program.into())
    }

    pub fn new_program(program: IrProgram) -> Result<Self, String> {
        let IrProgram { nodes, functions } = program;
        let mut node_indices = BTreeMap::new();
        let mut dependents = BTreeMap::<NodeId, Vec<NodeId>>::new();
        let mut source_nodes = BTreeMap::new();
        let mut mirror_nodes = BTreeMap::new();
        let mut values = BTreeMap::new();
        let mut links = BTreeMap::new();
        let mut math_sum_inputs = BTreeMap::new();
        let mut function_map = BTreeMap::new();

        for function in functions {
            if function_map.insert(function.id, function).is_some() {
                return Err("duplicate IR function id".to_string());
            }
        }

        for (index, node) in nodes.iter().enumerate() {
            if node_indices.insert(node.id, index).is_some() {
                return Err(format!("duplicate IR node id {}", node.id.0));
            }
            values.insert(node.id, ValueState::skip());
            if matches!(node.kind, IrNodeKind::LinkCell) {
                links.insert(node.id, LinkState::default());
            }
            if matches!(node.kind, IrNodeKind::MathSum { .. }) {
                math_sum_inputs.insert(node.id, None);
            }
        }

        for node in &nodes {
            let dependencies = all_node_dependencies(&node.kind, &function_map);
            for dependency in dependencies {
                if !node_indices.contains_key(&dependency) {
                    return Err(format!(
                        "node {} depends on missing node {}",
                        node.id.0, dependency.0
                    ));
                }
                dependents.entry(dependency).or_default().push(node.id);
            }
            match node.kind {
                IrNodeKind::SourcePort(port) => {
                    source_nodes.insert(port, node.id);
                }
                IrNodeKind::MirrorCell(cell) => {
                    mirror_nodes.insert(cell, node.id);
                }
                _ => {}
            }
        }

        let mut executor = Self {
            nodes,
            functions: function_map,
            node_indices,
            dependents,
            source_nodes,
            mirror_nodes,
            values,
            links,
            math_sum_inputs,
            sink_values: BTreeMap::new(),
            function_instances: BTreeMap::new(),
            list_identities: BTreeMap::new(),
            next_list_identity: 1,
        };
        let initial_dirty = executor
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<Vec<_>>();
        executor.recompute_dirty(&initial_dirty)?;
        Ok(executor)
    }

    pub fn apply_messages(&mut self, messages: &[(ActorId, Msg)]) -> Result<(), String> {
        let active_source_nodes = self
            .source_nodes
            .values()
            .copied()
            .filter(|node_id| {
                self.values
                    .get(node_id)
                    .is_some_and(|state| !state.value.is_skip())
            })
            .collect::<Vec<_>>();
        if !active_source_nodes.is_empty() {
            for node_id in &active_source_nodes {
                self.values.insert(*node_id, ValueState::skip());
            }
            self.recompute_dirty(&active_source_nodes)?;
        }

        for (_actor_id, message) in messages {
            let changed = match message {
                Msg::SourcePulse { port, value, seq } => {
                    self.source_nodes.get(port).copied().map(|node| {
                        self.values.insert(
                            node,
                            ValueState {
                                value: value.clone(),
                                last_changed: *seq,
                            },
                        );
                        node
                    })
                }
                Msg::MirrorWrite { cell, value, seq } => {
                    self.mirror_nodes.get(cell).copied().map(|node| {
                        self.values.insert(
                            node,
                            ValueState {
                                value: value.clone(),
                                last_changed: *seq,
                            },
                        );
                        node
                    })
                }
                _ => None,
            };

            if let Some(node_id) = changed {
                self.recompute_dirty(&[node_id])?;
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn sink_value(&self, sink: SinkPortId) -> Option<&KernelValue> {
        self.sink_values.get(&sink)
    }

    #[must_use]
    pub fn sink_values(&self) -> &BTreeMap<SinkPortId, KernelValue> {
        &self.sink_values
    }

    #[cfg(test)]
    fn link_state(&self, node_id: NodeId) -> Option<LinkState> {
        self.links.get(&node_id).copied()
    }

    fn recompute_dirty(&mut self, dirty_roots: &[NodeId]) -> Result<(), String> {
        let mut dirty = BTreeSet::new();
        self.extend_dirty_closure(dirty_roots, &mut dirty);

        loop {
            let mut extra_dirty = Vec::new();
            let mut changed_any = false;
            let ordered_dirty = self.nodes.iter().map(|node| node.id).collect::<Vec<_>>();
            for node_id in ordered_dirty {
                if !dirty.contains(&node_id) {
                    continue;
                }
                if self.evaluate_node(node_id, &mut extra_dirty)? {
                    changed_any = true;
                    self.extend_dirty_closure(&self.dependents_for(node_id), &mut dirty);
                }
            }

            let new_dirty = extra_dirty
                .into_iter()
                .filter(|node_id| !dirty.contains(node_id))
                .collect::<Vec<_>>();
            if new_dirty.is_empty() {
                if !changed_any {
                    return Ok(());
                }
                continue;
            }
            self.extend_dirty_closure(&new_dirty, &mut dirty);
        }
    }

    fn extend_dirty_closure(&self, roots: &[NodeId], dirty: &mut BTreeSet<NodeId>) {
        let mut stack = roots.to_vec();
        while let Some(node_id) = stack.pop() {
            if !dirty.insert(node_id) {
                continue;
            }
            if let Some(dependents) = self.dependents.get(&node_id) {
                stack.extend(dependents.iter().copied());
            }
        }
    }

    fn dependents_for(&self, node_id: NodeId) -> Vec<NodeId> {
        self.dependents.get(&node_id).cloned().unwrap_or_default()
    }

    fn evaluate_node(
        &mut self,
        node_id: NodeId,
        extra_dirty: &mut Vec<NodeId>,
    ) -> Result<bool, String> {
        let node = self
            .nodes
            .get(
                *self
                    .node_indices
                    .get(&node_id)
                    .ok_or_else(|| format!("missing node index for {}", node_id.0))?,
            )
            .ok_or_else(|| format!("missing IR node {}", node_id.0))?
            .clone();
        let kind_for_identities = node.kind.clone();

        let previous = self
            .values
            .get(&node_id)
            .cloned()
            .unwrap_or_else(ValueState::skip);

        let next = match node.kind {
            IrNodeKind::Literal(value) => ValueState {
                value,
                last_changed: CausalSeq::new(0, 0),
            },
            IrNodeKind::Parameter { .. } => {
                return Err(format!(
                    "Parameter node may only appear inside function templates at node {}",
                    node_id.0
                ));
            }
            IrNodeKind::ObjectLiteral { fields } => {
                let mut object = std::collections::BTreeMap::new();
                let mut last_changed = CausalSeq::new(0, 0);
                for (field, input) in fields {
                    let input = self.value(input)?;
                    last_changed = last_changed.max(input.last_changed);
                    object.insert(field, input.value.clone());
                }
                ValueState {
                    value: KernelValue::Object(object),
                    last_changed,
                }
            }
            IrNodeKind::FieldRead { object, ref field } => {
                let object = self.value(object)?;
                match &object.value {
                    KernelValue::Object(fields) => ValueState {
                        value: fields
                            .get(field.as_str())
                            .cloned()
                            .unwrap_or(KernelValue::Skip),
                        last_changed: object.last_changed,
                    },
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: object.last_changed,
                    },
                    _ => {
                        return Err(format!("FieldRead on non-object at node {}", node_id.0));
                    }
                }
            }
            IrNodeKind::BoolNot { input } => {
                let input = self.value(input)?;
                match &input.value {
                    KernelValue::Bool(value) => ValueState {
                        value: KernelValue::from(!value),
                        last_changed: input.last_changed,
                    },
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: input.last_changed,
                    },
                    _ => return Err(format!("BoolNot on non-bool at node {}", node_id.0)),
                }
            }
            IrNodeKind::Block { inputs } => inputs
                .last()
                .copied()
                .map(|input| self.value(input))
                .transpose()?
                .unwrap_or_else(ValueState::skip),
            IrNodeKind::Hold { seed, updates } => {
                let seed = self.value(seed)?;
                let updates = self.value(updates)?;
                if previous.value.is_skip() {
                    if updates.value.is_skip() {
                        seed
                    } else {
                        updates
                    }
                } else if !updates.value.is_skip() && updates.last_changed > previous.last_changed {
                    updates
                } else {
                    previous.clone()
                }
            }
            IrNodeKind::Then { source, body } => {
                let source = self.value(source)?;
                if source.value.is_skip() {
                    ValueState {
                        value: KernelValue::Skip,
                        last_changed: source.last_changed,
                    }
                } else {
                    let body = self.value(body)?;
                    ValueState {
                        value: body.value,
                        last_changed: source.last_changed,
                    }
                }
            }
            IrNodeKind::Call {
                function,
                call_site,
                args,
            } => {
                let args = args
                    .iter()
                    .map(|arg| self.value(*arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.evaluate_function_call(function, call_site, None, &args)?
            }
            IrNodeKind::ListMap {
                list,
                function,
                call_site,
            } => {
                let source_list = list;
                let list = self.value(source_list)?;
                match &list.value {
                    KernelValue::List(items) => {
                        let item_ids = self.list_identities_for(source_list, items.len());
                        let mut outputs = Vec::with_capacity(items.len());
                        let mut last_changed = list.last_changed;
                        for (item, item_id) in items.iter().cloned().zip(item_ids.into_iter()) {
                            let mapped = self.evaluate_function_call(
                                function,
                                call_site,
                                Some(item_id),
                                &[ValueState {
                                    value: item,
                                    last_changed: list.last_changed,
                                }],
                            )?;
                            last_changed = last_changed.max(mapped.last_changed);
                            outputs.push(mapped.value);
                        }
                        ValueState {
                            value: KernelValue::List(outputs),
                            last_changed,
                        }
                    }
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed,
                    },
                    _ => return Err(format!("ListMap expects list at node {}", node_id.0)),
                }
            }
            IrNodeKind::Latest { inputs } => {
                let candidates = inputs
                    .iter()
                    .map(|input| self.value(*input))
                    .collect::<Result<Vec<_>, _>>()?;
                let selected = candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| !candidate.value.is_skip())
                    .max_by(|(lhs_idx, lhs), (rhs_idx, rhs)| {
                        lhs.last_changed
                            .cmp(&rhs.last_changed)
                            .then_with(|| rhs_idx.cmp(lhs_idx))
                    });
                match selected {
                    Some((_, candidate)) => ValueState {
                        value: candidate.value.clone(),
                        last_changed: candidate.last_changed,
                    },
                    None => ValueState {
                        value: KernelValue::Skip,
                        last_changed: candidates
                            .iter()
                            .map(|candidate| candidate.last_changed)
                            .max()
                            .unwrap_or_else(|| CausalSeq::new(0, 0)),
                    },
                }
            }
            IrNodeKind::When {
                source,
                arms,
                fallback,
            }
            | IrNodeKind::While {
                source,
                arms,
                fallback,
            } => {
                let source = self.value(source)?;
                let selected = arms
                    .iter()
                    .find(|arm| source.value == arm.matcher)
                    .map(|arm| arm.result)
                    .unwrap_or(fallback);
                let selected = self.value(selected)?;
                ValueState {
                    value: selected.value,
                    last_changed: source.last_changed.max(selected.last_changed),
                }
            }
            IrNodeKind::Skip => ValueState::skip(),
            IrNodeKind::LinkCell => self
                .links
                .get(&node_id)
                .copied()
                .map(|state| ValueState {
                    value: KernelValue::Skip,
                    last_changed: state.last_changed,
                })
                .unwrap_or_else(ValueState::skip),
            IrNodeKind::LinkRead { cell } => {
                let link = self
                    .links
                    .get(&cell)
                    .copied()
                    .ok_or_else(|| format!("LinkRead target {} is not a LinkCell", cell.0))?;
                if let Some(target) = link.target {
                    self.value(target)?
                } else {
                    ValueState {
                        value: KernelValue::Skip,
                        last_changed: link.last_changed,
                    }
                }
            }
            IrNodeKind::LinkBind { value, target } => {
                let value = self.value(value)?;
                if value.value.is_skip() {
                    return Ok(false);
                }
                let link = self
                    .links
                    .get(&target)
                    .copied()
                    .ok_or_else(|| format!("LinkBind target {} is not a LinkCell", target.0))?;
                let new_target = Some(value_node_id(&node.kind));
                let mut updated = link;
                let binding_changed =
                    link.target != new_target || link.last_changed != value.last_changed;
                if binding_changed {
                    updated.target = new_target;
                    if link.target != new_target {
                        updated.edge_epoch = updated.edge_epoch.wrapping_add(1);
                    }
                    updated.last_changed = value.last_changed;
                    self.links.insert(target, updated);
                    extra_dirty.push(target);
                }
                ValueState {
                    value: value.value,
                    last_changed: value.last_changed,
                }
            }
            IrNodeKind::Add { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                add_number_states(lhs, rhs, node_id)?
            }
            IrNodeKind::Sub { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                binary_number_state(lhs, rhs, node_id, |lhs, rhs| lhs - rhs)?
            }
            IrNodeKind::Mul { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                binary_number_state(lhs, rhs, node_id, |lhs, rhs| lhs * rhs)?
            }
            IrNodeKind::Div { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                binary_number_state(lhs, rhs, node_id, |lhs, rhs| lhs / rhs)?
            }
            IrNodeKind::Eq { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                ValueState {
                    value: KernelValue::from(lhs.value == rhs.value),
                    last_changed: lhs.last_changed.max(rhs.last_changed),
                }
            }
            IrNodeKind::Ge { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                ValueState {
                    value: KernelValue::from(compare_ge(&lhs.value, &rhs.value, node_id)?),
                    last_changed: lhs.last_changed.max(rhs.last_changed),
                }
            }
            IrNodeKind::MathSum { input } => {
                let input = self.value(input)?;
                let last_input = self.math_sum_inputs.get(&node_id).cloned().unwrap_or(None);
                if input.value.is_skip() || last_input.as_ref() == Some(&input) {
                    previous.clone()
                } else {
                    let prior = if previous.value.is_skip() {
                        0.0
                    } else {
                        as_number(&previous.value, node_id)?
                    };
                    self.math_sum_inputs.insert(node_id, Some(input.clone()));
                    ValueState {
                        value: KernelValue::from(prior + as_number(&input.value, node_id)?),
                        last_changed: input.last_changed,
                    }
                }
            }
            IrNodeKind::MathMin { lhs, rhs } => {
                let lhs = self.value(lhs)?;
                let rhs = self.value(rhs)?;
                binary_number_state(lhs, rhs, node_id, f64::min)?
            }
            IrNodeKind::MathRound { input } => {
                let input = self.value(input)?;
                ValueState {
                    value: KernelValue::from(as_number(&input.value, node_id)?.round()),
                    last_changed: input.last_changed,
                }
            }
            IrNodeKind::TextToNumber { input } => {
                let input = self.value(input)?;
                let value = match &input.value {
                    KernelValue::Text(text) | KernelValue::Tag(text) => match text.parse::<f64>() {
                        Ok(number) => KernelValue::from(number),
                        Err(_) => KernelValue::Tag("NaN".to_string()),
                    },
                    KernelValue::Number(number) => KernelValue::from(*number),
                    KernelValue::Skip => KernelValue::Tag("NaN".to_string()),
                    _ => KernelValue::Tag("NaN".to_string()),
                };
                ValueState {
                    value,
                    last_changed: input.last_changed,
                }
            }
            IrNodeKind::TextTrim { input } => {
                let input = self.value(input)?;
                let value = match &input.value {
                    KernelValue::Text(text) | KernelValue::Tag(text) => {
                        KernelValue::from(text.trim().to_string())
                    }
                    KernelValue::Skip => KernelValue::Skip,
                    _ => return Err(format!("TextTrim on non-text at node {}", node_id.0)),
                };
                ValueState {
                    value,
                    last_changed: input.last_changed,
                }
            }
            IrNodeKind::KeyDownKey { input } => {
                let input = self.value(input)?;
                let value = match &input.value {
                    KernelValue::Text(text) | KernelValue::Tag(text) => {
                        KernelValue::from(decode_key_down_payload(Some(text)).key)
                    }
                    KernelValue::Skip => KernelValue::Skip,
                    _ => return Err(format!("KeyDownKey on non-text at node {}", node_id.0)),
                };
                ValueState {
                    value,
                    last_changed: input.last_changed,
                }
            }
            IrNodeKind::KeyDownText { input } => {
                let input = self.value(input)?;
                let value = match &input.value {
                    KernelValue::Text(text) | KernelValue::Tag(text) => KernelValue::from(
                        decode_key_down_payload(Some(text))
                            .current_text
                            .unwrap_or_default(),
                    ),
                    KernelValue::Skip => KernelValue::Skip,
                    _ => return Err(format!("KeyDownText on non-text at node {}", node_id.0)),
                };
                ValueState {
                    value,
                    last_changed: input.last_changed,
                }
            }
            IrNodeKind::TextJoin { inputs } => {
                let inputs = inputs
                    .iter()
                    .map(|input| self.value(*input))
                    .collect::<Result<Vec<_>, _>>()?;
                ValueState {
                    value: KernelValue::from(
                        inputs
                            .iter()
                            .map(|input| as_text(&input.value))
                            .collect::<String>(),
                    ),
                    last_changed: inputs
                        .iter()
                        .map(|input| input.last_changed)
                        .max()
                        .unwrap_or_else(|| CausalSeq::new(0, 0)),
                }
            }
            IrNodeKind::ListLiteral { items } => {
                let inputs = items
                    .iter()
                    .map(|input| self.value(*input))
                    .collect::<Result<Vec<_>, _>>()?;
                ValueState {
                    value: KernelValue::List(
                        inputs.iter().map(|input| input.value.clone()).collect(),
                    ),
                    last_changed: inputs
                        .iter()
                        .map(|input| input.last_changed)
                        .max()
                        .unwrap_or_else(|| CausalSeq::new(0, 0)),
                }
            }
            IrNodeKind::ListRange { from, to } => {
                let from = self.value(from)?;
                let to = self.value(to)?;
                match (&from.value, &to.value) {
                    (KernelValue::Number(from_number), KernelValue::Number(to_number)) => {
                        let from_int = integral_number(*from_number, node_id, "ListRange from")?;
                        let to_int = integral_number(*to_number, node_id, "ListRange to")?;
                        let items = if from_int <= to_int {
                            (from_int..=to_int)
                                .map(|value| KernelValue::from(value as f64))
                                .collect::<Vec<_>>()
                        } else {
                            (to_int..=from_int)
                                .rev()
                                .map(|value| KernelValue::from(value as f64))
                                .collect::<Vec<_>>()
                        };
                        ValueState {
                            value: KernelValue::List(items),
                            last_changed: from.last_changed.max(to.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) | (_, KernelValue::Skip) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: from.last_changed.max(to.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListRange expects numeric bounds at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListAppend { list, item } => {
                let list = self.value(list)?;
                let item = self.value(item)?;
                match (&list.value, &item.value) {
                    (KernelValue::List(items), KernelValue::Skip) => ValueState {
                        value: KernelValue::List(items.clone()),
                        last_changed: list.last_changed,
                    },
                    (KernelValue::List(items), item_value) => {
                        let mut next = items.clone();
                        next.push(item_value.clone());
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list.last_changed.max(item.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(item.last_changed),
                    },
                    _ => return Err(format!("ListAppend on non-list at node {}", node_id.0)),
                }
            }
            IrNodeKind::ListRemoveLast { list, on } => {
                let list = self.value(list)?;
                let on = self.value(on)?;
                match (&list.value, &on.value) {
                    (KernelValue::List(items), KernelValue::Skip) => ValueState {
                        value: KernelValue::List(items.clone()),
                        last_changed: list.last_changed.max(on.last_changed),
                    },
                    (KernelValue::List(items), _) => {
                        let mut next = items.clone();
                        next.pop();
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list.last_changed.max(on.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(on.last_changed),
                    },
                    _ => {
                        return Err(format!("ListRemoveLast on non-list at node {}", node_id.0));
                    }
                }
            }
            IrNodeKind::ListMapObjectBoolField {
                list,
                ref field,
                value,
            } => {
                let list = self.value(list)?;
                let value = self.value(value)?;
                match (&list.value, &value.value) {
                    (KernelValue::List(items), KernelValue::Bool(next_bool)) => {
                        let mut next = Vec::with_capacity(items.len());
                        for item in items {
                            let KernelValue::Object(fields) = item else {
                                return Err(format!(
                                    "ListMapObjectBoolField expects object items at node {}",
                                    node_id.0
                                ));
                            };
                            let mut next_fields = fields.clone();
                            next_fields.insert(field.clone(), KernelValue::from(*next_bool));
                            next.push(KernelValue::Object(next_fields));
                        }
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list.last_changed.max(value.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) | (_, KernelValue::Skip) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(value.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListMapObjectBoolField expects list/bool at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListMapToggleObjectBoolFieldByFieldEq {
                list,
                ref match_field,
                match_value,
                ref bool_field,
            } => {
                let list = self.value(list)?;
                let match_value_state = self.value(match_value)?;
                match (&list.value, &match_value_state.value) {
                    (KernelValue::List(items), match_value) => {
                        let mut next = Vec::with_capacity(items.len());
                        for item in items {
                            let KernelValue::Object(fields) = item else {
                                return Err(format!(
                                    "ListMapToggleObjectBoolFieldByFieldEq expects object items at node {}",
                                    node_id.0
                                ));
                            };
                            let mut next_fields = fields.clone();
                            if next_fields.get(match_field.as_str()) == Some(match_value) {
                                let current = match next_fields.get(bool_field.as_str()) {
                                    Some(KernelValue::Bool(value)) => *value,
                                    Some(KernelValue::Skip) | None => false,
                                    _ => {
                                        return Err(format!(
                                            "ListMapToggleObjectBoolFieldByFieldEq expects bool field at node {}",
                                            node_id.0
                                        ));
                                    }
                                };
                                next_fields.insert(bool_field.clone(), KernelValue::from(!current));
                            }
                            next.push(KernelValue::Object(next_fields));
                        }
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list.last_changed.max(match_value_state.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) | (_, KernelValue::Skip) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(match_value_state.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListMapToggleObjectBoolFieldByFieldEq expects list/value at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListMapObjectFieldByFieldEq {
                list,
                ref match_field,
                match_value,
                ref update_field,
                update_value,
            } => {
                let list = self.value(list)?;
                let match_value_state = self.value(match_value)?;
                let update_value_state = self.value(update_value)?;
                match (&list.value, &match_value_state.value) {
                    (KernelValue::List(items), match_value) => {
                        let mut next = Vec::with_capacity(items.len());
                        for item in items {
                            let KernelValue::Object(fields) = item else {
                                return Err(format!(
                                    "ListMapObjectFieldByFieldEq expects object items at node {}",
                                    node_id.0
                                ));
                            };
                            let mut next_fields = fields.clone();
                            if next_fields.get(match_field.as_str()) == Some(match_value) {
                                next_fields
                                    .insert(update_field.clone(), update_value_state.value.clone());
                            }
                            next.push(KernelValue::Object(next_fields));
                        }
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list
                                .last_changed
                                .max(match_value_state.last_changed)
                                .max(update_value_state.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) | (_, KernelValue::Skip) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list
                            .last_changed
                            .max(match_value_state.last_changed)
                            .max(update_value_state.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListMapObjectFieldByFieldEq expects list/value at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListAllObjectBoolField { list, ref field } => {
                let list = self.value(list)?;
                match &list.value {
                    KernelValue::List(items) => {
                        let mut all_true = true;
                        for item in items {
                            let KernelValue::Object(fields) = item else {
                                return Err(format!(
                                    "ListAllObjectBoolField expects object items at node {}",
                                    node_id.0
                                ));
                            };
                            match fields.get(field.as_str()) {
                                Some(KernelValue::Bool(true)) => {}
                                Some(KernelValue::Bool(false)) | Some(KernelValue::Skip) | None => {
                                    all_true = false;
                                    break;
                                }
                                _ => {
                                    return Err(format!(
                                        "ListAllObjectBoolField expects bool field at node {}",
                                        node_id.0
                                    ));
                                }
                            }
                        }
                        ValueState {
                            value: KernelValue::from(all_true),
                            last_changed: list.last_changed,
                        }
                    }
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed,
                    },
                    _ => {
                        return Err(format!(
                            "ListAllObjectBoolField expects list at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListCount { list } => {
                let list = self.value(list)?;
                match &list.value {
                    KernelValue::List(items) => ValueState {
                        value: KernelValue::from(items.len() as f64),
                        last_changed: list.last_changed,
                    },
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed,
                    },
                    _ => return Err(format!("ListCount on non-list at node {}", node_id.0)),
                }
            }
            IrNodeKind::ListGet { list, index } => {
                let list = self.value(list)?;
                let index = self.value(index)?;
                match (&list.value, &index.value) {
                    (KernelValue::List(items), KernelValue::Number(index_number)) => {
                        let one_based = integral_number(*index_number, node_id, "ListGet index")?;
                        let value = get_one_based(items, one_based)
                            .cloned()
                            .unwrap_or_else(|| KernelValue::Tag("OutOfBounds".to_string()));
                        ValueState {
                            value,
                            last_changed: list.last_changed.max(index.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) | (_, KernelValue::Skip) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(index.last_changed),
                    },
                    _ => {
                        return Err(format!("ListGet expects list/index at node {}", node_id.0));
                    }
                }
            }
            IrNodeKind::ListIsEmpty { list } => {
                let list = self.value(list)?;
                match &list.value {
                    KernelValue::List(items) => ValueState {
                        value: KernelValue::from(items.is_empty()),
                        last_changed: list.last_changed,
                    },
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed,
                    },
                    _ => return Err(format!("ListIsEmpty on non-list at node {}", node_id.0)),
                }
            }
            IrNodeKind::ListSum { list } => {
                let list = self.value(list)?;
                match &list.value {
                    KernelValue::List(items) => {
                        let mut sum = 0.0;
                        for item in items {
                            sum += as_number(item, node_id)?;
                        }
                        ValueState {
                            value: KernelValue::from(sum),
                            last_changed: list.last_changed,
                        }
                    }
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed,
                    },
                    _ => return Err(format!("ListSum on non-list at node {}", node_id.0)),
                }
            }
            IrNodeKind::ListRetain { list, predicate } => {
                let list = self.value(list)?;
                let predicate = self.value(predicate)?;
                match (&list.value, &predicate.value) {
                    (KernelValue::List(items), KernelValue::Bool(true)) => ValueState {
                        value: KernelValue::List(items.clone()),
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    (KernelValue::List(_), KernelValue::Bool(false)) => ValueState {
                        value: KernelValue::List(Vec::new()),
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    (KernelValue::List(items), KernelValue::Skip) => ValueState {
                        value: KernelValue::List(items.clone()),
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    (KernelValue::Skip, _) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListRetain expects list/bool at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListRemove { list, predicate } => {
                let list = self.value(list)?;
                let predicate = self.value(predicate)?;
                match (&list.value, &predicate.value) {
                    (KernelValue::List(_), KernelValue::Bool(true)) => ValueState {
                        value: KernelValue::List(Vec::new()),
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    (KernelValue::List(items), KernelValue::Bool(false))
                    | (KernelValue::List(items), KernelValue::Skip) => ValueState {
                        value: KernelValue::List(items.clone()),
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    (KernelValue::Skip, _) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(predicate.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListRemove expects list/bool at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListRetainObjectBoolField {
                list,
                ref field,
                keep_if,
            } => {
                let list = self.value(list)?;
                match &list.value {
                    KernelValue::List(items) => {
                        let mut next = Vec::with_capacity(items.len());
                        for item in items {
                            let KernelValue::Object(fields) = item else {
                                return Err(format!(
                                    "ListRetainObjectBoolField expects object items at node {}",
                                    node_id.0
                                ));
                            };
                            let field_value = match fields.get(field.as_str()) {
                                Some(KernelValue::Bool(value)) => *value,
                                Some(KernelValue::Skip) | None => false,
                                _ => {
                                    return Err(format!(
                                        "ListRetainObjectBoolField expects bool field at node {}",
                                        node_id.0
                                    ));
                                }
                            };
                            if field_value == keep_if {
                                next.push(item.clone());
                            }
                        }
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list.last_changed,
                        }
                    }
                    KernelValue::Skip => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed,
                    },
                    _ => {
                        return Err(format!(
                            "ListRetainObjectBoolField expects list at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::ListRemoveObjectByFieldEq {
                list,
                ref field,
                value,
            } => {
                let list = self.value(list)?;
                let value_state = self.value(value)?;
                match (&list.value, &value_state.value) {
                    (KernelValue::List(items), match_value) => {
                        let mut next = Vec::with_capacity(items.len());
                        for item in items {
                            let KernelValue::Object(fields) = item else {
                                return Err(format!(
                                    "ListRemoveObjectByFieldEq expects object items at node {}",
                                    node_id.0
                                ));
                            };
                            if fields.get(field.as_str()) != Some(match_value) {
                                next.push(item.clone());
                            }
                        }
                        ValueState {
                            value: KernelValue::List(next),
                            last_changed: list.last_changed.max(value_state.last_changed),
                        }
                    }
                    (KernelValue::Skip, _) | (_, KernelValue::Skip) => ValueState {
                        value: KernelValue::Skip,
                        last_changed: list.last_changed.max(value_state.last_changed),
                    },
                    _ => {
                        return Err(format!(
                            "ListRemoveObjectByFieldEq expects list/value at node {}",
                            node_id.0
                        ));
                    }
                }
            }
            IrNodeKind::SourcePort(_) | IrNodeKind::MirrorCell(_) => previous.clone(),
            IrNodeKind::SinkPort { port, input } => {
                let input = self.value(input)?;
                self.sink_values.insert(port, input.value.clone());
                input
            }
        };

        self.update_list_identities(node_id, &kind_for_identities, &previous, &next)?;

        let changed = previous != next;
        if changed {
            self.values.insert(node_id, next);
        }

        Ok(changed)
    }

    fn value(&self, node_id: NodeId) -> Result<ValueState, String> {
        self.values
            .get(&node_id)
            .cloned()
            .ok_or_else(|| format!("missing value for node {}", node_id.0))
    }

    fn update_list_identities(
        &mut self,
        node_id: NodeId,
        kind: &IrNodeKind,
        previous: &ValueState,
        next: &ValueState,
    ) -> Result<(), String> {
        let KernelValue::List(items) = &next.value else {
            self.list_identities.remove(&node_id);
            return Ok(());
        };

        let next_ids = match kind {
            IrNodeKind::ListLiteral { .. } => {
                if previous.value == next.value {
                    self.list_identities
                        .get(&node_id)
                        .cloned()
                        .filter(|ids| ids.len() == items.len())
                        .unwrap_or_else(|| self.fresh_list_identities(items.len()))
                } else {
                    self.fresh_list_identities(items.len())
                }
            }
            IrNodeKind::ListRange { .. } => items
                .iter()
                .map(|item| match item {
                    KernelValue::Number(number) if number.is_finite() && number.fract() == 0.0 => {
                        Ok(*number as u64)
                    }
                    _ => Err(format!(
                        "ListRange must produce integral numeric identities at node {}",
                        node_id.0
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?,
            IrNodeKind::ListAppend { list, item } => {
                let mut ids = self.list_identities_for(*list, items.len().saturating_sub(1));
                if !self.value(*item)?.value.is_skip() {
                    ids.push(self.fresh_list_identities(1)[0]);
                }
                ids
            }
            IrNodeKind::ListRemoveLast { list, on } => {
                let mut ids = self.list_identities_for(*list, items.len().saturating_add(1));
                if !self.value(*on)?.value.is_skip() && !ids.is_empty() {
                    ids.pop();
                } else {
                    ids.truncate(items.len());
                }
                ids
            }
            IrNodeKind::ListRetain { list, predicate } => match &self.value(*predicate)?.value {
                KernelValue::Bool(true) | KernelValue::Skip => {
                    self.list_identities_for(*list, items.len())
                }
                KernelValue::Bool(false) => Vec::new(),
                _ => self.list_identities_for(*list, items.len()),
            },
            IrNodeKind::ListRemove { list, predicate } => match &self.value(*predicate)?.value {
                KernelValue::Bool(false) | KernelValue::Skip => {
                    self.list_identities_for(*list, items.len())
                }
                KernelValue::Bool(true) => Vec::new(),
                _ => self.list_identities_for(*list, items.len()),
            },
            IrNodeKind::ListMap {
                list, call_site, ..
            } => self
                .list_identities_for(*list, items.len())
                .into_iter()
                .map(|identity| mapped_list_identity(*call_site, identity))
                .collect(),
            IrNodeKind::ListMapObjectBoolField { list, .. }
            | IrNodeKind::ListMapToggleObjectBoolFieldByFieldEq { list, .. }
            | IrNodeKind::ListMapObjectFieldByFieldEq { list, .. } => {
                self.list_identities_for(*list, items.len())
            }
            IrNodeKind::ListRetainObjectBoolField { list, .. }
            | IrNodeKind::ListRemoveObjectByFieldEq { list, .. } => {
                let source_items = match &self.value(*list)?.value {
                    KernelValue::List(items) => items.clone(),
                    _ => Vec::new(),
                };
                self.matching_subset_identities(*list, &source_items, items)
            }
            _ => self
                .list_identities
                .get(&node_id)
                .cloned()
                .filter(|ids| ids.len() == items.len())
                .unwrap_or_else(|| self.fresh_list_identities(items.len())),
        };

        self.list_identities.insert(node_id, next_ids);
        Ok(())
    }

    fn list_identities_for(&mut self, node_id: NodeId, len: usize) -> Vec<u64> {
        if let Some(ids) = self.list_identities.get(&node_id).cloned() {
            if ids.len() == len {
                return ids;
            }
        }
        self.fresh_list_identities(len)
    }

    fn matching_subset_identities(
        &mut self,
        source_node: NodeId,
        source_items: &[KernelValue],
        output_items: &[KernelValue],
    ) -> Vec<u64> {
        let source_ids = self.list_identities_for(source_node, source_items.len());
        let mut used = vec![false; source_items.len()];
        let mut out = Vec::with_capacity(output_items.len());
        for output in output_items {
            if let Some((index, _)) = source_items
                .iter()
                .enumerate()
                .find(|(index, candidate)| !used[*index] && *candidate == output)
            {
                used[index] = true;
                out.push(source_ids[index]);
            } else {
                out.push(self.fresh_list_identities(1)[0]);
            }
        }
        out
    }

    fn fresh_list_identities(&mut self, len: usize) -> Vec<u64> {
        (0..len)
            .map(|_| {
                let next = self.next_list_identity;
                self.next_list_identity = self.next_list_identity.wrapping_add(1);
                next
            })
            .collect()
    }

    fn evaluate_function_call(
        &mut self,
        function: FunctionId,
        call_site: CallSiteId,
        mapped_item_identity: Option<u64>,
        args: &[ValueState],
    ) -> Result<ValueState, String> {
        let template = self
            .functions
            .get(&function)
            .cloned()
            .ok_or_else(|| format!("missing IR function template {}", function.0))?;
        if args.len() != template.parameter_count {
            return Err(format!(
                "function {} expected {} args, got {}",
                function.0,
                template.parameter_count,
                args.len()
            ));
        }

        let key = FunctionInstanceKey {
            function,
            call_site,
            parent_scope: ScopeId {
                index: 0,
                generation: 0,
            },
            mapped_item_identity,
        };
        self.function_instances
            .entry(key)
            .and_modify(|instance| instance.parameter_values = args.to_vec())
            .or_insert_with(|| FunctionInstanceState {
                parameter_values: args.to_vec(),
            });

        let mut instantiated = Vec::with_capacity(template.nodes.len());
        let mut external_last_changed = args
            .iter()
            .map(|state| state.last_changed)
            .max()
            .unwrap_or_else(|| CausalSeq::new(0, 0));
        let template_node_ids = template
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<BTreeSet<_>>();
        let mut capture_literals = BTreeMap::<NodeId, (NodeId, ValueState)>::new();
        let mut next_node_id = template
            .nodes
            .iter()
            .map(|node| node.id.0)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);

        for mut node in template.nodes {
            if let IrNodeKind::Parameter { index } = node.kind {
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("missing function arg {}", index))?
                    .value
                    .clone();
                node.kind = IrNodeKind::Literal(value);
                instantiated.push(node);
                continue;
            }

            let external_dependencies = node_dependencies(&node.kind)
                .into_iter()
                .filter(|dependency| !template_node_ids.contains(dependency))
                .collect::<Vec<_>>();
            if !external_dependencies.is_empty() {
                let mut replacements = BTreeMap::new();
                for dependency in external_dependencies {
                    let replacement =
                        if let Some((replacement_id, state)) = capture_literals.get(&dependency) {
                            external_last_changed = external_last_changed.max(state.last_changed);
                            *replacement_id
                        } else {
                            let state = self.value(dependency)?;
                            external_last_changed = external_last_changed.max(state.last_changed);
                            let replacement_id = NodeId(next_node_id);
                            next_node_id = next_node_id.wrapping_add(1);
                            capture_literals.insert(dependency, (replacement_id, state.clone()));
                            replacement_id
                        };
                    replacements.insert(dependency, replacement);
                }
                rewrite_node_kind_dependencies(&mut node.kind, &replacements);
            }
            instantiated.push(node);
        }

        instantiated.extend(
            capture_literals
                .into_values()
                .map(|(node_id, state)| IrNode {
                    id: node_id,
                    source_expr: None,
                    kind: IrNodeKind::Literal(state.value),
                }),
        );

        let nested = IrExecutor::new_program(IrProgram {
            nodes: instantiated,
            functions: self.functions.values().cloned().collect(),
        })?;
        let output = nested
            .value(template.output)
            .map_err(|error| format!("function {} output: {error}", function.0))?;

        Ok(ValueState {
            value: output.value,
            last_changed: output.last_changed.max(external_last_changed),
        })
    }
}

fn node_dependencies(kind: &IrNodeKind) -> Vec<NodeId> {
    match kind {
        IrNodeKind::Literal(_)
        | IrNodeKind::Parameter { .. }
        | IrNodeKind::Skip
        | IrNodeKind::LinkCell
        | IrNodeKind::SourcePort(_)
        | IrNodeKind::MirrorCell(_) => Vec::new(),
        IrNodeKind::ObjectLiteral { fields } => fields.iter().map(|(_, input)| *input).collect(),
        IrNodeKind::FieldRead { object, .. } => vec![*object],
        IrNodeKind::Block { inputs } | IrNodeKind::Latest { inputs } => inputs.clone(),
        IrNodeKind::Hold { seed, updates } => vec![*seed, *updates],
        IrNodeKind::Then { source, body } => vec![*source, *body],
        IrNodeKind::When {
            source,
            arms,
            fallback,
        }
        | IrNodeKind::While {
            source,
            arms,
            fallback,
        } => {
            let mut inputs = vec![*source, *fallback];
            inputs.extend(arms.iter().map(|arm| arm.result));
            inputs
        }
        IrNodeKind::LinkRead { cell } => vec![*cell],
        IrNodeKind::LinkBind { value, target } => vec![*value, *target],
        IrNodeKind::Call { args, .. } | IrNodeKind::ListLiteral { items: args } => args.clone(),
        IrNodeKind::ListRange { from, to } => vec![*from, *to],
        IrNodeKind::ListMap { list, .. } => vec![*list],
        IrNodeKind::ListAppend { list, item } => vec![*list, *item],
        IrNodeKind::ListRemoveLast { list, on } => vec![*list, *on],
        IrNodeKind::ListMapObjectBoolField { list, value, .. } => vec![*list, *value],
        IrNodeKind::ListMapToggleObjectBoolFieldByFieldEq {
            list, match_value, ..
        } => vec![*list, *match_value],
        IrNodeKind::ListMapObjectFieldByFieldEq {
            list,
            match_value,
            update_value,
            ..
        } => vec![*list, *match_value, *update_value],
        IrNodeKind::ListAllObjectBoolField { list, .. } => vec![*list],
        IrNodeKind::ListRemove { list, predicate } | IrNodeKind::ListRetain { list, predicate } => {
            vec![*list, *predicate]
        }
        IrNodeKind::ListRetainObjectBoolField { list, .. } => vec![*list],
        IrNodeKind::ListRemoveObjectByFieldEq { list, value, .. } => vec![*list, *value],
        IrNodeKind::ListCount { list }
        | IrNodeKind::ListIsEmpty { list }
        | IrNodeKind::ListSum { list } => vec![*list],
        IrNodeKind::ListGet { list, index } => vec![*list, *index],
        IrNodeKind::Add { lhs, rhs }
        | IrNodeKind::Sub { lhs, rhs }
        | IrNodeKind::Mul { lhs, rhs }
        | IrNodeKind::Div { lhs, rhs }
        | IrNodeKind::Eq { lhs, rhs }
        | IrNodeKind::Ge { lhs, rhs } => {
            vec![*lhs, *rhs]
        }
        IrNodeKind::BoolNot { input } => vec![*input],
        IrNodeKind::MathSum { input } => vec![*input],
        IrNodeKind::MathMin { lhs, rhs } => vec![*lhs, *rhs],
        IrNodeKind::MathRound { input }
        | IrNodeKind::TextToNumber { input }
        | IrNodeKind::TextTrim { input }
        | IrNodeKind::KeyDownKey { input }
        | IrNodeKind::KeyDownText { input } => vec![*input],
        IrNodeKind::TextJoin { inputs } => inputs.clone(),
        IrNodeKind::SinkPort { input, .. } => vec![*input],
    }
}

fn all_node_dependencies(
    kind: &IrNodeKind,
    functions: &BTreeMap<FunctionId, IrFunctionTemplate>,
) -> Vec<NodeId> {
    let mut dependencies = node_dependencies(kind);
    match kind {
        IrNodeKind::Call { function, .. } | IrNodeKind::ListMap { function, .. } => {
            if let Some(template) = functions.get(function) {
                dependencies.extend(function_capture_dependencies(template));
            }
        }
        _ => {}
    }
    dependencies.sort();
    dependencies.dedup();
    dependencies
}

fn mapped_list_identity(call_site: CallSiteId, upstream_identity: u64) -> u64 {
    ((call_site.0 as u64) << 32) ^ upstream_identity
}

fn function_capture_dependencies(template: &IrFunctionTemplate) -> Vec<NodeId> {
    let template_node_ids = template
        .nodes
        .iter()
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();
    let mut captures = template
        .nodes
        .iter()
        .flat_map(|node| node_dependencies(&node.kind))
        .filter(|dependency| !template_node_ids.contains(dependency))
        .collect::<Vec<_>>();
    captures.sort();
    captures.dedup();
    captures
}

fn rewrite_node_kind_dependencies(kind: &mut IrNodeKind, replacements: &BTreeMap<NodeId, NodeId>) {
    let replace = |node_id: &mut NodeId| {
        if let Some(replacement) = replacements.get(node_id) {
            *node_id = *replacement;
        }
    };

    match kind {
        IrNodeKind::Literal(_)
        | IrNodeKind::Parameter { .. }
        | IrNodeKind::Skip
        | IrNodeKind::LinkCell
        | IrNodeKind::SourcePort(_)
        | IrNodeKind::MirrorCell(_) => {}
        IrNodeKind::ObjectLiteral { fields } => {
            for (_, input) in fields {
                replace(input);
            }
        }
        IrNodeKind::FieldRead { object, .. } => replace(object),
        IrNodeKind::Block { inputs }
        | IrNodeKind::Latest { inputs }
        | IrNodeKind::TextJoin { inputs } => {
            for input in inputs {
                replace(input);
            }
        }
        IrNodeKind::Hold { seed, updates } => {
            replace(seed);
            replace(updates);
        }
        IrNodeKind::Then { source, body } => {
            replace(source);
            replace(body);
        }
        IrNodeKind::When {
            source,
            arms,
            fallback,
        }
        | IrNodeKind::While {
            source,
            arms,
            fallback,
        } => {
            replace(source);
            replace(fallback);
            for arm in arms {
                replace(&mut arm.result);
            }
        }
        IrNodeKind::LinkRead { cell } => replace(cell),
        IrNodeKind::LinkBind { value, target } => {
            replace(value);
            replace(target);
        }
        IrNodeKind::Add { lhs, rhs }
        | IrNodeKind::Sub { lhs, rhs }
        | IrNodeKind::Mul { lhs, rhs }
        | IrNodeKind::Div { lhs, rhs }
        | IrNodeKind::Eq { lhs, rhs }
        | IrNodeKind::Ge { lhs, rhs }
        | IrNodeKind::MathMin { lhs, rhs } => {
            replace(lhs);
            replace(rhs);
        }
        IrNodeKind::BoolNot { input }
        | IrNodeKind::MathSum { input }
        | IrNodeKind::MathRound { input }
        | IrNodeKind::TextToNumber { input }
        | IrNodeKind::TextTrim { input }
        | IrNodeKind::KeyDownKey { input }
        | IrNodeKind::KeyDownText { input }
        | IrNodeKind::SinkPort { input, .. } => replace(input),
        IrNodeKind::Call { args, .. } | IrNodeKind::ListLiteral { items: args } => {
            for arg in args {
                replace(arg);
            }
        }
        IrNodeKind::ListRange { from, to } => {
            replace(from);
            replace(to);
        }
        IrNodeKind::ListMap { list, .. } => {
            replace(list);
        }
        IrNodeKind::ListAppend { list, item } => {
            replace(list);
            replace(item);
        }
        IrNodeKind::ListRemoveLast { list, on } => {
            replace(list);
            replace(on);
        }
        IrNodeKind::ListMapObjectBoolField { list, value, .. } => {
            replace(list);
            replace(value);
        }
        IrNodeKind::ListMapToggleObjectBoolFieldByFieldEq {
            list, match_value, ..
        } => {
            replace(list);
            replace(match_value);
        }
        IrNodeKind::ListMapObjectFieldByFieldEq {
            list,
            match_value,
            update_value,
            ..
        } => {
            replace(list);
            replace(match_value);
            replace(update_value);
        }
        IrNodeKind::ListAllObjectBoolField { list, .. }
        | IrNodeKind::ListCount { list }
        | IrNodeKind::ListIsEmpty { list }
        | IrNodeKind::ListSum { list }
        | IrNodeKind::ListRetainObjectBoolField { list, .. } => replace(list),
        IrNodeKind::ListRemove { list, predicate } | IrNodeKind::ListRetain { list, predicate } => {
            replace(list);
            replace(predicate);
        }
        IrNodeKind::ListRemoveObjectByFieldEq { list, value, .. } => {
            replace(list);
            replace(value);
        }
        IrNodeKind::ListGet { list, index } => {
            replace(list);
            replace(index);
        }
    }
}

fn add_number_states(
    lhs: ValueState,
    rhs: ValueState,
    node_id: NodeId,
) -> Result<ValueState, String> {
    binary_number_state(lhs, rhs, node_id, |lhs, rhs| lhs + rhs)
}

fn binary_number_state(
    lhs: ValueState,
    rhs: ValueState,
    node_id: NodeId,
    op: impl FnOnce(f64, f64) -> f64,
) -> Result<ValueState, String> {
    Ok(ValueState {
        value: KernelValue::from(op(
            as_number(&lhs.value, node_id)?,
            as_number(&rhs.value, node_id)?,
        )),
        last_changed: lhs.last_changed.max(rhs.last_changed),
    })
}

fn as_number(value: &KernelValue, node_id: NodeId) -> Result<f64, String> {
    match value {
        KernelValue::Number(number) => Ok(*number),
        KernelValue::Tag(tag) if tag == "NaN" => Ok(f64::NAN),
        KernelValue::Skip => Ok(0.0),
        _ => Err(format!("node {} expected numeric input", node_id.0)),
    }
}

fn integral_number(value: f64, node_id: NodeId, label: &str) -> Result<i64, String> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(format!("{label} must be an integer at node {}", node_id.0));
    }
    Ok(value as i64)
}

fn compare_ge(lhs: &KernelValue, rhs: &KernelValue, node_id: NodeId) -> Result<bool, String> {
    match (lhs, rhs) {
        (KernelValue::Number(lhs), KernelValue::Number(rhs)) => Ok(lhs >= rhs),
        (KernelValue::Tag(lhs), _) | (_, KernelValue::Tag(lhs)) if lhs == "NaN" => Ok(false),
        (KernelValue::Text(lhs), KernelValue::Text(rhs))
        | (KernelValue::Text(lhs), KernelValue::Tag(rhs))
        | (KernelValue::Tag(lhs), KernelValue::Text(rhs))
        | (KernelValue::Tag(lhs), KernelValue::Tag(rhs)) => Ok(lhs >= rhs),
        (KernelValue::Skip, KernelValue::Skip) => Ok(true),
        _ => Err(format!(
            "node {} expected comparable scalar inputs",
            node_id.0
        )),
    }
}

fn as_text(value: &KernelValue) -> String {
    match value {
        KernelValue::Text(text) | KernelValue::Tag(text) => text.clone(),
        KernelValue::Number(number) => number.to_string(),
        KernelValue::Bool(value) => value.to_string(),
        KernelValue::Object(_) | KernelValue::List(_) | KernelValue::Skip => String::new(),
    }
}

fn value_node_id(kind: &IrNodeKind) -> NodeId {
    match kind {
        IrNodeKind::LinkBind { value, .. } => *value,
        _ => unreachable!("value_node_id only used with LinkBind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostInput, HostSnapshot};
    use crate::ir::{CallSiteId, FunctionId, IrProgram};
    use crate::lower::{
        TodoProgram, try_lower_circle_drawer, try_lower_complex_counter, try_lower_counter,
        try_lower_flight_booker, try_lower_list_retain_count, try_lower_list_retain_remove,
        try_lower_shopping_list, try_lower_timer, try_lower_todo_mvc,
    };
    use crate::runtime::Msg;
    use crate::text_input::KEYDOWN_TEXT_SEPARATOR;

    #[test]
    fn executes_lowered_counter_ir_across_batched_pulses() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = try_lower_counter(source).expect("counter lowers");
        let mut executor = IrExecutor::new(program.ir).expect("counter ir executes");

        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.press_port,
                        value: KernelValue::from("press"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.press_port,
                        value: KernelValue::from("press"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.press_port,
                        value: KernelValue::from("press"),
                        seq: CausalSeq::new(1, 2),
                    },
                ),
            ])
            .expect("messages execute");

        assert_eq!(
            executor.sink_value(program.counter_sink),
            Some(&KernelValue::from(3.0))
        );
    }

    #[test]
    fn link_bind_rebinds_link_read_to_latest_target() {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::SourcePort(SourcePortId(10)),
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("left")),
            },
            IrNode {
                id: NodeId(3),
                source_expr: None,
                kind: IrNodeKind::Then {
                    source: NodeId(1),
                    body: NodeId(2),
                },
            },
            IrNode {
                id: NodeId(4),
                source_expr: None,
                kind: IrNodeKind::SourcePort(SourcePortId(11)),
            },
            IrNode {
                id: NodeId(5),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("right")),
            },
            IrNode {
                id: NodeId(6),
                source_expr: None,
                kind: IrNodeKind::Then {
                    source: NodeId(4),
                    body: NodeId(5),
                },
            },
            IrNode {
                id: NodeId(7),
                source_expr: None,
                kind: IrNodeKind::LinkCell,
            },
            IrNode {
                id: NodeId(8),
                source_expr: None,
                kind: IrNodeKind::LinkBind {
                    value: NodeId(3),
                    target: NodeId(7),
                },
            },
            IrNode {
                id: NodeId(9),
                source_expr: None,
                kind: IrNodeKind::LinkBind {
                    value: NodeId(6),
                    target: NodeId(7),
                },
            },
            IrNode {
                id: NodeId(10),
                source_expr: None,
                kind: IrNodeKind::LinkRead { cell: NodeId(7) },
            },
            IrNode {
                id: NodeId(11),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(1),
                    input: NodeId(10),
                },
            },
        ];
        let mut executor = IrExecutor::new(nodes).expect("link ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(10),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("left bind executes");
        assert_eq!(
            executor.sink_value(SinkPortId(1)),
            Some(&KernelValue::from("left"))
        );
        let first_epoch = executor
            .link_state(NodeId(7))
            .expect("link state")
            .edge_epoch;

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(11),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("right bind executes");
        assert_eq!(
            executor.sink_value(SinkPortId(1)),
            Some(&KernelValue::from("right"))
        );
        assert!(
            executor
                .link_state(NodeId(7))
                .expect("link state")
                .edge_epoch
                > first_epoch
        );
    }

    #[test]
    fn executes_lowered_complex_counter_ir_with_mixed_snapshot_order() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let program = try_lower_complex_counter(source).expect("complex_counter lowers");
        let mut executor = IrExecutor::new(program.ir).expect("complex counter ir executes");

        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        let messages = HostSnapshot::new(vec![
            HostInput::Pulse {
                actor,
                port: program.increment_port,
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 0),
            },
            HostInput::Pulse {
                actor,
                port: program.increment_port,
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 1),
            },
            HostInput::Pulse {
                actor,
                port: program.decrement_port,
                value: KernelValue::from("press"),
                seq: CausalSeq::new(1, 2),
            },
        ]);

        let drained = messages
            .inputs
            .into_iter()
            .map(|input| match input {
                HostInput::Pulse {
                    actor,
                    port,
                    value,
                    seq,
                } => (actor, Msg::SourcePulse { port, value, seq }),
                HostInput::Mirror { .. } => unreachable!("complex counter uses only pulses"),
            })
            .collect::<Vec<_>>();
        executor.apply_messages(&drained).expect("messages execute");

        assert_eq!(
            executor.sink_value(program.counter_sink),
            Some(&KernelValue::from(1.0))
        );
    }

    #[test]
    fn source_pulses_do_not_retrigger_on_later_mirror_writes() {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::SourcePort(SourcePortId(10)),
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::MirrorCell(MirrorCellId(20)),
            },
            IrNode {
                id: NodeId(3),
                source_expr: None,
                kind: IrNodeKind::Then {
                    source: NodeId(1),
                    body: NodeId(2),
                },
            },
            IrNode {
                id: NodeId(4),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("")),
            },
            IrNode {
                id: NodeId(5),
                source_expr: None,
                kind: IrNodeKind::Hold {
                    seed: NodeId(4),
                    updates: NodeId(3),
                },
            },
            IrNode {
                id: NodeId(6),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(77),
                    input: NodeId(5),
                },
            },
        ];
        let mut executor = IrExecutor::new(nodes).expect("test ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: MirrorCellId(20),
                        value: KernelValue::from("first"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: SourcePortId(10),
                        value: KernelValue::from("go"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("first batch executes");
        assert_eq!(
            executor.sink_value(SinkPortId(77)),
            Some(&KernelValue::from("first"))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: MirrorCellId(20),
                    value: KernelValue::from("second"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("second batch executes");
        assert_eq!(
            executor.sink_value(SinkPortId(77)),
            Some(&KernelValue::from("first"))
        );
    }

    #[test]
    fn executes_range_get_sum_and_is_empty_nodes() {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(2.0)),
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(4.0)),
            },
            IrNode {
                id: NodeId(3),
                source_expr: None,
                kind: IrNodeKind::ListRange {
                    from: NodeId(1),
                    to: NodeId(2),
                },
            },
            IrNode {
                id: NodeId(4),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(2.0)),
            },
            IrNode {
                id: NodeId(5),
                source_expr: None,
                kind: IrNodeKind::ListGet {
                    list: NodeId(3),
                    index: NodeId(4),
                },
            },
            IrNode {
                id: NodeId(6),
                source_expr: None,
                kind: IrNodeKind::ListSum { list: NodeId(3) },
            },
            IrNode {
                id: NodeId(7),
                source_expr: None,
                kind: IrNodeKind::ListIsEmpty { list: NodeId(3) },
            },
            IrNode {
                id: NodeId(8),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(201),
                    input: NodeId(5),
                },
            },
            IrNode {
                id: NodeId(9),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(202),
                    input: NodeId(6),
                },
            },
            IrNode {
                id: NodeId(10),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(203),
                    input: NodeId(7),
                },
            },
        ];

        let executor = IrExecutor::new(nodes).expect("test ir executes");

        assert_eq!(
            executor.sink_value(SinkPortId(201)),
            Some(&KernelValue::from(3.0))
        );
        assert_eq!(
            executor.sink_value(SinkPortId(202)),
            Some(&KernelValue::from(9.0))
        );
        assert_eq!(
            executor.sink_value(SinkPortId(203)),
            Some(&KernelValue::from(false))
        );
    }

    #[test]
    fn list_get_returns_out_of_bounds_tag_for_invalid_index() {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::ListLiteral {
                    items: vec![NodeId(2)],
                },
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("only")),
            },
            IrNode {
                id: NodeId(3),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(3.0)),
            },
            IrNode {
                id: NodeId(4),
                source_expr: None,
                kind: IrNodeKind::ListGet {
                    list: NodeId(1),
                    index: NodeId(3),
                },
            },
            IrNode {
                id: NodeId(5),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(204),
                    input: NodeId(4),
                },
            },
        ];

        let executor = IrExecutor::new(nodes).expect("test ir executes");
        assert_eq!(
            executor.sink_value(SinkPortId(204)),
            Some(&KernelValue::Tag("OutOfBounds".to_string()))
        );
    }

    #[test]
    fn list_remove_removes_all_items_on_true_and_keeps_on_false() {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::ListLiteral {
                    items: vec![NodeId(2), NodeId(3)],
                },
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("left")),
            },
            IrNode {
                id: NodeId(3),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from("right")),
            },
            IrNode {
                id: NodeId(4),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(true)),
            },
            IrNode {
                id: NodeId(5),
                source_expr: None,
                kind: IrNodeKind::ListRemove {
                    list: NodeId(1),
                    predicate: NodeId(4),
                },
            },
            IrNode {
                id: NodeId(6),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(205),
                    input: NodeId(5),
                },
            },
            IrNode {
                id: NodeId(7),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::from(false)),
            },
            IrNode {
                id: NodeId(8),
                source_expr: None,
                kind: IrNodeKind::ListRemove {
                    list: NodeId(1),
                    predicate: NodeId(7),
                },
            },
            IrNode {
                id: NodeId(9),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: SinkPortId(206),
                    input: NodeId(8),
                },
            },
        ];

        let executor = IrExecutor::new(nodes).expect("test ir executes");
        assert_eq!(
            executor.sink_value(SinkPortId(205)),
            Some(&KernelValue::List(Vec::new()))
        );
        assert_eq!(
            executor.sink_value(SinkPortId(206)),
            Some(&KernelValue::List(vec![
                KernelValue::from("left"),
                KernelValue::from("right"),
            ]))
        );
    }

    #[test]
    fn executes_function_template_calls_with_parameters_and_captures() {
        let program = IrProgram {
            nodes: vec![
                IrNode {
                    id: NodeId(1),
                    source_expr: None,
                    kind: IrNodeKind::MirrorCell(MirrorCellId(1)),
                },
                IrNode {
                    id: NodeId(2),
                    source_expr: None,
                    kind: IrNodeKind::MirrorCell(MirrorCellId(2)),
                },
                IrNode {
                    id: NodeId(3),
                    source_expr: None,
                    kind: IrNodeKind::Call {
                        function: FunctionId(1),
                        call_site: CallSiteId(1),
                        args: vec![NodeId(1)],
                    },
                },
                IrNode {
                    id: NodeId(4),
                    source_expr: None,
                    kind: IrNodeKind::SinkPort {
                        port: SinkPortId(300),
                        input: NodeId(3),
                    },
                },
            ],
            functions: vec![IrFunctionTemplate {
                id: FunctionId(1),
                parameter_count: 1,
                output: NodeId(102),
                nodes: vec![
                    IrNode {
                        id: NodeId(100),
                        source_expr: None,
                        kind: IrNodeKind::Parameter { index: 0 },
                    },
                    IrNode {
                        id: NodeId(101),
                        source_expr: None,
                        kind: IrNodeKind::Add {
                            lhs: NodeId(100),
                            rhs: NodeId(2),
                        },
                    },
                    IrNode {
                        id: NodeId(102),
                        source_expr: None,
                        kind: IrNodeKind::Add {
                            lhs: NodeId(101),
                            rhs: NodeId(100),
                        },
                    },
                ],
            }],
        };
        let mut executor = IrExecutor::new_program(program).expect("function program executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: MirrorCellId(1),
                        value: KernelValue::from(7.0),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: MirrorCellId(2),
                        value: KernelValue::from(10.0),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("first function batch executes");

        assert_eq!(
            executor.sink_value(SinkPortId(300)),
            Some(&KernelValue::from(24.0))
        );
        assert_eq!(executor.function_instances.len(), 1);

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: MirrorCellId(1),
                    value: KernelValue::from(8.0),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("second function batch executes");

        assert_eq!(
            executor.sink_value(SinkPortId(300)),
            Some(&KernelValue::from(26.0))
        );
        assert_eq!(executor.function_instances.len(), 1);
    }

    #[test]
    fn list_map_reuses_stable_function_instances_for_range_items() {
        let program = IrProgram {
            nodes: vec![
                IrNode {
                    id: NodeId(1),
                    source_expr: None,
                    kind: IrNodeKind::Literal(KernelValue::from(1.0)),
                },
                IrNode {
                    id: NodeId(2),
                    source_expr: None,
                    kind: IrNodeKind::Literal(KernelValue::from(3.0)),
                },
                IrNode {
                    id: NodeId(3),
                    source_expr: None,
                    kind: IrNodeKind::MirrorCell(MirrorCellId(10)),
                },
                IrNode {
                    id: NodeId(4),
                    source_expr: None,
                    kind: IrNodeKind::ListRange {
                        from: NodeId(1),
                        to: NodeId(2),
                    },
                },
                IrNode {
                    id: NodeId(5),
                    source_expr: None,
                    kind: IrNodeKind::ListMap {
                        list: NodeId(4),
                        function: FunctionId(2),
                        call_site: CallSiteId(20),
                    },
                },
                IrNode {
                    id: NodeId(6),
                    source_expr: None,
                    kind: IrNodeKind::SinkPort {
                        port: SinkPortId(301),
                        input: NodeId(5),
                    },
                },
            ],
            functions: vec![IrFunctionTemplate {
                id: FunctionId(2),
                parameter_count: 1,
                output: NodeId(202),
                nodes: vec![
                    IrNode {
                        id: NodeId(200),
                        source_expr: None,
                        kind: IrNodeKind::Parameter { index: 0 },
                    },
                    IrNode {
                        id: NodeId(201),
                        source_expr: None,
                        kind: IrNodeKind::Add {
                            lhs: NodeId(200),
                            rhs: NodeId(3),
                        },
                    },
                    IrNode {
                        id: NodeId(202),
                        source_expr: None,
                        kind: IrNodeKind::Add {
                            lhs: NodeId(201),
                            rhs: NodeId(200),
                        },
                    },
                ],
            }],
        };
        let mut executor = IrExecutor::new_program(program).expect("list map program executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: MirrorCellId(10),
                    value: KernelValue::from(10.0),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("first list map batch executes");

        assert_eq!(
            executor.sink_value(SinkPortId(301)),
            Some(&KernelValue::List(vec![
                KernelValue::from(12.0),
                KernelValue::from(14.0),
                KernelValue::from(16.0),
            ]))
        );
        assert_eq!(executor.function_instances.len(), 3);

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: MirrorCellId(10),
                    value: KernelValue::from(20.0),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("second list map batch executes");

        assert_eq!(
            executor.sink_value(SinkPortId(301)),
            Some(&KernelValue::List(vec![
                KernelValue::from(22.0),
                KernelValue::from(24.0),
                KernelValue::from(26.0),
            ]))
        );
        assert_eq!(executor.function_instances.len(), 3);
    }

    #[test]
    fn executes_lowered_flight_booker_ir_for_return_booking() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
        );
        let program = try_lower_flight_booker(source).expect("flight_booker lowers");
        let mut executor = IrExecutor::new(program.ir).expect("flight_booker ir executes");

        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.flight_type_change_port,
                        value: KernelValue::from("return"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.departure_change_port,
                        value: KernelValue::from("2026-03-03"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.return_change_port,
                        value: KernelValue::from("2026-03-04"),
                        seq: CausalSeq::new(1, 2),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.book_press_port,
                        value: KernelValue::from("press"),
                        seq: CausalSeq::new(1, 3),
                    },
                ),
            ])
            .expect("messages execute");

        assert_eq!(
            executor.sink_value(program.booked_sink),
            Some(&KernelValue::from(
                "Booked return flight: 2026-03-03 to 2026-03-04"
            ))
        );
        assert_eq!(
            executor.sink_value(program.book_button_disabled_sink),
            Some(&KernelValue::from(false))
        );
    }

    #[test]
    fn executes_lowered_timer_ir_for_ticks_duration_and_reset() {
        let source = include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let program = try_lower_timer(source).expect("timer lowers");
        let mut executor = IrExecutor::new(program.ir).expect("timer ir executes");

        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.tick_port,
                        value: KernelValue::from("tick"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.tick_port,
                        value: KernelValue::from("tick"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.duration_change_port,
                        value: KernelValue::from("2"),
                        seq: CausalSeq::new(1, 2),
                    },
                ),
            ])
            .expect("messages execute");

        assert_eq!(
            executor.sink_value(program.elapsed_value_sink),
            Some(&KernelValue::from("0.2s"))
        );
        assert_eq!(
            executor.sink_value(program.progress_percent_sink),
            Some(&KernelValue::from("10%"))
        );
        assert_eq!(
            executor.sink_value(program.duration_value_sink),
            Some(&KernelValue::from("2s"))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: program.reset_press_port,
                    value: KernelValue::from("press"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("reset executes");

        assert_eq!(
            executor.sink_value(program.elapsed_value_sink),
            Some(&KernelValue::from("0s"))
        );
    }

    #[test]
    fn executes_lowered_circle_drawer_ir_for_clicks_and_undo() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
        );
        let program = try_lower_circle_drawer(source).expect("circle_drawer lowers");
        let mut executor = IrExecutor::new(program.ir).expect("circle_drawer ir executes");

        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.canvas_click_port,
                        value: KernelValue::Object(BTreeMap::from([
                            ("x".to_string(), KernelValue::from(10.0)),
                            ("y".to_string(), KernelValue::from(10.0)),
                        ])),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.canvas_click_port,
                        value: KernelValue::Object(BTreeMap::from([
                            ("x".to_string(), KernelValue::from(20.0)),
                            ("y".to_string(), KernelValue::from(20.0)),
                        ])),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("clicks execute");
        assert_eq!(
            executor.sink_value(program.count_sink),
            Some(&KernelValue::from("Circles: 2"))
        );
        assert_eq!(
            executor.sink_value(program.circles_sink),
            Some(&KernelValue::List(vec![
                KernelValue::Object(BTreeMap::from([
                    ("x".to_string(), KernelValue::from(10.0)),
                    ("y".to_string(), KernelValue::from(10.0)),
                ])),
                KernelValue::Object(BTreeMap::from([
                    ("x".to_string(), KernelValue::from(20.0)),
                    ("y".to_string(), KernelValue::from(20.0)),
                ])),
            ]))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: program.undo_press_port,
                    value: KernelValue::from("press"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("undo executes");
        assert_eq!(
            executor.sink_value(program.count_sink),
            Some(&KernelValue::from("Circles: 1"))
        );
        assert_eq!(
            executor.sink_value(program.circles_sink),
            Some(&KernelValue::List(vec![KernelValue::Object(
                BTreeMap::from([
                    ("x".to_string(), KernelValue::from(10.0)),
                    ("y".to_string(), KernelValue::from(10.0)),
                ])
            )]))
        );
    }

    #[test]
    fn executes_lowered_todo_filter_ir_for_filter_roundtrip() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo filter ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        assert_eq!(
            executor.sink_value(program.selected_filter_sink),
            Some(&KernelValue::from("all"))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(111),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("active filter executes");
        assert_eq!(
            executor.sink_value(program.selected_filter_sink),
            Some(&KernelValue::from("active"))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(112),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("completed filter executes");
        assert_eq!(
            executor.sink_value(program.selected_filter_sink),
            Some(&KernelValue::from("completed"))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(110),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(3, 0),
                },
            )])
            .expect("all filter executes");
        assert_eq!(
            executor.sink_value(program.selected_filter_sink),
            Some(&KernelValue::from("all"))
        );
    }

    #[test]
    fn executes_lowered_todo_main_input_ports_and_mirrors() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_TEXT_SINK),
            Some(&KernelValue::from(""))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(true))
        );

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::MAIN_INPUT_CHANGE_PORT,
                        value: KernelValue::from("Buy milk"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::MAIN_INPUT_FOCUSED_CELL,
                        value: KernelValue::from(false),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::MAIN_INPUT_FOCUS_HINT_CELL,
                        value: KernelValue::from(false),
                        seq: CausalSeq::new(1, 2),
                    },
                ),
            ])
            .expect("todo main input mirrors execute");

        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_TEXT_SINK),
            Some(&KernelValue::from("Buy milk"))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: KernelValue::List(vec![KernelValue::Object(BTreeMap::from([
                            ("id".to_string(), KernelValue::from(1.0)),
                            ("title".to_string(), KernelValue::from("Existing")),
                            ("completed".to_string(), KernelValue::from(false)),
                        ]))]),
                        seq: CausalSeq::new(2, 0),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::NEXT_TODO_ID_CELL,
                        value: KernelValue::from(2.0),
                        seq: CausalSeq::new(2, 1),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::MAIN_INPUT_CHANGE_PORT,
                        value: KernelValue::from("Buy milk"),
                        seq: CausalSeq::new(2, 2),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::MAIN_INPUT_KEY_DOWN_PORT,
                        value: KernelValue::from("Enter"),
                        seq: CausalSeq::new(2, 3),
                    },
                ),
            ])
            .expect("todo main input submit executes");

        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(true))
        );
    }

    #[test]
    fn executes_lowered_todo_main_input_focus_hint_clears_for_control_pulses() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(true))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::FILTER_ACTIVE_PORT,
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("filter pulse clears focus hint");
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: TodoProgram::MAIN_INPUT_FOCUS_HINT_CELL,
                    value: KernelValue::from(true),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("focus hint resets");
        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TOGGLE_ALL_PORT,
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(2, 1),
                },
            )])
            .expect("toggle-all pulse clears focus hint");
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: TodoProgram::MAIN_INPUT_FOCUS_HINT_CELL,
                    value: KernelValue::from(true),
                    seq: CausalSeq::new(3, 0),
                },
            )])
            .expect("focus hint resets again");
        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_BEGIN_EDIT_PORT,
                    value: KernelValue::Object(std::collections::BTreeMap::from([
                        ("id".to_string(), KernelValue::from(1.0)),
                        ("title".to_string(), KernelValue::from("Buy groceries")),
                    ])),
                    seq: CausalSeq::new(3, 1),
                },
            )])
            .expect("begin-edit pulse clears focus hint");
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );
    }

    #[test]
    fn executes_lowered_todo_main_input_focus_and_blur_ports() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[(
                actor,
                Msg::MirrorWrite {
                    cell: TodoProgram::MAIN_INPUT_FOCUS_HINT_CELL,
                    value: KernelValue::from(true),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("focus hint reset");

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::MAIN_INPUT_BLUR_PORT,
                    value: KernelValue::from("blur"),
                    seq: CausalSeq::new(1, 1),
                },
            )])
            .expect("blur pulse executes");
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(true))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::MAIN_INPUT_FOCUS_PORT,
                    value: KernelValue::from("focus"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("focus pulse executes");
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );
    }

    #[test]
    fn executes_lowered_todo_list_append_from_main_input_submit() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::MAIN_INPUT_DRAFT_CELL,
                        value: KernelValue::from("Test todo"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: KernelValue::List(vec![
                            KernelValue::Object(std::collections::BTreeMap::from([
                                ("id".to_string(), KernelValue::from(1.0)),
                                ("title".to_string(), KernelValue::from("Buy groceries")),
                                ("completed".to_string(), KernelValue::from(false)),
                            ])),
                            KernelValue::Object(std::collections::BTreeMap::from([
                                ("id".to_string(), KernelValue::from(2.0)),
                                ("title".to_string(), KernelValue::from("Clean room")),
                                ("completed".to_string(), KernelValue::from(false)),
                            ])),
                        ]),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::NEXT_TODO_ID_CELL,
                        value: KernelValue::from(3.0),
                        seq: CausalSeq::new(1, 2),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::MAIN_INPUT_KEY_DOWN_PORT,
                        value: KernelValue::from(format!("Enter{KEYDOWN_TEXT_SEPARATOR}Test todo")),
                        seq: CausalSeq::new(1, 3),
                    },
                ),
            ])
            .expect("todo submit executes");

        assert_eq!(
            executor.sink_value(TodoProgram::TODOS_LIST_SINK),
            Some(&KernelValue::List(vec![
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(1.0)),
                    ("title".to_string(), KernelValue::from("Buy groceries")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])),
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(2.0)),
                    ("title".to_string(), KernelValue::from("Clean room")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])),
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(3.0)),
                    ("title".to_string(), KernelValue::from("Test todo")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])),
            ]))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_TEXT_SINK),
            Some(&KernelValue::from(""))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ACTIVE_COUNT_SINK),
            Some(&KernelValue::from(3.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::COMPLETED_COUNT_SINK),
            Some(&KernelValue::from(0.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ALL_COMPLETED_SINK),
            Some(&KernelValue::from(false))
        );
    }

    #[test]
    fn executes_lowered_todo_list_toggle_toggle_all_and_clear_completed() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        let initial_list = KernelValue::List(vec![
            KernelValue::Object(std::collections::BTreeMap::from([
                ("id".to_string(), KernelValue::from(1.0)),
                ("title".to_string(), KernelValue::from("Buy groceries")),
                ("completed".to_string(), KernelValue::from(false)),
            ])),
            KernelValue::Object(std::collections::BTreeMap::from([
                ("id".to_string(), KernelValue::from(2.0)),
                ("title".to_string(), KernelValue::from("Clean room")),
                ("completed".to_string(), KernelValue::from(false)),
            ])),
        ]);

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: initial_list.clone(),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::TODO_TOGGLE_PORT,
                        value: KernelValue::Object(std::collections::BTreeMap::from([(
                            "id".to_string(),
                            KernelValue::from(1.0),
                        )])),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("todo toggle executes");

        assert_eq!(
            executor.sink_value(TodoProgram::TODOS_LIST_SINK),
            Some(&KernelValue::List(vec![
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(1.0)),
                    ("title".to_string(), KernelValue::from("Buy groceries")),
                    ("completed".to_string(), KernelValue::from(true)),
                ])),
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(2.0)),
                    ("title".to_string(), KernelValue::from("Clean room")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])),
            ]))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ACTIVE_COUNT_SINK),
            Some(&KernelValue::from(1.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::COMPLETED_COUNT_SINK),
            Some(&KernelValue::from(1.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ALL_COMPLETED_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: executor
                            .sink_value(TodoProgram::TODOS_LIST_SINK)
                            .cloned()
                            .expect("list after toggle"),
                        seq: CausalSeq::new(2, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::TOGGLE_ALL_PORT,
                        value: KernelValue::from("click"),
                        seq: CausalSeq::new(2, 1),
                    },
                ),
            ])
            .expect("toggle all executes");

        assert_eq!(
            executor.sink_value(TodoProgram::TODOS_LIST_SINK),
            Some(&KernelValue::List(vec![
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(1.0)),
                    ("title".to_string(), KernelValue::from("Buy groceries")),
                    ("completed".to_string(), KernelValue::from(true)),
                ])),
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(2.0)),
                    ("title".to_string(), KernelValue::from("Clean room")),
                    ("completed".to_string(), KernelValue::from(true)),
                ])),
            ]))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ACTIVE_COUNT_SINK),
            Some(&KernelValue::from(0.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::COMPLETED_COUNT_SINK),
            Some(&KernelValue::from(2.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ALL_COMPLETED_SINK),
            Some(&KernelValue::from(true))
        );

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: executor
                            .sink_value(TodoProgram::TODOS_LIST_SINK)
                            .cloned()
                            .expect("list after toggle all"),
                        seq: CausalSeq::new(3, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::CLEAR_COMPLETED_PORT,
                        value: KernelValue::from("click"),
                        seq: CausalSeq::new(3, 1),
                    },
                ),
            ])
            .expect("clear completed executes");

        assert_eq!(
            executor.sink_value(TodoProgram::TODOS_LIST_SINK),
            Some(&KernelValue::List(Vec::new()))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::ACTIVE_COUNT_SINK),
            Some(&KernelValue::from(0.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::COMPLETED_COUNT_SINK),
            Some(&KernelValue::from(0.0))
        );
    }

    #[test]
    fn executes_lowered_todo_list_edit_and_delete() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };
        let initial_list = KernelValue::List(vec![
            KernelValue::Object(std::collections::BTreeMap::from([
                ("id".to_string(), KernelValue::from(1.0)),
                ("title".to_string(), KernelValue::from("Buy groceries")),
                ("completed".to_string(), KernelValue::from(false)),
            ])),
            KernelValue::Object(std::collections::BTreeMap::from([
                ("id".to_string(), KernelValue::from(2.0)),
                ("title".to_string(), KernelValue::from("Clean room")),
                ("completed".to_string(), KernelValue::from(false)),
            ])),
        ]);

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: initial_list.clone(),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::TODO_EDIT_COMMIT_PORT,
                        value: KernelValue::Object(std::collections::BTreeMap::from([
                            ("id".to_string(), KernelValue::from(2.0)),
                            ("title".to_string(), KernelValue::from("Clean kitchen")),
                        ])),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("todo edit executes");

        assert_eq!(
            executor.sink_value(TodoProgram::TODOS_LIST_SINK),
            Some(&KernelValue::List(vec![
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(1.0)),
                    ("title".to_string(), KernelValue::from("Buy groceries")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])),
                KernelValue::Object(std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(2.0)),
                    ("title".to_string(), KernelValue::from("Clean kitchen")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])),
            ]))
        );

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::TODOS_LIST_CELL,
                        value: executor
                            .sink_value(TodoProgram::TODOS_LIST_SINK)
                            .cloned()
                            .expect("list after edit"),
                        seq: CausalSeq::new(2, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: TodoProgram::TODO_DELETE_PORT,
                        value: KernelValue::Object(std::collections::BTreeMap::from([(
                            "id".to_string(),
                            KernelValue::from(1.0),
                        )])),
                        seq: CausalSeq::new(2, 1),
                    },
                ),
            ])
            .expect("todo delete executes");

        assert_eq!(
            executor.sink_value(TodoProgram::TODOS_LIST_SINK),
            Some(&KernelValue::List(vec![KernelValue::Object(
                std::collections::BTreeMap::from([
                    ("id".to_string(), KernelValue::from(2.0)),
                    ("title".to_string(), KernelValue::from("Clean kitchen")),
                    ("completed".to_string(), KernelValue::from(false)),
                ])
            )]))
        );
    }

    #[test]
    fn executes_lowered_todo_edit_session_ui_state() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_BEGIN_EDIT_PORT,
                    value: KernelValue::Object(std::collections::BTreeMap::from([
                        ("id".to_string(), KernelValue::from(2.0)),
                        ("title".to_string(), KernelValue::from("Clean room")),
                    ])),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("begin edit executes");

        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_TARGET_SINK),
            Some(&KernelValue::from(2.0))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_DRAFT_SINK),
            Some(&KernelValue::from("Clean room"))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );
        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_EDIT_FOCUS_PORT,
                    value: KernelValue::from("focus"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("edit focus port executes");

        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUSED_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::EDIT_FOCUSED_CELL,
                        value: KernelValue::from(true),
                        seq: CausalSeq::new(3, 0),
                    },
                ),
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: TodoProgram::EDIT_TITLE_CELL,
                        value: KernelValue::from("Clean kitchen"),
                        seq: CausalSeq::new(3, 1),
                    },
                ),
            ])
            .expect("edit facts execute");

        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_DRAFT_SINK),
            Some(&KernelValue::from("Clean kitchen"))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUSED_SINK),
            Some(&KernelValue::from(true))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::MAIN_INPUT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_EDIT_BLUR_PORT,
                    value: KernelValue::from("blur"),
                    seq: CausalSeq::new(4, 0),
                },
            )])
            .expect("edit blur port executes");

        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_EDIT_CANCEL_PORT,
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(5, 0),
                },
            )])
            .expect("cancel edit executes");

        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_TARGET_SINK),
            Some(&KernelValue::Tag("none".to_string()))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_DRAFT_SINK),
            Some(&KernelValue::from(""))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUS_HINT_SINK),
            Some(&KernelValue::from(false))
        );
        assert_eq!(
            executor.sink_value(TodoProgram::EDIT_FOCUSED_SINK),
            Some(&KernelValue::from(false))
        );
    }

    #[test]
    fn executes_lowered_todo_hover_state() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = try_lower_todo_mvc(source).expect("todo_mvc lowers");
        let mut executor = IrExecutor::new(program.ir).expect("todo ui-state ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_HOVER_PORT,
                    value: KernelValue::Object(std::collections::BTreeMap::from([
                        ("id".to_string(), KernelValue::from(2.0)),
                        ("hovered".to_string(), KernelValue::from(true)),
                    ])),
                    seq: CausalSeq::new(1, 0),
                },
            )])
            .expect("hover executes");

        assert_eq!(
            executor.sink_value(TodoProgram::HOVERED_TARGET_SINK),
            Some(&KernelValue::from(2.0))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_HOVER_PORT,
                    value: KernelValue::Object(std::collections::BTreeMap::from([
                        ("id".to_string(), KernelValue::from(2.0)),
                        ("hovered".to_string(), KernelValue::from(false)),
                    ])),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("unhover executes");

        assert_eq!(
            executor.sink_value(TodoProgram::HOVERED_TARGET_SINK),
            Some(&KernelValue::Tag("none".to_string()))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_HOVER_PORT,
                    value: KernelValue::Object(std::collections::BTreeMap::from([
                        ("id".to_string(), KernelValue::from(2.0)),
                        ("hovered".to_string(), KernelValue::from(true)),
                    ])),
                    seq: CausalSeq::new(3, 0),
                },
            )])
            .expect("rehove executes");

        assert_eq!(
            executor.sink_value(TodoProgram::HOVERED_TARGET_SINK),
            Some(&KernelValue::from(2.0))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: TodoProgram::TODO_BEGIN_EDIT_PORT,
                    value: KernelValue::Object(std::collections::BTreeMap::from([
                        ("id".to_string(), KernelValue::from(2.0)),
                        ("title".to_string(), KernelValue::from("Clean room")),
                    ])),
                    seq: CausalSeq::new(3, 2),
                },
            )])
            .expect("begin edit clears hover");

        assert_eq!(
            executor.sink_value(TodoProgram::HOVERED_TARGET_SINK),
            Some(&KernelValue::Tag("none".to_string()))
        );
    }

    #[test]
    fn executes_lowered_shopping_list_ir_for_append_and_clear() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        );
        let program = try_lower_shopping_list(source).expect("shopping_list lowers");
        let mut executor = IrExecutor::new(program.ir).expect("shopping_list ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.input_change_port,
                        value: KernelValue::from("Milk"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.input_key_down_port,
                        value: KernelValue::from("Enter\u{1F}Milk"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("append executes");

        assert_eq!(
            executor.sink_value(program.input_sink),
            Some(&KernelValue::from(""))
        );
        assert_eq!(
            executor.sink_value(program.count_sink),
            Some(&KernelValue::from("1 items"))
        );
        assert_eq!(
            executor.sink_value(program.items_list_sink),
            Some(&KernelValue::List(vec![KernelValue::from("Milk")]))
        );

        executor
            .apply_messages(&[(
                actor,
                Msg::SourcePulse {
                    port: program.clear_press_port,
                    value: KernelValue::from("press"),
                    seq: CausalSeq::new(2, 0),
                },
            )])
            .expect("clear executes");

        assert_eq!(
            executor.sink_value(program.count_sink),
            Some(&KernelValue::from("0 items"))
        );
        assert_eq!(
            executor.sink_value(program.items_list_sink),
            Some(&KernelValue::List(Vec::new()))
        );
    }

    #[test]
    fn executes_lowered_list_retain_count_ir_for_append() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
        );
        let program = try_lower_list_retain_count(source).expect("list_retain_count lowers");
        let mut executor = IrExecutor::new(program.ir).expect("list_retain_count ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.input_change_port,
                        value: KernelValue::from("Apple"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.input_key_down_port,
                        value: KernelValue::from("Enter\u{1F}Apple"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("append executes");

        assert_eq!(
            executor.sink_value(program.all_count_sink),
            Some(&KernelValue::from("All count: 2"))
        );
        assert_eq!(
            executor.sink_value(program.retain_count_sink),
            Some(&KernelValue::from("Retain count: 2"))
        );
        assert_eq!(
            executor.sink_value(program.items_list_sink),
            Some(&KernelValue::List(vec![
                KernelValue::from("Initial"),
                KernelValue::from("Apple"),
            ]))
        );
    }

    #[test]
    fn executes_lowered_list_retain_remove_ir_for_append() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_remove/list_retain_remove.bn"
        );
        let program = try_lower_list_retain_remove(source).expect("list_retain_remove lowers");
        let mut executor = IrExecutor::new(program.ir).expect("list_retain_remove ir executes");
        let actor = ActorId {
            index: 0,
            generation: 0,
        };

        executor
            .apply_messages(&[
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.input_change_port,
                        value: KernelValue::from("Orange"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: program.input_key_down_port,
                        value: KernelValue::from("Enter\u{1F}Orange"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ])
            .expect("append executes");

        assert_eq!(
            executor.sink_value(program.count_sink),
            Some(&KernelValue::from("Count: 4"))
        );
        assert_eq!(
            executor.sink_value(program.items_list_sink),
            Some(&KernelValue::List(vec![
                KernelValue::from("Apple"),
                KernelValue::from("Banana"),
                KernelValue::from("Cherry"),
                KernelValue::from("Orange"),
            ]))
        );
    }
}
