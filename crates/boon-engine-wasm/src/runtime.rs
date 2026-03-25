use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use boon_scene::{RenderDiffBatch, UiEventBatch, UiFactBatch};

use super::PersistedWasmBatch;
use super::abi::{
    BufferRange, decode_render_diff_batch, decode_ui_event_batch, decode_ui_fact_batch,
    encode_render_diff_batch,
};
use super::codegen;
use super::exec_ir::ExecProgram;
use super::semantic_ir::{
    DerivedArithmeticOp, DerivedScalarOperand, DerivedScalarSpec, DerivedTextOperand, IntCompareOp,
    ItemScalarUpdate, ItemTextUpdate, NestedObjectListAction, NestedObjectListUpdate,
    ObjectDerivedScalarOperand, ObjectItemActionKind, ObjectItemActionSpec, ObjectListFilter,
    ObjectListItem, ObjectListUpdate, RuntimeModel, ScalarUpdate, SemanticAction,
    SemanticFactBinding, SemanticFactKind, SemanticInputValue, SemanticNode, SemanticProgram,
    SemanticStyleFragment, SemanticTextPart, StateRuntimeModel, TextListFilter, TextListUpdate,
    TextUpdate, bootstrap_runtime_scaffold,
};

pub(crate) const KEYDOWN_TEXT_SEPARATOR: char = '\u{1F}';
const SCALAR_PROPERTY_TOKEN_PREFIX: &str = "__boon_scalar_binding__:";
const OBJECT_FIELD_PROPERTY_TOKEN_PREFIX: &str = "__boon_object_field__:";

#[derive(Debug, Default)]
pub struct WasmRuntime {
    memory: Vec<u8>,
    pending_commands: Option<BufferRange>,
    persistence_history: Vec<PersistedWasmBatch>,
    event_history: Vec<UiEventBatch>,
    fact_history: Vec<UiFactBatch>,
    source_len: usize,
    external_functions: usize,
    persistence_enabled: bool,
    draft_text: String,
    focused: bool,
    active_program: Option<SemanticProgram>,
    incremental_diff_enabled: bool,
    last_exec: Option<ExecProgram>,
    active_actions: HashMap<boon_scene::EventPortId, SemanticAction>,
    active_input_bindings: HashMap<boon_scene::EventPortId, String>,
    active_input_nodes: HashMap<boon_scene::NodeId, String>,
    active_fact_bindings: HashMap<(boon_scene::NodeId, SemanticFactKind), String>,
    active_draft_binding: Option<String>,
    scalar_values: BTreeMap<String, i64>,
    scalar_tenths_remainders: BTreeMap<String, i64>,
    text_values: BTreeMap<String, String>,
    text_lists: BTreeMap<String, Vec<String>>,
    object_lists: BTreeMap<String, Vec<ObjectListItem>>,
    input_texts: BTreeMap<String, String>,
    scalar_mirrors: BTreeMap<String, Vec<String>>,
    text_mirrors: BTreeMap<String, Vec<String>>,
    route_bindings: Vec<String>,
    derived_scalars: Vec<DerivedScalarSpec>,
    next_object_item_id: u64,
    action_history: Vec<String>,
}

impl WasmRuntime {
    #[must_use]
    pub fn new(source_len: usize, external_functions: usize, persistence_enabled: bool) -> Self {
        Self {
            source_len,
            external_functions,
            persistence_enabled,
            ..Self::default()
        }
    }

    pub fn init(&mut self, program: &ExecProgram) -> u64 {
        self.event_history.clear();
        self.fact_history.clear();
        self.persistence_history.clear();
        self.action_history.clear();
        self.draft_text.clear();
        self.focused = false;
        self.last_exec = None;
        self.active_draft_binding = None;
        self.active_program = Some(SemanticProgram {
            root: program.semantic_root.clone(),
            runtime: program.runtime.clone(),
        });
        self.scalar_tenths_remainders.clear();
        self.scalar_values = match &program.runtime {
            RuntimeModel::Scalars(model) => model.values.clone(),
            RuntimeModel::State(StateRuntimeModel {
                scalar_values,
                text_values,
                text_lists,
                object_lists,
                input_texts,
                scalar_mirrors,
                text_mirrors,
                route_bindings,
                derived_scalars,
            }) => {
                self.text_values = text_values.clone();
                self.text_lists = text_lists.clone();
                self.object_lists = object_lists.clone();
                self.input_texts = input_texts.clone();
                self.scalar_mirrors = scalar_mirrors.clone();
                self.text_mirrors = text_mirrors.clone();
                self.route_bindings = route_bindings.clone();
                self.derived_scalars = derived_scalars.clone();
                scalar_values.clone()
            }
            RuntimeModel::Static => BTreeMap::new(),
        };
        if !matches!(program.runtime, RuntimeModel::State(_)) {
            self.text_values.clear();
            self.text_lists.clear();
            self.object_lists.clear();
            self.input_texts.clear();
            self.scalar_mirrors.clear();
            self.text_mirrors.clear();
            self.route_bindings.clear();
            self.derived_scalars.clear();
        }
        self.next_object_item_id = self
            .object_lists
            .values()
            .flatten()
            .map(max_object_item_id)
            .max()
            .unwrap_or_default()
            + 1;
        self.refresh_derived_scalars();
        self.render_active_program()
    }

    pub fn dispatch_events(&mut self, bytes: &[u8]) -> Result<u64, serde_json::Error> {
        let batch = decode_ui_event_batch(bytes)?;
        if self.active_program.is_some() {
            let suppress_cells_edit_render = batch
                .events
                .iter()
                .all(|event| self.is_cells_edit_input_event(event));
            let mut changed = false;
            for event in &batch.events {
                self.apply_event_effect(event);
                if let Some(action) = self.active_actions.get(&event.target).cloned() {
                    let event_changed = self.apply_action(action.clone(), event);
                    self.record_action(event, &action, event_changed);
                    changed |= event_changed;
                } else {
                    self.record_missing_action(event);
                }
            }
            if changed {
                self.refresh_derived_scalars();
            }
            self.persistence_history
                .push(PersistedWasmBatch::Events(batch.clone()));
            self.event_history.push(batch);
            return Ok(if changed {
                if suppress_cells_edit_render {
                    0
                } else {
                    self.render_active_program()
                }
            } else {
                0
            });
        }
        for event in &batch.events {
            self.apply_event_effect(event);
        }
        self.persistence_history
            .push(PersistedWasmBatch::Events(batch.clone()));
        self.event_history.push(batch);
        Ok(self.queue_commands(self.status_batch()))
    }

    pub fn apply_facts(&mut self, bytes: &[u8]) -> Result<u64, serde_json::Error> {
        let batch = decode_ui_fact_batch(bytes)?;
        if self.active_program.is_some() {
            let changed = self.apply_fact_effects(&batch);
            self.persistence_history
                .push(PersistedWasmBatch::Facts(batch.clone()));
            self.fact_history.push(batch);
            return Ok(if changed {
                self.render_active_program()
            } else {
                0
            });
        }
        self.apply_fact_effects(&batch);
        self.persistence_history
            .push(PersistedWasmBatch::Facts(batch.clone()));
        self.fact_history.push(batch);
        Ok(self.queue_commands(self.status_batch()))
    }

    #[must_use]
    pub fn take_commands(&mut self) -> u64 {
        self.pending_commands.take().map_or(0, BufferRange::pack)
    }

    #[must_use]
    pub fn memory(&self) -> &[u8] {
        &self.memory
    }

    pub fn decode_commands(&self, descriptor: u64) -> Result<RenderDiffBatch, serde_json::Error> {
        let range = BufferRange::from_packed(descriptor);
        let bytes = range.slice(&self.memory).unwrap_or(&[]);
        decode_render_diff_batch(bytes)
    }

    #[must_use]
    pub fn persisted_history(&self) -> Vec<PersistedWasmBatch> {
        self.persistence_history.clone()
    }

    #[must_use]
    pub fn debug_snapshot(&self) -> serde_json::Value {
        let overrides = self.object_lists.get("overrides");
        let last_override = overrides
            .and_then(|overrides| overrides.last())
            .map(|item| {
                serde_json::json!({
                    "row": item.scalar_fields.get("row").copied().unwrap_or_default(),
                    "column": item.scalar_fields.get("column").copied().unwrap_or_default(),
                    "text": item.text_fields.get("text").cloned().unwrap_or_default(),
                })
            });
        let last_exec_event_bindings = self
            .last_exec
            .as_ref()
            .map(|exec| {
                exec.event_bindings
                    .iter()
                    .take(12)
                    .map(|binding| {
                        serde_json::json!({
                            "port": binding.port.0.to_string(),
                            "source_binding": binding.source_binding,
                            "has_action": binding.action.is_some(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let object_list_item_actions = self
            .active_program
            .as_ref()
            .map(|program| collect_all_object_list_item_action_summaries(&program.root))
            .unwrap_or_default();
        let object_list_bindings = self
            .active_program
            .as_ref()
            .map(|program| collect_object_list_bindings(&program.root))
            .unwrap_or_default();
        let editing_branches = self
            .active_program
            .as_ref()
            .map(|program| {
                collect_branch_summaries(&program.root)
                    .into_iter()
                    .filter(|summary| summary.contains("editing"))
                    .take(20)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let materialized_has_input = self.active_program.as_ref().is_some_and(|program| {
            semantic_tree_contains_tag(&self.materialize_node(&program.root, &[]), "input")
        });
        serde_json::json!({
            "draftText": self.draft_text,
            "focused": self.focused,
            "eventCount": self.event_history.len(),
            "factCount": self.fact_history.len(),
            "lastEvent": self.last_event_summary(),
            "lastAction": self.last_action_summary(),
            "lastFact": self.last_fact_summary(),
            "recentEvents": self.recent_event_summaries(8),
            "recentActions": self.recent_action_summaries(8),
            "recentFacts": self.recent_fact_summaries(8),
            "activeActions": self.active_actions.len(),
            "activeInputBindingEntries": self.active_input_bindings
                .iter()
                .map(|(port, binding)| (port.0.to_string(), binding.clone()))
                .collect::<Vec<_>>(),
            "lastExecEventBindingCount": self.last_exec.as_ref().map_or(0, |exec| exec.event_bindings.len()),
            "lastExecEventBindings": last_exec_event_bindings,
            "objectListBindings": object_list_bindings,
            "objectListItemActions": object_list_item_actions,
            "activeInputNodeEntries": self.active_input_nodes
                .iter()
                .map(|(node_id, binding)| (node_id.0.to_string(), binding.clone()))
                .collect::<Vec<_>>(),
            "activeDraftBinding": self.active_draft_binding,
            "scalarValues": self.scalar_values,
            "textValues": self.text_values,
            "inputTexts": self.input_texts,
            "routeBindings": self.route_bindings,
            "currentRoute": self.primary_route_path(),
            "overridesCount": overrides.map_or(0, Vec::len),
            "lastOverride": last_override,
            "editingBranches": editing_branches,
            "materializedHasInput": materialized_has_input,
        })
    }

    #[must_use]
    pub fn primary_route_path(&self) -> Option<String> {
        self.route_bindings
            .first()
            .and_then(|binding| self.text_values.get(binding))
            .cloned()
    }

    pub fn set_route_path(&mut self, path: &str) -> bool {
        if self.route_bindings.is_empty() {
            return false;
        }
        let next = if path.is_empty() { "/" } else { path };
        let mut changed = false;
        for binding in &self.route_bindings {
            let entry = self.text_values.entry(binding.clone()).or_default();
            if entry.as_str() != next {
                *entry = next.to_string();
                changed = true;
            }
        }
        if changed {
            self.refresh_derived_scalars();
        }
        changed
    }

    #[must_use]
    pub fn set_route_path_and_render(&mut self, path: &str) -> u64 {
        if !self.set_route_path(path) || self.active_program.is_none() {
            return 0;
        }
        self.render_active_program()
    }

    fn queue_commands(&mut self, batch: RenderDiffBatch) -> u64 {
        let bytes = encode_render_diff_batch(&batch);
        let start = self.memory.len() as u32;
        let len = bytes.len() as u32;
        self.memory.extend_from_slice(&bytes);
        let range = BufferRange::new(start, len);
        self.pending_commands = Some(range);
        range.pack()
    }

    fn render_active_program(&mut self) -> u64 {
        let Some(program) = self.active_program.as_ref() else {
            return self.queue_commands(self.status_batch());
        };
        let materialized = SemanticProgram {
            root: self.materialize_node(&program.root, &[]),
            runtime: program.runtime.clone(),
        };
        let exec = ExecProgram::from_semantic(&materialized);
        let batch = if self.incremental_diff_enabled {
            self.last_exec.as_ref().map_or_else(
                || codegen::emit_render_batch(&exec),
                |previous| codegen::emit_render_diff(previous, &exec),
            )
        } else {
            codegen::emit_render_batch(&exec)
        };
        self.active_actions = exec
            .event_bindings
            .iter()
            .filter_map(|binding| binding.action.clone().map(|action| (binding.port, action)))
            .collect();
        self.active_input_bindings = exec
            .event_bindings
            .iter()
            .filter_map(|binding| {
                binding
                    .source_binding
                    .clone()
                    .map(|source_binding| (binding.port, source_binding))
            })
            .collect();
        let port_to_node: HashMap<boon_scene::EventPortId, boon_scene::NodeId> = exec
            .setup_ops
            .iter()
            .filter_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort { id, port, .. } => Some((*port, *id)),
                _ => None,
            })
            .collect();
        self.active_input_nodes = self
            .active_input_bindings
            .iter()
            .filter_map(|(port, source_binding)| {
                port_to_node
                    .get(port)
                    .copied()
                    .map(|id| (id, source_binding.clone()))
            })
            .collect();
        self.active_fact_bindings = exec
            .fact_bindings
            .iter()
            .map(|binding| ((binding.id, binding.kind.clone()), binding.binding.clone()))
            .collect();
        self.last_exec = Some(exec);
        if batch.ops.is_empty() {
            return 0;
        }
        self.queue_commands(batch)
    }

    pub fn enable_incremental_diff(&mut self) {
        self.incremental_diff_enabled = true;
    }

    fn is_cells_edit_input_event(&self, event: &boon_scene::UiEvent) -> bool {
        matches!(
            event.kind,
            boon_scene::UiEventKind::Input | boon_scene::UiEventKind::Change
        ) && self
            .active_input_bindings
            .get(&event.target)
            .is_some_and(|binding| binding.ends_with(".cell_elements.editing"))
    }

    pub(crate) fn should_strip_preview_keydown_text(
        &self,
        target: boon_scene::EventPortId,
    ) -> bool {
        let Some(binding) = self.active_input_bindings.get(&target) else {
            return false;
        };
        binding.ends_with(".cell_elements.editing")
            && self.active_draft_binding.as_deref() == Some(binding.as_str())
            && self
                .input_texts
                .get(binding)
                .is_none_or(|value| value != &self.draft_text)
    }

    fn status_batch(&self) -> RenderDiffBatch {
        let semantic = bootstrap_runtime_scaffold(
            self.source_len,
            self.external_functions,
            self.persistence_enabled,
            self.event_history.len(),
            self.fact_history.len(),
            self.last_event_summary().as_deref(),
            self.last_fact_summary().as_deref(),
            &self.draft_text,
            self.focused,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        codegen::emit_render_batch(&exec)
    }

    fn materialize_node(&self, node: &SemanticNode, path: &[usize]) -> SemanticNode {
        self.materialize_node_with_scope(node, path, None)
    }

    fn materialize_node_with_scope(
        &self,
        node: &SemanticNode,
        path: &[usize],
        current_element_scope: Option<&str>,
    ) -> SemanticNode {
        match node {
            SemanticNode::Fragment(children) => SemanticNode::Fragment(
                children
                    .iter()
                    .enumerate()
                    .map(|(index, child)| {
                        let child_path = child_path(path, index);
                        self.materialize_node_with_scope(child, &child_path, current_element_scope)
                    })
                    .collect(),
            ),
            SemanticNode::Keyed { key, node } => SemanticNode::Keyed {
                key: *key,
                node: Box::new(self.materialize_node_with_scope(node, path, current_element_scope)),
            },
            SemanticNode::Element {
                tag,
                text,
                properties,
                input_value,
                style_fragments,
                event_bindings,
                fact_bindings,
                children,
            } => {
                let mut properties = properties.clone();
                self.materialize_dynamic_properties(&mut properties, None);
                if tag == "input" || tag == "select" {
                    if let Some(value_source) = input_value {
                        let value = self.render_input_value(value_source);
                        if let Some((_, property_value)) =
                            properties.iter_mut().find(|(name, _)| name == "value")
                        {
                            *property_value = value;
                        } else {
                            properties.push(("value".to_string(), value));
                        }
                    }
                    if let Some(source_binding) = event_bindings
                        .iter()
                        .find_map(|binding| binding.source_binding.as_ref())
                    {
                        if self.active_draft_binding.as_deref() == Some(source_binding.as_str()) {
                            let value = self.draft_text.clone();
                            if let Some((_, property_value)) =
                                properties.iter_mut().find(|(name, _)| name == "value")
                            {
                                *property_value = value;
                            } else {
                                properties.push(("value".to_string(), value));
                            }
                        }
                    }
                }
                let next_element_scope = local_element_scope(path, None, fact_bindings)
                    .or_else(|| current_element_scope.map(ToString::to_string));
                merge_style_fragments(
                    &mut properties,
                    style_fragments,
                    &self.scalar_values,
                    &self.text_lists,
                    &self.object_lists,
                    next_element_scope.as_deref(),
                    None,
                );
                let fact_bindings = fact_bindings
                    .iter()
                    .map(|binding| SemanticFactBinding {
                        kind: binding.kind.clone(),
                        binding: scope_element_binding(
                            &binding.binding,
                            next_element_scope.as_deref(),
                        ),
                    })
                    .collect();
                SemanticNode::element_with_facts_and_styles(
                    tag.clone(),
                    text.clone(),
                    properties,
                    Vec::new(),
                    event_bindings.clone(),
                    fact_bindings,
                    children
                        .iter()
                        .enumerate()
                        .map(|(index, child)| {
                            let child_path = child_path(path, index);
                            self.materialize_node_with_scope(
                                child,
                                &child_path,
                                next_element_scope.as_deref(),
                            )
                        })
                        .collect(),
                )
            }
            SemanticNode::Text(text) => SemanticNode::Text(text.clone()),
            SemanticNode::TextTemplate { parts, .. } => {
                SemanticNode::Text(self.render_text_parts(parts))
            }
            SemanticNode::TextBindingBranch {
                binding,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = self.text_values.get(binding).map_or(true, String::is_empty);
                let branch = if is_empty != *invert { truthy } else { falsy };
                self.materialize_node_with_scope(branch, path, current_element_scope)
            }
            SemanticNode::BoolBranch {
                binding,
                truthy,
                falsy,
            } => {
                let scoped_binding = scope_element_binding(binding, current_element_scope);
                let branch = if self
                    .scalar_values
                    .get(&scoped_binding)
                    .copied()
                    .unwrap_or_default()
                    != 0
                {
                    truthy
                } else {
                    falsy
                };
                self.materialize_node_with_scope(branch, path, current_element_scope)
            }
            SemanticNode::ScalarCompareBranch {
                left,
                op,
                right,
                truthy,
                falsy,
            } => {
                let branch = if self.scalar_compare_matches(left, op.clone(), right) {
                    truthy
                } else {
                    falsy
                };
                self.materialize_node_with_scope(branch, path, current_element_scope)
            }
            SemanticNode::ObjectScalarCompareBranch { falsy, .. } => {
                self.materialize_node_with_scope(falsy, path, current_element_scope)
            }
            SemanticNode::ObjectBoolFieldBranch { falsy, .. } => {
                self.materialize_node_with_scope(falsy, path, current_element_scope)
            }
            SemanticNode::ObjectTextFieldBranch { falsy, .. } => {
                self.materialize_node_with_scope(falsy, path, current_element_scope)
            }
            SemanticNode::ListEmptyBranch {
                binding,
                object_items,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = if *object_items {
                    self.object_lists.get(binding).map_or(true, Vec::is_empty)
                } else {
                    self.text_lists.get(binding).map_or(true, Vec::is_empty)
                };
                let branch = if is_empty != *invert { truthy } else { falsy };
                self.materialize_node_with_scope(branch, path, current_element_scope)
            }
            SemanticNode::ScalarValue { binding, .. } => SemanticNode::Text(
                self.scalar_values
                    .get(binding)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            SemanticNode::ObjectFieldValue { .. } => SemanticNode::Text(String::new()),
            SemanticNode::TextList {
                binding,
                filter,
                template,
                ..
            } => SemanticNode::TextList {
                binding: binding.clone(),
                values: self.filtered_text_list_values(binding, filter.as_ref()),
                filter: filter.clone(),
                template: template.clone(),
            },
            SemanticNode::ObjectList {
                binding,
                filter,
                item_actions,
                template,
            } => SemanticNode::Fragment(
                self.filtered_object_list_items(binding, filter.as_ref())
                    .into_iter()
                    .map(|item| {
                        let child_path = keyed_child_path(path, item.id);
                        SemanticNode::keyed(
                            item.id,
                            self.materialize_object_template(
                                binding,
                                &item,
                                item_actions,
                                template.as_ref(),
                                &child_path,
                                None,
                            ),
                        )
                    })
                    .collect(),
            ),
        }
    }

    fn set_scalar_value(&mut self, binding: &str, value: i64) -> bool {
        let mut changed = false;
        let mut pending = VecDeque::from([binding.to_string()]);
        let mut seen = BTreeSet::new();
        while let Some(current) = pending.pop_front() {
            if !seen.insert(current.clone()) {
                continue;
            }
            let entry = self.scalar_values.entry(current.clone()).or_default();
            if *entry != value {
                *entry = value;
                changed = true;
            }
            self.scalar_tenths_remainders.remove(&current);
            if let Some(targets) = self.scalar_mirrors.get(&current) {
                pending.extend(targets.iter().cloned());
            }
        }
        changed
    }

    fn set_text_value(&mut self, binding: &str, value: String) -> bool {
        let mut changed = false;
        let mut pending = VecDeque::from([binding.to_string()]);
        let mut seen = BTreeSet::new();
        while let Some(current) = pending.pop_front() {
            if !seen.insert(current.clone()) {
                continue;
            }
            let entry = self.text_values.entry(current.clone()).or_default();
            if *entry != value {
                *entry = value.clone();
                changed = true;
            }
            if let Some(targets) = self.text_mirrors.get(&current) {
                pending.extend(targets.iter().cloned());
            }
        }
        changed
    }

    fn apply_action(&mut self, action: SemanticAction, event: &boon_scene::UiEvent) -> bool {
        match action {
            SemanticAction::Batch { actions } => {
                let mut changed = false;
                for action in actions {
                    changed |= self.apply_action(action, event);
                }
                changed
            }
            SemanticAction::UpdateScalars { updates } => {
                let mut changed = false;
                for update in updates {
                    match update {
                        ScalarUpdate::Set { binding, value } => {
                            changed |= self.set_scalar_value(&binding, value);
                        }
                        ScalarUpdate::SetFromPayloadNumber { binding } => {
                            let Some(value) = event_numeric_payload(event) else {
                                continue;
                            };
                            changed |= self.set_scalar_value(&binding, value);
                        }
                        ScalarUpdate::SetFiltered {
                            binding,
                            value,
                            payload_filter,
                        } => {
                            if !event_payload_matches(event, &payload_filter) {
                                continue;
                            }
                            changed |= self.set_scalar_value(&binding, value);
                        }
                        ScalarUpdate::Add { binding, delta } => {
                            let next = self
                                .scalar_values
                                .get(&binding)
                                .copied()
                                .unwrap_or_default()
                                + delta;
                            changed |= self.set_scalar_value(&binding, next);
                        }
                        ScalarUpdate::AddTenths {
                            binding,
                            tenths_delta,
                        } => {
                            let remainder = self
                                .scalar_tenths_remainders
                                .entry(binding.clone())
                                .or_default();
                            *remainder += tenths_delta;
                            let whole_delta = *remainder / 10;
                            *remainder %= 10;
                            if whole_delta != 0 {
                                let next = self
                                    .scalar_values
                                    .get(&binding)
                                    .copied()
                                    .unwrap_or_default()
                                    + whole_delta;
                                changed |= self.set_scalar_value(&binding, next);
                            }
                        }
                        ScalarUpdate::ToggleBool { binding } => {
                            let next = if self
                                .scalar_values
                                .get(&binding)
                                .copied()
                                .unwrap_or_default()
                                == 0
                            {
                                1
                            } else {
                                0
                            };
                            changed |= self.set_scalar_value(&binding, next);
                        }
                    }
                }
                changed
            }
            SemanticAction::UpdateTexts { updates } => {
                let mut changed = false;
                for update in updates {
                    match update {
                        TextUpdate::SetStatic {
                            binding,
                            value,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            changed |= self.set_text_value(&binding, value);
                        }
                        TextUpdate::SetComputed {
                            binding,
                            parts,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            let next = self.render_text_parts(&parts);
                            changed |= self.set_text_value(&binding, next);
                        }
                        TextUpdate::SetComputedBranch {
                            binding,
                            condition_binding,
                            truthy_parts,
                            falsy_parts,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            let next = if self
                                .scalar_values
                                .get(&condition_binding)
                                .copied()
                                .unwrap_or_default()
                                != 0
                            {
                                self.render_text_parts(&truthy_parts)
                            } else {
                                self.render_text_parts(&falsy_parts)
                            };
                            changed |= self.set_text_value(&binding, next);
                        }
                        TextUpdate::SetFromInput {
                            binding,
                            source_binding,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            let next = if event.kind == boon_scene::UiEventKind::KeyDown {
                                event_keydown_text(event)
                                    .map(ToString::to_string)
                                    .or_else(|| {
                                        self.input_texts.get(&source_binding).cloned().or_else(
                                            || {
                                                self.active_input_bindings
                                                    .get(&event.target)
                                                    .and_then(|binding| {
                                                        self.input_texts.get(binding)
                                                    })
                                                    .cloned()
                                            },
                                        )
                                    })
                                    .unwrap_or_default()
                            } else {
                                self.input_texts
                                    .get(&source_binding)
                                    .cloned()
                                    .or_else(|| {
                                        self.active_input_bindings
                                            .get(&event.target)
                                            .and_then(|binding| self.input_texts.get(binding))
                                            .cloned()
                                    })
                                    .unwrap_or_default()
                            };
                            changed |= self.set_text_value(&binding, next);
                        }
                        TextUpdate::SetFromPayload { binding } => {
                            let next = event_primary_payload(event).unwrap_or_default().to_string();
                            changed |= self.set_text_value(&binding, next);
                        }
                        TextUpdate::SetFromValueSource {
                            binding,
                            value,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            let next = self.render_input_value(&value);
                            changed |= self.set_text_value(&binding, next);
                        }
                    }
                }
                changed
            }
            SemanticAction::UpdateTextLists { updates } => {
                let mut changed = false;
                for update in updates {
                    match update {
                        TextListUpdate::AppendDraftText {
                            binding,
                            source_binding,
                            key,
                            trim,
                            reject_empty,
                            clear_draft,
                        } => {
                            if key
                                .as_ref()
                                .is_some_and(|expected| !event_payload_matches(event, expected))
                            {
                                continue;
                            }
                            let mut item = self
                                .input_texts
                                .get(&source_binding)
                                .cloned()
                                .unwrap_or_default();
                            if trim {
                                item = item.trim().to_string();
                            }
                            if reject_empty && item.is_empty() {
                                continue;
                            }
                            self.text_lists.entry(binding).or_default().push(item);
                            if clear_draft {
                                self.input_texts.insert(source_binding, String::new());
                                self.draft_text.clear();
                            }
                            changed = true;
                        }
                        TextListUpdate::Clear { binding } => {
                            let list = self.text_lists.entry(binding).or_default();
                            if !list.is_empty() {
                                list.clear();
                                changed = true;
                            }
                        }
                    }
                }
                changed
            }
            SemanticAction::UpdateObjectLists { updates } => {
                let mut changed = false;
                for update in updates {
                    match update {
                        ObjectListUpdate::AppendObject { binding, item } => {
                            let item = self.assign_fresh_object_item_ids(item);
                            self.object_lists.entry(binding).or_default().push(item);
                            changed = true;
                        }
                        ObjectListUpdate::AppendBoundObject {
                            binding,
                            scalar_bindings,
                            text_bindings,
                            payload_filter,
                        } => {
                            if payload_filter
                                .as_ref()
                                .is_some_and(|expected| !event_payload_matches(event, expected))
                            {
                                continue;
                            }
                            let mut item = ObjectListItem {
                                id: self.next_object_item_id,
                                title: String::new(),
                                completed: false,
                                text_fields: BTreeMap::new(),
                                bool_fields: BTreeMap::new(),
                                scalar_fields: BTreeMap::new(),
                                object_lists: BTreeMap::new(),
                                nested_item_actions: BTreeMap::new(),
                            };
                            for (field, binding_name) in scalar_bindings {
                                item.scalar_fields.insert(
                                    field.clone(),
                                    self.scalar_values
                                        .get(&binding_name)
                                        .copied()
                                        .unwrap_or_default(),
                                );
                            }
                            for (field, binding_name) in text_bindings {
                                let value = self
                                    .text_values
                                    .get(&binding_name)
                                    .cloned()
                                    .unwrap_or_default();
                                if field == "title" {
                                    item.title = value.clone();
                                }
                                item.text_fields.insert(field.clone(), value);
                            }
                            self.object_lists.entry(binding).or_default().push(item);
                            self.next_object_item_id += 1;
                            changed = true;
                        }
                        ObjectListUpdate::AppendPayloadObject {
                            binding,
                            scalar_payload_fields,
                        } => {
                            let Some(scalar_fields) =
                                event_scalar_payload_fields(event, &scalar_payload_fields)
                            else {
                                continue;
                            };
                            self.object_lists
                                .entry(binding)
                                .or_default()
                                .push(ObjectListItem {
                                    id: self.next_object_item_id,
                                    title: String::new(),
                                    completed: false,
                                    text_fields: BTreeMap::new(),
                                    bool_fields: BTreeMap::new(),
                                    scalar_fields,
                                    object_lists: BTreeMap::new(),
                                    nested_item_actions: BTreeMap::new(),
                                });
                            self.next_object_item_id += 1;
                            changed = true;
                        }
                        ObjectListUpdate::AppendDraftObject {
                            binding,
                            source_binding,
                            key,
                            trim,
                            reject_empty,
                            clear_draft,
                            bool_fields,
                        } => {
                            if key
                                .as_ref()
                                .is_some_and(|expected| !event_payload_matches(event, expected))
                            {
                                continue;
                            }
                            let mut title = self
                                .input_texts
                                .get(&source_binding)
                                .cloned()
                                .unwrap_or_default();
                            if trim {
                                title = title.trim().to_string();
                            }
                            if reject_empty && title.is_empty() {
                                continue;
                            }
                            self.object_lists
                                .entry(binding)
                                .or_default()
                                .push(ObjectListItem {
                                    id: self.next_object_item_id,
                                    title,
                                    completed: false,
                                    text_fields: BTreeMap::new(),
                                    bool_fields,
                                    scalar_fields: BTreeMap::new(),
                                    object_lists: BTreeMap::new(),
                                    nested_item_actions: BTreeMap::new(),
                                });
                            self.next_object_item_id += 1;
                            if clear_draft {
                                self.input_texts.insert(source_binding, String::new());
                                self.draft_text.clear();
                            }
                            changed = true;
                        }
                        ObjectListUpdate::ToggleBoolField {
                            binding,
                            item_id,
                            field,
                        } => {
                            let Some(item) = self
                                .object_lists
                                .entry(binding)
                                .or_default()
                                .iter_mut()
                                .find(|item| item.id == item_id)
                            else {
                                continue;
                            };
                            match field.as_str() {
                                "completed" => {
                                    item.completed = !item.completed;
                                    changed = true;
                                }
                                other => {
                                    let entry =
                                        item.bool_fields.entry(other.to_string()).or_default();
                                    *entry = !*entry;
                                    changed = true;
                                }
                            }
                        }
                        ObjectListUpdate::SetBoolField {
                            binding,
                            item_id,
                            field,
                            value,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            let Some(item) = self
                                .object_lists
                                .entry(binding)
                                .or_default()
                                .iter_mut()
                                .find(|item| item.id == item_id)
                            else {
                                continue;
                            };
                            match field.as_str() {
                                "completed" => {
                                    if item.completed != value {
                                        item.completed = value;
                                        changed = true;
                                    }
                                }
                                other => {
                                    let entry =
                                        item.bool_fields.entry(other.to_string()).or_default();
                                    if *entry != value {
                                        *entry = value;
                                        changed = true;
                                    }
                                }
                            }
                        }
                        ObjectListUpdate::SetTitle {
                            binding,
                            item_id,
                            trim,
                            reject_empty,
                            payload_filter,
                        } => {
                            if let Some(filter) = payload_filter.as_ref() {
                                if !event_payload_matches(event, filter) {
                                    continue;
                                }
                            }
                            let Some(item) = self
                                .object_lists
                                .entry(binding)
                                .or_default()
                                .iter_mut()
                                .find(|item| item.id == item_id)
                            else {
                                continue;
                            };
                            let mut title =
                                event_primary_payload(event).unwrap_or_default().to_string();
                            if trim {
                                title = title.trim().to_string();
                            }
                            if reject_empty && title.is_empty() {
                                continue;
                            }
                            if item.title != title {
                                item.title = title;
                                changed = true;
                            }
                        }
                        ObjectListUpdate::ToggleAllBoolField { binding, field } => {
                            let items = self.object_lists.entry(binding).or_default();
                            match field.as_str() {
                                "completed" => {
                                    let next = !items.iter().all(|item| item.completed);
                                    let mut local_changed = false;
                                    for item in items.iter_mut() {
                                        if item.completed != next {
                                            item.completed = next;
                                            local_changed = true;
                                        }
                                    }
                                    changed |= local_changed;
                                }
                                _ => {}
                            }
                        }
                        ObjectListUpdate::RemoveItem { binding, item_id } => {
                            let items = self.object_lists.entry(binding).or_default();
                            let before = items.len();
                            items.retain(|item| item.id != item_id);
                            if items.len() != before {
                                changed = true;
                            }
                        }
                        ObjectListUpdate::RemoveLast { binding } => {
                            if self
                                .object_lists
                                .entry(binding)
                                .or_default()
                                .pop()
                                .is_some()
                            {
                                changed = true;
                            }
                        }
                        ObjectListUpdate::RemoveMatching { binding, filter } => {
                            let items = self.object_lists.entry(binding).or_default();
                            let before = items.len();
                            items.retain(|item| {
                                !object_list_item_matches_filter(
                                    item,
                                    &filter,
                                    &self.scalar_values,
                                    &self.text_values,
                                )
                            });
                            if items.len() != before {
                                changed = true;
                            }
                        }
                    }
                }
                changed
            }
            SemanticAction::UpdateNestedObjectLists {
                parent_binding,
                parent_item_id,
                updates,
            } => {
                let mut changed = false;
                let updates = updates
                    .into_iter()
                    .map(|update| match update {
                        NestedObjectListUpdate::AppendObject { field, item } => {
                            NestedObjectListUpdate::AppendObject {
                                field,
                                item: self.assign_fresh_object_item_ids(item),
                            }
                        }
                        NestedObjectListUpdate::RemoveItem { field, item_id } => {
                            NestedObjectListUpdate::RemoveItem { field, item_id }
                        }
                    })
                    .collect::<Vec<_>>();
                let Some(parent_item) = self
                    .object_lists
                    .entry(parent_binding)
                    .or_default()
                    .iter_mut()
                    .find(|item| item.id == parent_item_id)
                else {
                    return false;
                };
                for update in updates {
                    match update {
                        NestedObjectListUpdate::AppendObject { field, item } => {
                            parent_item
                                .object_lists
                                .entry(field)
                                .or_default()
                                .push(item);
                            changed = true;
                        }
                        NestedObjectListUpdate::RemoveItem { field, item_id } => {
                            let items = parent_item.object_lists.entry(field).or_default();
                            let before = items.len();
                            items.retain(|item| item.id != item_id);
                            if items.len() != before {
                                changed = true;
                            }
                        }
                    }
                }
                changed
            }
        }
    }

    fn render_text_parts(&self, parts: &[SemanticTextPart]) -> String {
        let mut output = String::new();
        for part in parts {
            match part {
                SemanticTextPart::Static(text) => output.push_str(text),
                SemanticTextPart::TextBinding(binding) => output.push_str(
                    self.text_values
                        .get(binding)
                        .map(String::as_str)
                        .unwrap_or_default(),
                ),
                SemanticTextPart::DerivedScalarExpr(expr) => {
                    output.push_str(&self.derived_scalar_operand_value(expr).to_string())
                }
                SemanticTextPart::ScalarBinding(binding) => output.push_str(
                    &self
                        .scalar_values
                        .get(binding)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                SemanticTextPart::ObjectFieldBinding(_) => {}
                SemanticTextPart::ListCountBinding(binding) => output.push_str(
                    &self
                        .filtered_text_list_values(binding, None)
                        .len()
                        .to_string(),
                ),
                SemanticTextPart::ObjectListCountBinding(binding) => output.push_str(
                    &self
                        .object_lists
                        .get(binding)
                        .map_or(0, Vec::len)
                        .to_string(),
                ),
                SemanticTextPart::FilteredListCountBinding { binding, filter } => output.push_str(
                    &self
                        .filtered_text_list_values(binding, Some(filter))
                        .len()
                        .to_string(),
                ),
                SemanticTextPart::FilteredObjectListCountBinding { binding, filter } => output
                    .push_str(
                        &self
                            .filtered_object_list_items(binding, Some(filter))
                            .len()
                            .to_string(),
                    ),
                SemanticTextPart::BoolBindingText {
                    binding,
                    true_text,
                    false_text,
                } => output.push_str(
                    if self.scalar_values.get(binding).copied().unwrap_or_default() != 0 {
                        true_text
                    } else {
                        false_text
                    },
                ),
                SemanticTextPart::ScalarValueText {
                    expr,
                    expected,
                    true_text,
                    false_text,
                } => output.push_str(if self.derived_scalar_operand_value(expr) == *expected {
                    true_text
                } else {
                    false_text
                }),
                SemanticTextPart::ObjectBoolFieldText { .. } => {}
            }
        }
        output
    }

    fn semantic_node_text(node: &SemanticNode) -> String {
        match node {
            SemanticNode::Fragment(children) => {
                children.iter().map(Self::semantic_node_text).collect()
            }
            SemanticNode::Element { text, children, .. } => {
                let mut output = text.clone().unwrap_or_default();
                for child in children {
                    output.push_str(&Self::semantic_node_text(child));
                }
                output
            }
            SemanticNode::Text(text) => text.clone(),
            SemanticNode::TextTemplate { value, .. } => value.clone(),
            SemanticNode::ScalarValue { value, .. } => value.to_string(),
            _ => String::new(),
        }
    }

    fn render_input_value(&self, value: &SemanticInputValue) -> String {
        match value {
            SemanticInputValue::Static(value) => value.clone(),
            SemanticInputValue::TextParts { parts, .. } => self.render_text_parts(parts),
            SemanticInputValue::Node(node) => {
                Self::semantic_node_text(&self.materialize_node(node, &[]))
            }
            SemanticInputValue::ParsedTextBindingBranch {
                binding,
                number,
                nan,
            } => {
                if self.text_binding_number_value(binding).is_some() {
                    self.render_input_value(number)
                } else {
                    self.render_input_value(nan)
                }
            }
            SemanticInputValue::TextValueBranch {
                binding,
                expected,
                truthy,
                falsy,
            } => {
                if self.text_values.get(binding).map(String::as_str) == Some(expected.as_str()) {
                    self.render_input_value(truthy)
                } else {
                    self.render_input_value(falsy)
                }
            }
            SemanticInputValue::TextBindingBranch {
                binding,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = self.text_values.get(binding).map_or(true, String::is_empty);
                if is_empty != *invert {
                    self.render_input_value(truthy)
                } else {
                    self.render_input_value(falsy)
                }
            }
            SemanticInputValue::ObjectTextFieldBranch { falsy, .. } => {
                self.render_input_value(falsy)
            }
        }
    }

    fn apply_event_effect(&mut self, event: &boon_scene::UiEvent) {
        match event.kind {
            boon_scene::UiEventKind::Input | boon_scene::UiEventKind::Change => {
                self.draft_text = event_primary_payload(event).unwrap_or_default().to_string();
                if let Some(source_binding) = self.active_input_bindings.get(&event.target) {
                    self.active_draft_binding = Some(source_binding.clone());
                    self.input_texts
                        .insert(source_binding.clone(), self.draft_text.clone());
                }
            }
            boon_scene::UiEventKind::KeyDown => {
                if let Some(text) = event_keydown_text(event) {
                    self.draft_text = text.to_string();
                    if let Some(source_binding) = self.active_input_bindings.get(&event.target) {
                        self.active_draft_binding = Some(source_binding.clone());
                        self.input_texts
                            .insert(source_binding.clone(), self.draft_text.clone());
                    }
                }
            }
            boon_scene::UiEventKind::Focus => {
                self.focused = true;
            }
            boon_scene::UiEventKind::Blur => {
                self.focused = false;
            }
            _ => {}
        }
    }

    fn apply_fact_effects(&mut self, batch: &UiFactBatch) -> bool {
        let mut changed = false;
        for fact in &batch.facts {
            match &fact.kind {
                boon_scene::UiFactKind::DraftText(text) => {
                    if self.draft_text != *text {
                        self.draft_text = text.clone();
                        changed = true;
                    }
                    if let Some(source_binding) = self.active_input_nodes.get(&fact.id).cloned() {
                        if self.active_draft_binding.as_deref() != Some(source_binding.as_str()) {
                            self.active_draft_binding = Some(source_binding);
                            changed = true;
                        }
                    }
                }
                boon_scene::UiFactKind::Focused(focused) => {
                    self.focused = *focused;
                    if let Some(binding) = self
                        .active_fact_bindings
                        .get(&(fact.id, SemanticFactKind::Focused))
                        .cloned()
                    {
                        let next = i64::from(*focused);
                        let entry = self.scalar_values.entry(binding).or_default();
                        if *entry != next {
                            *entry = next;
                            changed = true;
                        }
                    }
                }
                boon_scene::UiFactKind::Hovered(hovered) => {
                    if let Some(binding) = self
                        .active_fact_bindings
                        .get(&(fact.id, SemanticFactKind::Hovered))
                        .cloned()
                    {
                        let next = i64::from(*hovered);
                        let entry = self.scalar_values.entry(binding).or_default();
                        if *entry != next {
                            *entry = next;
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }
        changed
    }

    fn refresh_derived_scalars(&mut self) {
        if self.derived_scalars.is_empty() {
            return;
        }
        let iterations = self.derived_scalars.len().max(1);
        for _ in 0..iterations {
            let mut changed = false;
            for spec in &self.derived_scalars {
                let next_value = match spec {
                    DerivedScalarSpec::TextListCount {
                        binding,
                        filter,
                        target: _,
                    } => self
                        .filtered_text_list_values(binding, filter.as_ref())
                        .len() as i64,
                    DerivedScalarSpec::ObjectListCount {
                        binding,
                        filter,
                        target: _,
                    } => self
                        .filtered_object_list_items(binding, filter.as_ref())
                        .len() as i64,
                    DerivedScalarSpec::Arithmetic { target: _, expr } => {
                        self.derived_scalar_operand_value(expr)
                    }
                    DerivedScalarSpec::TextValueBranch {
                        target: _,
                        binding,
                        branches,
                        fallback,
                    } => {
                        let current = self
                            .text_values
                            .get(binding)
                            .map(String::as_str)
                            .unwrap_or("");
                        branches
                            .iter()
                            .find_map(|(expected, value)| (expected == current).then_some(*value))
                            .unwrap_or(*fallback)
                    }
                    DerivedScalarSpec::Comparison {
                        target: _,
                        op,
                        left,
                        right,
                    } => {
                        let left = self.derived_scalar_operand_value(left);
                        let right = self.derived_scalar_operand_value(right);
                        i64::from(match op {
                            IntCompareOp::Equal => left == right,
                            IntCompareOp::NotEqual => left != right,
                            IntCompareOp::Greater => left > right,
                            IntCompareOp::GreaterOrEqual => left >= right,
                            IntCompareOp::Less => left < right,
                            IntCompareOp::LessOrEqual => left <= right,
                        })
                    }
                    DerivedScalarSpec::TextComparison {
                        target: _,
                        op,
                        left,
                        right,
                    } => {
                        let left = self.derived_text_operand_value(left);
                        let right = self.derived_text_operand_value(right);
                        i64::from(match op {
                            IntCompareOp::Equal => left == right,
                            IntCompareOp::NotEqual => left != right,
                            IntCompareOp::Greater => left > right,
                            IntCompareOp::GreaterOrEqual => left >= right,
                            IntCompareOp::Less => left < right,
                            IntCompareOp::LessOrEqual => left <= right,
                        })
                    }
                };
                let target = match spec {
                    DerivedScalarSpec::TextListCount { target, .. }
                    | DerivedScalarSpec::ObjectListCount { target, .. }
                    | DerivedScalarSpec::Arithmetic { target, .. }
                    | DerivedScalarSpec::TextValueBranch { target, .. }
                    | DerivedScalarSpec::Comparison { target, .. }
                    | DerivedScalarSpec::TextComparison { target, .. } => target,
                };
                let entry = self.scalar_values.entry(target.clone()).or_default();
                if *entry != next_value {
                    *entry = next_value;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn derived_scalar_operand_value(&self, operand: &DerivedScalarOperand) -> i64 {
        match operand {
            DerivedScalarOperand::Binding(binding) => {
                self.scalar_values.get(binding).copied().unwrap_or_default()
            }
            DerivedScalarOperand::TextBindingNumber(binding) => {
                self.text_binding_number_value(binding).unwrap_or_default()
            }
            DerivedScalarOperand::TextListCount { binding, filter } => {
                self.filtered_text_list_values(binding, filter.as_ref())
                    .len() as i64
            }
            DerivedScalarOperand::ObjectListCount { binding, filter } => {
                self.filtered_object_list_items(binding, filter.as_ref())
                    .len() as i64
            }
            DerivedScalarOperand::Literal(value) => *value,
            DerivedScalarOperand::Arithmetic { op, left, right } => {
                let left = self.derived_scalar_operand_value(left);
                let right = self.derived_scalar_operand_value(right);
                match op {
                    DerivedArithmeticOp::Add => left + right,
                    DerivedArithmeticOp::Subtract => left - right,
                    DerivedArithmeticOp::Multiply => left * right,
                    DerivedArithmeticOp::Divide => {
                        if right == 0 {
                            0
                        } else {
                            left / right
                        }
                    }
                }
            }
            DerivedScalarOperand::Min { left, right } => self
                .derived_scalar_operand_value(left)
                .min(self.derived_scalar_operand_value(right)),
            DerivedScalarOperand::Round { source } => self.derived_scalar_operand_value(source),
        }
    }

    fn derived_text_operand_value(&self, operand: &DerivedTextOperand) -> String {
        match operand {
            DerivedTextOperand::Binding(binding) => {
                self.text_values.get(binding).cloned().unwrap_or_default()
            }
            DerivedTextOperand::Literal(value) => value.clone(),
        }
    }

    fn scalar_compare_matches(
        &self,
        left: &DerivedScalarOperand,
        op: IntCompareOp,
        right: &DerivedScalarOperand,
    ) -> bool {
        let left = self.derived_scalar_operand_value(left);
        let right = self.derived_scalar_operand_value(right);
        match op {
            IntCompareOp::Equal => left == right,
            IntCompareOp::NotEqual => left != right,
            IntCompareOp::Greater => left > right,
            IntCompareOp::GreaterOrEqual => left >= right,
            IntCompareOp::Less => left < right,
            IntCompareOp::LessOrEqual => left <= right,
        }
    }

    fn last_event_summary(&self) -> Option<String> {
        let event = self.event_history.last()?.events.last()?;
        let payload = event.payload.as_deref().unwrap_or("none");
        Some(format!("{:?} payload={payload}", event.kind))
    }

    fn last_action_summary(&self) -> Option<String> {
        self.action_history.last().cloned()
    }

    fn last_fact_summary(&self) -> Option<String> {
        let fact = self.fact_history.last()?.facts.last()?;
        Some(match &fact.kind {
            boon_scene::UiFactKind::Hovered(hovered) => format!("Hovered({hovered})"),
            boon_scene::UiFactKind::Focused(focused) => format!("Focused({focused})"),
            boon_scene::UiFactKind::DraftText(text) => format!("DraftText({text})"),
            boon_scene::UiFactKind::LayoutSize { width, height } => {
                format!("LayoutSize({width}x{height})")
            }
            boon_scene::UiFactKind::Custom { name, value } => format!("Custom({name}={value})"),
        })
    }

    fn recent_event_summaries(&self, limit: usize) -> Vec<String> {
        self.event_history
            .iter()
            .flat_map(|batch| batch.events.iter())
            .rev()
            .take(limit)
            .map(|event| {
                let payload = event.payload.as_deref().unwrap_or("none");
                format!("{:?} payload={payload}", event.kind)
            })
            .collect()
    }

    fn recent_action_summaries(&self, limit: usize) -> Vec<String> {
        self.action_history
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    fn recent_fact_summaries(&self, limit: usize) -> Vec<String> {
        self.fact_history
            .iter()
            .flat_map(|batch| batch.facts.iter())
            .rev()
            .take(limit)
            .map(|fact| match &fact.kind {
                boon_scene::UiFactKind::Hovered(hovered) => format!("Hovered({hovered})"),
                boon_scene::UiFactKind::Focused(focused) => format!("Focused({focused})"),
                boon_scene::UiFactKind::DraftText(text) => format!("DraftText({text})"),
                boon_scene::UiFactKind::LayoutSize { width, height } => {
                    format!("LayoutSize({width}x{height})")
                }
                boon_scene::UiFactKind::Custom { name, value } => {
                    format!("Custom({name}={value})")
                }
            })
            .collect()
    }

    fn record_action(
        &mut self,
        event: &boon_scene::UiEvent,
        action: &SemanticAction,
        changed: bool,
    ) {
        self.action_history.push(format!(
            "{:?} port={:?} changed={} -> {}",
            event.kind,
            event.target,
            changed,
            format_action_summary(action),
        ));
    }

    fn record_missing_action(&mut self, event: &boon_scene::UiEvent) {
        self.action_history.push(format!(
            "{:?} port={:?} changed=false -> <no action>",
            event.kind, event.target
        ));
    }

    fn filtered_text_list_values(
        &self,
        binding: &str,
        filter: Option<&TextListFilter>,
    ) -> Vec<String> {
        let values = self.text_lists.get(binding).cloned().unwrap_or_default();
        match filter {
            None => values,
            Some(filter) => values
                .into_iter()
                .filter(|value| text_list_item_matches_filter(value, filter))
                .collect(),
        }
    }

    fn filtered_object_list_items(
        &self,
        binding: &str,
        filter: Option<&ObjectListFilter>,
    ) -> Vec<ObjectListItem> {
        let items = self.object_lists.get(binding).cloned().unwrap_or_default();
        match filter {
            None => items,
            Some(filter) => items
                .into_iter()
                .filter(|item| {
                    object_list_item_matches_filter(
                        item,
                        filter,
                        &self.scalar_values,
                        &self.text_values,
                    )
                })
                .collect(),
        }
    }

    fn assign_fresh_object_item_ids(&mut self, mut item: ObjectListItem) -> ObjectListItem {
        item.id = self.next_object_item_id;
        self.next_object_item_id += 1;
        for nested_items in item.object_lists.values_mut() {
            let drained = std::mem::take(nested_items);
            *nested_items = drained
                .into_iter()
                .map(|nested_item| self.assign_fresh_object_item_ids(nested_item))
                .collect();
        }
        item
    }

    fn materialize_object_template(
        &self,
        list_binding: &str,
        item: &ObjectListItem,
        item_actions: &[ObjectItemActionSpec],
        node: &SemanticNode,
        path: &[usize],
        current_element_scope: Option<&str>,
    ) -> SemanticNode {
        match node {
            SemanticNode::Fragment(children) => SemanticNode::Fragment(
                children
                    .iter()
                    .enumerate()
                    .map(|(index, child)| {
                        let child_path = child_path(path, index);
                        self.materialize_object_template(
                            list_binding,
                            item,
                            item_actions,
                            child,
                            &child_path,
                            current_element_scope,
                        )
                    })
                    .collect(),
            ),
            SemanticNode::Keyed { key, node } => SemanticNode::Keyed {
                key: *key,
                node: Box::new(self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    node,
                    path,
                    current_element_scope,
                )),
            },
            SemanticNode::Element {
                tag,
                text,
                properties,
                input_value,
                style_fragments,
                event_bindings,
                fact_bindings,
                children,
            } => {
                let mut properties = properties.clone();
                self.materialize_dynamic_properties(&mut properties, Some(item));
                if tag == "input" || tag == "select" {
                    if let Some(value_source) = input_value {
                        let value = self.render_object_input_value(item, value_source);
                        if let Some((_, property_value)) =
                            properties.iter_mut().find(|(name, _)| name == "value")
                        {
                            *property_value = value;
                        } else {
                            properties.push(("value".to_string(), value));
                        }
                    }
                    if let Some(source_binding) = event_bindings
                        .iter()
                        .find_map(|binding| binding.source_binding.as_deref())
                        .and_then(|source_binding| {
                            scope_object_source_binding(Some(source_binding), list_binding, item.id)
                        })
                    {
                        if self.active_draft_binding.as_deref() == Some(source_binding.as_str()) {
                            let value = self.draft_text.clone();
                            if let Some((_, property_value)) =
                                properties.iter_mut().find(|(name, _)| name == "value")
                            {
                                *property_value = value;
                            } else {
                                properties.push(("value".to_string(), value));
                            }
                        }
                    }
                }
                let event_bindings = event_bindings
                    .iter()
                    .map(|binding| {
                        let action = binding.action.clone().or_else(|| {
                            let suffix = binding
                                .source_binding
                                .as_deref()
                                .map(|source| source.strip_prefix("__item__.").unwrap_or(source));
                            let matched = item_actions
                                .iter()
                                .filter(|spec| {
                                    Some(spec.source_binding_suffix.as_str()) == suffix
                                        && spec.kind == binding.kind
                                })
                                .collect::<Vec<_>>();
                            if matched.is_empty() {
                                return None;
                            }
                            let object_updates = matched
                                .iter()
                                .filter_map(|spec| match &spec.action {
                                    ObjectItemActionKind::ToggleBoolField { field } => {
                                        Some(ObjectListUpdate::ToggleBoolField {
                                            binding: list_binding.to_string(),
                                            item_id: item.id,
                                            field: field.clone(),
                                        })
                                    }
                                    ObjectItemActionKind::SetBoolField {
                                        field,
                                        value,
                                        payload_filter,
                                    } => Some(ObjectListUpdate::SetBoolField {
                                        binding: list_binding.to_string(),
                                        item_id: item.id,
                                        field: field.clone(),
                                        value: *value,
                                        payload_filter: payload_filter.clone(),
                                    }),
                                    ObjectItemActionKind::SetTitle {
                                        trim,
                                        reject_empty,
                                        payload_filter,
                                    } => Some(ObjectListUpdate::SetTitle {
                                        binding: list_binding.to_string(),
                                        item_id: item.id,
                                        trim: *trim,
                                        reject_empty: *reject_empty,
                                        payload_filter: payload_filter.clone(),
                                    }),
                                    ObjectItemActionKind::RemoveSelf
                                        if parse_nested_list_binding(list_binding).is_none() =>
                                    {
                                        Some(ObjectListUpdate::RemoveItem {
                                            binding: list_binding.to_string(),
                                            item_id: item.id,
                                        })
                                    }
                                    ObjectItemActionKind::RemoveSelf
                                    | ObjectItemActionKind::UpdateBindings { .. }
                                    | ObjectItemActionKind::UpdateNestedObjectLists { .. } => None,
                                })
                                .collect::<Vec<_>>();
                            let nested_actions = matched
                                .iter()
                                .filter_map(|spec| match &spec.action {
                                    ObjectItemActionKind::UpdateNestedObjectLists { updates } => {
                                        let (parent_binding, parent_item_id) =
                                            parse_nested_parent(list_binding, item.id)?;
                                        Some(SemanticAction::UpdateNestedObjectLists {
                                            parent_binding,
                                            parent_item_id,
                                            updates: updates
                                                .iter()
                                                .map(|update| match update {
                                                    NestedObjectListAction::AppendObject {
                                                        field,
                                                        item,
                                                    } => NestedObjectListUpdate::AppendObject {
                                                        field: field.clone(),
                                                        item: item.clone(),
                                                    },
                                                })
                                                .collect(),
                                        })
                                    }
                                    ObjectItemActionKind::RemoveSelf => {
                                        let (parent_binding, parent_item_id, field) =
                                            parse_nested_list_binding(list_binding)?;
                                        Some(SemanticAction::UpdateNestedObjectLists {
                                            parent_binding,
                                            parent_item_id,
                                            updates: vec![NestedObjectListUpdate::RemoveItem {
                                                field,
                                                item_id: item.id,
                                            }],
                                        })
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>();
                            let mut actions = Vec::new();
                            if !object_updates.is_empty() {
                                actions.push(SemanticAction::UpdateObjectLists {
                                    updates: object_updates,
                                });
                            }
                            actions.extend(nested_actions);
                            for spec in matched {
                                if let ObjectItemActionKind::UpdateBindings {
                                    scalar_updates,
                                    text_updates,
                                    object_list_updates,
                                    payload_filter,
                                } = &spec.action
                                {
                                    if !scalar_updates.is_empty() {
                                        actions.push(SemanticAction::UpdateScalars {
                                            updates: scalar_updates
                                                .iter()
                                                .map(|update| match update {
                                                    ItemScalarUpdate::SetStatic {
                                                        binding,
                                                        value,
                                                    } => payload_filter.as_ref().map_or_else(
                                                        || ScalarUpdate::Set {
                                                            binding: binding.clone(),
                                                            value: *value,
                                                        },
                                                        |payload_filter| {
                                                            ScalarUpdate::SetFiltered {
                                                                binding: binding.clone(),
                                                                value: *value,
                                                                payload_filter: payload_filter
                                                                    .clone(),
                                                            }
                                                        },
                                                    ),
                                                    ItemScalarUpdate::SetFromField {
                                                        binding,
                                                        field,
                                                    } => payload_filter.as_ref().map_or_else(
                                                        || ScalarUpdate::Set {
                                                            binding: binding.clone(),
                                                            value: self
                                                                .object_item_field_scalar_value(
                                                                    item, field,
                                                                ),
                                                        },
                                                        |payload_filter| {
                                                            ScalarUpdate::SetFiltered {
                                                                binding: binding.clone(),
                                                                value: self
                                                                    .object_item_field_scalar_value(
                                                                        item, field,
                                                                    ),
                                                                payload_filter: payload_filter
                                                                    .clone(),
                                                            }
                                                        },
                                                    ),
                                                })
                                                .collect(),
                                        });
                                    }
                                    if !text_updates.is_empty() {
                                        actions.push(SemanticAction::UpdateTexts {
                                            updates: text_updates
                                                .iter()
                                                .map(|update| match update {
                                                    ItemTextUpdate::SetStatic {
                                                        binding,
                                                        value,
                                                    } => TextUpdate::SetStatic {
                                                        binding: binding.clone(),
                                                        value: value.clone(),
                                                        payload_filter: payload_filter.clone(),
                                                    },
                                                    ItemTextUpdate::SetFromField {
                                                        binding,
                                                        field,
                                                    } => {
                                                        if field.ends_with(".event.key_down.text")
                                                            && spec.kind
                                                                == boon_scene::UiEventKind::KeyDown
                                                        {
                                                            let scoped_source = format!(
                                                                "__item__.{}",
                                                                spec.source_binding_suffix
                                                            );
                                                            let source_binding =
                                                                scope_object_source_binding(
                                                                    Some(&scoped_source),
                                                                    list_binding,
                                                                    item.id,
                                                                )
                                                                .unwrap_or_else(|| binding.clone());
                                                            TextUpdate::SetFromInput {
                                                                binding: binding.clone(),
                                                                source_binding,
                                                                payload_filter: payload_filter
                                                                    .clone(),
                                                            }
                                                        } else {
                                                            TextUpdate::SetStatic {
                                                                binding: binding.clone(),
                                                                value: self
                                                                    .object_item_field_text_value(
                                                                        item, field,
                                                                    ),
                                                                payload_filter: payload_filter
                                                                    .clone(),
                                                            }
                                                        }
                                                    }
                                                    ItemTextUpdate::SetFromPayload { binding } => {
                                                        TextUpdate::SetFromPayload {
                                                            binding: binding.clone(),
                                                        }
                                                    }
                                                    ItemTextUpdate::SetFromInputSource {
                                                        binding,
                                                        source_suffix,
                                                    } => TextUpdate::SetFromInput {
                                                        binding: binding.clone(),
                                                        source_binding:
                                                            scope_object_source_binding(
                                                                Some(&format!(
                                                                    "__item__.{source_suffix}"
                                                                )),
                                                                list_binding,
                                                                item.id,
                                                            )
                                                            .unwrap_or_else(|| {
                                                                format!(
                                                                    "{}.{}",
                                                                    object_item_scope(
                                                                        list_binding,
                                                                        item.id
                                                                    ),
                                                                    source_suffix
                                                                )
                                                            }),
                                                        payload_filter: payload_filter.clone(),
                                                    },
                                                    ItemTextUpdate::SetFromValueSource {
                                                        binding,
                                                        value,
                                                    } => TextUpdate::SetStatic {
                                                        binding: binding.clone(),
                                                        value: self
                                                            .render_object_input_value(item, value),
                                                        payload_filter: payload_filter.clone(),
                                                    },
                                                })
                                                .collect(),
                                        });
                                    }
                                    if !object_list_updates.is_empty() {
                                        actions.push(SemanticAction::UpdateObjectLists {
                                            updates: object_list_updates.clone(),
                                        });
                                    }
                                }
                            }
                            match actions.len() {
                                0 => None,
                                1 => actions.into_iter().next(),
                                _ => Some(SemanticAction::Batch { actions }),
                            }
                        });
                        super::semantic_ir::SemanticEventBinding {
                            kind: binding.kind.clone(),
                            source_binding: scope_object_source_binding(
                                binding.source_binding.as_deref(),
                                list_binding,
                                item.id,
                            ),
                            action,
                        }
                    })
                    .collect();
                let item_scope = object_item_scope(list_binding, item.id);
                let next_element_scope =
                    local_element_scope(path, Some(&item_scope), fact_bindings)
                        .or_else(|| current_element_scope.map(ToString::to_string));
                merge_style_fragments(
                    &mut properties,
                    style_fragments,
                    &self.scalar_values,
                    &self.text_lists,
                    &self.object_lists,
                    next_element_scope.as_deref(),
                    Some(item),
                );
                let fact_bindings = fact_bindings
                    .iter()
                    .map(|binding| SemanticFactBinding {
                        kind: binding.kind.clone(),
                        binding: scope_element_binding(
                            &binding.binding,
                            next_element_scope.as_deref(),
                        ),
                    })
                    .collect();
                SemanticNode::element_with_facts_and_styles(
                    tag.clone(),
                    text.clone(),
                    properties,
                    Vec::new(),
                    event_bindings,
                    fact_bindings,
                    children
                        .iter()
                        .enumerate()
                        .map(|(index, child)| {
                            let child_path = child_path(path, index);
                            self.materialize_object_template(
                                list_binding,
                                item,
                                item_actions,
                                child,
                                &child_path,
                                next_element_scope.as_deref(),
                            )
                        })
                        .collect(),
                )
            }
            SemanticNode::Text(text) => SemanticNode::Text(text.clone()),
            SemanticNode::TextTemplate { parts, .. } => {
                SemanticNode::Text(self.render_object_text_parts(item, parts))
            }
            SemanticNode::TextBindingBranch {
                binding,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = self.text_values.get(binding).map_or(true, String::is_empty);
                let branch = if is_empty != *invert {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::BoolBranch {
                binding,
                truthy,
                falsy,
            } => {
                let scoped_binding = scope_element_binding(binding, current_element_scope);
                let branch = if self
                    .scalar_values
                    .get(&scoped_binding)
                    .copied()
                    .unwrap_or_default()
                    != 0
                {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::ScalarCompareBranch {
                left,
                op,
                right,
                truthy,
                falsy,
            } => {
                let branch = if self.scalar_compare_matches(left, op.clone(), right) {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::ObjectScalarCompareBranch {
                left,
                op,
                right,
                truthy,
                falsy,
            } => {
                let branch = if self.object_scalar_compare_matches(item, left, op.clone(), right) {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::ObjectBoolFieldBranch {
                field,
                truthy,
                falsy,
            } => {
                let branch = if object_item_bool_field(item, field) {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::ObjectTextFieldBranch {
                field,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = self.object_item_field_text_value(item, field).is_empty();
                let branch = if is_empty != *invert {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::ListEmptyBranch {
                binding,
                object_items,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = if *object_items {
                    self.object_lists.get(binding).map_or(true, Vec::is_empty)
                } else {
                    self.text_lists.get(binding).map_or(true, Vec::is_empty)
                };
                let branch = if is_empty != *invert {
                    truthy.as_ref()
                } else {
                    falsy.as_ref()
                };
                self.materialize_object_template(
                    list_binding,
                    item,
                    item_actions,
                    branch,
                    path,
                    current_element_scope,
                )
            }
            SemanticNode::ScalarValue { binding, .. } => SemanticNode::Text(
                self.scalar_values
                    .get(binding)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            SemanticNode::ObjectFieldValue { field } => {
                SemanticNode::Text(self.object_item_field_text_value(item, field))
            }
            SemanticNode::TextList {
                binding,
                filter,
                template,
                ..
            } => SemanticNode::TextList {
                binding: binding.clone(),
                values: self.filtered_text_list_values(binding, filter.as_ref()),
                filter: filter.clone(),
                template: template.clone(),
            },
            SemanticNode::ObjectList {
                binding,
                filter,
                item_actions,
                template,
            } => {
                let Some(field) = binding.strip_prefix("__item__.") else {
                    return SemanticNode::Fragment(Vec::new());
                };
                let nested_list_binding =
                    format!("{}.{}", object_item_scope(list_binding, item.id), field);
                let nested_item_actions = item
                    .nested_item_actions
                    .get(field)
                    .cloned()
                    .unwrap_or_else(|| item_actions.clone());
                let nested_items = item.object_lists.get(field).cloned().unwrap_or_default();
                let nested_items = match filter {
                    None => nested_items,
                    Some(filter) => nested_items
                        .into_iter()
                        .filter(|nested_item| {
                            object_list_item_matches_filter(
                                nested_item,
                                filter,
                                &self.scalar_values,
                                &self.text_values,
                            )
                        })
                        .collect(),
                };
                SemanticNode::Fragment(
                    nested_items
                        .into_iter()
                        .map(|nested_item| {
                            let child_path = keyed_child_path(path, nested_item.id);
                            SemanticNode::keyed(
                                nested_item.id,
                                self.materialize_object_template(
                                    &nested_list_binding,
                                    &nested_item,
                                    &nested_item_actions,
                                    template.as_ref(),
                                    &child_path,
                                    current_element_scope,
                                ),
                            )
                        })
                        .collect(),
                )
            }
        }
    }

    fn text_binding_number_value(&self, binding: &str) -> Option<i64> {
        self.text_values
            .get(binding)
            .and_then(|value| value.trim().parse::<i64>().ok())
    }

    fn scoped_scalar_value(&self, binding: &str, current_element_scope: Option<&str>) -> i64 {
        let scoped = scope_element_binding(binding, current_element_scope);
        self.scalar_values.get(&scoped).copied().unwrap_or_default()
    }

    fn render_object_text_parts(
        &self,
        item: &ObjectListItem,
        parts: &[SemanticTextPart],
    ) -> String {
        let mut output = String::new();
        for part in parts {
            match part {
                SemanticTextPart::Static(text) => output.push_str(text),
                SemanticTextPart::TextBinding(binding) => output.push_str(
                    self.text_values
                        .get(binding)
                        .map(String::as_str)
                        .unwrap_or_default(),
                ),
                SemanticTextPart::DerivedScalarExpr(expr) => {
                    output.push_str(&self.derived_scalar_operand_value(expr).to_string())
                }
                SemanticTextPart::ScalarBinding(binding) => output.push_str(
                    &self
                        .scalar_values
                        .get(binding)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                SemanticTextPart::ObjectFieldBinding(field) => {
                    output.push_str(&self.object_item_field_text_value(item, field));
                }
                SemanticTextPart::ListCountBinding(binding) => output.push_str(
                    &self
                        .filtered_text_list_values(binding, None)
                        .len()
                        .to_string(),
                ),
                SemanticTextPart::ObjectListCountBinding(binding) => output.push_str(
                    &self
                        .object_lists
                        .get(binding)
                        .map_or(0, Vec::len)
                        .to_string(),
                ),
                SemanticTextPart::FilteredListCountBinding { binding, filter } => output.push_str(
                    &self
                        .filtered_text_list_values(binding, Some(filter))
                        .len()
                        .to_string(),
                ),
                SemanticTextPart::FilteredObjectListCountBinding { binding, filter } => output
                    .push_str(
                        &self
                            .filtered_object_list_items(binding, Some(filter))
                            .len()
                            .to_string(),
                    ),
                SemanticTextPart::BoolBindingText {
                    binding,
                    true_text,
                    false_text,
                } => output.push_str(
                    if self.scalar_values.get(binding).copied().unwrap_or_default() != 0 {
                        true_text
                    } else {
                        false_text
                    },
                ),
                SemanticTextPart::ScalarValueText {
                    expr,
                    expected,
                    true_text,
                    false_text,
                } => output.push_str(if self.derived_scalar_operand_value(expr) == *expected {
                    true_text
                } else {
                    false_text
                }),
                SemanticTextPart::ObjectBoolFieldText {
                    field,
                    true_text,
                    false_text,
                } => {
                    output.push_str(if object_item_bool_field(item, field) {
                        true_text
                    } else {
                        false_text
                    });
                }
            }
        }
        output
    }

    fn render_object_input_value(
        &self,
        item: &ObjectListItem,
        value: &SemanticInputValue,
    ) -> String {
        match value {
            SemanticInputValue::Static(value) => value.clone(),
            SemanticInputValue::TextParts { parts, .. } => {
                self.render_object_text_parts(item, parts)
            }
            SemanticInputValue::Node(node) => Self::semantic_node_text(
                &self.materialize_object_template("", item, &[], node, &[], None),
            ),
            SemanticInputValue::ParsedTextBindingBranch {
                binding,
                number,
                nan,
            } => {
                if self.text_binding_number_value(binding).is_some() {
                    self.render_object_input_value(item, number)
                } else {
                    self.render_object_input_value(item, nan)
                }
            }
            SemanticInputValue::TextValueBranch {
                binding,
                expected,
                truthy,
                falsy,
            } => {
                if self.text_values.get(binding).map(String::as_str) == Some(expected.as_str()) {
                    self.render_object_input_value(item, truthy)
                } else {
                    self.render_object_input_value(item, falsy)
                }
            }
            SemanticInputValue::TextBindingBranch {
                binding,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = self.text_values.get(binding).map_or(true, String::is_empty);
                if is_empty != *invert {
                    self.render_object_input_value(item, truthy)
                } else {
                    self.render_object_input_value(item, falsy)
                }
            }
            SemanticInputValue::ObjectTextFieldBranch {
                field,
                invert,
                truthy,
                falsy,
            } => {
                let is_empty = self.object_item_field_text_value(item, field).is_empty();
                if is_empty != *invert {
                    self.render_object_input_value(item, truthy)
                } else {
                    self.render_object_input_value(item, falsy)
                }
            }
        }
    }

    fn object_scalar_compare_matches(
        &self,
        item: &ObjectListItem,
        left: &ObjectDerivedScalarOperand,
        op: IntCompareOp,
        right: &ObjectDerivedScalarOperand,
    ) -> bool {
        let left = self.object_scalar_operand_value(item, left);
        let right = self.object_scalar_operand_value(item, right);
        match op {
            IntCompareOp::Equal => left == right,
            IntCompareOp::NotEqual => left != right,
            IntCompareOp::Greater => left > right,
            IntCompareOp::GreaterOrEqual => left >= right,
            IntCompareOp::Less => left < right,
            IntCompareOp::LessOrEqual => left <= right,
        }
    }

    fn object_scalar_operand_value(
        &self,
        item: &ObjectListItem,
        operand: &ObjectDerivedScalarOperand,
    ) -> i64 {
        match operand {
            ObjectDerivedScalarOperand::Binding(binding) => {
                self.scalar_values.get(binding).copied().unwrap_or_default()
            }
            ObjectDerivedScalarOperand::Field(field) => {
                self.object_item_field_scalar_value(item, field)
            }
            ObjectDerivedScalarOperand::Literal(value) => *value,
        }
    }

    fn object_item_field_text_value(&self, item: &ObjectListItem, field: &str) -> String {
        match field {
            "formula" => {
                let formula = object_item_field_text(item, field);
                if formula.is_empty() {
                    self.cell_position(item)
                        .map_or(formula, |(column, row)| self.cell_formula_text(column, row))
                } else {
                    formula
                }
            }
            "formula_text" => self
                .cell_position(item)
                .map_or_else(String::new, |(column, row)| {
                    self.cell_formula_text(column, row)
                }),
            "display_value" => self
                .cell_position(item)
                .map_or_else(String::new, |(column, row)| {
                    self.cell_display_value_text(column, row)
                }),
            "input_text" => {
                let editing_text = self
                    .text_values
                    .get("editing_text")
                    .cloned()
                    .unwrap_or_default();
                if editing_text.is_empty() {
                    self.object_item_field_text_value(item, "formula_text")
                } else {
                    editing_text
                }
            }
            _ => object_item_field_text(item, field),
        }
    }

    fn object_item_field_scalar_value(&self, item: &ObjectListItem, field: &str) -> i64 {
        match field {
            "display_value" => self.cell_position(item).map_or(0, |(column, row)| {
                let mut visited = BTreeSet::new();
                self.cell_value_at(column, row, &mut visited)
            }),
            _ => object_item_field_scalar_value(item, field),
        }
    }

    fn materialize_dynamic_properties(
        &self,
        properties: &mut Vec<(String, String)>,
        item: Option<&ObjectListItem>,
    ) {
        for (_, value) in properties.iter_mut() {
            if let Some(binding) = value.strip_prefix(SCALAR_PROPERTY_TOKEN_PREFIX) {
                *value = self
                    .scalar_values
                    .get(binding)
                    .copied()
                    .unwrap_or_default()
                    .to_string();
            } else if let Some(field) = value.strip_prefix(OBJECT_FIELD_PROPERTY_TOKEN_PREFIX) {
                *value = item
                    .map(|item| self.object_item_field_scalar_value(item, field).to_string())
                    .unwrap_or_default();
            }
        }
    }

    fn cell_position(&self, item: &ObjectListItem) -> Option<(i64, i64)> {
        Some((
            *item.scalar_fields.get("column")?,
            *item.scalar_fields.get("row")?,
        ))
    }

    fn cell_formula_text(&self, column: i64, row: i64) -> String {
        self.find_override_formula_text(column, row)
            .unwrap_or_else(|| default_cell_formula_text(column, row))
    }

    fn find_override_formula_text(&self, column: i64, row: i64) -> Option<String> {
        self.object_lists
            .get("overrides")
            .and_then(|overrides| {
                overrides.iter().rev().find_map(|item| {
                    (item.scalar_fields.get("column") == Some(&column)
                        && item.scalar_fields.get("row") == Some(&row))
                    .then(|| self.object_item_field_text_value(item, "text"))
                })
            })
            .filter(|text| !text.is_empty())
    }

    fn cell_display_value_text(&self, column: i64, row: i64) -> String {
        let mut visited = BTreeSet::new();
        self.cell_value_at(column, row, &mut visited).to_string()
    }

    fn cell_value_at(&self, column: i64, row: i64, visited: &mut BTreeSet<(i64, i64)>) -> i64 {
        if !visited.insert((column, row)) {
            return 0;
        }
        let formula_text = self.cell_formula_text(column, row);
        let value = self.compute_formula_text_value(&formula_text, visited);
        visited.remove(&(column, row));
        value
    }

    fn compute_formula_text_value(
        &self,
        formula_text: &str,
        visited: &mut BTreeSet<(i64, i64)>,
    ) -> i64 {
        let formula_text = formula_text.trim();
        if formula_text.is_empty() {
            return 0;
        }
        if let Some(expression) = formula_text.strip_prefix('=') {
            let expression = expression.trim();
            if let Some(arguments) = expression
                .strip_prefix("add(")
                .and_then(|rest| rest.strip_suffix(')'))
            {
                let Some((left, right)) = arguments.split_once(',') else {
                    return 0;
                };
                return self.reference_value(left.trim(), visited)
                    + self.reference_value(right.trim(), visited);
            }
            if let Some(arguments) = expression
                .strip_prefix("sum(")
                .and_then(|rest| rest.strip_suffix(')'))
            {
                let Some((start, end)) = arguments.split_once(':') else {
                    return 0;
                };
                let Some((start_column, start_row)) = parse_cell_reference(start.trim()) else {
                    return 0;
                };
                let Some((end_column, end_row)) = parse_cell_reference(end.trim()) else {
                    return 0;
                };
                if start_column != end_column {
                    return 0;
                }
                let (row_start, row_end) = if start_row <= end_row {
                    (start_row, end_row)
                } else {
                    (end_row, start_row)
                };
                return (row_start..=row_end)
                    .map(|current_row| self.cell_value_at(start_column, current_row, visited))
                    .sum();
            }
            return 0;
        }
        formula_text.parse::<i64>().unwrap_or_default()
    }

    fn reference_value(&self, reference: &str, visited: &mut BTreeSet<(i64, i64)>) -> i64 {
        let Some((column, row)) = parse_cell_reference(reference) else {
            return 0;
        };
        self.cell_value_at(column, row, visited)
    }
}

fn collect_all_object_list_item_action_summaries(node: &SemanticNode) -> Vec<serde_json::Value> {
    match node {
        SemanticNode::ObjectList {
            binding,
            item_actions,
            template,
            ..
        } => {
            let mut output = Vec::new();
            output.extend(item_actions.iter().map(|action| {
                serde_json::json!({
                    "binding": binding,
                    "source_binding_suffix": action.source_binding_suffix,
                    "kind": format!("{:?}", action.kind),
                })
            }));
            output.extend(collect_all_object_list_item_action_summaries(template));
            output
        }
        SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
            .iter()
            .flat_map(collect_all_object_list_item_action_summaries)
            .collect(),
        SemanticNode::Keyed { node, .. } => collect_all_object_list_item_action_summaries(node),
        SemanticNode::BoolBranch { truthy, falsy, .. }
        | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
        | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
        | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
        | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
        | SemanticNode::TextBindingBranch { truthy, falsy, .. }
        | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
            let mut output = collect_all_object_list_item_action_summaries(truthy);
            output.extend(collect_all_object_list_item_action_summaries(falsy));
            output
        }
        _ => Vec::new(),
    }
}

fn collect_object_list_bindings(node: &SemanticNode) -> Vec<String> {
    match node {
        SemanticNode::ObjectList {
            binding, template, ..
        } => {
            let mut output = vec![binding.clone()];
            output.extend(collect_object_list_bindings(template));
            output
        }
        SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
            .iter()
            .flat_map(collect_object_list_bindings)
            .collect(),
        SemanticNode::Keyed { node, .. } => collect_object_list_bindings(node),
        SemanticNode::BoolBranch { truthy, falsy, .. }
        | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
        | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
        | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
        | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
        | SemanticNode::TextBindingBranch { truthy, falsy, .. }
        | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
            let mut output = collect_object_list_bindings(truthy);
            output.extend(collect_object_list_bindings(falsy));
            output
        }
        _ => Vec::new(),
    }
}

fn collect_branch_summaries(node: &SemanticNode) -> Vec<String> {
    match node {
        SemanticNode::BoolBranch {
            binding,
            truthy,
            falsy,
        } => {
            let mut output = vec![format!("bool:{binding}")];
            output.extend(collect_branch_summaries(truthy));
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::TextBindingBranch {
            binding,
            truthy,
            falsy,
            ..
        } => {
            let mut output = vec![format!("text:{binding}")];
            output.extend(collect_branch_summaries(truthy));
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::ScalarCompareBranch {
            left,
            op,
            right,
            truthy,
            falsy,
        } => {
            let mut output = vec![format!("scalar:{left:?}{op:?}{right:?}")];
            output.extend(collect_branch_summaries(truthy));
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::ObjectScalarCompareBranch {
            left,
            op,
            right,
            truthy,
            falsy,
        } => {
            let mut output = vec![format!("object-scalar:{left:?}{op:?}{right:?}")];
            output.extend(collect_branch_summaries(truthy));
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::ObjectBoolFieldBranch {
            field,
            truthy,
            falsy,
        } => {
            let mut output = vec![format!("object-bool:{field}")];
            output.extend(collect_branch_summaries(truthy));
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::ObjectTextFieldBranch {
            field,
            truthy,
            falsy,
            ..
        } => {
            let mut output = vec![format!("object-text:{field}")];
            output.extend(collect_branch_summaries(truthy));
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
            let mut output = collect_branch_summaries(truthy);
            output.extend(collect_branch_summaries(falsy));
            output
        }
        SemanticNode::ObjectList { template, .. } => collect_branch_summaries(template),
        SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => {
            children.iter().flat_map(collect_branch_summaries).collect()
        }
        SemanticNode::Keyed { node, .. } => collect_branch_summaries(node),
        SemanticNode::Text(_)
        | SemanticNode::TextTemplate { .. }
        | SemanticNode::ScalarValue { .. }
        | SemanticNode::ObjectFieldValue { .. }
        | SemanticNode::TextList { .. } => Vec::new(),
    }
}

fn split_keydown_payload(payload: &str) -> (&str, Option<&str>) {
    match payload.split_once(KEYDOWN_TEXT_SEPARATOR) {
        Some((key, text)) => (key, Some(text)),
        None => (payload, None),
    }
}

fn event_primary_payload(event: &boon_scene::UiEvent) -> Option<&str> {
    let payload = event.payload.as_deref()?;
    if event.kind == boon_scene::UiEventKind::KeyDown {
        Some(split_keydown_payload(payload).0)
    } else {
        Some(payload)
    }
}

fn event_numeric_payload(event: &boon_scene::UiEvent) -> Option<i64> {
    let payload = event_primary_payload(event)?.trim();
    payload.parse::<i64>().ok().or_else(|| {
        payload
            .parse::<f64>()
            .ok()
            .map(|value| value.round() as i64)
    })
}

fn event_keydown_text(event: &boon_scene::UiEvent) -> Option<&str> {
    if event.kind != boon_scene::UiEventKind::KeyDown {
        return None;
    }
    let payload = event.payload.as_deref()?;
    split_keydown_payload(payload).1
}

fn event_payload_matches(event: &boon_scene::UiEvent, expected: &str) -> bool {
    event_primary_payload(event) == Some(expected)
}

fn event_scalar_payload_fields(
    event: &boon_scene::UiEvent,
    field_map: &BTreeMap<String, String>,
) -> Option<BTreeMap<String, i64>> {
    let payload = event.payload.as_deref()?;
    let payload = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let object = payload.as_object()?;
    field_map
        .iter()
        .map(|(field, payload_field)| {
            let value = object.get(payload_field)?;
            let value = value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| value.as_f64().map(|value| value.round() as i64))
                .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))?;
            Some((field.clone(), value))
        })
        .collect()
}

fn default_cell_formula_text(column: i64, row: i64) -> String {
    match (column, row) {
        (1, 1) => "5".to_string(),
        (1, 2) => "10".to_string(),
        (1, 3) => "15".to_string(),
        (2, 1) => "=add(A1, A2)".to_string(),
        (3, 1) => "=sum(A1:A3)".to_string(),
        _ => String::new(),
    }
}

fn parse_cell_reference(reference: &str) -> Option<(i64, i64)> {
    let mut chars = reference.chars();
    let column_char = chars.next()?;
    if !column_char.is_ascii_uppercase() {
        return None;
    }
    let row = chars.as_str().parse::<i64>().ok()?;
    let column = i64::from((column_char as u8) - b'A' + 1);
    Some((column, row))
}

fn text_list_item_matches_filter(value: &str, filter: &TextListFilter) -> bool {
    match filter {
        TextListFilter::IntCompare { op, value: target } => {
            let Ok(parsed) = value.parse::<i64>() else {
                return false;
            };
            match op {
                IntCompareOp::Equal => parsed == *target,
                IntCompareOp::NotEqual => parsed != *target,
                IntCompareOp::Greater => parsed > *target,
                IntCompareOp::GreaterOrEqual => parsed >= *target,
                IntCompareOp::Less => parsed < *target,
                IntCompareOp::LessOrEqual => parsed <= *target,
            }
        }
    }
}

fn object_item_field_text(item: &ObjectListItem, field: &str) -> String {
    match field {
        "title" => item.title.clone(),
        "completed" => {
            if item.completed {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        other if item.scalar_fields.contains_key(other) => item
            .scalar_fields
            .get(other)
            .copied()
            .unwrap_or_default()
            .to_string(),
        other if item.text_fields.contains_key(other) => {
            item.text_fields.get(other).cloned().unwrap_or_default()
        }
        other => item
            .bool_fields
            .get(other)
            .copied()
            .map(|value| if value { "True" } else { "False" }.to_string())
            .unwrap_or_default(),
    }
}

fn object_item_field_scalar_value(item: &ObjectListItem, field: &str) -> i64 {
    match field {
        "id" if item.scalar_fields.contains_key("id") => {
            item.scalar_fields.get("id").copied().unwrap_or_default()
        }
        "id" => item.id as i64,
        "completed" => i64::from(item.completed),
        "title" => item.title.parse::<i64>().unwrap_or_default(),
        other if item.scalar_fields.contains_key(other) => {
            item.scalar_fields.get(other).copied().unwrap_or_default()
        }
        other if item.bool_fields.contains_key(other) => {
            i64::from(item.bool_fields.get(other).copied().unwrap_or(false))
        }
        other => item
            .text_fields
            .get(other)
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default(),
    }
}

fn object_item_bool_field(item: &ObjectListItem, field: &str) -> bool {
    match field {
        "completed" => item.completed,
        other => item.bool_fields.get(other).copied().unwrap_or(false),
    }
}

fn scope_object_source_binding(
    source_binding: Option<&str>,
    list_binding: &str,
    item_id: u64,
) -> Option<String> {
    let source_binding = source_binding?;
    let Some(suffix) = source_binding.strip_prefix("__item__.") else {
        return Some(source_binding.to_string());
    };
    Some(format!(
        "{}.{suffix}",
        object_item_scope(list_binding, item_id)
    ))
}

fn merge_style_fragments(
    properties: &mut Vec<(String, String)>,
    fragments: &[SemanticStyleFragment],
    scalar_values: &BTreeMap<String, i64>,
    text_lists: &BTreeMap<String, Vec<String>>,
    object_lists: &BTreeMap<String, Vec<ObjectListItem>>,
    current_element_scope: Option<&str>,
    item: Option<&ObjectListItem>,
) {
    for fragment in fragments {
        if let Some(style) = evaluate_style_fragment(
            fragment,
            scalar_values,
            text_lists,
            object_lists,
            current_element_scope,
            item,
        ) {
            merge_style_property(properties, &style);
        }
    }
}

fn evaluate_style_fragment(
    fragment: &SemanticStyleFragment,
    scalar_values: &BTreeMap<String, i64>,
    text_lists: &BTreeMap<String, Vec<String>>,
    object_lists: &BTreeMap<String, Vec<ObjectListItem>>,
    current_element_scope: Option<&str>,
    item: Option<&ObjectListItem>,
) -> Option<String> {
    match fragment {
        SemanticStyleFragment::Static(style) => style.clone(),
        SemanticStyleFragment::BoolBinding {
            binding,
            truthy,
            falsy,
        } => {
            let binding = scope_element_binding(binding, current_element_scope);
            let branch = if scalar_values.get(&binding).copied().unwrap_or_default() != 0 {
                truthy
            } else {
                falsy
            };
            evaluate_style_fragment(
                branch,
                scalar_values,
                text_lists,
                object_lists,
                current_element_scope,
                item,
            )
        }
        SemanticStyleFragment::ScalarCompare {
            left,
            op,
            right,
            truthy,
            falsy,
        } => {
            let branch = if style_scalar_compare_matches(
                left,
                op.clone(),
                right,
                scalar_values,
                text_lists,
                object_lists,
                current_element_scope,
            ) {
                truthy
            } else {
                falsy
            };
            evaluate_style_fragment(
                branch,
                scalar_values,
                text_lists,
                object_lists,
                current_element_scope,
                item,
            )
        }
        SemanticStyleFragment::ObjectBoolField {
            field,
            truthy,
            falsy,
        } => {
            let branch = if item.is_some_and(|item| object_item_bool_field(item, field)) {
                truthy
            } else {
                falsy
            };
            evaluate_style_fragment(
                branch,
                scalar_values,
                text_lists,
                object_lists,
                current_element_scope,
                item,
            )
        }
    }
}

fn style_scalar_compare_matches(
    left: &DerivedScalarOperand,
    op: IntCompareOp,
    right: &DerivedScalarOperand,
    scalar_values: &BTreeMap<String, i64>,
    text_lists: &BTreeMap<String, Vec<String>>,
    object_lists: &BTreeMap<String, Vec<ObjectListItem>>,
    current_element_scope: Option<&str>,
) -> bool {
    let left = style_scalar_operand_value(
        left,
        scalar_values,
        text_lists,
        object_lists,
        current_element_scope,
    );
    let right = style_scalar_operand_value(
        right,
        scalar_values,
        text_lists,
        object_lists,
        current_element_scope,
    );
    match op {
        IntCompareOp::Equal => left == right,
        IntCompareOp::NotEqual => left != right,
        IntCompareOp::Greater => left > right,
        IntCompareOp::GreaterOrEqual => left >= right,
        IntCompareOp::Less => left < right,
        IntCompareOp::LessOrEqual => left <= right,
    }
}

fn style_scalar_operand_value(
    operand: &DerivedScalarOperand,
    scalar_values: &BTreeMap<String, i64>,
    text_lists: &BTreeMap<String, Vec<String>>,
    object_lists: &BTreeMap<String, Vec<ObjectListItem>>,
    current_element_scope: Option<&str>,
) -> i64 {
    match operand {
        DerivedScalarOperand::Literal(value) => *value,
        DerivedScalarOperand::Binding(binding) => {
            let binding = scope_element_binding(binding, current_element_scope);
            scalar_values.get(&binding).copied().unwrap_or_default()
        }
        DerivedScalarOperand::TextBindingNumber(_) => 0,
        DerivedScalarOperand::TextListCount { binding, filter } => text_lists
            .get(binding)
            .map(|items| {
                items
                    .iter()
                    .filter(|value| {
                        filter
                            .as_ref()
                            .is_none_or(|filter| text_list_matches_filter(value, filter))
                    })
                    .count() as i64
            })
            .unwrap_or_default(),
        DerivedScalarOperand::ObjectListCount { binding, filter } => object_lists
            .get(binding)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        filter.as_ref().is_none_or(|filter| {
                            object_list_item_matches_filter(
                                item,
                                filter,
                                scalar_values,
                                &BTreeMap::new(),
                            )
                        })
                    })
                    .count() as i64
            })
            .unwrap_or_default(),
        DerivedScalarOperand::Arithmetic { op, left, right } => {
            let left = style_scalar_operand_value(
                left,
                scalar_values,
                text_lists,
                object_lists,
                current_element_scope,
            );
            let right = style_scalar_operand_value(
                right,
                scalar_values,
                text_lists,
                object_lists,
                current_element_scope,
            );
            match op {
                DerivedArithmeticOp::Add => left + right,
                DerivedArithmeticOp::Subtract => left - right,
                DerivedArithmeticOp::Multiply => left * right,
                DerivedArithmeticOp::Divide => {
                    if right == 0 {
                        0
                    } else {
                        left / right
                    }
                }
            }
        }
        DerivedScalarOperand::Min { left, right } => style_scalar_operand_value(
            left,
            scalar_values,
            text_lists,
            object_lists,
            current_element_scope,
        )
        .min(style_scalar_operand_value(
            right,
            scalar_values,
            text_lists,
            object_lists,
            current_element_scope,
        )),
        DerivedScalarOperand::Round { source } => style_scalar_operand_value(
            source,
            scalar_values,
            text_lists,
            object_lists,
            current_element_scope,
        ),
    }
}

fn merge_style_property(properties: &mut Vec<(String, String)>, fragment: &str) {
    if let Some((_, style)) = properties.iter_mut().find(|(name, _)| name == "style") {
        if style.is_empty() {
            *style = fragment.to_string();
        } else {
            style.push(';');
            style.push_str(fragment);
        }
    } else {
        properties.push(("style".to_string(), fragment.to_string()));
    }
}

fn child_path(path: &[usize], index: usize) -> Vec<usize> {
    let mut next = path.to_vec();
    next.push(index);
    next
}

fn keyed_child_path(path: &[usize], key: u64) -> Vec<usize> {
    let mut next = path.to_vec();
    next.push(usize::MAX);
    next.push((key >> 32) as usize);
    next.push((key & 0xFFFF_FFFF) as usize);
    next
}

fn path_key(path: &[usize]) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.iter()
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join("_")
    }
}

fn object_item_scope(list_binding: &str, item_id: u64) -> String {
    format!("{list_binding}.__item__.{item_id}")
}

fn parse_nested_list_binding(list_binding: &str) -> Option<(String, u64, String)> {
    let (parent_binding, nested) = list_binding.split_once(".__item__.")?;
    let (parent_item_id, field) = nested.split_once('.')?;
    Some((
        parent_binding.to_string(),
        parent_item_id.parse().ok()?,
        field.to_string(),
    ))
}

fn parse_nested_parent(list_binding: &str, current_item_id: u64) -> Option<(String, u64)> {
    if let Some((parent_binding, parent_item_id, _)) = parse_nested_list_binding(list_binding) {
        return Some((parent_binding, parent_item_id));
    }
    Some((list_binding.to_string(), current_item_id))
}

fn format_action_summary(action: &SemanticAction) -> String {
    match action {
        SemanticAction::UpdateScalars { updates } => format!("UpdateScalars({updates:?})"),
        SemanticAction::UpdateTexts { updates } => format!("UpdateTexts({updates:?})"),
        SemanticAction::UpdateTextLists { updates } => format!("UpdateTextLists({updates:?})"),
        SemanticAction::UpdateObjectLists { updates } => format!("UpdateObjectLists({updates:?})"),
        SemanticAction::UpdateNestedObjectLists {
            parent_binding,
            parent_item_id,
            updates,
        } => format!(
            "UpdateNestedObjectLists(parent={parent_binding}, item={parent_item_id}, updates={updates:?})"
        ),
        SemanticAction::Batch { actions } => format!("Batch({actions:?})"),
    }
}

fn semantic_tree_contains_tag(node: &SemanticNode, expected_tag: &str) -> bool {
    match node {
        SemanticNode::Element { tag, children, .. } => {
            tag == expected_tag
                || children
                    .iter()
                    .any(|child| semantic_tree_contains_tag(child, expected_tag))
        }
        SemanticNode::Fragment(children) => children
            .iter()
            .any(|child| semantic_tree_contains_tag(child, expected_tag)),
        SemanticNode::Keyed { node, .. } => semantic_tree_contains_tag(node, expected_tag),
        SemanticNode::BoolBranch { truthy, falsy, .. }
        | SemanticNode::TextBindingBranch { truthy, falsy, .. }
        | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
        | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
        | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
        | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
        | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
            semantic_tree_contains_tag(truthy, expected_tag)
                || semantic_tree_contains_tag(falsy, expected_tag)
        }
        SemanticNode::Text(_)
        | SemanticNode::TextTemplate { .. }
        | SemanticNode::ScalarValue { .. }
        | SemanticNode::ObjectFieldValue { .. }
        | SemanticNode::TextList { .. }
        | SemanticNode::ObjectList { .. } => false,
    }
}

fn max_object_item_id(item: &ObjectListItem) -> u64 {
    let nested_max = item
        .object_lists
        .values()
        .flatten()
        .map(max_object_item_id)
        .max()
        .unwrap_or_default();
    item.id.max(nested_max)
}

fn local_element_scope(
    path: &[usize],
    item_scope: Option<&str>,
    fact_bindings: &[SemanticFactBinding],
) -> Option<String> {
    fact_bindings
        .iter()
        .any(|binding| binding.binding.starts_with("__element__."))
        .then(|| match item_scope {
            Some(scope) => format!("{scope}.__element__.{}", path_key(path)),
            None => format!("__element__.{}", path_key(path)),
        })
}

fn scope_element_binding(binding: &str, current_element_scope: Option<&str>) -> String {
    let Some(suffix) = binding.strip_prefix("__element__.") else {
        return binding.to_string();
    };
    match current_element_scope {
        Some(scope) => format!("{scope}.{suffix}"),
        None => binding.to_string(),
    }
}

fn text_list_matches_filter(value: &str, filter: &TextListFilter) -> bool {
    match filter {
        TextListFilter::IntCompare { op, value: target } => value
            .parse::<i64>()
            .map(|parsed| match op {
                IntCompareOp::Equal => parsed == *target,
                IntCompareOp::NotEqual => parsed != *target,
                IntCompareOp::Greater => parsed > *target,
                IntCompareOp::GreaterOrEqual => parsed >= *target,
                IntCompareOp::Less => parsed < *target,
                IntCompareOp::LessOrEqual => parsed <= *target,
            })
            .unwrap_or(false),
    }
}

fn object_list_item_matches_filter(
    item: &ObjectListItem,
    filter: &ObjectListFilter,
    scalar_values: &BTreeMap<String, i64>,
    text_values: &BTreeMap<String, String>,
) -> bool {
    match filter {
        ObjectListFilter::BoolFieldEquals { field, value } => {
            object_item_bool_field(item, field) == *value
        }
        ObjectListFilter::SelectedCompletedByScalar { binding } => {
            match scalar_values.get(binding).copied().unwrap_or_default() {
                0 => true,
                1 => !item.completed,
                2 => item.completed,
                _ => true,
            }
        }
        ObjectListFilter::TextFieldStartsWithTextBinding { field, binding } => {
            object_item_field_text(item, field).starts_with(
                text_values
                    .get(binding)
                    .map(String::as_str)
                    .unwrap_or_default(),
            )
        }
        ObjectListFilter::ItemIdEqualsScalarBinding { binding } => scalar_values
            .get(binding)
            .is_some_and(|value| *value == item.id as i64),
    }
}

pub fn bootstrap_runtime(
    source: &str,
    external_functions: usize,
    persistence_enabled: bool,
) -> WasmRuntime {
    let mut runtime = WasmRuntime::new(source.len(), external_functions, persistence_enabled);
    runtime.enable_incremental_diff();
    runtime
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use boon_renderer_zoon::FakeRenderState;
    use boon_scene::{
        EventPortId, RenderDiffBatch, RenderRoot, UiEvent, UiEventBatch, UiEventKind, UiFact,
        UiFactBatch, UiFactKind, UiNodeKind,
    };
    use serde::Serialize;

    use super::{KEYDOWN_TEXT_SEPARATOR, WasmRuntime, bootstrap_runtime};
    use crate::abi::{encode_render_diff_batch, encode_ui_event_batch, encode_ui_fact_batch};
    use crate::exec_ir::ExecProgram;
    use crate::lower::lower_to_semantic;
    use crate::semantic_ir::{
        ObjectListItem, RuntimeModel, ScalarRuntimeModel, ScalarUpdate, SemanticAction,
        SemanticEventBinding, SemanticInputValue, SemanticNode, SemanticProgram, SemanticTextPart,
        StateRuntimeModel, TextUpdate,
    };

    fn ui_text_content(node: &boon_scene::UiNode) -> Option<&str> {
        match &node.kind {
            UiNodeKind::Text { text } => Some(text.as_str()),
            UiNodeKind::Element { text, .. } => text.as_deref(),
        }
    }

    fn tree_contains_text(node: &boon_scene::UiNode, expected: &str) -> bool {
        ui_text_content(node).is_some_and(|text| text == expected)
            || node
                .children
                .iter()
                .any(|child| tree_contains_text(child, expected))
    }

    fn action_contains_booked_return_branch(action: &SemanticAction) -> bool {
        match action {
            SemanticAction::UpdateTexts { updates } => updates.iter().any(|update| {
                matches!(
                    update,
                    TextUpdate::SetComputedBranch {
                        binding,
                        condition_binding,
                        ..
                    } if binding == "store.booked" && condition_binding == "store.is_return"
                )
            }),
            SemanticAction::Batch { actions } => {
                actions.iter().any(action_contains_booked_return_branch)
            }
            _ => false,
        }
    }

    fn subtree_text(node: &boon_scene::UiNode) -> String {
        let mut text = String::new();
        if let Some(content) = ui_text_content(node) {
            text.push_str(content);
        }
        for child in &node.children {
            text.push_str(&subtree_text(child));
        }
        text
    }

    fn find_node_by_id(
        node: &boon_scene::UiNode,
        id: boon_scene::NodeId,
    ) -> Option<&boon_scene::UiNode> {
        if node.id == id {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| find_node_by_id(child, id))
    }

    fn find_first_tag<'a>(
        node: &'a boon_scene::UiNode,
        tag: &str,
    ) -> Option<&'a boon_scene::UiNode> {
        match &node.kind {
            UiNodeKind::Element { tag: node_tag, .. } if node_tag == tag => Some(node),
            _ => node
                .children
                .iter()
                .find_map(|child| find_first_tag(child, tag)),
        }
    }

    fn find_nth_tag<'a>(
        node: &'a boon_scene::UiNode,
        tag: &str,
        ordinal: usize,
    ) -> Option<&'a boon_scene::UiNode> {
        fn collect<'a>(
            node: &'a boon_scene::UiNode,
            tag: &str,
            out: &mut Vec<&'a boon_scene::UiNode>,
        ) {
            if matches!(&node.kind, UiNodeKind::Element { tag: node_tag, .. } if node_tag == tag) {
                out.push(node);
            }
            for child in &node.children {
                collect(child, tag, out);
            }
        }

        let mut matches = Vec::new();
        collect(node, tag, &mut matches);
        matches.into_iter().nth(ordinal)
    }

    fn direct_child_texts(node: &boon_scene::UiNode) -> Vec<String> {
        node.children.iter().map(subtree_text).collect()
    }

    fn object_list_item(id: u64, title: &str) -> ObjectListItem {
        ObjectListItem {
            id,
            title: title.to_string(),
            completed: false,
            text_fields: BTreeMap::new(),
            bool_fields: BTreeMap::new(),
            scalar_fields: BTreeMap::new(),
            object_lists: BTreeMap::new(),
            nested_item_actions: BTreeMap::new(),
        }
    }

    fn simple_object_list_program(items: Vec<ObjectListItem>) -> SemanticProgram {
        SemanticProgram {
            root: SemanticNode::element(
                "div",
                None,
                Vec::new(),
                Vec::new(),
                vec![SemanticNode::object_list(
                    "items",
                    None,
                    Vec::new(),
                    SemanticNode::element(
                        "div",
                        None,
                        vec![("data-kind".to_string(), "row".to_string())],
                        Vec::new(),
                        vec![SemanticNode::ObjectFieldValue {
                            field: "title".to_string(),
                        }],
                    ),
                )],
            ),
            runtime: RuntimeModel::State(StateRuntimeModel {
                object_lists: [("items".to_string(), items)].into_iter().collect(),
                ..StateRuntimeModel::default()
            }),
        }
    }

    fn nested_object_list_program(rows: Vec<ObjectListItem>) -> SemanticProgram {
        SemanticProgram {
            root: SemanticNode::element(
                "div",
                None,
                Vec::new(),
                Vec::new(),
                vec![SemanticNode::object_list(
                    "rows",
                    None,
                    Vec::new(),
                    SemanticNode::element(
                        "section",
                        None,
                        Vec::new(),
                        Vec::new(),
                        vec![
                            SemanticNode::element(
                                "label",
                                None,
                                Vec::new(),
                                Vec::new(),
                                vec![SemanticNode::ObjectFieldValue {
                                    field: "title".to_string(),
                                }],
                            ),
                            SemanticNode::element(
                                "div",
                                None,
                                vec![("data-kind".to_string(), "cells".to_string())],
                                Vec::new(),
                                vec![SemanticNode::object_list(
                                    "__item__.cells",
                                    None,
                                    Vec::new(),
                                    SemanticNode::element(
                                        "div",
                                        None,
                                        vec![("data-kind".to_string(), "cell".to_string())],
                                        Vec::new(),
                                        vec![SemanticNode::ObjectFieldValue {
                                            field: "title".to_string(),
                                        }],
                                    ),
                                )],
                            ),
                        ],
                    ),
                )],
            ),
            runtime: RuntimeModel::State(StateRuntimeModel {
                object_lists: [("rows".to_string(), rows)].into_iter().collect(),
                ..StateRuntimeModel::default()
            }),
        }
    }

    fn find_first_tag_with_text<'a>(
        node: &'a boon_scene::UiNode,
        tag: &str,
        text: &str,
    ) -> Option<&'a boon_scene::UiNode> {
        if let Some(found) = node
            .children
            .iter()
            .find_map(|child| find_first_tag_with_text(child, tag, text))
        {
            return Some(found);
        }
        match &node.kind {
            UiNodeKind::Element { tag: node_tag, .. }
                if node_tag == tag && tree_contains_text(node, text) =>
            {
                Some(node)
            }
            _ => None,
        }
    }

    fn find_first_tag_with_child_tag<'a>(
        node: &'a boon_scene::UiNode,
        tag: &str,
        child_tag: &str,
    ) -> Option<&'a boon_scene::UiNode> {
        if let Some(found) = node
            .children
            .iter()
            .find_map(|child| find_first_tag_with_child_tag(child, tag, child_tag))
        {
            return Some(found);
        }
        match &node.kind {
            UiNodeKind::Element { tag: node_tag, .. }
                if node_tag == tag
                    && node.children.iter().any(|child| {
                        matches!(
                            &child.kind,
                            UiNodeKind::Element { tag, .. } if tag == child_tag
                        )
                    }) =>
            {
                Some(node)
            }
            _ => None,
        }
    }

    fn find_first_tag_with_direct_child_tags<'a>(
        node: &'a boon_scene::UiNode,
        tag: &str,
        child_tags: &[&str],
    ) -> Option<&'a boon_scene::UiNode> {
        if let Some(found) = node
            .children
            .iter()
            .find_map(|child| find_first_tag_with_direct_child_tags(child, tag, child_tags))
        {
            return Some(found);
        }
        let UiNodeKind::Element { tag: node_tag, .. } = &node.kind else {
            return None;
        };
        if node_tag != tag {
            return None;
        }
        child_tags
            .iter()
            .all(|child_tag| {
                node.children.iter().any(|child| {
                    matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == child_tag)
                })
            })
            .then_some(node)
    }

    fn property_value<'a>(
        batch: &'a RenderDiffBatch,
        node_id: boon_scene::NodeId,
        name: &str,
    ) -> Option<&'a str> {
        batch.ops.iter().find_map(|op| match op {
            boon_scene::RenderOp::SetProperty {
                id,
                name: property_name,
                value: Some(value),
            } if *id == node_id && property_name == name => Some(value.as_str()),
            _ => None,
        })
    }

    fn batch_contains_property_fragment(
        batch: &RenderDiffBatch,
        name: &str,
        expected_fragment: &str,
    ) -> bool {
        batch.ops.iter().any(|op| {
            matches!(
                op,
                boon_scene::RenderOp::SetProperty {
                    name: property_name,
                    value: Some(value),
                    ..
                } if property_name == name && value.contains(expected_fragment)
            )
        })
    }

    fn find_port(
        batch: &RenderDiffBatch,
        node_id: boon_scene::NodeId,
        kind: UiEventKind,
    ) -> Option<boon_scene::EventPortId> {
        batch.ops.iter().find_map(|op| match op {
            boon_scene::RenderOp::AttachEventPort {
                id,
                port,
                kind: op_kind,
            } if *id == node_id && *op_kind == kind => Some(*port),
            _ => None,
        })
    }

    fn find_nth_port(
        batch: &RenderDiffBatch,
        kind: UiEventKind,
        ordinal: usize,
    ) -> Option<EventPortId> {
        batch
            .ops
            .iter()
            .filter_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort {
                    port,
                    kind: op_kind,
                    ..
                } if *op_kind == kind => Some(*port),
                _ => None,
            })
            .nth(ordinal)
    }

    fn find_nth_port_target(
        batch: &RenderDiffBatch,
        kind: UiEventKind,
        ordinal: usize,
    ) -> Option<boon_scene::NodeId> {
        batch
            .ops
            .iter()
            .filter_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort {
                    id, kind: op_kind, ..
                } if *op_kind == kind => Some(*id),
                _ => None,
            })
            .nth(ordinal)
    }

    fn nth_port_target_text(
        root: &boon_scene::UiNode,
        batch: &RenderDiffBatch,
        kind: UiEventKind,
        ordinal: usize,
    ) -> Option<String> {
        let node_id = find_nth_port_target(batch, kind, ordinal)?;
        let node = find_node_by_id(root, node_id)?;
        Some(subtree_text(node))
    }

    fn find_input_without_focus_port(
        batch: &RenderDiffBatch,
        kind: UiEventKind,
    ) -> Option<boon_scene::EventPortId> {
        let mut input_ids = Vec::new();
        let mut focus_ids = Vec::new();
        for op in &batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, kind, .. } = op {
                match kind {
                    UiEventKind::Input => {
                        if !input_ids.contains(id) {
                            input_ids.push(*id);
                        }
                    }
                    UiEventKind::Focus => {
                        if !focus_ids.contains(id) {
                            focus_ids.push(*id);
                        }
                    }
                    _ => {}
                }
            }
        }
        let input_id = input_ids
            .into_iter()
            .find(|id| !focus_ids.iter().any(|focus_id| focus_id == id))?;
        find_port(batch, input_id, kind)
    }

    fn find_nth_port_in_state(
        root: &boon_scene::UiNode,
        state: &FakeRenderState,
        kind: UiEventKind,
        ordinal: usize,
    ) -> Option<boon_scene::EventPortId> {
        fn collect(
            node: &boon_scene::UiNode,
            state: &FakeRenderState,
            kind: &UiEventKind,
            ports: &mut Vec<boon_scene::EventPortId>,
        ) {
            for (port, port_kind) in state.event_ports_for(node.id) {
                if &port_kind == kind {
                    ports.push(port);
                }
            }
            for child in &node.children {
                collect(child, state, kind, ports);
            }
        }

        let mut ports = Vec::new();
        collect(root, state, &kind, &mut ports);
        ports.into_iter().nth(ordinal)
    }

    fn find_nth_port_target_in_state(
        root: &boon_scene::UiNode,
        state: &FakeRenderState,
        kind: UiEventKind,
        ordinal: usize,
    ) -> Option<boon_scene::NodeId> {
        fn collect(
            node: &boon_scene::UiNode,
            state: &FakeRenderState,
            kind: &UiEventKind,
            ids: &mut Vec<boon_scene::NodeId>,
        ) {
            if state
                .event_ports_for(node.id)
                .into_iter()
                .any(|(_, port_kind)| &port_kind == kind)
            {
                ids.push(node.id);
            }
            for child in &node.children {
                collect(child, state, kind, ids);
            }
        }

        let mut ids = Vec::new();
        collect(root, state, &kind, &mut ids);
        ids.into_iter().nth(ordinal)
    }

    fn find_port_in_state(
        state: &FakeRenderState,
        node_id: boon_scene::NodeId,
        kind: UiEventKind,
    ) -> Option<boon_scene::EventPortId> {
        state
            .event_ports_for(node_id)
            .into_iter()
            .find_map(|(port, port_kind)| (port_kind == kind).then_some(port))
    }

    fn count_ports_in_state(
        root: &boon_scene::UiNode,
        state: &FakeRenderState,
        kind: UiEventKind,
    ) -> usize {
        fn count(node: &boon_scene::UiNode, state: &FakeRenderState, kind: &UiEventKind) -> usize {
            let local = state
                .event_ports_for(node.id)
                .into_iter()
                .filter(|(_, port_kind)| port_kind == kind)
                .count();
            local
                + node
                    .children
                    .iter()
                    .map(|child| count(child, state, kind))
                    .sum::<usize>()
        }

        count(root, state, &kind)
    }

    fn ui_node_count(node: &boon_scene::UiNode) -> usize {
        1 + node.children.iter().map(ui_node_count).sum::<usize>()
    }

    fn extract_ui_root(batch: &RenderDiffBatch) -> &boon_scene::UiNode {
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        root
    }

    fn apply_batch_and_root(
        state: &mut FakeRenderState,
        batch: &RenderDiffBatch,
    ) -> boon_scene::UiNode {
        state.apply_batch(batch).expect("batch should apply");
        let Some(RenderRoot::UiTree(root)) = state.root() else {
            panic!("expected ui root after batch");
        };
        root.clone()
    }

    fn assert_node_id_and_port_kind(
        root: &boon_scene::UiNode,
        state: &FakeRenderState,
        node_id: boon_scene::NodeId,
        kind: UiEventKind,
        message: &str,
    ) {
        let node = find_node_by_id(root, node_id).expect(message);
        assert!(
            find_port_in_state(state, node.id, kind.clone()).is_some(),
            "{message}: expected {:?} port on {:?}",
            kind,
            node.id
        );
    }

    fn init_cells_incremental_runtime() -> (WasmRuntime, FakeRenderState, boon_scene::UiNode) {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        (runtime, state, init_root)
    }

    fn capture_cells_double_click_ids(
        root: &boon_scene::UiNode,
        state: &FakeRenderState,
        ordinals: &[usize],
    ) -> Vec<(usize, boon_scene::NodeId)> {
        ordinals
            .iter()
            .map(|ordinal| {
                (
                    *ordinal,
                    find_nth_port_target_in_state(root, state, UiEventKind::DoubleClick, *ordinal)
                        .unwrap_or_else(|| {
                            panic!("cell ordinal {ordinal} should expose DoubleClick")
                        }),
                )
            })
            .collect()
    }

    fn assert_cells_double_click_ids_stable(
        root: &boon_scene::UiNode,
        state: &FakeRenderState,
        ids: &[(usize, boon_scene::NodeId)],
        context: &str,
    ) {
        for (ordinal, node_id) in ids {
            assert_node_id_and_port_kind(
                root,
                state,
                *node_id,
                UiEventKind::DoubleClick,
                &format!(
                    "{context}: sibling cell ordinal {ordinal} should keep its display identity and port"
                ),
            );
        }
    }

    fn enter_edit_mode_for_nth_cells_cell(
        runtime: &mut WasmRuntime,
        state: &mut FakeRenderState,
        root: &boon_scene::UiNode,
        ordinal: usize,
    ) -> (
        boon_scene::UiNode,
        boon_scene::NodeId,
        boon_scene::EventPortId,
        boon_scene::EventPortId,
        Option<boon_scene::EventPortId>,
    ) {
        let display_id =
            find_nth_port_target_in_state(root, state, UiEventKind::DoubleClick, ordinal)
                .unwrap_or_else(|| panic!("cell ordinal {ordinal} should expose DoubleClick"));
        let double_click_port =
            find_nth_port_in_state(root, state, UiEventKind::DoubleClick, ordinal)
                .unwrap_or_else(|| panic!("cell ordinal {ordinal} should expose DoubleClick"));

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        assert!(
            edit_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "edit entry should stay incremental",
        );
        let edit_root = apply_batch_and_root(state, &edit_batch);
        let input = find_first_tag(&edit_root, "input").expect("edit mode should render input");
        let input_port = find_port_in_state(state, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port_in_state(state, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");
        let blur_port = find_port_in_state(state, input.id, UiEventKind::Blur);
        (edit_root, display_id, input_port, key_down_port, blur_port)
    }

    fn attached_port_count(batch: &RenderDiffBatch, expected_kind: UiEventKind) -> usize {
        batch
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    boon_scene::RenderOp::AttachEventPort { kind, .. } if *kind == expected_kind
                )
            })
            .count()
    }

    #[derive(Debug, Serialize)]
    struct BatchMetrics {
        encoded_bytes: usize,
        op_count: usize,
        ui_node_count: usize,
        double_click_ports: usize,
        input_ports: usize,
        key_down_ports: usize,
    }

    fn batch_metrics(batch: &RenderDiffBatch) -> BatchMetrics {
        BatchMetrics {
            encoded_bytes: encode_render_diff_batch(batch).len(),
            op_count: batch.ops.len(),
            ui_node_count: batch
                .ops
                .iter()
                .find_map(|op| match op {
                    boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) => {
                        Some(ui_node_count(root))
                    }
                    _ => None,
                })
                .unwrap_or_default(),
            double_click_ports: attached_port_count(batch, UiEventKind::DoubleClick),
            input_ports: attached_port_count(batch, UiEventKind::Input),
            key_down_ports: attached_port_count(batch, UiEventKind::KeyDown),
        }
    }

    #[derive(Debug, Serialize)]
    struct WasmInternalPipelineMetrics {
        lower_exec_millis: u128,
        init_millis: u128,
        a1_commit_millis: u128,
        init_batch: BatchMetrics,
        a1_commit_batch: BatchMetrics,
    }

    const CELLS_26X100_INIT_MAX_ENCODED_BYTES: usize = 950_000;
    const CELLS_26X100_INIT_MAX_OPS: usize = 3_000;
    const CELLS_26X100_COMMIT_MAX_ENCODED_BYTES: usize = 2_000;
    const CELLS_26X100_COMMIT_MAX_OPS: usize = 16;
    const CELLS_26X100_COMMIT_MAX_DOUBLE_CLICK_ATTACHES: usize = 4;

    fn wasm_pro_pipeline_metrics_for_cells() -> WasmInternalPipelineMetrics {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");

        let lower_started = Instant::now();
        let semantic = lower_to_semantic(source, None, false);
        let exec = ExecProgram::from_semantic(&semantic);
        let lower_exec_millis = lower_started.elapsed().as_millis();

        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();

        let init_started = Instant::now();
        let init_descriptor = runtime.init(&exec);
        let init_millis = init_started.elapsed().as_millis();
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("Wasm Pro init batch should decode");
        let init_metrics = batch_metrics(&init_batch);

        let mut state = FakeRenderState::default();
        let _ = apply_batch_and_root(&mut state, &init_batch);

        let commit_started = Instant::now();
        let a1_commit_batch =
            edit_nth_cells_grid_cell_and_commit_batch_with_state(&mut runtime, &mut state, 0, "7");
        let a1_commit_millis = commit_started.elapsed().as_millis();
        let a1_commit_metrics = batch_metrics(&a1_commit_batch);

        WasmInternalPipelineMetrics {
            lower_exec_millis,
            init_millis,
            a1_commit_millis,
            init_batch: init_metrics,
            a1_commit_batch: a1_commit_metrics,
        }
    }

    fn edit_nth_cells_grid_cell_and_commit(
        runtime: &mut WasmRuntime,
        ordinal: usize,
        value: &str,
    ) -> boon_scene::UiNode {
        let mut state = FakeRenderState::default();
        let commit_batch = edit_nth_cells_grid_cell_and_commit_batch_with_state(
            runtime, &mut state, ordinal, value,
        );
        extract_ui_root(&commit_batch).clone()
    }

    fn edit_nth_cells_grid_cell_and_commit_batch(
        runtime: &mut WasmRuntime,
        ordinal: usize,
        value: &str,
    ) -> RenderDiffBatch {
        let mut state = FakeRenderState::default();
        edit_nth_cells_grid_cell_and_commit_batch_with_state(runtime, &mut state, ordinal, value)
    }

    fn edit_nth_cells_grid_cell_and_commit_batch_with_state(
        runtime: &mut WasmRuntime,
        state: &mut FakeRenderState,
        ordinal: usize,
        value: &str,
    ) -> RenderDiffBatch {
        let init_descriptor = runtime.take_commands();
        if init_descriptor != 0 {
            let init_batch = runtime
                .decode_commands(init_descriptor)
                .expect("init batch should decode");
            let _ = apply_batch_and_root(state, &init_batch);
        }
        let Some(RenderRoot::UiTree(root)) = state.root() else {
            panic!("expected cells root before editing");
        };
        let double_click_port =
            find_nth_port_in_state(root, state, UiEventKind::DoubleClick, ordinal)
                .expect("target cell should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let root = apply_batch_and_root(state, &edit_batch);
        let input = find_first_tag(&root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some(value.to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let _ = apply_batch_and_root(state, &commit_batch);
        commit_batch
    }

    #[test]
    fn init_queues_replace_root_batch() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "section",
                Some("Wasm runtime scaffold".to_string()),
                Vec::new(),
                Vec::new(),
                vec![SemanticNode::text("child")],
            ),
            runtime: RuntimeModel::Static,
        });
        let mut runtime = WasmRuntime::new(12, 0, false);

        let descriptor = runtime.init(&exec);
        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");

        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Element { text, .. } = &root.kind else {
            panic!("expected element root");
        };
        assert_eq!(text.as_deref(), Some("Wasm runtime scaffold"));
    }

    #[test]
    fn dispatch_events_updates_status_batch() {
        let mut runtime = bootstrap_runtime("source", 0, false);
        let descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: EventPortId::new(),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("event batch should decode");

        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[5].kind else {
            panic!("expected event status text");
        };
        assert_eq!(text, "Event batches: 1");
        let UiNodeKind::Text { text } = &root.children[7].kind else {
            panic!("expected last event text");
        };
        assert_eq!(text, "Last event: Click payload=none");
    }

    #[test]
    fn apply_facts_updates_status_batch() {
        let mut runtime = bootstrap_runtime("source", 0, false);
        let descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: boon_scene::NodeId::new(),
                    kind: UiFactKind::Focused(true),
                }],
            }))
            .expect("fact batch should decode");

        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[6].kind else {
            panic!("expected fact status text");
        };
        assert_eq!(text, "Fact batches: 1");
        let UiNodeKind::Text { text } = &root.children[9].kind else {
            panic!("expected focused status text");
        };
        assert_eq!(text, "Focused: yes");
    }

    #[test]
    fn hover_fact_updates_element_hover_branch() {
        let semantic = lower_to_semantic(
            r#"
document: Document/new(root: Element/stripe(
    element: [hovered: LINK]
    direction: Column
    gap: 0
    style: []

    items: LIST {
        element.hovered |> WHILE {
            True => Element/label(element: [], style: [], label: TEXT { Hovered })
            False => NoElement
        }
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(!tree_contains_text(root, "Hovered"));

        let hover_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: root.id,
                    kind: UiFactKind::Hovered(true),
                }],
            }))
            .expect("hover fact should decode");
        let hover_batch = runtime
            .decode_commands(hover_descriptor)
            .expect("hover diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &hover_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert!(tree_contains_text(root, "Hovered"));

        let unhover_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: root.id,
                    kind: UiFactKind::Hovered(false),
                }],
            }))
            .expect("unhover fact should decode");
        let unhover_batch = runtime
            .decode_commands(unhover_descriptor)
            .expect("unhover diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &unhover_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert!(!tree_contains_text(root, "Hovered"));
    }

    #[test]
    fn focus_fact_updates_element_focus_branch() {
        let semantic = lower_to_semantic(
            r#"
document: Document/new(root: Element/button(
    element: [focused: LINK]
    style: []
    label: element.focused |> WHILE {
        True => TEXT { Focused }
        False => TEXT { Idle }
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Idle"));
        assert!(!tree_contains_text(root, "Focused"));

        let focus_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: root.id,
                    kind: UiFactKind::Focused(true),
                }],
            }))
            .expect("focus fact should decode");
        let focus_batch = runtime
            .decode_commands(focus_descriptor)
            .expect("focus diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &focus_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Focused"));
        assert!(!tree_contains_text(root, "Idle"));
    }

    #[test]
    fn focus_fact_is_scoped_per_helper_instance() {
        let semantic = lower_to_semantic(
            r#"
document: Document/new(root: Element/stripe(
    element: []
    direction: Row
    gap: 10
    style: []

    items: LIST {
        focus_button(label: TEXT { Left })
        focus_button(label: TEXT { Right })
    }
))

FUNCTION focus_button(label) {
    Element/stripe(
        element: [focused: LINK]
        direction: Row
        gap: 4
        style: []
        items: LIST {
            label
            element.focused |> WHILE {
                True => TEXT { active }
                False => TEXT { idle }
            }
        }
    )
}
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let left_button =
            find_first_tag_with_text(root, "div", "Left").expect("left helper should render");
        let right_button =
            find_first_tag_with_text(root, "div", "Right").expect("right helper should render");

        let focus_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: left_button.id,
                    kind: UiFactKind::Focused(true),
                }],
            }))
            .expect("focus fact should decode");
        let focus_batch = runtime
            .decode_commands(focus_descriptor)
            .expect("focus diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &focus_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let left_after =
            find_first_tag_with_text(root, "div", "Left").expect("left helper should rerender");
        let right_after =
            find_first_tag_with_text(root, "div", "Right").expect("right helper should rerender");
        assert!(tree_contains_text(left_after, "active"));
        assert!(tree_contains_text(right_after, "idle"));

        let blur_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![
                    UiFact {
                        id: left_after.id,
                        kind: UiFactKind::Focused(false),
                    },
                    UiFact {
                        id: right_after.id,
                        kind: UiFactKind::Focused(true),
                    },
                ],
            }))
            .expect("blur/focus facts should decode");
        let blur_batch = runtime
            .decode_commands(blur_descriptor)
            .expect("blur/focus diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &blur_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let left_after =
            find_first_tag_with_text(root, "div", "Left").expect("left helper should rerender");
        let right_after =
            find_first_tag_with_text(root, "div", "Right").expect("right helper should rerender");
        assert!(tree_contains_text(left_after, "idle"));
        assert!(tree_contains_text(right_after, "active"));
    }

    #[test]
    fn take_commands_returns_pending_descriptor_once() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                Some("root".to_string()),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            runtime: RuntimeModel::Static,
        });
        let mut runtime = WasmRuntime::new(4, 0, false);

        let descriptor = runtime.init(&exec);

        assert_eq!(runtime.take_commands(), descriptor);
        assert_eq!(runtime.take_commands(), 0);
    }

    #[test]
    fn input_event_updates_draft_text() {
        let mut runtime = bootstrap_runtime("source", 0, false);
        let descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: EventPortId::new(),
                    kind: UiEventKind::Input,
                    payload: Some("cells".to_string()),
                }],
            }))
            .expect("event batch should decode");

        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[7].kind else {
            panic!("expected last event text");
        };
        assert_eq!(text, "Last event: Input payload=cells");
    }

    #[test]
    fn init_renders_static_text_root_without_status_scaffold() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::text("Hello world"),
            runtime: RuntimeModel::Static,
        });
        let mut runtime = WasmRuntime::new(0, 0, false);

        let descriptor = runtime.init(&exec);
        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");

        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.kind else {
            panic!("expected text root");
        };
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn counter_action_updates_rendered_value() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                None,
                Vec::new(),
                Vec::new(),
                vec![
                    SemanticNode::ScalarValue {
                        binding: "counter".to_string(),
                        value: 0,
                    },
                    SemanticNode::element(
                        "button",
                        Some("+".to_string()),
                        Vec::new(),
                        vec![SemanticEventBinding {
                            kind: UiEventKind::Click,
                            source_binding: None,
                            action: Some(SemanticAction::UpdateScalars {
                                updates: vec![ScalarUpdate::Add {
                                    binding: "counter".to_string(),
                                    delta: 1,
                                }],
                            }),
                        }],
                        Vec::new(),
                    ),
                ],
            ),
            runtime: RuntimeModel::Scalars(ScalarRuntimeModel {
                values: [("counter".to_string(), 0)].into_iter().collect(),
            }),
        });
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[0].kind else {
            panic!("expected counter text child");
        };
        assert_eq!(text, "0");

        let UiNodeKind::Element { event_ports, .. } = &root.children[1].kind else {
            panic!("expected button element child");
        };
        let port = *event_ports
            .first()
            .expect("counter button should expose an event port");
        let descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("event batch should decode");

        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[0].kind else {
            panic!("expected counter text child");
        };
        assert_eq!(text, "1");
    }

    #[test]
    fn text_template_rerenders_from_scalar_state() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                None,
                Vec::new(),
                Vec::new(),
                vec![
                    SemanticNode::text_template(
                        vec![
                            SemanticTextPart::Static("Sum: ".to_string()),
                            SemanticTextPart::ScalarBinding("sum".to_string()),
                        ],
                        "Sum: 7",
                    ),
                    SemanticNode::element(
                        "button",
                        Some("Send 1".to_string()),
                        Vec::new(),
                        vec![SemanticEventBinding {
                            kind: UiEventKind::Click,
                            source_binding: None,
                            action: Some(SemanticAction::UpdateScalars {
                                updates: vec![ScalarUpdate::Add {
                                    binding: "sum".to_string(),
                                    delta: 1,
                                }],
                            }),
                        }],
                        Vec::new(),
                    ),
                ],
            ),
            runtime: RuntimeModel::Scalars(ScalarRuntimeModel {
                values: [("sum".to_string(), 7)].into_iter().collect(),
            }),
        });
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[0].kind else {
            panic!("expected text template child");
        };
        assert_eq!(text, "Sum: 7");

        let UiNodeKind::Element { event_ports, .. } = &root.children[1].kind else {
            panic!("expected button element child");
        };
        let port = *event_ports
            .first()
            .expect("template button should expose an event port");
        let descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("event batch should decode");

        let batch = runtime
            .decode_commands(descriptor)
            .expect("render diff batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[0].kind else {
            panic!("expected text template child");
        };
        assert_eq!(text, "Sum: 8");
    }

    #[test]
    fn incremental_diff_reorders_object_list_items_by_stable_item_identity() {
        let exec = ExecProgram::from_semantic(&simple_object_list_program(vec![
            object_list_item(11, "A"),
            object_list_item(22, "B"),
            object_list_item(33, "C"),
        ]));
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let initial_ids = init_root
            .children
            .iter()
            .map(|child| child.id)
            .collect::<Vec<_>>();
        assert_eq!(direct_child_texts(&init_root), vec!["A", "B", "C"]);

        runtime
            .object_lists
            .get_mut("items")
            .expect("items should exist")
            .swap(0, 1);

        let reorder_descriptor = runtime.render_active_program();
        let reorder_batch = runtime
            .decode_commands(reorder_descriptor)
            .expect("reorder batch should decode");
        assert!(
            reorder_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "reorder should be incremental",
        );
        assert!(
            reorder_batch
                .ops
                .iter()
                .any(|op| matches!(op, boon_scene::RenderOp::MoveChild { .. })),
            "reorder should emit MoveChild",
        );
        assert!(
            reorder_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::SetText { .. })),
            "pure reorder should not rewrite text payloads",
        );

        let reorder_root = apply_batch_and_root(&mut state, &reorder_batch);
        assert_eq!(direct_child_texts(&reorder_root), vec!["B", "A", "C"]);
        assert_eq!(reorder_root.children[0].id, initial_ids[1]);
        assert_eq!(reorder_root.children[1].id, initial_ids[0]);
        assert_eq!(reorder_root.children[2].id, initial_ids[2]);
    }

    #[test]
    fn incremental_diff_removes_object_list_item_without_reidentifying_survivors() {
        let exec = ExecProgram::from_semantic(&simple_object_list_program(vec![
            object_list_item(11, "A"),
            object_list_item(22, "B"),
            object_list_item(33, "C"),
        ]));
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let removed_id = init_root.children[0].id;
        let survivor_ids = init_root.children[1..]
            .iter()
            .map(|child| child.id)
            .collect::<Vec<_>>();

        runtime
            .object_lists
            .get_mut("items")
            .expect("items should exist")
            .remove(0);

        let remove_descriptor = runtime.render_active_program();
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove batch should decode");
        assert!(
            remove_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "remove should be incremental",
        );
        assert!(
            remove_batch.ops.iter().any(
                |op| matches!(op, boon_scene::RenderOp::RemoveNode { id } if *id == removed_id)
            ),
            "remove should target the removed item's stable node id",
        );
        assert!(
            remove_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::SetText { .. })),
            "remove should not rewrite surviving rows as text churn",
        );

        let remove_root = apply_batch_and_root(&mut state, &remove_batch);
        assert_eq!(direct_child_texts(&remove_root), vec!["B", "C"]);
        assert_eq!(remove_root.children[0].id, survivor_ids[0]);
        assert_eq!(remove_root.children[1].id, survivor_ids[1]);
    }

    #[test]
    fn complex_counter_buttons_update_nested_scalar_binding() {
        let semantic = lower_to_semantic(
            r#"
store: [
    elements: [decrement_button: LINK, increment_button: LINK]

    counter: 0 |> HOLD counter {
        LATEST {
            elements.decrement_button.event.press |> THEN { counter - 1 }
            elements.increment_button.event.press |> THEN { counter + 1 }
        }
    }
]

document: Document/new(root: root_element(PASS: store))

FUNCTION root_element() {
    Element/stripe(
        element: []
        direction: Row
        gap: 15
        style: [align: Center]

        items: LIST {
            counter_button(label: TEXT { - }) |> LINK { PASSED.elements.decrement_button }
            PASSED.counter
            counter_button(label: TEXT { + }) |> LINK { PASSED.elements.increment_button }
        }
    )
}

FUNCTION counter_button(label) {
    Element/button(
        element: [event: [press: LINK], hovered: LINK]
        style: [width: 45]
        label: label
    )
}
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Element {
            event_ports: decrement_ports,
            ..
        } = &root.children[0].kind
        else {
            panic!("expected decrement button");
        };
        let UiNodeKind::Text { text } = &root.children[1].kind else {
            panic!("expected counter text");
        };
        assert_eq!(text, "0");
        let UiNodeKind::Element {
            event_ports: increment_ports,
            ..
        } = &root.children[2].kind
        else {
            panic!("expected increment button");
        };

        let increment_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: increment_ports[0],
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("increment batch should decode");
        let increment_batch = runtime
            .decode_commands(increment_descriptor)
            .expect("increment diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &increment_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Element {
            event_ports: decrement_ports,
            ..
        } = &root.children[0].kind
        else {
            panic!("expected decrement button");
        };
        let UiNodeKind::Text { text } = &root.children[1].kind else {
            panic!("expected counter text");
        };
        assert_eq!(text, "1");

        let decrement_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: decrement_ports[0],
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("decrement batch should decode");
        let decrement_batch = runtime
            .decode_commands(decrement_descriptor)
            .expect("decrement diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &decrement_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[1].kind else {
            panic!("expected counter text");
        };
        assert_eq!(text, "0");
    }

    #[test]
    fn static_object_list_item_buttons_update_independent_counts() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let counters_row = &root.children[1];
        assert_eq!(counters_row.children.len(), 3);
        for counter in &counters_row.children {
            let UiNodeKind::Text { text } = &counter.children[1].children[0].kind else {
                panic!("expected count text");
            };
            assert_eq!(text, "Count: 0");
        }

        let mut first_port = None;
        let mut second_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *kind != UiEventKind::Click {
                    continue;
                }
                if *id == counters_row.children[0].children[0].id {
                    first_port = Some(*port);
                } else if *id == counters_row.children[1].children[0].id {
                    second_port = Some(*port);
                }
            }
        }
        let first_port = first_port.expect("first counter button should expose Click");
        let second_port = second_port.expect("second counter button should expose Click");

        let first_click_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("first click batch should decode");
        let first_click_batch = runtime
            .decode_commands(first_click_descriptor)
            .expect("first click diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &first_click_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let counters_row = &root.children[1];
        let UiNodeKind::Text { text } = &counters_row.children[0].children[1].children[0].kind
        else {
            panic!("expected first counter text");
        };
        assert_eq!(text, "Count: 1");
        let UiNodeKind::Text { text } = &counters_row.children[1].children[1].children[0].kind
        else {
            panic!("expected second counter text");
        };
        assert_eq!(text, "Count: 0");

        let mut first_port = None;
        let mut second_port = None;
        for op in &first_click_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *kind != UiEventKind::Click {
                    continue;
                }
                if *id == counters_row.children[0].children[0].id {
                    first_port = Some(*port);
                } else if *id == counters_row.children[1].children[0].id {
                    second_port = Some(*port);
                }
            }
        }
        let first_port = first_port.expect("rerendered first counter button should expose Click");
        let second_port =
            second_port.expect("rerendered second counter button should expose Click");

        let second_click_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![
                    UiEvent {
                        target: first_port,
                        kind: UiEventKind::Click,
                        payload: None,
                    },
                    UiEvent {
                        target: second_port,
                        kind: UiEventKind::Click,
                        payload: None,
                    },
                ],
            }))
            .expect("second click batch should decode");
        let second_click_batch = runtime
            .decode_commands(second_click_descriptor)
            .expect("second click diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &second_click_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let counters_row = &root.children[1];
        let UiNodeKind::Text { text } = &counters_row.children[0].children[1].children[0].kind
        else {
            panic!("expected first counter text after second click");
        };
        assert_eq!(text, "Count: 2");
        let UiNodeKind::Text { text } = &counters_row.children[1].children[1].children[0].kind
        else {
            panic!("expected second counter text after second click");
        };
        assert_eq!(text, "Count: 1");
        let UiNodeKind::Text { text } = &counters_row.children[2].children[1].children[0].kind
        else {
            panic!("expected third counter text after second click");
        };
        assert_eq!(text, "Count: 0");
    }

    #[test]
    fn static_object_list_item_checkboxes_toggle_independently() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/checkbox_test.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children.len(), 2);
        let first_row = &root.children[0];
        let second_row = &root.children[1];
        let UiNodeKind::Text { text } = &first_row.children[0].children[0].kind else {
            panic!("expected first checkbox icon text");
        };
        assert_eq!(text, "[ ]");
        let UiNodeKind::Text { text } = &first_row.children[2].children[0].kind else {
            panic!("expected first status text");
        };
        assert_eq!(text, "(unchecked)");
        let UiNodeKind::Text { text } = &second_row.children[2].children[0].kind else {
            panic!("expected second status text");
        };
        assert_eq!(text, "(unchecked)");

        let mut first_port = None;
        let mut second_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *kind != UiEventKind::Click {
                    continue;
                }
                if *id == first_row.children[0].id {
                    first_port = Some(*port);
                } else if *id == second_row.children[0].id {
                    second_port = Some(*port);
                }
            }
        }
        let first_port = first_port.expect("first checkbox should expose Click");
        let second_port = second_port.expect("second checkbox should expose Click");

        let first_click_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("first checkbox click should decode");
        let first_click_batch = runtime
            .decode_commands(first_click_descriptor)
            .expect("first checkbox diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &first_click_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let first_row = &root.children[0];
        let second_row = &root.children[1];
        let UiNodeKind::Text { text } = &first_row.children[0].children[0].kind else {
            panic!("expected first checkbox icon after click");
        };
        assert_eq!(text, "[X]");
        let UiNodeKind::Text { text } = &first_row.children[2].children[0].kind else {
            panic!("expected first status after click");
        };
        assert_eq!(text, "(checked)");
        let UiNodeKind::Text { text } = &second_row.children[2].children[0].kind else {
            panic!("expected second status after first click");
        };
        assert_eq!(text, "(unchecked)");

        let mut second_port = None;
        for op in &first_click_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *kind == UiEventKind::Click && *id == second_row.children[0].id {
                    second_port = Some(*port);
                }
            }
        }
        let second_port = second_port.expect("rerendered second checkbox should expose Click");

        let second_click_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: second_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("second checkbox click should decode");
        let second_click_batch = runtime
            .decode_commands(second_click_descriptor)
            .expect("second checkbox diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &second_click_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let first_row = &root.children[0];
        let second_row = &root.children[1];
        let UiNodeKind::Text { text } = &first_row.children[2].children[0].kind else {
            panic!("expected first status after second click");
        };
        assert_eq!(text, "(checked)");
        let UiNodeKind::Text { text } = &second_row.children[0].children[0].kind else {
            panic!("expected second checkbox icon after click");
        };
        assert_eq!(text, "[X]");
        let UiNodeKind::Text { text } = &second_row.children[2].children[0].kind else {
            panic!("expected second status after click");
        };
        assert_eq!(text, "(checked)");
    }

    #[test]
    fn static_object_list_item_remove_hides_only_target_row() {
        let semantic = lower_to_semantic(
            r#"
store: [
    items:
        LIST {
            make_item(name: TEXT { Item A })
            make_item(name: TEXT { Item B })
        }
        |> List/remove(item, on: item.remove_button.event.click)
]

FUNCTION make_item(name) {
    [
        remove_button: LINK
        name: name
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.items |> List/map(item, new: Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/label(element: [], style: [], label: item.name)
            Element/button(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Remove }
            )
            |> LINK { item.remove_button }
        }
    ))
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children.len(), 2);
        let first_row = &root.children[0];
        let second_row = &root.children[1];
        let UiNodeKind::Text { text } = &first_row.children[0].children[0].kind else {
            panic!("expected first row label");
        };
        assert_eq!(text, "Item A");
        let UiNodeKind::Text { text } = &second_row.children[0].children[0].kind else {
            panic!("expected second row label");
        };
        assert_eq!(text, "Item B");

        let mut first_remove_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *kind == UiEventKind::Click && *id == first_row.children[1].id {
                    first_remove_port = Some(*port);
                }
            }
        }
        let first_remove_port = first_remove_port.expect("first remove button should expose Click");

        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_remove_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove click should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &remove_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children.len(), 1);
        let UiNodeKind::Text { text } = &root.children[0].children[0].children[0].kind else {
            panic!("expected remaining row label");
        };
        assert_eq!(text, "Item B");
    }

    #[test]
    fn object_text_empty_branch_renders_per_item_text_fields() {
        let semantic = lower_to_semantic(
            r#"
store: [
    items: LIST {
        make_item(name: TEXT {  })
        make_item(name: TEXT { Item B })
    }
]

FUNCTION make_item(name) {
    [
        name: name
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.items |> List/map(item, new:
        item.name
        |> Text/is_empty()
        |> WHILE {
            True => Element/label(element: [], style: [], label: TEXT { Empty })
            False => Element/label(element: [], style: [], label: item.name)
        }
    )
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children.len(), 2);
        let UiNodeKind::Text { text } = &root.children[0].children[0].kind else {
            panic!("expected first row label");
        };
        assert_eq!(text, "Empty");
        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected second row label");
        };
        assert_eq!(text, "Item B");
    }

    #[test]
    fn runtime_text_list_append_updates_count_and_items() {
        let semantic = lower_to_semantic(
            r#"
store: [
    input: LINK

    text_to_add: store.input.event.key_down.key |> WHEN {
        Enter => store.input.text
        __ => SKIP
    }

    items:
        LIST {
            TEXT { Initial }
        }
        |> List/append(item: text_to_add)
]

document: Document/new(root: root_element(PASS: [store: store]))

FUNCTION root_element() {
    Element/stripe(
        element: []
        direction: Column
        gap: 10
        style: [padding: 20]

        items: LIST {
            Element/text_input(
                element: [event: [key_down: LINK, change: LINK]]
                style: []
                label: Hidden[text: TEXT { Add item }]

                text: LATEST {
                    Text/empty()
                    element.event.change.text
                }

                placeholder: [text: TEXT { Type and press Enter }]
                focus: True
            )
            |> LINK { PASSED.store.input }

            Element/label(element: [], style: [], label: BLOCK {
                count: PASSED.store.items |> List/count()

                TEXT { All count: {count} }
            })

            Element/stripe(
                element: []
                direction: Column
                gap: 5
                style: []

                items: PASSED.store.items
                |> List/map(item, new: Element/label(element: [], style: [], label: item))
            )
        }
    )
}
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input_id = root.children[0].id;
        let mut input_port = None;
        let mut key_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == input_id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                }
            }
        }
        let input_port = input_port.expect("input should expose an Input port");
        let key_port = key_port.expect("input should expose a KeyDown port");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Milk".to_string()),
                }],
            }))
            .expect("input batch should decode");

        let append_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("keydown batch should decode");
        let append_batch = runtime
            .decode_commands(append_descriptor)
            .expect("append batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &append_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let UiNodeKind::Element { event_ports: _, .. } = &root.children[0].kind else {
            panic!("expected input child");
        };
        let count_node = &root.children[1];
        let UiNodeKind::Element { .. } = &count_node.kind else {
            panic!("expected count label");
        };
        let UiNodeKind::Text { text } = &count_node.children[0].kind else {
            panic!("expected count text");
        };
        assert_eq!(text, "All count: 2");

        let list_node = &root.children[2];
        let UiNodeKind::Element { .. } = &list_node.kind else {
            panic!("expected items stripe");
        };
        let list_children = &list_node.children;
        assert_eq!(list_children.len(), 2);
        let first_item = &list_children[0];
        let UiNodeKind::Element { .. } = &first_item.kind else {
            panic!("expected first list item");
        };
        let UiNodeKind::Text { text } = &first_item.children[0].kind else {
            panic!("expected first item text");
        };
        assert_eq!(text, "Initial");
        let second_item = &list_children[1];
        let UiNodeKind::Element { .. } = &second_item.kind else {
            panic!("expected second list item");
        };
        let UiNodeKind::Text { text } = &second_item.children[0].kind else {
            panic!("expected second item text");
        };
        assert_eq!(text, "Milk");
    }

    #[test]
    fn runtime_text_binding_branch_updates_from_input_payload() {
        let semantic = lower_to_semantic(
            r#"
store: [
    input: LINK
    value: LATEST {
        Text/empty()
        store.input.event.change.text
    }
]

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [change: LINK]]
            style: []
            label: Hidden[text: TEXT { Value }]
            text: Text/empty()
            placeholder: []
            focus: False
        )
        |> LINK { store.input }

        store.value |> Text/is_empty() |> WHILE {
            True => Element/label(element: [], style: [], label: TEXT { Empty })
            False => Element/label(element: [], style: [], label: TEXT { {store.value} })
        }
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert!(tree_contains_text(root, "Empty"));

        let input_id = root.children[0].id;
        let input_port = init_batch
            .ops
            .iter()
            .find_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort { id, port, kind }
                    if *id == input_id && *kind == UiEventKind::Input =>
                {
                    Some(*port)
                }
                _ => None,
            })
            .expect("input should expose Input");

        let update_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Hello".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let update_batch = runtime
            .decode_commands(update_descriptor)
            .expect("update batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &update_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert!(!tree_contains_text(root, "Empty"));
        assert!(tree_contains_text(root, "Hello"));
    }

    #[test]
    fn object_template_text_input_uses_runtime_text_binding_and_object_field_branch() {
        let semantic = lower_to_semantic(
            r#"
store: [
    source: LINK

    editing_text: LATEST {
        TEXT { Draft }
        store.source.event.change.text
    }

    rows: LIST {
        [
            title: TEXT { A1 }
            formula: Text/empty()
        ]
        [
            title: TEXT { A2 }
            formula: TEXT { =A1 }
        ]
    }
]

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [change: LINK]]
            style: []
            label: Hidden[text: TEXT { Source }]
            text: LATEST {
                TEXT { Draft }
                element.event.change.text
            }
            placeholder: []
            focus: False
        )
        |> LINK { store.source }

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.rows
            |> List/map(item, new: Element/text_input(
                element: []
                style: []
                label: Hidden[text: item.title]
                text: item.formula |> Text/is_empty() |> WHILE {
                    True => TEXT { {store.editing_text} }
                    False => item.formula
                }
                placeholder: []
                focus: False
            ))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let source_input_id = root.children[0].id;
        let rows_container = &root.children[1];
        assert_eq!(rows_container.children.len(), 2);
        let first_row_input_id = rows_container.children[0].id;
        let second_row_input_id = rows_container.children[1].id;

        assert_eq!(
            property_value(&init_batch, source_input_id, "value"),
            Some("Draft")
        );
        assert_eq!(
            property_value(&init_batch, first_row_input_id, "value"),
            Some("Draft")
        );
        assert_eq!(
            property_value(&init_batch, second_row_input_id, "value"),
            Some("=A1")
        );

        let source_input_port = init_batch
            .ops
            .iter()
            .find_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort { id, port, kind }
                    if *id == source_input_id && *kind == UiEventKind::Input =>
                {
                    Some(*port)
                }
                _ => None,
            })
            .expect("source input should expose Input");

        let update_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: source_input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Recalc".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let update_batch = runtime
            .decode_commands(update_descriptor)
            .expect("update batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &update_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let rows_container = &root.children[1];
        let first_row_input_id = rows_container.children[0].id;
        let second_row_input_id = rows_container.children[1].id;

        assert_eq!(
            property_value(&update_batch, first_row_input_id, "value"),
            Some("Recalc")
        );
        assert_eq!(
            property_value(&update_batch, second_row_input_id, "value"),
            Some("=A1")
        );
    }

    #[test]
    fn shopping_list_runtime_append_and_clear_updates_rendered_items() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input_id = root.children[1].id;
        let mut input_port = None;
        let mut key_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == input_id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                }
            }
        }
        let input_port = input_port.expect("shopping_list input should expose Input");
        let key_port = key_port.expect("shopping_list input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Milk".to_string()),
                }],
            }))
            .expect("input batch should decode");

        let append_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("keydown batch should decode");
        let append_batch = runtime
            .decode_commands(append_descriptor)
            .expect("append batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &append_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let count_node = &root.children[3].children[0];
        let UiNodeKind::Element { .. } = &count_node.kind else {
            panic!("expected count label");
        };
        let UiNodeKind::Text { text } = &count_node.children[0].kind else {
            panic!("expected count text");
        };
        assert_eq!(text, "1 items");

        let items_node = &root.children[2];
        let UiNodeKind::Element { .. } = &items_node.kind else {
            panic!("expected items_list stripe");
        };
        assert_eq!(items_node.children.len(), 1);
        let UiNodeKind::Element { .. } = &items_node.children[0].kind else {
            panic!("expected rendered item");
        };
        let UiNodeKind::Text { text } = &items_node.children[0].children[0].kind else {
            panic!("expected rendered item text");
        };
        assert_eq!(text, "- Milk");

        let clear_button_id = root.children[3].children[1].id;
        let mut clear_port = None;
        for op in &append_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == clear_button_id && *kind == UiEventKind::Click {
                    clear_port = Some(*port);
                }
            }
        }
        let clear_port =
            clear_port.expect("shopping_list clear button should expose Click after rerender");

        let clear_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: clear_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("clear batch should decode");
        let clear_batch = runtime
            .decode_commands(clear_descriptor)
            .expect("clear diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &clear_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let count_node = &root.children[3].children[0];
        let UiNodeKind::Text { text } = &count_node.children[0].kind else {
            panic!("expected count text after clear");
        };
        assert_eq!(text, "0 items");
        assert!(root.children[2].children.is_empty());
    }

    #[test]
    fn list_retain_count_runtime_updates_both_counts() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input_id = root.children[0].id;
        let mut input_port = None;
        let mut key_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == input_id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                }
            }
        }
        let input_port = input_port.expect("list_retain_count input should expose Input");
        let key_port = key_port.expect("list_retain_count input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Milk".to_string()),
                }],
            }))
            .expect("input batch should decode");

        let append_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("keydown batch should decode");
        let append_batch = runtime
            .decode_commands(append_descriptor)
            .expect("append batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &append_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let all_count_node = &root.children[1];
        let UiNodeKind::Text { text } = &all_count_node.children[0].kind else {
            panic!("expected all count text");
        };
        assert_eq!(text, "All count: 2");

        let retain_count_node = &root.children[2];
        let UiNodeKind::Text { text } = &retain_count_node.children[0].kind else {
            panic!("expected retain count text");
        };
        assert_eq!(text, "Retain count: 2");

        let items_node = &root.children[3];
        assert_eq!(items_node.children.len(), 2);
    }

    #[test]
    fn filtered_runtime_text_list_updates_filtered_count_and_items() {
        let semantic = lower_to_semantic(
            r#"
store: [
    input: LINK

    text_to_add: store.input.event.key_down.key |> WHEN {
        Enter => store.input.text
        __ => SKIP
    }

    items:
        LIST {
            1
            3
        }
        |> List/append(item: text_to_add)
]

document: Document/new(root: root_element(PASS: [store: store]))

FUNCTION root_element() {
    Element/stripe(
        element: []
        direction: Column
        gap: 10
        style: []

        items: LIST {
            Element/text_input(
                element: [event: [key_down: LINK, change: LINK]]
                style: []
                label: Hidden[text: TEXT { Add item }]

                text: LATEST {
                    Text/empty()
                    element.event.change.text
                }

                placeholder: [text: TEXT { Type number and press Enter }]
                focus: True
            )
            |> LINK { PASSED.store.input }

            Element/label(element: [], style: [], label: BLOCK {
                count: PASSED.store.items |> List/retain(item, if: item > 2) |> List/count()

                TEXT { Filtered count: {count} }
            })

            Element/stripe(
                element: []
                direction: Column
                gap: 5
                style: []

                items: PASSED.store.items
                |> List/retain(item, if: item > 2)
                |> List/map(item, new: Element/label(element: [], style: [], label: item))
            )
        }
    )
}
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected filtered count text");
        };
        assert_eq!(text, "Filtered count: 1");
        assert_eq!(root.children[2].children.len(), 1);
        let UiNodeKind::Text { text } = &root.children[2].children[0].children[0].kind else {
            panic!("expected initial filtered item text");
        };
        assert_eq!(text, "3");

        let input_id = root.children[0].id;
        let mut input_port = None;
        let mut key_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == input_id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                }
            }
        }
        let input_port = input_port.expect("filtered runtime list input should expose Input");
        let key_port = key_port.expect("filtered runtime list input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("2".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let append_two_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("keydown batch should decode");
        let append_two_batch = runtime
            .decode_commands(append_two_descriptor)
            .expect("first append batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &append_two_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected filtered count text after appending 2");
        };
        assert_eq!(text, "Filtered count: 1");
        assert_eq!(root.children[2].children.len(), 1);

        let input_id = root.children[0].id;
        let mut input_port = None;
        let mut key_port = None;
        for op in &append_two_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == input_id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                }
            }
        }
        let input_port =
            input_port.expect("rerendered filtered runtime list input should expose Input");
        let key_port =
            key_port.expect("rerendered filtered runtime list input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("5".to_string()),
                }],
            }))
            .expect("second input batch should decode");
        let append_five_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("second keydown batch should decode");
        let append_five_batch = runtime
            .decode_commands(append_five_descriptor)
            .expect("second append batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &append_five_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected filtered count text after appending 5");
        };
        assert_eq!(text, "Filtered count: 2");
        assert_eq!(root.children[2].children.len(), 2);
        let UiNodeKind::Text { text } = &root.children[2].children[0].children[0].kind else {
            panic!("expected first filtered item text");
        };
        assert_eq!(text, "3");
        let UiNodeKind::Text { text } = &root.children[2].children[1].children[0].kind else {
            panic!("expected second filtered item text");
        };
        assert_eq!(text, "5");
    }

    #[test]
    fn runtime_object_list_append_toggle_and_remove_updates_rendered_rows() {
        let semantic = lower_to_semantic(
            r#"
store: [
    input: LINK

    title_to_add: store.input.event.key_down.key |> WHEN {
        Enter => BLOCK {
            trimmed: store.input.text |> Text/trim()

            trimmed |> Text/is_not_empty() |> WHEN {
                True => trimmed
                False => SKIP
            }
        }
        __ => SKIP
    }

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/append(item: title_to_add |> new_todo())
        |> List/remove(item, on: item.remove_button.event.click)
]

FUNCTION new_todo(title) {
    [
        toggle_button: LINK
        remove_button: LINK
        title: title

        completed: False |> HOLD state {
            toggle_button.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [key_down: LINK, change: LINK]]
            style: []
            label: Hidden[text: TEXT { Add todo }]

            text: LATEST {
                Text/empty()
                element.event.change.text
            }

            placeholder: [text: TEXT { Type and press Enter }]
            focus: True
        )
        |> LINK { store.input }

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/count()

            TEXT { Count: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos |> List/map(item, new: Element/stripe(
                element: []
                direction: Row
                gap: 10
                style: []

                items: LIST {
                    Element/checkbox(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Toggle }
                        checked: item.completed

                        icon: item.completed |> WHEN {
                            True => TEXT { [X] }
                            False => TEXT { [ ] }
                        }
                    )
                    |> LINK { item.toggle_button }

                    Element/label(element: [], style: [], label: item.title)

                    Element/label(element: [], style: [], label: item.completed |> WHEN {
                        True => TEXT { (done) }
                        False => TEXT { (active) }
                    })

                    Element/button(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Remove }
                    )
                    |> LINK { item.remove_button }
                }
            ))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input_id = root.children[0].id;
        let list_node = &root.children[2];
        assert_eq!(list_node.children.len(), 2);
        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected count text");
        };
        assert_eq!(text, "Count: 2");
        let UiNodeKind::Text { text } = &list_node.children[0].children[1].children[0].kind else {
            panic!("expected first todo title");
        };
        assert_eq!(text, "Buy milk");

        let mut input_port = None;
        let mut key_port = None;
        let mut first_toggle_port = None;
        let mut second_remove_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == input_id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                } else if *id == list_node.children[0].children[0].id && *kind == UiEventKind::Click
                {
                    first_toggle_port = Some(*port);
                } else if *id == list_node.children[1].children[3].id && *kind == UiEventKind::Click
                {
                    second_remove_port = Some(*port);
                }
            }
        }
        let input_port = input_port.expect("input should expose Input");
        let key_port = key_port.expect("input should expose KeyDown");
        let first_toggle_port = first_toggle_port.expect("first checkbox should expose Click");
        let second_remove_port = second_remove_port.expect("second remove should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let list_node = &root.children[2];
        let UiNodeKind::Text { text } = &list_node.children[0].children[0].children[0].kind else {
            panic!("expected toggled checkbox icon");
        };
        assert_eq!(text, "[X]");
        let UiNodeKind::Text { text } = &list_node.children[0].children[2].children[0].kind else {
            panic!("expected toggled status text");
        };
        assert_eq!(text, "(done)");

        let mut input_port = None;
        let mut key_port = None;
        let mut second_remove_port = None;
        for op in &toggle_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[0].id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                } else if *id == root.children[2].children[1].children[3].id
                    && *kind == UiEventKind::Click
                {
                    second_remove_port = Some(*port);
                }
            }
        }
        let second_remove_port =
            second_remove_port.expect("rerendered second remove button should expose Click");

        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: second_remove_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove batch should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &remove_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected count text after remove");
        };
        assert_eq!(text, "Count: 1");
        assert_eq!(root.children[2].children.len(), 1);

        for op in &remove_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[0].id {
                    match kind {
                        UiEventKind::Input => input_port = Some(*port),
                        UiEventKind::KeyDown => key_port = Some(*port),
                        _ => {}
                    }
                }
            }
        }
        let input_port = input_port.expect("rerendered input should expose Input");
        let key_port = key_port.expect("rerendered input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Wash car".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let append_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("append batch should decode");
        let append_batch = runtime
            .decode_commands(append_descriptor)
            .expect("append diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &append_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected count text after append");
        };
        assert_eq!(text, "Count: 2");
        assert_eq!(root.children[2].children.len(), 2);
        let UiNodeKind::Text { text } = &root.children[2].children[1].children[1].children[0].kind
        else {
            panic!("expected appended todo title");
        };
        assert_eq!(text, "Wash car");
        let UiNodeKind::Text { text } = &root.children[2].children[1].children[2].children[0].kind
        else {
            panic!("expected appended todo status");
        };
        assert_eq!(text, "(active)");
    }

    #[test]
    fn incremental_diff_runtime_object_list_append_toggle_and_remove_preserves_survivor_identity() {
        let semantic = lower_to_semantic(
            r#"
store: [
    input: LINK

    title_to_add: store.input.event.key_down.key |> WHEN {
        Enter => BLOCK {
            trimmed: store.input.text |> Text/trim()

            trimmed |> Text/is_not_empty() |> WHEN {
                True => trimmed
                False => SKIP
            }
        }
        __ => SKIP
    }

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/append(item: title_to_add |> new_todo())
        |> List/remove(item, on: item.remove_button.event.click)
]

FUNCTION new_todo(title) {
    [
        toggle_button: LINK
        remove_button: LINK
        title: title

        completed: False |> HOLD state {
            toggle_button.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [key_down: LINK, change: LINK]]
            style: []
            label: Hidden[text: TEXT { Add todo }]

            text: LATEST {
                Text/empty()
                element.event.change.text
            }

            placeholder: [text: TEXT { Type and press Enter }]
            focus: True
        )
        |> LINK { store.input }

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/count()

            TEXT { Count: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos |> List/map(item, new: Element/stripe(
                element: []
                direction: Row
                gap: 10
                style: []

                items: LIST {
                    Element/checkbox(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Toggle }
                        checked: item.completed

                        icon: item.completed |> WHEN {
                            True => TEXT { [X] }
                            False => TEXT { [ ] }
                        }
                    )
                    |> LINK { item.toggle_button }

                    Element/label(element: [], style: [], label: item.title)

                    Element/label(element: [], style: [], label: item.completed |> WHEN {
                        True => TEXT { (done) }
                        False => TEXT { (active) }
                    })

                    Element/button(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Remove }
                    )
                    |> LINK { item.remove_button }
                }
            ))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);

        let input_id = init_root.children[0].id;
        let list_id = init_root.children[2].id;
        let first_row_id = init_root.children[2].children[0].id;
        let second_row_id = init_root.children[2].children[1].id;
        assert_eq!(init_root.children[2].children.len(), 2);

        let first_toggle_port = find_port_in_state(
            &state,
            init_root.children[2].children[0].children[0].id,
            UiEventKind::Click,
        )
        .expect("first checkbox should expose Click");
        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        assert!(
            toggle_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "toggle should be incremental",
        );
        let toggled_root = apply_batch_and_root(&mut state, &toggle_batch);
        assert_eq!(toggled_root.children[2].children[0].id, first_row_id);
        assert_eq!(toggled_root.children[2].children[1].id, second_row_id);
        let UiNodeKind::Text { text } =
            &toggled_root.children[2].children[0].children[2].children[0].kind
        else {
            panic!("expected toggled status text");
        };
        assert_eq!(text, "(done)");

        let second_remove_port = find_port_in_state(
            &state,
            toggled_root.children[2].children[1].children[3].id,
            UiEventKind::Click,
        )
        .expect("second remove button should expose Click");
        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: second_remove_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove batch should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove diff should decode");
        assert!(
            remove_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "remove should be incremental",
        );
        assert!(
            remove_batch.ops.iter().any(
                |op| matches!(op, boon_scene::RenderOp::RemoveNode { id } if *id == second_row_id)
            ),
            "remove should target the removed row's stable id",
        );
        let removed_root = apply_batch_and_root(&mut state, &remove_batch);
        assert_eq!(removed_root.children[2].children.len(), 1);
        assert_eq!(removed_root.children[2].children[0].id, first_row_id);

        let input_port = find_port_in_state(&state, input_id, UiEventKind::Input)
            .expect("input should expose Input");
        let key_port = find_port_in_state(&state, input_id, UiEventKind::KeyDown)
            .expect("input should expose KeyDown");
        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Wash car".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let append_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("append batch should decode");
        let append_batch = runtime
            .decode_commands(append_descriptor)
            .expect("append diff should decode");
        assert!(
            append_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "append should be incremental",
        );
        assert!(
            append_batch
                .ops
                .iter()
                .any(|op| matches!(op, boon_scene::RenderOp::InsertChild { parent, .. } if *parent == list_id)),
            "append should insert a new row into the list container",
        );
        let appended_root = apply_batch_and_root(&mut state, &append_batch);
        assert_eq!(appended_root.children[2].children.len(), 2);
        assert_eq!(appended_root.children[2].children[0].id, first_row_id);
        assert_ne!(appended_root.children[2].children[1].id, first_row_id);
        assert_ne!(appended_root.children[2].children[1].id, second_row_id);
        let UiNodeKind::Text { text } =
            &appended_root.children[2].children[1].children[1].children[0].kind
        else {
            panic!("expected appended todo title");
        };
        assert_eq!(text, "Wash car");
    }

    #[test]
    fn incremental_diff_nested_object_list_remove_and_append_preserve_parent_identity() {
        let mut first_row = object_list_item(100, "Row 1");
        first_row.object_lists.insert(
            "cells".to_string(),
            vec![object_list_item(101, "A1"), object_list_item(102, "B1")],
        );
        let mut second_row = object_list_item(200, "Row 2");
        second_row.object_lists.insert(
            "cells".to_string(),
            vec![object_list_item(201, "A2"), object_list_item(202, "B2")],
        );

        let exec = ExecProgram::from_semantic(&nested_object_list_program(vec![
            first_row.clone(),
            second_row.clone(),
        ]));
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        assert_eq!(
            direct_child_texts(&init_root),
            vec!["Row 1A1B1", "Row 2A2B2"]
        );

        let row_one_id = init_root.children[0].id;
        let row_two_id = init_root.children[1].id;
        let row_one_cells_id = init_root.children[0].children[1].id;
        let row_one_a_id = init_root.children[0].children[1].children[0].id;
        let row_one_b_id = init_root.children[0].children[1].children[1].id;
        let row_two_a_id = init_root.children[1].children[1].children[0].id;

        runtime
            .object_lists
            .get_mut("rows")
            .expect("rows should exist")[0]
            .object_lists
            .get_mut("cells")
            .expect("row one cells should exist")
            .remove(0);

        let remove_descriptor = runtime.render_active_program();
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove batch should decode");
        assert!(
            remove_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "nested remove should stay incremental",
        );
        assert!(
            remove_batch.ops.iter().any(
                |op| matches!(op, boon_scene::RenderOp::RemoveNode { id } if *id == row_one_a_id)
            ),
            "nested remove should target the removed child id",
        );
        let removed_root = apply_batch_and_root(&mut state, &remove_batch);
        assert_eq!(removed_root.children[0].id, row_one_id);
        assert_eq!(removed_root.children[1].id, row_two_id);
        assert_eq!(removed_root.children[0].children[1].id, row_one_cells_id);
        assert_eq!(removed_root.children[0].children[1].children.len(), 1);
        assert_eq!(
            removed_root.children[0].children[1].children[0].id,
            row_one_b_id
        );
        assert_eq!(
            removed_root.children[1].children[1].children[0].id,
            row_two_a_id
        );

        runtime
            .object_lists
            .get_mut("rows")
            .expect("rows should exist")[0]
            .object_lists
            .get_mut("cells")
            .expect("row one cells should exist")
            .push(object_list_item(103, "C1"));

        let append_descriptor = runtime.render_active_program();
        let append_batch = runtime
            .decode_commands(append_descriptor)
            .expect("append batch should decode");
        assert!(
            append_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "nested append should stay incremental",
        );
        assert!(
            append_batch.ops.iter().any(|op| matches!(
                op,
                boon_scene::RenderOp::InsertChild { parent, index, .. }
                    if *parent == row_one_cells_id && *index == 1
            )),
            "nested append should insert into the row one cells container",
        );
        let appended_root = apply_batch_and_root(&mut state, &append_batch);
        assert_eq!(appended_root.children[0].id, row_one_id);
        assert_eq!(appended_root.children[1].id, row_two_id);
        assert_eq!(appended_root.children[0].children[1].id, row_one_cells_id);
        assert_eq!(
            appended_root.children[0].children[1].children[0].id,
            row_one_b_id
        );
        assert_eq!(
            appended_root.children[1].children[1].children[0].id,
            row_two_a_id
        );
        assert_eq!(
            direct_child_texts(&appended_root.children[0].children[1]),
            vec!["B1", "C1"]
        );
        assert_ne!(
            appended_root.children[0].children[1].children[1].id,
            row_one_a_id
        );
        assert_ne!(
            appended_root.children[0].children[1].children[1].id,
            row_one_b_id
        );
    }

    #[test]
    fn helper_filtered_object_list_counts_and_rows_follow_toggle() {
        let semantic = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        new_todo(title: TEXT { Buy milk })
        new_todo(title: TEXT { Clean room })
    }
]

FUNCTION new_todo(title) {
    [
        toggle_button: LINK
        title: title

        completed: False |> HOLD state {
            toggle_button.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

FUNCTION todo_row(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/checkbox(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Toggle }
                checked: todo.completed

                icon: todo.completed |> WHEN {
                    True => TEXT { [X] }
                    False => TEXT { [ ] }
                }
            )
            |> LINK { todo.toggle_button }

            Element/label(element: [], style: [], label: todo.title)
        }
    )
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/retain(item, if: item.completed) |> List/count()

            TEXT { Completed: {count} }
        })

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/retain(item, if: item.completed |> Bool/not()) |> List/count()

            TEXT { Active: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos |> List/map(item, new: todo_row(todo: item))
        )

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos
            |> List/retain(item, if: item.completed)
            |> List/map(item, new: todo_row(todo: item))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let UiNodeKind::Text { text } = &root.children[0].children[0].kind else {
            panic!("expected completed count text");
        };
        assert_eq!(text, "Completed: 0");
        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected active count text");
        };
        assert_eq!(text, "Active: 2");
        assert_eq!(root.children[2].children.len(), 2);
        assert!(root.children[3].children.is_empty());

        let mut toggle_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *kind == UiEventKind::Click && *id == root.children[2].children[0].children[0].id
                {
                    toggle_port = Some(*port);
                    break;
                }
            }
        }
        let toggle_port = toggle_port.expect("first toggle should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let UiNodeKind::Text { text } = &root.children[0].children[0].kind else {
            panic!("expected completed count after toggle");
        };
        assert_eq!(text, "Completed: 1");
        let UiNodeKind::Text { text } = &root.children[1].children[0].kind else {
            panic!("expected active count after toggle");
        };
        assert_eq!(text, "Active: 1");
        assert_eq!(root.children[3].children.len(), 1);
        let UiNodeKind::Text { text } = &root.children[3].children[0].children[1].children[0].kind
        else {
            panic!("expected completed row title");
        };
        assert_eq!(text, "Buy milk");
        let UiNodeKind::Text { text } = &root.children[3].children[0].children[0].children[0].kind
        else {
            panic!("expected completed row checkbox icon");
        };
        assert_eq!(text, "[X]");
    }

    #[test]
    fn object_list_bulk_toggle_and_remove_completed_updates_rows() {
        let semantic = lower_to_semantic(
            r#"
store: [
    elements: [toggle_all: LINK, remove_completed: LINK]

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/remove(item, on: elements.remove_completed.event.press |> THEN {
            item.completed |> WHEN {
                True => []
                False => SKIP
            }
        })
]

FUNCTION new_todo(title) {
    [
        todo_elements: [todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            LATEST {
                todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
                store.elements.toggle_all.event.click |> THEN { state |> Bool/not() }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [click: LINK]], style: [], label: TEXT { Toggle all })
        |> LINK { store.elements.toggle_all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Remove completed })
        |> LINK { store.elements.remove_completed }

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos |> List/map(item, new: Element/stripe(
                element: []
                direction: Row
                gap: 10
                style: []

                items: LIST {
                    Element/checkbox(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Toggle }
                        checked: item.completed

                        icon: item.completed |> WHEN {
                            True => TEXT { [X] }
                            False => TEXT { [ ] }
                        }
                    )
                    |> LINK { item.todo_elements.todo_checkbox }

                    Element/label(element: [], style: [], label: item.title)
                }
            ))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children[2].children.len(), 2);
        let mut toggle_all_port = None;
        let mut remove_completed_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[0].id && *kind == UiEventKind::Click {
                    toggle_all_port = Some(*port);
                } else if *id == root.children[1].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let toggle_all_port = toggle_all_port.expect("toggle_all should expose Click");
        let remove_completed_port =
            remove_completed_port.expect("remove_completed should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle_all batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle_all diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        for row in &root.children[2].children {
            let UiNodeKind::Text { text } = &row.children[0].children[0].kind else {
                panic!("expected toggled checkbox icon");
            };
            assert_eq!(text, "[X]");
        }

        let mut remove_completed_port = None;
        for op in &toggle_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[1].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let remove_completed_port =
            remove_completed_port.expect("rerendered remove_completed should expose Click");

        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove_completed batch should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove_completed diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &remove_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(root.children[2].children.is_empty());
    }

    #[test]
    fn incremental_diff_filtered_object_list_preserves_row_identity_across_filter_switches() {
        let semantic = lower_to_semantic(
            r#"
store: [
    elements: [
        filter_buttons: [all: LINK, active: LINK, completed: LINK]
        remove_completed_button: LINK
        toggle_all_checkbox: LINK
    ]

    navigation_result:
        LATEST {
            elements.filter_buttons.all.event.press |> THEN { TEXT { / } }
            elements.filter_buttons.active.event.press |> THEN { TEXT { /active } }
            elements.filter_buttons.completed.event.press |> THEN { TEXT { /completed } }
        }
        |> Router/go_to()

    selected_filter: Router/route() |> WHILE {
        TEXT { / } => All
        TEXT { /active } => Active
        TEXT { /completed } => Completed
        __ => All
    }

    todos:
        LIST {
            new_todo(title: TEXT { Buy groceries })
            new_todo(title: TEXT { Clean room })
        }
]

FUNCTION new_todo(title) {
    [
        todo_elements: [todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

FUNCTION todo_item(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/checkbox(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Toggle }
                checked: todo.completed

                icon: todo.completed |> WHEN {
                    True => TEXT { [X] }
                    False => TEXT { [ ] }
                }
            )
            |> LINK { todo.todo_elements.todo_checkbox }

            Element/label(element: [], style: [], label: todo.title)
        }
    )
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { All })
        |> LINK { store.elements.filter_buttons.all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Active })
        |> LINK { store.elements.filter_buttons.active }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Completed })
        |> LINK { store.elements.filter_buttons.completed }

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items:
                store.todos
                |> List/retain(item, if: store.selected_filter |> WHILE {
                    All => True
                    Active => item.completed |> Bool/not()
                    Completed => item.completed
                })
                |> List/map(item, new: todo_item(todo: item))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);

        let all_button_id = init_root.children[0].id;
        let active_button_id = init_root.children[1].id;
        let completed_button_id = init_root.children[2].id;
        let list_id = init_root.children[3].id;
        let buy_row_id = init_root.children[3].children[0].id;
        let clean_row_id = init_root.children[3].children[1].id;
        assert_eq!(
            direct_child_texts(&init_root.children[3]),
            vec!["[ ]Buy groceries", "[ ]Clean room"]
        );

        let first_checkbox_port = find_port_in_state(
            &state,
            init_root.children[3].children[0].children[0].id,
            UiEventKind::Click,
        )
        .expect("first checkbox should expose Click");
        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_checkbox_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        let toggled_root = apply_batch_and_root(&mut state, &toggle_batch);
        assert_eq!(
            direct_child_texts(&toggled_root.children[3]),
            vec!["[X]Buy groceries", "[ ]Clean room"]
        );
        assert_eq!(toggled_root.children[3].children[0].id, buy_row_id);
        assert_eq!(toggled_root.children[3].children[1].id, clean_row_id);

        let active_port = find_port_in_state(&state, active_button_id, UiEventKind::Click)
            .expect("active button should expose Click");
        let active_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: active_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("active filter batch should decode");
        let active_batch = runtime
            .decode_commands(active_descriptor)
            .expect("active filter diff should decode");
        assert!(
            active_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "active filter should be incremental",
        );
        assert!(
            active_batch.ops.iter().any(
                |op| matches!(op, boon_scene::RenderOp::RemoveNode { id } if *id == buy_row_id)
            ),
            "active filter should remove the completed row by stable id",
        );
        let active_root = apply_batch_and_root(&mut state, &active_batch);
        assert_eq!(active_root.children[3].id, list_id);
        assert_eq!(
            direct_child_texts(&active_root.children[3]),
            vec!["[ ]Clean room"]
        );
        assert_eq!(active_root.children[3].children[0].id, clean_row_id);

        let completed_port = find_port_in_state(&state, completed_button_id, UiEventKind::Click)
            .expect("completed button should expose Click");
        let completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("completed filter batch should decode");
        let completed_batch = runtime
            .decode_commands(completed_descriptor)
            .expect("completed filter diff should decode");
        assert!(
            completed_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "completed filter should be incremental",
        );
        assert!(
            completed_batch
                .ops
                .iter()
                .any(|op| matches!(op, boon_scene::RenderOp::InsertChild { parent, .. } if *parent == list_id)),
            "completed filter should insert the completed row into the filtered list",
        );
        let completed_root = apply_batch_and_root(&mut state, &completed_batch);
        assert_eq!(completed_root.children[3].id, list_id);
        assert_eq!(
            direct_child_texts(&completed_root.children[3]),
            vec!["[X]Buy groceries"]
        );
        assert_eq!(completed_root.children[3].children[0].id, buy_row_id);

        let all_port = find_port_in_state(&state, all_button_id, UiEventKind::Click)
            .expect("all button should expose Click");
        let all_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("all filter batch should decode");
        let all_batch = runtime
            .decode_commands(all_descriptor)
            .expect("all filter diff should decode");
        let all_root = apply_batch_and_root(&mut state, &all_batch);
        assert_eq!(
            direct_child_texts(&all_root.children[3]),
            vec!["[X]Buy groceries", "[ ]Clean room"]
        );
        assert_eq!(all_root.children[3].children[0].id, buy_row_id);
        assert_eq!(all_root.children[3].children[1].id, clean_row_id);
    }

    #[test]
    fn router_selected_filter_object_list_view_updates_rows_and_bulk_actions() {
        let semantic = lower_to_semantic(
            r#"
store: [
    elements: [
        filter_buttons: [all: LINK, active: LINK, completed: LINK]
        remove_completed_button: LINK
        toggle_all_checkbox: LINK
    ]

    navigation_result:
        LATEST {
            elements.filter_buttons.all.event.press |> THEN { TEXT { / } }
            elements.filter_buttons.active.event.press |> THEN { TEXT { /active } }
            elements.filter_buttons.completed.event.press |> THEN { TEXT { /completed } }
        }
        |> Router/go_to()

    selected_filter: Router/route() |> WHILE {
        TEXT { / } => All
        TEXT { /active } => Active
        TEXT { /completed } => Completed
        __ => All
    }

    todos:
        LIST {
            new_todo(title: TEXT { Buy groceries })
            new_todo(title: TEXT { Clean room })
        }
        |> List/remove(item, on: item.todo_elements.remove_todo_button.event.press)
        |> List/remove(item, on: elements.remove_completed_button.event.press |> THEN {
            item.completed |> WHEN {
                True => []
                False => SKIP
            }
        })
]

FUNCTION new_todo(title) {
    [
        todo_elements: [remove_todo_button: LINK, todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            LATEST {
                todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
                store.elements.toggle_all_checkbox.event.click |> THEN { state |> Bool/not() }
            }
        }
    ]
}

FUNCTION todo_item(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/checkbox(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Toggle }
                checked: todo.completed

                icon: todo.completed |> WHEN {
                    True => TEXT { [X] }
                    False => TEXT { [ ] }
                }
            )
            |> LINK { todo.todo_elements.todo_checkbox }

            Element/label(element: [], style: [], label: todo.title)

            Element/button(
                element: [event: [press: LINK]]
                style: []
                label: TEXT { Remove }
            )
            |> LINK { todo.todo_elements.remove_todo_button }
        }
    )
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { All })
        |> LINK { store.elements.filter_buttons.all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Active })
        |> LINK { store.elements.filter_buttons.active }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Completed })
        |> LINK { store.elements.filter_buttons.completed }

        Element/button(element: [event: [click: LINK]], style: [], label: TEXT { Toggle all })
        |> LINK { store.elements.toggle_all_checkbox }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Remove completed })
        |> LINK { store.elements.remove_completed_button }

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/retain(item, if: store.selected_filter |> WHILE {
                All => True
                Active => item.completed |> Bool/not()
                Completed => item.completed
            }) |> List/count()

            TEXT { Visible: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items:
                store.todos
                |> List/retain(item, if: store.selected_filter |> WHILE {
                    All => True
                    Active => item.completed |> Bool/not()
                    Completed => item.completed
                })
                |> List/map(item, new: todo_item(todo: item))
        )
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(
            ui_text_content(&root.children[5].children[0]),
            Some("Visible: 2")
        );
        assert_eq!(root.children[6].children.len(), 2);

        let mut active_port = None;
        let mut completed_port = None;
        let mut toggle_all_port = None;
        let mut remove_completed_port = None;
        let mut first_checkbox_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[1].id && *kind == UiEventKind::Click {
                    active_port = Some(*port);
                } else if *id == root.children[2].id && *kind == UiEventKind::Click {
                    completed_port = Some(*port);
                } else if *id == root.children[3].id && *kind == UiEventKind::Click {
                    toggle_all_port = Some(*port);
                } else if *id == root.children[4].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                } else if *id == root.children[6].children[0].children[0].id
                    && *kind == UiEventKind::Click
                {
                    first_checkbox_port = Some(*port);
                }
            }
        }
        let active_port = active_port.expect("active filter should expose Click");
        let completed_port = completed_port.expect("completed filter should expose Click");
        let toggle_all_port = toggle_all_port.expect("toggle_all should expose Click");
        let remove_completed_port =
            remove_completed_port.expect("remove_completed should expose Click");
        let first_checkbox_port = first_checkbox_port.expect("first checkbox should expose Click");

        let toggle_one_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: first_checkbox_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("first toggle batch should decode");
        let toggle_one_batch = runtime
            .decode_commands(toggle_one_descriptor)
            .expect("first toggle diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_one_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert_eq!(
            ui_text_content(&root.children[5].children[0]),
            Some("Visible: 2")
        );
        assert_eq!(root.children[6].children.len(), 2);
        let mut active_port = None;
        let mut completed_port = None;
        let mut toggle_all_port = None;
        let mut remove_completed_port = None;
        for op in &toggle_one_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[1].id && *kind == UiEventKind::Click {
                    active_port = Some(*port);
                } else if *id == root.children[2].id && *kind == UiEventKind::Click {
                    completed_port = Some(*port);
                } else if *id == root.children[3].id && *kind == UiEventKind::Click {
                    toggle_all_port = Some(*port);
                } else if *id == root.children[4].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let active_port = active_port.expect("rerendered active filter should expose Click");
        let completed_port =
            completed_port.expect("rerendered completed filter should expose Click");
        let toggle_all_port = toggle_all_port.expect("rerendered toggle_all should expose Click");
        let remove_completed_port =
            remove_completed_port.expect("rerendered remove_completed should expose Click");

        let active_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: active_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("active filter batch should decode");
        let active_batch = runtime
            .decode_commands(active_descriptor)
            .expect("active filter diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &active_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert_eq!(
            ui_text_content(&root.children[5].children[0]),
            Some("Visible: 1")
        );
        assert_eq!(root.children[6].children.len(), 1);
        assert_eq!(
            ui_text_content(&root.children[6].children[0].children[1].children[0]),
            Some("Clean room")
        );
        let mut completed_port = None;
        for op in &active_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[2].id && *kind == UiEventKind::Click {
                    completed_port = Some(*port);
                }
            }
        }
        let completed_port =
            completed_port.expect("active rerender should expose completed filter Click");

        let completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("completed filter batch should decode");
        let completed_batch = runtime
            .decode_commands(completed_descriptor)
            .expect("completed filter diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &completed_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert_eq!(
            ui_text_content(&root.children[5].children[0]),
            Some("Visible: 1")
        );
        assert_eq!(root.children[6].children.len(), 1);
        assert_eq!(
            ui_text_content(&root.children[6].children[0].children[1].children[0]),
            Some("Buy groceries")
        );
        let mut toggle_all_port = None;
        let mut remove_completed_port = None;
        for op in &completed_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[3].id && *kind == UiEventKind::Click {
                    toggle_all_port = Some(*port);
                } else if *id == root.children[4].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let toggle_all_port =
            toggle_all_port.expect("completed rerender should expose toggle_all Click");
        let remove_completed_port =
            remove_completed_port.expect("completed rerender should expose remove_completed Click");

        let toggle_all_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle_all batch should decode");
        let toggle_all_batch = runtime
            .decode_commands(toggle_all_descriptor)
            .expect("toggle_all diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_all_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert_eq!(
            ui_text_content(&root.children[5].children[0]),
            Some("Visible: 2")
        );
        assert_eq!(root.children[6].children.len(), 2);
        let mut remove_completed_port = None;
        for op in &toggle_all_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[4].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let remove_completed_port = remove_completed_port
            .expect("toggle_all rerender should expose remove_completed Click");

        let remove_completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove_completed batch should decode");
        let remove_completed_batch = runtime
            .decode_commands(remove_completed_descriptor)
            .expect("remove_completed diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &remove_completed_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert_eq!(
            ui_text_content(&root.children[5].children[0]),
            Some("Visible: 0")
        );
        assert!(root.children[6].children.is_empty());
    }

    #[test]
    fn list_is_empty_ui_branch_hides_panel_after_remove_completed() {
        let semantic = lower_to_semantic(
            r#"
store: [
    elements: [toggle_all: LINK, remove_completed: LINK]

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/remove(item, on: elements.remove_completed.event.press |> THEN {
            item.completed |> WHEN {
                True => []
                False => SKIP
            }
        })
]

FUNCTION new_todo(title) {
    [
        todo_elements: [todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            LATEST {
                todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
                store.elements.toggle_all.event.click |> THEN { state |> Bool/not() }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [click: LINK]], style: [], label: TEXT { Toggle all })
        |> LINK { store.elements.toggle_all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Remove completed })
        |> LINK { store.elements.remove_completed }

        store.todos |> List/is_empty() |> WHILE {
            True => NoElement
            False => Element/label(element: [], style: [], label: TEXT { Has todos })
        }
    }
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children.len(), 3);
        assert_eq!(
            ui_text_content(&root.children[2].children[0]),
            Some("Has todos")
        );

        let mut toggle_all_port = None;
        let mut remove_completed_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[0].id && *kind == UiEventKind::Click {
                    toggle_all_port = Some(*port);
                } else if *id == root.children[1].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let toggle_all_port = toggle_all_port.expect("toggle_all should expose Click");
        let remove_completed_port =
            remove_completed_port.expect("remove_completed should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle_all batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle_all diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert_eq!(root.children.len(), 3);

        let mut remove_completed_port = None;
        for op in &toggle_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == root.children[1].id && *kind == UiEventKind::Click {
                    remove_completed_port = Some(*port);
                }
            }
        }
        let remove_completed_port =
            remove_completed_port.expect("rerendered remove_completed should expose Click");

        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove_completed batch should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove_completed diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &remove_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(root.children.len(), 2);
    }

    #[test]
    fn object_item_editing_branch_updates_on_double_click_and_escape() {
        let semantic = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        new_todo(title: TEXT { Buy milk })
    }
]

FUNCTION new_todo(title) {
    [
        todo_elements: [editing_input: LINK, todo_title: LINK]
        title: title

        editing: False |> HOLD state {
            LATEST {
                todo_elements.todo_title.event.double_click |> THEN { True }
                todo_elements.editing_input.event.blur |> THEN { False }
                todo_elements.editing_input.event.key_down.key |> WHEN {
                    Enter => False
                    Escape => False
                    __ => SKIP
                }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.todos |> List/map(item, new: item.editing |> WHILE {
        True =>
            Element/label(element: [event: [blur: LINK, key_down: LINK]], style: [], label: TEXT { Editing })
            |> LINK { item.todo_elements.editing_input }

        False =>
            Element/label(element: [event: [double_click: LINK]], style: [], label: item.title)
            |> LINK { item.todo_elements.todo_title }
    })
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(
            ui_text_content(&root.children[0].children[0]),
            Some("Buy milk")
        );

        let title_id = root.children[0].id;
        let mut double_click_port = None;
        for op in &init_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == title_id && *kind == UiEventKind::DoubleClick {
                    double_click_port = Some(*port);
                }
            }
        }
        let double_click_port = double_click_port.expect("todo title should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double_click batch should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("editing diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(
            ui_text_content(&root.children[0].children[0]),
            Some("Editing")
        );

        let editing_id = root.children[0].id;
        let mut escape_port = None;
        for op in &edit_batch.ops {
            if let boon_scene::RenderOp::AttachEventPort { id, port, kind } = op {
                if *id == editing_id && *kind == UiEventKind::KeyDown {
                    escape_port = Some(*port);
                }
            }
        }
        let escape_port = escape_port.expect("editing input should expose KeyDown");

        let exit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: escape_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Escape".to_string()),
                }],
            }))
            .expect("escape batch should decode");
        let exit_batch = runtime
            .decode_commands(exit_descriptor)
            .expect("exit editing diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &exit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(
            ui_text_content(&root.children[0].children[0]),
            Some("Buy milk")
        );
    }

    #[test]
    fn object_item_title_updates_on_edit_input_and_enter() {
        let semantic = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        new_todo(title: TEXT { Buy milk })
    }
]

FUNCTION new_todo(title) {
    [
        todo_elements: [editing_input: LINK, todo_title: LINK]

        title: LATEST {
            title

            todo_elements.editing_input.event.change.text |> WHEN {
                changed_text =>
                    changed_text
                    |> Text/is_not_empty()
                    |> WHEN {
                        True => changed_text
                        False => SKIP
                    }
            }
        }

        editing: False |> HOLD state {
            LATEST {
                todo_elements.todo_title.event.double_click |> THEN { True }
                todo_elements.editing_input.event.key_down.key |> WHEN {
                    Enter => False
                    Escape => False
                    __ => SKIP
                }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.todos |> List/map(item, new: item.editing |> WHILE {
        True =>
            Element/text_input(
                element: [event: [change: LINK, key_down: LINK]]
                style: []
                label: Hidden[text: TEXT { Editing }]
                text: item.title
                placeholder: []
                focus: True
            )
            |> LINK { item.todo_elements.editing_input }

        False =>
            Element/label(
                element: [event: [double_click: LINK]]
                style: []
                label: item.title
            )
            |> LINK { item.todo_elements.todo_title }
    })
))
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let title =
            find_first_tag_with_text(root, "span", "Buy milk").expect("todo title should render");
        let double_click_port = find_port(&init_batch, title.id, UiEventKind::DoubleClick)
            .expect("todo title should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double_click batch should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("editing diff should decode");

        let input_port = find_input_without_focus_port(&edit_batch, UiEventKind::Input)
            .expect("editing input should expose Input");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Buy milk now".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let input_batch = runtime
            .decode_commands(input_descriptor)
            .expect("input diff should decode");

        let enter_port = find_input_without_focus_port(&input_batch, UiEventKind::KeyDown)
            .expect("editing input should expose KeyDown");

        let exit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: enter_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter batch should decode");
        let exit_batch = runtime
            .decode_commands(exit_descriptor)
            .expect("exit diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &exit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Buy milk now"));
        assert!(!tree_contains_text(root, "Buy milk"));
    }

    #[test]
    fn todo_mvc_real_file_runtime_add_toggle_filter_and_remove_completed() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(
            subtree_text(root).contains("2 item left"),
            "expected two physical items after add, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );
        assert!(
            tree_contains_text(root, "Buy groceries"),
            "expected Buy groceries after add, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );
        assert!(
            tree_contains_text(root, "Clean room"),
            "expected Clean room after add, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );

        let input = find_first_tag(root, "input").expect("todo_mvc should render an input");
        let input_port = find_port(&init_batch, input.id, UiEventKind::Input)
            .expect("input should expose Input");
        let key_port = find_port(&init_batch, input.id, UiEventKind::KeyDown)
            .expect("input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Write tests".to_string()),
                }],
            }))
            .expect("input event should decode");

        let add_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("add event should decode");
        let add_batch = runtime
            .decode_commands(add_descriptor)
            .expect("add batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "3 items left"));
        assert!(tree_contains_text(root, "Write tests"));

        let groceries_row = find_first_tag_with_text(root, "div", "Buy groceries")
            .expect("first todo row should be present");
        let groceries_toggle = find_first_tag(groceries_row, "button")
            .expect("todo row should render a checkbox button");
        let groceries_toggle_port = find_port(&add_batch, groceries_toggle.id, UiEventKind::Click)
            .expect("todo checkbox should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: groceries_toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle event should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "2 items left"));

        let completed_button = find_first_tag_with_text(root, "button", "Completed")
            .expect("Completed filter button should render");
        let completed_port = find_port(&toggle_batch, completed_button.id, UiEventKind::Click)
            .expect("Completed filter should expose Click");

        let completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("completed filter event should decode");
        let completed_batch = runtime
            .decode_commands(completed_descriptor)
            .expect("completed filter batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &completed_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Buy groceries"));
        assert!(!tree_contains_text(root, "Clean room"));
        assert!(!tree_contains_text(root, "Write tests"));

        let remove_completed_button = find_first_tag_with_text(root, "button", "Clear completed")
            .expect("remove completed button should be visible");
        let remove_completed_port = find_port(
            &completed_batch,
            remove_completed_button.id,
            UiEventKind::Click,
        )
        .expect("remove completed should expose Click");

        let remove_completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove completed event should decode");
        let remove_completed_batch = runtime
            .decode_commands(remove_completed_descriptor)
            .expect("remove completed batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &remove_completed_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(!tree_contains_text(root, "Buy groceries"));
        assert!(!tree_contains_text(root, "Clean room"));
        assert!(!tree_contains_text(root, "Write tests"));
        assert!(tree_contains_text(root, "2 items left"));

        let all_button = find_first_tag_with_text(root, "button", "All")
            .expect("All filter button should render");
        let all_port = find_port(&remove_completed_batch, all_button.id, UiEventKind::Click)
            .expect("All filter should expose Click");

        let all_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("all filter event should decode");
        let all_batch = runtime
            .decode_commands(all_descriptor)
            .expect("all filter batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &all_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(!tree_contains_text(root, "Buy groceries"));
        assert!(tree_contains_text(root, "Clean room"));
        assert!(tree_contains_text(root, "Write tests"));
        assert!(tree_contains_text(root, "2 items left"));
    }

    #[test]
    fn todo_mvc_physical_runtime_checkbox_toggle_decrements_counter() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("physical todo_mvc should render input");
        let input_port = find_port(&init_batch, input.id, UiEventKind::Input)
            .expect("physical input should expose Input");
        let key_port = find_port(&init_batch, input.id, UiEventKind::KeyDown)
            .expect("physical input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Buy groceries".to_string()),
                }],
            }))
            .expect("first physical input event should decode");

        let add_first_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("first physical add event should decode");
        let add_first_batch = runtime
            .decode_commands(add_first_descriptor)
            .expect("first physical add batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_first_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("physical todo_mvc should rerender input");
        let input_port = find_port(&add_first_batch, input.id, UiEventKind::Input)
            .expect("physical input should expose Input after first add");
        let key_port = find_port(&add_first_batch, input.id, UiEventKind::KeyDown)
            .expect("physical input should expose KeyDown after first add");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Clean room".to_string()),
                }],
            }))
            .expect("second physical input event should decode");

        let add_second_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("second physical add event should decode");
        let add_second_batch = runtime
            .decode_commands(add_second_descriptor)
            .expect("second physical add batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_second_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(
            subtree_text(root).contains("2 item left"),
            "expected two physical items after add, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );
        assert!(
            tree_contains_text(root, "Buy groceries"),
            "expected Buy groceries after add, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );
        assert!(
            tree_contains_text(root, "Clean room"),
            "expected Clean room after add, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );

        let groceries_row = find_first_tag_with_text(root, "div", "Buy groceries")
            .expect("first physical todo row should be present");
        let groceries_toggle = find_first_tag(groceries_row, "button")
            .expect("physical todo row should render a checkbox button");
        let groceries_toggle_port =
            find_port(&add_second_batch, groceries_toggle.id, UiEventKind::Click)
                .expect("physical todo checkbox should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: groceries_toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("physical toggle event should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("physical toggle batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(
            subtree_text(root).contains("1 item left"),
            "expected physical toggle to decrement remaining count, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );
    }

    #[test]
    fn todo_mvc_physical_runtime_active_filter_click_updates_selected_filter() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("physical todo_mvc should render input");
        let input_port = find_port(&init_batch, input.id, UiEventKind::Input)
            .expect("physical input should expose Input");
        let key_port = find_port(&init_batch, input.id, UiEventKind::KeyDown)
            .expect("physical input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Buy groceries".to_string()),
                }],
            }))
            .expect("first physical input event should decode");
        let add_first_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("first physical add event should decode");
        let add_first_batch = runtime
            .decode_commands(add_first_descriptor)
            .expect("first physical add batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_first_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("physical todo_mvc should rerender input");
        let input_port = find_port(&add_first_batch, input.id, UiEventKind::Input)
            .expect("physical input should expose Input after first add");
        let key_port = find_port(&add_first_batch, input.id, UiEventKind::KeyDown)
            .expect("physical input should expose KeyDown after first add");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Clean room".to_string()),
                }],
            }))
            .expect("second physical input event should decode");
        let add_second_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("second physical add event should decode");
        let add_second_batch = runtime
            .decode_commands(add_second_descriptor)
            .expect("second physical add batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_second_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let groceries_row = find_first_tag_with_text(root, "div", "Buy groceries")
            .expect("first physical todo row should be present");
        let groceries_toggle = find_first_tag(groceries_row, "button")
            .expect("physical todo row should render a checkbox button");
        let groceries_toggle_port =
            find_port(&add_second_batch, groceries_toggle.id, UiEventKind::Click)
                .expect("physical todo checkbox should expose Click");
        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: groceries_toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("physical toggle event should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("physical toggle batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let active_button = find_first_tag_with_text(root, "button", "Active")
            .expect("Active filter button should be present");
        let active_port = find_port(&toggle_batch, active_button.id, UiEventKind::Click)
            .expect("Active filter should expose Click");
        let active_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: active_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("Active filter click should decode");
        let active_batch = runtime
            .decode_commands(active_descriptor)
            .expect("Active filter batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &active_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert_eq!(
            runtime.scalar_values.get("store.selected_filter"),
            Some(&1),
            "expected Active click to set selected_filter=1, debug: {}",
            runtime.debug_snapshot(),
        );
        assert!(
            !tree_contains_text(root, "Buy groceries") && tree_contains_text(root, "Clean room"),
            "expected Active filter to render only the remaining row, tree text: {} debug: {}",
            subtree_text(root),
            runtime.debug_snapshot(),
        );
    }

    #[test]
    fn cells_real_file_runtime_initializes_grid_headers_and_rows() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Cells"));
        assert!(tree_contains_text(root, "A"));
        assert!(tree_contains_text(root, "Z"));
        assert!(tree_contains_text(root, "1"));
        assert!(tree_contains_text(root, "100"));
        assert_eq!(
            nth_port_target_text(root, &init_batch, UiEventKind::DoubleClick, 0).as_deref(),
            Some("5")
        );
        assert_eq!(
            nth_port_target_text(root, &init_batch, UiEventKind::DoubleClick, 1).as_deref(),
            Some("15")
        );
        assert_eq!(
            nth_port_target_text(root, &init_batch, UiEventKind::DoubleClick, 2).as_deref(),
            Some("30")
        );
        assert_eq!(
            nth_port_target_text(root, &init_batch, UiEventKind::DoubleClick, 26).as_deref(),
            Some("10")
        );
        let double_click_ports = init_batch
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    boon_scene::RenderOp::AttachEventPort {
                        kind: UiEventKind::DoubleClick,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            double_click_ports,
            26 * 100,
            "cells should expose one DoubleClick port per grid cell"
        );
    }

    #[test]
    fn cells_real_file_runtime_exposes_row_100_and_last_cell_ports() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "100"));
        assert_eq!(
            nth_port_target_text(root, &init_batch, UiEventKind::DoubleClick, 2599).as_deref(),
            Some("")
        );
    }

    #[test]
    fn cells_real_file_double_click_enters_edit_mode() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let first_cell = runtime
            .object_lists
            .get("all_row_cells")
            .and_then(|rows| rows.first())
            .and_then(|row| row.object_lists.get("cells"))
            .and_then(|cells| cells.first())
            .cloned()
            .expect("expected first cells item");
        assert_eq!(
            runtime.object_item_field_text_value(&first_cell, "input_text"),
            "5"
        );
        assert_eq!(
            runtime.render_object_input_value(
                &first_cell,
                &SemanticInputValue::TextParts {
                    parts: vec![SemanticTextPart::ObjectFieldBinding(
                        "input_text".to_string()
                    )],
                    value: String::new(),
                },
            ),
            "5"
        );
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("edit mode should render input");
        assert_eq!(property_value(&edit_batch, input.id, "value"), Some("5"));
    }

    #[test]
    fn cells_real_file_change_and_enter_commits_value() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let first_cell = runtime
            .object_lists
            .get("all_row_cells")
            .and_then(|rows| rows.first())
            .and_then(|row| row.object_lists.get("cells"))
            .and_then(|cells| cells.first())
            .cloned()
            .expect("expected first cells item");
        assert_eq!(
            runtime.object_item_field_text_value(&first_cell, "input_text"),
            "5"
        );
        assert_eq!(
            runtime.render_object_input_value(
                &first_cell,
                &SemanticInputValue::TextParts {
                    parts: vec![SemanticTextPart::ObjectFieldBinding(
                        "input_text".to_string()
                    )],
                    value: String::new(),
                },
            ),
            "5"
        );

        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 3)
            .expect("D1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let change_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let change_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: change_port,
                    kind: UiEventKind::Input,
                    payload: Some("5".to_string()),
                }],
            }))
            .expect("change should decode");
        assert_eq!(change_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "5"));
    }

    #[test]
    fn cells_real_file_input_fact_rerender_preserves_live_edit_text() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");

        let fact_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![boon_scene::UiFact {
                    id: input.id,
                    kind: boon_scene::UiFactKind::DraftText("7".to_string()),
                }],
            }))
            .expect("draft fact should decode");
        let fact_batch = runtime
            .decode_commands(fact_descriptor)
            .expect("draft fact batch should decode");

        assert!(fact_batch.ops.iter().any(|op| matches!(
            op,
            boon_scene::RenderOp::SetProperty { name, value, .. }
                if name == "value" && value.as_deref() == Some("7")
        )));

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("7".to_string()),
                }],
            }))
            .expect("input should decode");
        if input_descriptor != 0 {
            let input_batch = runtime
                .decode_commands(input_descriptor)
                .expect("input batch should decode");
            panic!(
                "expected input descriptor 0, got batch ops: {:?}",
                input_batch.ops
            );
        }
    }

    #[test]
    fn cells_real_file_fact_rerender_then_keydown_commits_value() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");

        let fact_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![boon_scene::UiFact {
                    id: input.id,
                    kind: boon_scene::UiFactKind::DraftText("7".to_string()),
                }],
            }))
            .expect("draft fact should decode");
        let fact_batch = runtime
            .decode_commands(fact_descriptor)
            .expect("draft fact batch should decode");

        let key_down_port = find_input_without_focus_port(&fact_batch, UiEventKind::KeyDown)
            .expect("rerendered input should expose KeyDown");

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some(format!("Enter{KEYDOWN_TEXT_SEPARATOR}7")),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(root, "17"));
        assert!(tree_contains_text(root, "32"));
    }

    #[test]
    fn cells_real_file_fact_input_then_plain_enter_commits_value() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let fact_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: input.id,
                    kind: UiFactKind::DraftText("7".to_string()),
                }],
            }))
            .expect("draft fact should decode");
        let fact_batch = runtime
            .decode_commands(fact_descriptor)
            .expect("fact batch should decode");
        assert!(fact_batch.ops.iter().any(|op| matches!(
            op,
            boon_scene::RenderOp::SetProperty { name, value, .. }
                if name == "value" && value.as_deref() == Some("7")
        )));

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("7".to_string()),
                }],
            }))
            .expect("input should decode");
        if input_descriptor != 0 {
            let input_batch = runtime
                .decode_commands(input_descriptor)
                .expect("input batch should decode");
            panic!(
                "expected input descriptor 0, got batch ops: {:?}",
                input_batch.ops
            );
        }

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(root, "17"));
        assert!(tree_contains_text(root, "32"));
    }

    #[test]
    fn temperature_converter_real_file_input_updates_reciprocal_value() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let celsius_input = find_nth_tag(root, "input", 0).expect("should render Celsius input");
        let celsius_port = find_port(&init_batch, celsius_input.id, UiEventKind::Input)
            .expect("Celsius input should expose Input");

        let update_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: celsius_port,
                    kind: UiEventKind::Input,
                    payload: Some("0".to_string()),
                }],
            }))
            .expect("input should decode");
        let update_batch = runtime
            .decode_commands(update_descriptor)
            .expect("update batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(updated_root)) =
            &update_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let updated_celsius =
            find_nth_tag(updated_root, "input", 0).expect("updated Celsius input should render");
        let updated_fahrenheit =
            find_nth_tag(updated_root, "input", 1).expect("updated Fahrenheit input should render");

        assert_eq!(
            property_value(&update_batch, updated_celsius.id, "value"),
            Some("0")
        );
        assert_eq!(
            property_value(&update_batch, updated_fahrenheit.id, "value"),
            Some("32")
        );
    }

    #[test]
    fn temperature_converter_real_file_reverse_input_updates_reciprocal_value() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let fahrenheit_input =
            find_nth_tag(root, "input", 1).expect("should render Fahrenheit input");
        let fahrenheit_port = find_port(&init_batch, fahrenheit_input.id, UiEventKind::Input)
            .expect("Fahrenheit input should expose Input");

        let update_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: fahrenheit_port,
                    kind: UiEventKind::Input,
                    payload: Some("32".to_string()),
                }],
            }))
            .expect("input should decode");
        let update_batch = runtime
            .decode_commands(update_descriptor)
            .expect("update batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(updated_root)) =
            &update_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let updated_celsius =
            find_nth_tag(updated_root, "input", 0).expect("updated Celsius input should render");
        let updated_fahrenheit =
            find_nth_tag(updated_root, "input", 1).expect("updated Fahrenheit input should render");

        assert_eq!(
            property_value(&update_batch, updated_celsius.id, "value"),
            Some("0")
        );
        assert_eq!(
            property_value(&update_batch, updated_fahrenheit.id, "value"),
            Some("32")
        );
    }

    #[test]
    fn temperature_converter_real_file_switches_active_draft_binding_between_inputs() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let celsius_input = find_nth_tag(root, "input", 0).expect("should render Celsius input");
        let fahrenheit_input =
            find_nth_tag(root, "input", 1).expect("should render Fahrenheit input");
        let celsius_port = find_port(&init_batch, celsius_input.id, UiEventKind::Input)
            .expect("Celsius input should expose Input");
        let fahrenheit_port = find_port(&init_batch, fahrenheit_input.id, UiEventKind::Input)
            .expect("Fahrenheit input should expose Input");

        let forward_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: celsius_port,
                    kind: UiEventKind::Input,
                    payload: Some("1000".to_string()),
                }],
            }))
            .expect("forward input should decode");
        let forward_batch = runtime
            .decode_commands(forward_descriptor)
            .expect("forward batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(forward_root)) =
            &forward_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let forward_fahrenheit =
            find_nth_tag(forward_root, "input", 1).expect("forward Fahrenheit input should render");
        assert_eq!(
            property_value(&forward_batch, forward_fahrenheit.id, "value"),
            Some("1832")
        );

        let reverse_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: fahrenheit_port,
                    kind: UiEventKind::Input,
                    payload: Some("32".to_string()),
                }],
            }))
            .expect("reverse input should decode");
        let reverse_batch = runtime
            .decode_commands(reverse_descriptor)
            .expect("reverse batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(reverse_root)) =
            &reverse_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let reverse_celsius =
            find_nth_tag(reverse_root, "input", 0).expect("updated Celsius input should render");
        let reverse_fahrenheit =
            find_nth_tag(reverse_root, "input", 1).expect("updated Fahrenheit input should render");
        assert_eq!(
            property_value(&reverse_batch, reverse_celsius.id, "value"),
            Some("0")
        );
        assert_eq!(
            property_value(&reverse_batch, reverse_fahrenheit.id, "value"),
            Some("32")
        );
    }

    #[test]
    fn flight_booker_real_file_select_switches_to_return_and_books_return_flight() {
        let semantic = lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let select = find_nth_tag(root, "select", 0).expect("should render flight type select");
        let select_port = find_port(&init_batch, select.id, UiEventKind::Input)
            .expect("select should expose Input");

        let select_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: select_port,
                    kind: UiEventKind::Input,
                    payload: Some("return".to_string()),
                }],
            }))
            .expect("select change should decode");
        let select_batch = runtime
            .decode_commands(select_descriptor)
            .expect("select batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(selected_root)) =
            &select_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let selected_select =
            find_nth_tag(selected_root, "select", 0).expect("updated select should render");
        let selected_button =
            find_nth_tag(selected_root, "button", 0).expect("updated book button should render");
        assert_eq!(
            property_value(&select_batch, selected_select.id, "value"),
            Some("return")
        );
        let after_select_debug = runtime.debug_snapshot();
        assert_eq!(
            after_select_debug["textValues"]["store.flight_type"].as_str(),
            Some("return")
        );
        assert_eq!(
            after_select_debug["scalarValues"]["store.is_return"].as_i64(),
            Some(1)
        );
        assert_eq!(
            after_select_debug["scalarValues"]["store.is_valid"].as_i64(),
            Some(1)
        );
        let button_port = find_port(&select_batch, selected_button.id, UiEventKind::Click)
            .expect("updated book button should expose Click");
        let button_action = runtime
            .active_actions
            .get(&button_port)
            .expect("updated book button should have an action");
        assert!(
            action_contains_booked_return_branch(button_action),
            "unexpected button action: {button_action:?}"
        );

        let book_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: button_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("book click should decode");
        let book_batch = runtime
            .decode_commands(book_descriptor)
            .expect("book batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(book_root)) = &book_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let rendered = subtree_text(book_root);
        assert!(
            rendered.contains("Booked return flight"),
            "unexpected rendered text: {rendered}"
        );
        assert!(
            rendered.contains("2026-03-03 to 2026-03-03"),
            "unexpected rendered text: {rendered}"
        );
    }

    #[test]
    fn timer_real_file_slider_and_interval_update_progress() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/timer/timer.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "15s"));
        let slider = find_nth_tag(root, "input", 0).expect("timer should render slider");
        let slider_port = find_port(&init_batch, slider.id, UiEventKind::Input)
            .expect("slider should expose Input");

        let slider_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: slider_port,
                    kind: UiEventKind::Input,
                    payload: Some("2".to_string()),
                }],
            }))
            .expect("slider input should decode");
        let mut current_batch = runtime
            .decode_commands(slider_descriptor)
            .expect("slider batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(updated_root)) =
            &current_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let mut current_root = updated_root.clone();

        assert!(tree_contains_text(&current_root, "2s"));

        for _ in 0..30 {
            let timer_port = find_nth_port(
                &current_batch,
                UiEventKind::Custom("timer:100".to_string()),
                0,
            )
            .expect("timer driver should expose custom port");
            let descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: timer_port,
                        kind: UiEventKind::Custom("timer:100".to_string()),
                        payload: None,
                    }],
                }))
                .expect("timer event should decode");
            if descriptor == 0 {
                continue;
            }
            current_batch = runtime
                .decode_commands(descriptor)
                .expect("timer batch should decode");
            let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(next_root)) =
                &current_batch.ops[0]
            else {
                panic!("expected ReplaceRoot(UiTree)");
            };
            current_root = next_root.clone();
        }

        assert!(tree_contains_text(&current_root, "100%"));
    }

    #[test]
    fn crud_real_file_filter_input_updates_visible_people() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Emil, Hans"));
        assert!(tree_contains_text(root, "Mustermann, Max"));
        assert!(tree_contains_text(root, "Tansen, Roman"));

        let filter_input = find_nth_tag(root, "input", 0).expect("crud should render filter input");
        let filter_port = find_port(&init_batch, filter_input.id, UiEventKind::Input)
            .expect("filter input should expose Input");

        let update_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: filter_port,
                    kind: UiEventKind::Input,
                    payload: Some("M".to_string()),
                }],
            }))
            .expect("filter input should decode");
        let update_batch = runtime
            .decode_commands(update_descriptor)
            .expect("filter update batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(updated_root)) =
            &update_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(!tree_contains_text(updated_root, "Emil, Hans"));
        assert!(tree_contains_text(updated_root, "Mustermann, Max"));
        assert!(!tree_contains_text(updated_root, "Tansen, Roman"));
    }

    #[test]
    fn crud_real_file_create_button_appends_new_person() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let current_root = match &init_batch.ops[0] {
            boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) => root.clone(),
            _ => panic!("expected ReplaceRoot(UiTree)"),
        };
        let mut current_batch = init_batch;
        let mut current_root = current_root;
        let name_input =
            find_nth_tag(&current_root, "input", 1).expect("crud should render name input");
        let name_port = find_port(&current_batch, name_input.id, UiEventKind::Input)
            .expect("name input should expose Input");
        let name_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: name_port,
                    kind: UiEventKind::Input,
                    payload: Some("John".to_string()),
                }],
            }))
            .expect("name input should decode");
        if name_descriptor != 0 {
            let name_batch = runtime
                .decode_commands(name_descriptor)
                .expect("name batch should decode");
            let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(name_root)) =
                &name_batch.ops[0]
            else {
                panic!("expected ReplaceRoot(UiTree)");
            };
            current_root = name_root.clone();
            current_batch = name_batch;
        }
        let after_name_debug = runtime.debug_snapshot();
        assert_eq!(
            after_name_debug["textValues"]["store.elements.name_input.text"].as_str(),
            Some("John")
        );
        let surname_input =
            find_nth_tag(&current_root, "input", 2).expect("crud should render surname input");
        let surname_port = find_port(&current_batch, surname_input.id, UiEventKind::Input)
            .expect("surname input should expose Input");
        let surname_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: surname_port,
                    kind: UiEventKind::Input,
                    payload: Some("Doe".to_string()),
                }],
            }))
            .expect("surname input should decode");
        if surname_descriptor != 0 {
            let surname_batch = runtime
                .decode_commands(surname_descriptor)
                .expect("surname batch should decode");
            let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(surname_root)) =
                &surname_batch.ops[0]
            else {
                panic!("expected ReplaceRoot(UiTree)");
            };
            current_root = surname_root.clone();
            current_batch = surname_batch;
        }
        let after_surname_debug = runtime.debug_snapshot();
        assert_eq!(
            after_surname_debug["textValues"]["store.elements.name_input.text"].as_str(),
            Some("John")
        );
        assert_eq!(
            after_surname_debug["textValues"]["store.elements.surname_input.text"].as_str(),
            Some("Doe")
        );
        let create_button = find_first_tag_with_text(&current_root, "button", "Create")
            .expect("crud should render Create button");
        let create_port = find_port(&current_batch, create_button.id, UiEventKind::Click)
            .expect("Create button should expose Click");
        let create_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: create_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("create click should decode");
        let create_batch = runtime
            .decode_commands(create_descriptor)
            .expect("create batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(created_root)) =
            &create_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let rendered = subtree_text(created_root);
        assert!(
            rendered.contains("Doe"),
            "created person surname should render, got: {rendered}"
        );
        assert!(
            rendered.contains("John"),
            "created person name should render, got: {rendered}"
        );
    }

    #[test]
    fn crud_real_file_clicking_person_row_selects_it() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let row_button = find_first_tag_with_text(root, "button", "Tansen, Roman")
            .expect("crud should render Tansen row button");
        let row_port = find_port(&init_batch, row_button.id, UiEventKind::Click)
            .expect("row button should expose Click");

        let select_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: row_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("row click should decode");
        let select_batch = runtime
            .decode_commands(select_descriptor)
            .expect("select batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(selected_root)) =
            &select_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let debug = runtime.debug_snapshot();
        assert!(
            debug["scalarValues"]["store.selected_id"]
                .as_i64()
                .unwrap_or_default()
                != 0,
            "expected selection click to update store.selected_id, got {debug}"
        );

        let rendered = subtree_text(selected_root);
        assert!(
            rendered.contains("► Tansen, Roman"),
            "unexpected rendered text after selection: {rendered}; debug: {debug}"
        );
    }

    #[test]
    fn cells_real_file_focus_facts_do_not_rerender_edit_input_without_binding() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");

        let focus_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: input.id,
                    kind: UiFactKind::Focused(true),
                }],
            }))
            .expect("focus fact should decode");
        assert_eq!(focus_descriptor, 0);

        let blur_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: input.id,
                    kind: UiFactKind::Focused(false),
                }],
            }))
            .expect("blur fact should decode");
        assert_eq!(blur_descriptor, 0);
    }

    #[test]
    fn cells_real_file_recomputes_formula_cells_after_edit() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let double_click_port = init_batch
            .ops
            .iter()
            .find_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort {
                    port,
                    kind: UiEventKind::DoubleClick,
                    ..
                } => Some(*port),
                _ => None,
            })
            .expect("cells grid should expose a DoubleClick port");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("7".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(root, "17"));
        assert!(tree_contains_text(root, "32"));
    }

    #[test]
    fn cells_real_file_escape_cancels_edit_without_commit() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("1234".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let cancel_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Escape".to_string()),
                }],
            }))
            .expect("escape should decode");
        let cancel_batch = runtime
            .decode_commands(cancel_descriptor)
            .expect("cancel batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &cancel_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(&root, "5"));
        assert!(!tree_contains_text(&root, "1234"));
    }

    #[test]
    fn cells_real_file_blur_exits_edit_without_commit() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let blur_port = find_port(&edit_batch, input.id, UiEventKind::Blur)
            .expect("edit input should expose Blur");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("2345".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let blur_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: blur_port,
                    kind: UiEventKind::Blur,
                    payload: None,
                }],
            }))
            .expect("blur should decode");
        let blur_batch = runtime
            .decode_commands(blur_descriptor)
            .expect("blur batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &blur_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "5"));
        assert!(!tree_contains_text(root, "2345"));
    }

    #[test]
    fn cells_real_file_formula_commit_updates_formula_cell() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 1)
            .expect("B1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("=add(A2, A3)".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "25"));
    }

    #[test]
    fn cells_real_file_editing_a2_recomputes_b1_and_c1() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 26)
            .expect("A2 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("20".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "20"));
        assert!(tree_contains_text(root, "25"));
        assert!(tree_contains_text(root, "40"));
    }

    #[test]
    fn cells_real_file_editing_a3_recomputes_c1_without_changing_b1() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 52)
            .expect("A3 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("30".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "30"));
        assert!(tree_contains_text(root, "15"));
        assert!(tree_contains_text(root, "45"));
    }

    #[test]
    fn cells_real_file_formula_commit_then_base_edit_recomputes_formula_cell() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");

        let formula_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 1)
            .expect("B1 should expose DoubleClick");

        let formula_edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: formula_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let formula_edit_batch = runtime
            .decode_commands(formula_edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &formula_edit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let formula_input = find_first_tag(root, "input").expect("edit mode should render input");
        let formula_input_port =
            find_port(&formula_edit_batch, formula_input.id, UiEventKind::Input)
                .expect("edit input should expose Input");
        let formula_key_down_port =
            find_port(&formula_edit_batch, formula_input.id, UiEventKind::KeyDown)
                .expect("edit input should expose KeyDown");

        let formula_input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: formula_input_port,
                    kind: UiEventKind::Input,
                    payload: Some("=add(A2, A3)".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(formula_input_descriptor, 0);

        let formula_commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: formula_key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let formula_commit_batch = runtime
            .decode_commands(formula_commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &formula_commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        assert!(tree_contains_text(root, "25"));

        let a2_port = find_nth_port(&formula_commit_batch, UiEventKind::DoubleClick, 26)
            .expect("A2 should expose DoubleClick");
        let a2_edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a2_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let a2_edit_batch = runtime
            .decode_commands(a2_edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &a2_edit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let a2_input = find_first_tag(root, "input").expect("edit mode should render input");
        let a2_input_port = find_port(&a2_edit_batch, a2_input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let a2_key_down_port = find_port(&a2_edit_batch, a2_input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let a2_input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a2_input_port,
                    kind: UiEventKind::Input,
                    payload: Some("20".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(a2_input_descriptor, 0);

        let a2_commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a2_key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let a2_commit_batch = runtime
            .decode_commands(a2_commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &a2_commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "20"));
        assert!(tree_contains_text(root, "35"));
        assert!(tree_contains_text(root, "40"));
    }

    #[test]
    fn cells_real_file_chained_base_edits_keep_dependency_graph_consistent() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        assert_ne!(init_descriptor, 0, "init should queue cells root");

        let root = edit_nth_cells_grid_cell_and_commit(&mut runtime, 0, "7");
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "17"));
        assert!(tree_contains_text(&root, "32"));

        let root = edit_nth_cells_grid_cell_and_commit(&mut runtime, 26, "20");
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "20"));
        assert!(tree_contains_text(&root, "27"));
        assert!(tree_contains_text(&root, "42"));

        let root = edit_nth_cells_grid_cell_and_commit(&mut runtime, 52, "30");
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(&root, "20"));
        assert!(tree_contains_text(&root, "30"));
        assert!(tree_contains_text(&root, "27"));
        assert!(tree_contains_text(&root, "57"));
    }

    #[test]
    fn cells_real_file_formula_commit_then_multiple_base_edits_stays_reactive() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        assert_ne!(init_descriptor, 0, "init should queue cells root");

        let root = edit_nth_cells_grid_cell_and_commit(&mut runtime, 1, "=add(A2, A3)");
        assert!(tree_contains_text(&root, "25"));
        assert!(tree_contains_text(&root, "30"));

        let root = edit_nth_cells_grid_cell_and_commit(&mut runtime, 26, "20");
        assert!(tree_contains_text(&root, "20"));
        assert!(tree_contains_text(&root, "35"));
        assert!(tree_contains_text(&root, "40"));

        let root = edit_nth_cells_grid_cell_and_commit(&mut runtime, 52, "30");
        assert!(tree_contains_text(&root, "20"));
        assert!(tree_contains_text(&root, "30"));
        assert!(tree_contains_text(&root, "50"));
        assert!(tree_contains_text(&root, "55"));
    }

    #[test]
    fn cells_real_file_multiple_commits_preserve_full_grid_event_surface() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        assert_eq!(
            attached_port_count(&init_batch, UiEventKind::DoubleClick),
            26 * 100
        );
        assert!(tree_contains_text(&init_root, "100"));

        let a1_commit_batch =
            edit_nth_cells_grid_cell_and_commit_batch_with_state(&mut runtime, &mut state, 0, "7");
        let a1_root = apply_batch_and_root(&mut state, &a1_commit_batch);
        assert_eq!(
            count_ports_in_state(&a1_root, &state, UiEventKind::DoubleClick),
            26 * 100
        );
        assert!(tree_contains_text(&a1_root, "100"));
        assert!(tree_contains_text(&a1_root, "7"));
        assert!(tree_contains_text(&a1_root, "17"));
        assert!(tree_contains_text(&a1_root, "32"));

        let formula_commit_batch = edit_nth_cells_grid_cell_and_commit_batch_with_state(
            &mut runtime,
            &mut state,
            1,
            "=add(A2, A3)",
        );
        let formula_root = apply_batch_and_root(&mut state, &formula_commit_batch);
        assert_eq!(
            count_ports_in_state(&formula_root, &state, UiEventKind::DoubleClick),
            26 * 100
        );
        assert!(tree_contains_text(&formula_root, "100"));
        assert!(tree_contains_text(&formula_root, "25"));
        assert!(tree_contains_text(&formula_root, "30"));

        let far_commit_batch = edit_nth_cells_grid_cell_and_commit_batch_with_state(
            &mut runtime,
            &mut state,
            26 * 100 - 1,
            "99",
        );
        let far_root = apply_batch_and_root(&mut state, &far_commit_batch);
        assert_eq!(
            count_ports_in_state(&far_root, &state, UiEventKind::DoubleClick),
            26 * 100
        );
        assert!(tree_contains_text(&far_root, "100"));
        assert!(tree_contains_text(&far_root, "99"));
    }

    #[test]
    fn cells_real_file_scale_metrics_report_current_batch_sizes() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_metrics = batch_metrics(&init_batch);

        let mut state = FakeRenderState::default();
        let _ = apply_batch_and_root(&mut state, &init_batch);

        let a1_commit_batch =
            edit_nth_cells_grid_cell_and_commit_batch_with_state(&mut runtime, &mut state, 0, "7");
        let a1_metrics = batch_metrics(&a1_commit_batch);

        let formula_commit_batch = edit_nth_cells_grid_cell_and_commit_batch_with_state(
            &mut runtime,
            &mut state,
            1,
            "=add(A2, A3)",
        );
        let formula_metrics = batch_metrics(&formula_commit_batch);

        let far_commit_batch = edit_nth_cells_grid_cell_and_commit_batch_with_state(
            &mut runtime,
            &mut state,
            26 * 100 - 1,
            "99",
        );
        let far_metrics = batch_metrics(&far_commit_batch);

        eprintln!(
            "[cells-scale-metrics] init={:?} a1_commit={:?} formula_commit={:?} far_commit={:?}",
            init_metrics, a1_metrics, formula_metrics, far_metrics
        );

        assert_eq!(init_metrics.double_click_ports, 26 * 100);
        assert!(a1_metrics.double_click_ports <= 4);
        assert!(formula_metrics.double_click_ports <= 4);
        assert!(far_metrics.double_click_ports <= 4);
        assert!(init_metrics.ui_node_count >= 26 * 100);
        assert!(init_metrics.encoded_bytes > 0);
        assert!(a1_metrics.encoded_bytes > 0);
        assert!(formula_metrics.encoded_bytes > 0);
        assert!(far_metrics.encoded_bytes > 0);
        assert!(a1_metrics.op_count > 0);
        assert!(formula_metrics.op_count > 0);
        assert!(far_metrics.op_count > 0);
        assert!(a1_metrics.encoded_bytes < init_metrics.encoded_bytes / 100);
        assert!(formula_metrics.encoded_bytes < init_metrics.encoded_bytes / 100);
        assert!(far_metrics.encoded_bytes < init_metrics.encoded_bytes / 100);
        assert!(a1_metrics.input_ports == 0);
        assert!(formula_metrics.input_ports == 0);
        assert!(far_metrics.input_ports == 0);
        assert!(a1_metrics.key_down_ports == 0);
        assert!(formula_metrics.key_down_ports == 0);
        assert!(far_metrics.key_down_ports == 0);
    }

    #[test]
    fn cells_real_file_incremental_commit_preserves_unedited_nested_cell_identity() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);

        let b1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 1)
            .expect("B1 should expose DoubleClick");
        let a2_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 26)
            .expect("A2 should expose DoubleClick");
        let z100_id =
            find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 2599)
                .expect("Z100 should expose DoubleClick");

        let commit_batch =
            edit_nth_cells_grid_cell_and_commit_batch_with_state(&mut runtime, &mut state, 0, "7");
        assert!(
            commit_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "cells commit should stay incremental",
        );

        let Some(RenderRoot::UiTree(root)) = state.root() else {
            panic!("expected cells root after commit");
        };
        assert_eq!(
            find_nth_port_target_in_state(root, &state, UiEventKind::DoubleClick, 1),
            Some(b1_id),
            "B1 identity should survive an A1 commit",
        );
        assert_node_id_and_port_kind(
            root,
            &state,
            b1_id,
            UiEventKind::DoubleClick,
            "B1 should keep its display port after A1 commit",
        );
        assert_eq!(
            find_nth_port_target_in_state(root, &state, UiEventKind::DoubleClick, 26),
            Some(a2_id),
            "A2 identity should survive an A1 commit",
        );
        assert_node_id_and_port_kind(
            root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should keep its display port after A1 commit",
        );
        assert_eq!(
            find_nth_port_target_in_state(root, &state, UiEventKind::DoubleClick, 2599),
            Some(z100_id),
            "far-grid identity should survive an A1 commit",
        );
        assert_node_id_and_port_kind(
            root,
            &state,
            z100_id,
            UiEventKind::DoubleClick,
            "Z100 should keep its display port after A1 commit",
        );
        assert!(tree_contains_text(&root, "7"));
        assert!(tree_contains_text(root, "17"));
        assert!(tree_contains_text(root, "32"));
    }

    #[test]
    fn cells_real_file_incremental_edit_entry_preserves_visible_sibling_cell_ports() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let b1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 1)
            .expect("B1 should expose DoubleClick");
        let a2_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 26)
            .expect("A2 should expose DoubleClick");
        let z100_id =
            find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 2599)
                .expect("Z100 should expose DoubleClick");
        let a1_port = find_nth_port_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a1_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        assert!(
            edit_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "edit entry should stay incremental",
        );
        let edit_root = apply_batch_and_root(&mut state, &edit_batch);
        assert_node_id_and_port_kind(
            &edit_root,
            &state,
            b1_id,
            UiEventKind::DoubleClick,
            "B1 should stay visible and clickable during A1 edit",
        );
        assert_node_id_and_port_kind(
            &edit_root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should stay visible and clickable during A1 edit",
        );
        assert_node_id_and_port_kind(
            &edit_root,
            &state,
            z100_id,
            UiEventKind::DoubleClick,
            "Z100 should stay visible and clickable during A1 edit",
        );
    }

    #[test]
    fn cells_real_file_incremental_commit_restores_same_edited_cell_identity() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let a1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");
        let a1_port = find_nth_port_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a1_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        assert!(
            edit_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "edit entry should stay incremental",
        );
        let edit_root = apply_batch_and_root(&mut state, &edit_batch);
        let input = find_first_tag(&edit_root, "input").expect("edit mode should render input");
        let input_port = find_port_in_state(&state, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port_in_state(&state, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("7".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        assert!(
            commit_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "commit should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &commit_batch);
        assert_eq!(
            find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 0),
            Some(a1_id),
            "edited A1 should regain the same display identity after commit",
        );
        assert!(tree_contains_text(&root, "7"));
    }

    #[test]
    fn cells_real_file_incremental_cancel_restores_same_edited_cell_identity() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let a1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");
        let a1_port = find_nth_port_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a1_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let edit_root = apply_batch_and_root(&mut state, &edit_batch);
        let input = find_first_tag(&edit_root, "input").expect("edit mode should render input");
        let input_port = find_port_in_state(&state, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port_in_state(&state, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("1234".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let cancel_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Escape".to_string()),
                }],
            }))
            .expect("escape should decode");
        let cancel_batch = runtime
            .decode_commands(cancel_descriptor)
            .expect("cancel batch should decode");
        assert!(
            cancel_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "cancel should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &cancel_batch);
        assert_eq!(
            find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 0),
            Some(a1_id),
            "edited A1 should regain the same display identity after cancel",
        );
        assert!(tree_contains_text(&root, "5"));
        assert!(!tree_contains_text(&root, "1234"));
    }

    #[test]
    fn cells_real_file_incremental_cancel_preserves_visible_sibling_cell_ports() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let b1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 1)
            .expect("B1 should expose DoubleClick");
        let a2_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 26)
            .expect("A2 should expose DoubleClick");
        let z100_id =
            find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 2599)
                .expect("Z100 should expose DoubleClick");
        let a1_port = find_nth_port_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a1_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let edit_root = apply_batch_and_root(&mut state, &edit_batch);
        let input = find_first_tag(&edit_root, "input").expect("edit mode should render input");
        let input_port = find_port_in_state(&state, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port_in_state(&state, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("1234".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let cancel_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Escape".to_string()),
                }],
            }))
            .expect("escape should decode");
        let cancel_batch = runtime
            .decode_commands(cancel_descriptor)
            .expect("cancel batch should decode");
        let root = apply_batch_and_root(&mut state, &cancel_batch);
        assert_node_id_and_port_kind(
            &root,
            &state,
            b1_id,
            UiEventKind::DoubleClick,
            "B1 should keep its display port after A1 cancel",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should keep its display port after A1 cancel",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            z100_id,
            UiEventKind::DoubleClick,
            "Z100 should keep its display port after A1 cancel",
        );
    }

    #[test]
    fn cells_real_file_incremental_blur_restores_same_edited_cell_identity() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let a1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");
        let a1_port = find_nth_port_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a1_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let edit_root = apply_batch_and_root(&mut state, &edit_batch);
        let input = find_first_tag(&edit_root, "input").expect("edit mode should render input");
        let input_port = find_port_in_state(&state, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let blur_port = find_port_in_state(&state, input.id, UiEventKind::Blur)
            .expect("edit input should expose Blur");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("2345".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let blur_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: blur_port,
                    kind: UiEventKind::Blur,
                    payload: None,
                }],
            }))
            .expect("blur should decode");
        let blur_batch = runtime
            .decode_commands(blur_descriptor)
            .expect("blur batch should decode");
        assert!(
            blur_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "blur exit should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &blur_batch);
        assert_eq!(
            find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 0),
            Some(a1_id),
            "edited A1 should regain the same display identity after blur",
        );
        assert!(tree_contains_text(&root, "5"));
        assert!(!tree_contains_text(&root, "2345"));
    }

    #[test]
    fn cells_real_file_incremental_blur_preserves_visible_sibling_cell_ports() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_root = apply_batch_and_root(&mut state, &init_batch);
        let b1_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 1)
            .expect("B1 should expose DoubleClick");
        let a2_id = find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 26)
            .expect("A2 should expose DoubleClick");
        let z100_id =
            find_nth_port_target_in_state(&init_root, &state, UiEventKind::DoubleClick, 2599)
                .expect("Z100 should expose DoubleClick");
        let a1_port = find_nth_port_in_state(&init_root, &state, UiEventKind::DoubleClick, 0)
            .expect("A1 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: a1_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let edit_root = apply_batch_and_root(&mut state, &edit_batch);
        let input = find_first_tag(&edit_root, "input").expect("edit mode should render input");
        let input_port = find_port_in_state(&state, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let blur_port = find_port_in_state(&state, input.id, UiEventKind::Blur)
            .expect("edit input should expose Blur");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("2345".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let blur_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: blur_port,
                    kind: UiEventKind::Blur,
                    payload: None,
                }],
            }))
            .expect("blur should decode");
        let blur_batch = runtime
            .decode_commands(blur_descriptor)
            .expect("blur batch should decode");
        let root = apply_batch_and_root(&mut state, &blur_batch);
        assert_node_id_and_port_kind(
            &root,
            &state,
            b1_id,
            UiEventKind::DoubleClick,
            "B1 should keep its display port after A1 blur",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should keep its display port after A1 blur",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            z100_id,
            UiEventKind::DoubleClick,
            "Z100 should keep its display port after A1 blur",
        );
    }

    #[test]
    fn cells_real_file_incremental_a2_edit_entry_preserves_siblings_and_target_identity() {
        let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
        let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 779, 2599]);

        let (edit_root, a2_id, ..) =
            enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 26);

        assert_cells_double_click_ids_stable(&edit_root, &state, &sibling_ids, "A2 edit entry");
        assert!(
            find_port_in_state(&state, a2_id, UiEventKind::DoubleClick).is_none(),
            "A2 display port should leave the tree while A2 is in edit mode",
        );
    }

    #[test]
    fn cells_real_file_incremental_a2_commit_restores_identity_and_preserves_siblings() {
        let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
        let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 779, 2599]);

        let (_edit_root, a2_id, input_port, key_down_port, _) =
            enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 26);

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("20".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let root = apply_batch_and_root(&mut state, &commit_batch);

        assert_eq!(
            find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 26),
            Some(a2_id),
            "A2 should regain the same display identity after commit",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should regain its display port after commit",
        );
        assert_cells_double_click_ids_stable(&root, &state, &sibling_ids, "A2 commit");
        assert!(tree_contains_text(&root, "20"));
    }

    #[test]
    fn cells_real_file_incremental_a2_cancel_restores_identity_and_preserves_siblings() {
        let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
        let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 779, 2599]);

        let (_edit_root, a2_id, input_port, key_down_port, _) =
            enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 26);

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("2000".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let cancel_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Escape".to_string()),
                }],
            }))
            .expect("escape should decode");
        let cancel_batch = runtime
            .decode_commands(cancel_descriptor)
            .expect("cancel batch should decode");
        let root = apply_batch_and_root(&mut state, &cancel_batch);

        assert_eq!(
            find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 26),
            Some(a2_id),
            "A2 should regain the same display identity after cancel",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should regain its display port after cancel",
        );
        assert_cells_double_click_ids_stable(&root, &state, &sibling_ids, "A2 cancel");
        assert!(!tree_contains_text(&root, "2000"));
    }

    #[test]
    fn cells_real_file_incremental_a2_blur_restores_identity_and_preserves_siblings() {
        let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
        let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 779, 2599]);

        let (_edit_root, a2_id, input_port, _key_down_port, blur_port) =
            enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 26);
        let blur_port = blur_port.expect("A2 edit input should expose Blur");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("2222".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let blur_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: blur_port,
                    kind: UiEventKind::Blur,
                    payload: None,
                }],
            }))
            .expect("blur should decode");
        let blur_batch = runtime
            .decode_commands(blur_descriptor)
            .expect("blur batch should decode");
        let root = apply_batch_and_root(&mut state, &blur_batch);

        assert_eq!(
            find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 26),
            Some(a2_id),
            "A2 should regain the same display identity after blur",
        );
        assert_node_id_and_port_kind(
            &root,
            &state,
            a2_id,
            UiEventKind::DoubleClick,
            "A2 should regain its display port after blur",
        );
        assert_cells_double_click_ids_stable(&root, &state, &sibling_ids, "A2 blur");
        assert!(!tree_contains_text(&root, "2222"));
    }

    #[test]
    fn cells_real_file_incremental_26x30_milestone_gate() {
        {
            let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
            let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[1, 26, 779]);

            let (_edit_root, a1_id, input_port, key_down_port, _) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 0);

            let input_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: input_port,
                        kind: UiEventKind::Input,
                        payload: Some("7".to_string()),
                    }],
                }))
                .expect("A1 input should decode");
            assert_eq!(input_descriptor, 0);

            let commit_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: key_down_port,
                        kind: UiEventKind::KeyDown,
                        payload: Some("Enter".to_string()),
                    }],
                }))
                .expect("A1 enter should decode");
            let commit_batch = runtime
                .decode_commands(commit_descriptor)
                .expect("A1 commit batch should decode");
            let root = apply_batch_and_root(&mut state, &commit_batch);

            assert_node_id_and_port_kind(
                &root,
                &state,
                a1_id,
                UiEventKind::DoubleClick,
                "A1 should regain its display identity after commit",
            );
            assert_cells_double_click_ids_stable(&root, &state, &sibling_ids, "26x30 A1 commit");
            assert!(tree_contains_text(&root, "7"));
        }

        {
            let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
            let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 26, 779]);

            let (_edit_root, b1_id, input_port, key_down_port, _) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 1);

            let input_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: input_port,
                        kind: UiEventKind::Input,
                        payload: Some("add(100,200)".to_string()),
                    }],
                }))
                .expect("B1 input should decode");
            assert_eq!(input_descriptor, 0);

            let cancel_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: key_down_port,
                        kind: UiEventKind::KeyDown,
                        payload: Some("Escape".to_string()),
                    }],
                }))
                .expect("B1 escape should decode");
            let cancel_batch = runtime
                .decode_commands(cancel_descriptor)
                .expect("B1 cancel batch should decode");
            let root = apply_batch_and_root(&mut state, &cancel_batch);

            assert_node_id_and_port_kind(
                &root,
                &state,
                b1_id,
                UiEventKind::DoubleClick,
                "B1 should regain its display identity after cancel",
            );
            assert_cells_double_click_ids_stable(&root, &state, &sibling_ids, "26x30 B1 cancel");
            assert!(!tree_contains_text(&root, "add(100,200)"));
        }

        {
            let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
            let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 779]);

            let (_edit_root, a2_id, input_port, _key_down_port, blur_port) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 26);
            let blur_port = blur_port.expect("A2 edit input should expose Blur");

            let input_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: input_port,
                        kind: UiEventKind::Input,
                        payload: Some("2222".to_string()),
                    }],
                }))
                .expect("A2 input should decode");
            assert_eq!(input_descriptor, 0);

            let blur_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: blur_port,
                        kind: UiEventKind::Blur,
                        payload: None,
                    }],
                }))
                .expect("A2 blur should decode");
            let blur_batch = runtime
                .decode_commands(blur_descriptor)
                .expect("A2 blur batch should decode");
            let root = apply_batch_and_root(&mut state, &blur_batch);

            assert_node_id_and_port_kind(
                &root,
                &state,
                a2_id,
                UiEventKind::DoubleClick,
                "A2 should regain its display identity after blur",
            );
            assert_cells_double_click_ids_stable(&root, &state, &sibling_ids, "26x30 A2 blur");
            assert!(!tree_contains_text(&root, "2222"));
        }

        {
            let (mut runtime, mut state, init_root) = init_cells_incremental_runtime();
            let sibling_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 26]);

            let (edit_root, z30_id, ..) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &init_root, 779);

            assert_cells_double_click_ids_stable(
                &edit_root,
                &state,
                &sibling_ids,
                "26x30 Z30 edit entry",
            );
            assert!(
                find_port_in_state(&state, z30_id, UiEventKind::DoubleClick).is_none(),
                "Z30 display port should leave the tree while Z30 is in edit mode",
            );
        }
    }

    #[test]
    fn cells_real_file_incremental_26x100_scale_gate() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_metrics = batch_metrics(&init_batch);
        let init_root = apply_batch_and_root(&mut state, &init_batch);

        assert_eq!(init_metrics.double_click_ports, 26 * 100);
        assert!(tree_contains_text(&init_root, "100"));

        let stable_ids = capture_cells_double_click_ids(&init_root, &state, &[0, 1, 26, 2599]);

        {
            let a1_commit_batch = edit_nth_cells_grid_cell_and_commit_batch_with_state(
                &mut runtime,
                &mut state,
                0,
                "7",
            );
            let a1_metrics = batch_metrics(&a1_commit_batch);
            let root = apply_batch_and_root(&mut state, &a1_commit_batch);

            assert_eq!(
                find_nth_port_target_in_state(&root, &state, UiEventKind::DoubleClick, 0),
                Some(stable_ids[0].1),
                "A1 should regain the same display identity after commit at full 26x100 scale",
            );
            assert_cells_double_click_ids_stable(
                &root,
                &state,
                &stable_ids[1..],
                "26x100 A1 commit",
            );
            assert!(tree_contains_text(&root, "7"));
            assert!(a1_metrics.double_click_ports <= 4);
            assert!(a1_metrics.input_ports == 0);
            assert!(a1_metrics.key_down_ports == 0);
            assert!(a1_metrics.encoded_bytes < init_metrics.encoded_bytes / 100);
            assert!(a1_metrics.op_count <= 16);
        }

        {
            let current_root = match state.root().expect("cells root should exist") {
                RenderRoot::UiTree(root) => root.clone(),
                _ => panic!("expected ui tree root"),
            };
            let (_edit_root, b1_id, input_port, key_down_port, _) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &current_root, 1);

            let input_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: input_port,
                        kind: UiEventKind::Input,
                        payload: Some("add(100,200)".to_string()),
                    }],
                }))
                .expect("B1 input should decode");
            assert_eq!(input_descriptor, 0);

            let cancel_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: key_down_port,
                        kind: UiEventKind::KeyDown,
                        payload: Some("Escape".to_string()),
                    }],
                }))
                .expect("B1 escape should decode");
            let cancel_batch = runtime
                .decode_commands(cancel_descriptor)
                .expect("B1 cancel batch should decode");
            let cancel_metrics = batch_metrics(&cancel_batch);
            let root = apply_batch_and_root(&mut state, &cancel_batch);

            assert_node_id_and_port_kind(
                &root,
                &state,
                b1_id,
                UiEventKind::DoubleClick,
                "B1 should regain its display identity after cancel at full 26x100 scale",
            );
            assert_node_id_and_port_kind(
                &root,
                &state,
                stable_ids[0].1,
                UiEventKind::DoubleClick,
                "A1 should keep its display identity after B1 cancel",
            );
            assert_node_id_and_port_kind(
                &root,
                &state,
                stable_ids[2].1,
                UiEventKind::DoubleClick,
                "A2 should keep its display identity after B1 cancel",
            );
            assert_node_id_and_port_kind(
                &root,
                &state,
                stable_ids[3].1,
                UiEventKind::DoubleClick,
                "Z100 should keep its display identity after B1 cancel",
            );
            assert!(!tree_contains_text(&root, "add(100,200)"));
            assert!(cancel_metrics.double_click_ports <= 4);
            assert!(cancel_metrics.input_ports == 0);
            assert!(cancel_metrics.key_down_ports == 0);
            assert!(cancel_metrics.encoded_bytes < init_metrics.encoded_bytes / 100);
            assert!(cancel_metrics.op_count <= 16);
        }

        {
            let current_root = match state.root().expect("cells root should exist") {
                RenderRoot::UiTree(root) => root.clone(),
                _ => panic!("expected ui tree root"),
            };
            let (_edit_root, a2_id, input_port, _key_down_port, blur_port) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &current_root, 26);
            let blur_port = blur_port.expect("A2 edit input should expose Blur");

            let input_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: input_port,
                        kind: UiEventKind::Input,
                        payload: Some("2222".to_string()),
                    }],
                }))
                .expect("A2 input should decode");
            assert_eq!(input_descriptor, 0);

            let blur_descriptor = runtime
                .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                    events: vec![UiEvent {
                        target: blur_port,
                        kind: UiEventKind::Blur,
                        payload: None,
                    }],
                }))
                .expect("A2 blur should decode");
            let blur_batch = runtime
                .decode_commands(blur_descriptor)
                .expect("A2 blur batch should decode");
            let blur_metrics = batch_metrics(&blur_batch);
            let root = apply_batch_and_root(&mut state, &blur_batch);

            assert_node_id_and_port_kind(
                &root,
                &state,
                a2_id,
                UiEventKind::DoubleClick,
                "A2 should regain its display identity after blur at full 26x100 scale",
            );
            assert_node_id_and_port_kind(
                &root,
                &state,
                stable_ids[0].1,
                UiEventKind::DoubleClick,
                "A1 should keep its display identity after A2 blur",
            );
            assert_node_id_and_port_kind(
                &root,
                &state,
                stable_ids[1].1,
                UiEventKind::DoubleClick,
                "B1 should keep its display identity after A2 blur",
            );
            assert_node_id_and_port_kind(
                &root,
                &state,
                stable_ids[3].1,
                UiEventKind::DoubleClick,
                "Z100 should keep its display identity after A2 blur",
            );
            assert!(!tree_contains_text(&root, "2222"));
            assert!(blur_metrics.double_click_ports <= 4);
            assert!(blur_metrics.input_ports == 0);
            assert!(blur_metrics.key_down_ports == 0);
            assert!(blur_metrics.encoded_bytes < init_metrics.encoded_bytes / 100);
            assert!(blur_metrics.op_count <= 16);
        }

        {
            let current_root = match state.root().expect("cells root should exist") {
                RenderRoot::UiTree(root) => root.clone(),
                _ => panic!("expected ui tree root"),
            };
            let (edit_root, z100_id, ..) =
                enter_edit_mode_for_nth_cells_cell(&mut runtime, &mut state, &current_root, 2599);

            assert_cells_double_click_ids_stable(
                &edit_root,
                &state,
                &stable_ids[..3],
                "26x100 Z100 edit entry",
            );
            assert!(
                find_port_in_state(&state, z100_id, UiEventKind::DoubleClick).is_none(),
                "Z100 display port should leave the tree while Z100 is in edit mode at full 26x100 scale",
            );
        }
    }

    #[test]
    fn cells_real_file_scale_metrics_meet_milestone_thresholds() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let init_metrics = batch_metrics(&init_batch);

        let mut state = FakeRenderState::default();
        let _ = apply_batch_and_root(&mut state, &init_batch);

        let a1_commit_metrics = batch_metrics(
            &edit_nth_cells_grid_cell_and_commit_batch_with_state(&mut runtime, &mut state, 0, "7"),
        );
        let formula_commit_metrics =
            batch_metrics(&edit_nth_cells_grid_cell_and_commit_batch_with_state(
                &mut runtime,
                &mut state,
                1,
                "=add(A2, A3)",
            ));
        let far_commit_metrics =
            batch_metrics(&edit_nth_cells_grid_cell_and_commit_batch_with_state(
                &mut runtime,
                &mut state,
                26 * 100 - 1,
                "99",
            ));

        assert_eq!(init_metrics.double_click_ports, 26 * 100);
        assert!(init_metrics.ui_node_count >= 26 * 100);
        assert!(
            init_metrics.encoded_bytes <= CELLS_26X100_INIT_MAX_ENCODED_BYTES,
            "init batch too large: {:?}",
            init_metrics
        );
        assert!(
            init_metrics.op_count <= CELLS_26X100_INIT_MAX_OPS,
            "init batch too many ops: {:?}",
            init_metrics
        );

        for (label, metrics) in [
            ("A1 commit", a1_commit_metrics),
            ("formula commit", formula_commit_metrics),
            ("far commit", far_commit_metrics),
        ] {
            assert!(
                metrics.encoded_bytes <= CELLS_26X100_COMMIT_MAX_ENCODED_BYTES,
                "{label} batch too large: {:?}",
                metrics
            );
            assert!(
                metrics.op_count <= CELLS_26X100_COMMIT_MAX_OPS,
                "{label} batch too many ops: {:?}",
                metrics
            );
            assert!(
                metrics.double_click_ports <= CELLS_26X100_COMMIT_MAX_DOUBLE_CLICK_ATTACHES,
                "{label} reattached too many DoubleClick ports: {:?}",
                metrics
            );
            assert_eq!(
                metrics.input_ports, 0,
                "{label} should not reattach Input ports after commit"
            );
            assert_eq!(
                metrics.key_down_ports, 0,
                "{label} should not reattach KeyDown ports after commit"
            );
        }
    }

    #[test]
    fn cells_backend_metrics_snapshot_reports_current_numbers() {
        let shared_metrics = crate::cells_backend_metrics_snapshot()
            .expect("shared cells backend metrics snapshot should build");
        let wasm_pro_metrics = wasm_pro_pipeline_metrics_for_cells();

        eprintln!("[cells-backend-metrics] wasm={:?}", shared_metrics.wasm);
        eprintln!("[cells-backend-metrics] wasm_pipeline={wasm_pro_metrics:?}");
        eprintln!(
            "[cells-backend-metrics-json] {}",
            serde_json::json!({
                "wasm": &shared_metrics.wasm,
                "wasm_pipeline": &wasm_pro_metrics,
            })
        );

        assert!(wasm_pro_metrics.init_batch.encoded_bytes > 0);
        assert!(wasm_pro_metrics.a1_commit_batch.encoded_bytes > 0);
        assert!(wasm_pro_metrics.init_batch.double_click_ports == 26 * 100);
        assert!(
            wasm_pro_metrics.a1_commit_batch.encoded_bytes <= CELLS_26X100_COMMIT_MAX_ENCODED_BYTES
        );
        assert!(wasm_pro_metrics.a1_commit_batch.op_count <= CELLS_26X100_COMMIT_MAX_OPS);
        assert!(
            wasm_pro_metrics.a1_commit_batch.double_click_ports
                <= CELLS_26X100_COMMIT_MAX_DOUBLE_CLICK_ATTACHES
        );
    }

    #[test]
    fn todo_mvc_real_file_incremental_add_filter_remove_preserves_survivor_identity() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let root = apply_batch_and_root(&mut state, &init_batch);

        let input = find_first_tag(&root, "input").expect("todo_mvc should render an input");
        let input_port = find_port_in_state(&state, input.id, UiEventKind::Input)
            .expect("input should expose Input");
        let key_port = find_port_in_state(&state, input.id, UiEventKind::KeyDown)
            .expect("input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Write tests".to_string()),
                }],
            }))
            .expect("input event should decode");

        let add_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("add event should decode");
        let add_batch = runtime
            .decode_commands(add_descriptor)
            .expect("add batch should decode");
        assert!(
            add_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "todo add should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &add_batch);
        let clean_room_id = find_first_tag_with_text(&root, "div", "Clean room")
            .map(|node| node.id)
            .expect("Clean room row should exist after add");
        let write_tests_id = find_first_tag_with_text(&root, "div", "Write tests")
            .map(|node| node.id)
            .expect("Write tests row should exist after add");

        let groceries_row = find_first_tag_with_text(&root, "div", "Buy groceries")
            .expect("Buy groceries row should exist");
        let groceries_toggle = find_first_tag(groceries_row, "button")
            .expect("todo row should render a checkbox button");
        let groceries_toggle_port =
            find_port_in_state(&state, groceries_toggle.id, UiEventKind::Click)
                .expect("todo checkbox should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: groceries_toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle event should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle batch should decode");
        assert!(
            toggle_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "todo toggle should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &toggle_batch);
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Clean room").map(|node| node.id),
            Some(clean_room_id),
            "Clean room row identity should survive another row toggle",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Write tests").map(|node| node.id),
            Some(write_tests_id),
            "newly added row identity should survive another row toggle",
        );

        let completed_button = find_first_tag_with_text(&root, "button", "Completed")
            .expect("Completed filter button should render");
        let completed_port = find_port_in_state(&state, completed_button.id, UiEventKind::Click)
            .expect("Completed filter should expose Click");

        let completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("completed filter event should decode");
        let completed_batch = runtime
            .decode_commands(completed_descriptor)
            .expect("completed filter batch should decode");
        assert!(
            completed_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "todo completed filter should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &completed_batch);
        assert!(
            find_first_tag_with_text(&root, "div", "Clean room").is_none(),
            "Clean room should be hidden in Completed view",
        );
        assert!(
            find_first_tag_with_text(&root, "div", "Write tests").is_none(),
            "Write tests should be hidden in Completed view",
        );

        let remove_completed_button = find_first_tag_with_text(&root, "button", "Clear completed")
            .expect("remove completed button should render");
        let remove_completed_port =
            find_port_in_state(&state, remove_completed_button.id, UiEventKind::Click)
                .expect("remove completed should expose Click");

        let remove_completed_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_completed_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove completed event should decode");
        let remove_completed_batch = runtime
            .decode_commands(remove_completed_descriptor)
            .expect("remove completed batch should decode");
        assert!(
            remove_completed_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "todo remove completed should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &remove_completed_batch);
        assert!(
            find_first_tag_with_text(&root, "div", "Buy groceries").is_none(),
            "completed row should be removed after Clear completed",
        );

        let all_button = find_first_tag_with_text(&root, "button", "All")
            .expect("All filter button should render");
        let all_port = find_port_in_state(&state, all_button.id, UiEventKind::Click)
            .expect("All filter should expose Click");

        let all_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("all filter event should decode");
        let all_batch = runtime
            .decode_commands(all_descriptor)
            .expect("all filter batch should decode");
        assert!(
            all_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "todo all filter should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &all_batch);

        assert_eq!(
            find_first_tag_with_text(&root, "div", "Clean room").map(|node| node.id),
            Some(clean_room_id),
            "Clean room row should regain the same identity after filter changes and removal",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Write tests").map(|node| node.id),
            Some(write_tests_id),
            "Write tests row should regain the same identity after filter changes and removal",
        );
        assert!(
            find_first_tag_with_text(&root, "div", "Buy groceries").is_none(),
            "removed completed row should not reappear",
        );
    }

    #[test]
    fn nested_runtime_object_list_real_source_append_and_remove_stay_incremental() {
        let semantic = lower_to_semantic(
            r#"
FUNCTION new_cell(title) {
    [title: title remove: LINK]
}

FUNCTION make_row() {
    [
        add: LINK
        cells:
            LIST {
                new_cell(title: TEXT { A })
                new_cell(title: TEXT { B })
            }
            |> List/append(item: add.event.press |> THEN { new_cell(title: TEXT { C }) })
            |> List/remove(item, on: item.remove.event.press)
    ]
}

store: [rows: LIST { make_row() }]

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 0
        style: []
        items:
            store.rows
            |> List/map(row, new:
                Element/stripe(
                    element: []
                    direction: Row
                    gap: 0
                    style: []
                    items: LIST {
                        Element/button(
                            element: [event: [press: LINK]]
                            style: []
                            label: TEXT { Add }
                        )
                        |> LINK { row.add }

                        Element/stripe(
                            element: []
                            direction: Row
                            gap: 0
                            style: []
                            items:
                                row.cells
                                |> List/map(cell, new:
                                    Element/stripe(
                                        element: []
                                        direction: Row
                                        gap: 0
                                        style: []
                                        items: LIST {
                                            Element/label(
                                                element: []
                                                style: []
                                                label: cell.title
                                            )
                                            Element/button(
                                                element: [event: [press: LINK]]
                                                style: []
                                                label: TEXT { x }
                                            )
                                            |> LINK { cell.remove }
                                        }
                                    )
                                )
                        )
                    }
                )
            )
    )
)
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let root = apply_batch_and_root(&mut state, &init_batch);
        let row = find_first_tag_with_text(&root, "div", "Add")
            .expect("row containing Add button should render");
        let row_id = row.id;
        let add_button =
            find_first_tag_with_text(&root, "button", "Add").expect("Add button should render");
        let add_port = find_port_in_state(&state, add_button.id, UiEventKind::Click)
            .expect("Add button should expose Click");
        let b_row_id = find_first_tag_with_text(&root, "div", "B")
            .map(|node| node.id)
            .expect("B row should render");

        let add_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: add_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("add should decode");
        let add_batch = runtime
            .decode_commands(add_descriptor)
            .expect("add batch should decode");
        assert!(
            add_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "nested add should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &add_batch);
        assert_eq!(
            find_first_tag_with_text(&root, "div", "B").map(|node| node.id),
            Some(b_row_id),
            "existing nested child should preserve identity after nested append",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Add").map(|node| node.id),
            Some(row_id),
            "parent row should preserve identity after nested append",
        );
        let c_row =
            find_first_tag_with_text(&root, "div", "C").expect("C row should render after add");
        let remove_button = find_first_tag_with_text(c_row, "button", "x")
            .expect("nested remove button should render");
        let remove_port = find_port_in_state(&state, remove_button.id, UiEventKind::Click)
            .expect("nested remove should expose Click");

        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("remove should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("remove batch should decode");
        assert!(
            remove_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "nested remove should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &remove_batch);
        assert!(
            find_first_tag_with_text(&root, "div", "C").is_none(),
            "removed nested child should disappear",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "B").map(|node| node.id),
            Some(b_row_id),
            "surviving nested child should preserve identity after nested remove",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Add").map(|node| node.id),
            Some(row_id),
            "parent row should preserve identity after nested remove",
        );
    }

    #[test]
    fn nested_runtime_object_list_multi_row_mutation_preserves_sibling_identity() {
        let semantic = lower_to_semantic(
            r#"
FUNCTION new_cell(title) {
    [title: title remove: LINK]
}

FUNCTION make_row(row_name, first_title, second_title) {
    [
        row_name: row_name
        add: LINK
        cells:
            LIST {
                new_cell(title: first_title)
                new_cell(title: second_title)
            }
            |> List/append(item: add.event.press |> THEN { new_cell(title: TEXT { C }) })
            |> List/remove(item, on: item.remove.event.press)
    ]
}

store: [rows: LIST {
    make_row(
        row_name: TEXT { Row 1 }
        first_title: TEXT { A }
        second_title: TEXT { B }
    )
    make_row(
        row_name: TEXT { Row 2 }
        first_title: TEXT { X }
        second_title: TEXT { Y }
    )
}]

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 0
        style: []
        items:
            store.rows
            |> List/map(row, new:
                Element/stripe(
                    element: []
                    direction: Row
                    gap: 0
                    style: []
                    items: LIST {
                        Element/label(
                            element: []
                            style: []
                            label: row.row_name
                        )
                        Element/button(
                            element: [event: [press: LINK]]
                            style: []
                            label: TEXT { Add }
                        )
                        |> LINK { row.add }

                        Element/stripe(
                            element: []
                            direction: Row
                            gap: 0
                            style: []
                            items:
                                row.cells
                                |> List/map(cell, new:
                                    Element/stripe(
                                        element: []
                                        direction: Row
                                        gap: 0
                                        style: []
                                        items: LIST {
                                            Element/label(
                                                element: []
                                                style: []
                                                label: cell.title
                                            )
                                            Element/button(
                                                element: [event: [press: LINK]]
                                                style: []
                                                label: TEXT { x }
                                            )
                                            |> LINK { cell.remove }
                                        }
                                    )
                                )
                        )
                    }
                )
            )
    )
)
"#,
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);
        runtime.enable_incremental_diff();
        let mut state = FakeRenderState::default();

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let root = apply_batch_and_root(&mut state, &init_batch);

        let row_1_id = find_first_tag_with_text(&root, "div", "Row 1")
            .map(|node| node.id)
            .expect("row 1 should render");
        let row_2_id = find_first_tag_with_text(&root, "div", "Row 2")
            .map(|node| node.id)
            .expect("row 2 should render");
        let x_row_id = find_first_tag_with_text(&root, "div", "X")
            .map(|node| node.id)
            .expect("X row should render");
        let y_row_id = find_first_tag_with_text(&root, "div", "Y")
            .map(|node| node.id)
            .expect("Y row should render");

        let row_1 =
            find_first_tag_with_text(&root, "div", "Row 1").expect("row 1 container should exist");
        let row_1_add_button =
            find_first_tag_with_text(row_1, "button", "Add").expect("row 1 add should render");
        let row_1_add_port = find_port_in_state(&state, row_1_add_button.id, UiEventKind::Click)
            .expect("row 1 add should expose click");

        let add_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: row_1_add_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("nested add should decode");
        let add_batch = runtime
            .decode_commands(add_descriptor)
            .expect("nested add batch should decode");
        assert!(
            add_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "multi-row nested add should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &add_batch);
        let c_row = find_first_tag_with_text(&root, "div", "C")
            .expect("row 1 nested C should appear after add");
        let c_row_id = c_row.id;
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Row 1").map(|node| node.id),
            Some(row_1_id),
            "mutated parent row should preserve identity",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Row 2").map(|node| node.id),
            Some(row_2_id),
            "sibling parent row should preserve identity",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "X").map(|node| node.id),
            Some(x_row_id),
            "sibling nested child X should preserve identity",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Y").map(|node| node.id),
            Some(y_row_id),
            "sibling nested child Y should preserve identity",
        );

        let remove_button =
            find_first_tag_with_text(c_row, "button", "x").expect("nested remove should render");
        let remove_port = find_port_in_state(&state, remove_button.id, UiEventKind::Click)
            .expect("nested remove should expose click");
        let remove_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: remove_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("nested remove should decode");
        let remove_batch = runtime
            .decode_commands(remove_descriptor)
            .expect("nested remove batch should decode");
        assert!(
            remove_batch
                .ops
                .iter()
                .all(|op| !matches!(op, boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)))),
            "multi-row nested remove should stay incremental",
        );
        let root = apply_batch_and_root(&mut state, &remove_batch);
        assert!(
            find_first_tag_with_text(&root, "div", "C").is_none(),
            "appended nested child should disappear after remove",
        );
        assert_ne!(
            c_row_id, x_row_id,
            "test should track distinct nested children"
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Row 1").map(|node| node.id),
            Some(row_1_id),
            "mutated parent row should preserve identity after remove",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Row 2").map(|node| node.id),
            Some(row_2_id),
            "sibling parent row should preserve identity after remove",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "X").map(|node| node.id),
            Some(x_row_id),
            "sibling nested child X should preserve identity after remove",
        );
        assert_eq!(
            find_first_tag_with_text(&root, "div", "Y").map(|node| node.id),
            Some(y_row_id),
            "sibling nested child Y should preserve identity after remove",
        );
    }

    #[test]
    fn cells_real_file_editing_last_cell_commits_far_grid_value() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let last_cell_port = find_nth_port(&init_batch, UiEventKind::DoubleClick, 26 * 100 - 1)
            .expect("Z100 should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: last_cell_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double click should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
        let input = find_first_tag(root, "input").expect("edit mode should render input");
        let input_port = find_port(&edit_batch, input.id, UiEventKind::Input)
            .expect("edit input should expose Input");
        let key_down_port = find_port(&edit_batch, input.id, UiEventKind::KeyDown)
            .expect("edit input should expose KeyDown");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("99".to_string()),
                }],
            }))
            .expect("input should decode");
        assert_eq!(input_descriptor, 0);

        let commit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_down_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter should decode");
        let commit_batch = runtime
            .decode_commands(commit_descriptor)
            .expect("commit batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &commit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "99"));
        assert!(tree_contains_text(root, "100"));
    }

    #[test]
    fn todo_mvc_real_file_runtime_edits_title() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let groceries_title = find_first_tag_with_text(root, "span", "Buy groceries")
            .expect("Buy groceries title should render");
        let double_click_port =
            find_port(&init_batch, groceries_title.id, UiEventKind::DoubleClick)
                .expect("Buy groceries should expose DoubleClick");

        let edit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: double_click_port,
                    kind: UiEventKind::DoubleClick,
                    payload: None,
                }],
            }))
            .expect("double_click batch should decode");
        let edit_batch = runtime
            .decode_commands(edit_descriptor)
            .expect("edit diff should decode");

        let input_port = find_input_without_focus_port(&edit_batch, UiEventKind::Input)
            .expect("editing input should expose Input");

        let input_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Buy groceries and fruit".to_string()),
                }],
            }))
            .expect("input batch should decode");
        let input_batch = runtime
            .decode_commands(input_descriptor)
            .expect("input diff should decode");

        let enter_port = find_input_without_focus_port(&input_batch, UiEventKind::KeyDown)
            .expect("editing input should expose KeyDown");

        let exit_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: enter_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("enter batch should decode");
        let exit_batch = runtime
            .decode_commands(exit_descriptor)
            .expect("exit diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &exit_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "Buy groceries and fruit"));
        assert!(!tree_contains_text(root, "Buy groceries"));
        assert!(tree_contains_text(root, "Clean room"));
    }

    #[test]
    fn todo_mvc_real_file_hover_reveals_remove_button() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(!tree_contains_text(root, "×"));

        let groceries_row =
            find_first_tag_with_text(root, "div", "Buy groceries").expect("todo row should render");

        let hover_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: groceries_row.id,
                    kind: UiFactKind::Hovered(true),
                }],
            }))
            .expect("hover fact should decode");
        let hover_batch = runtime
            .decode_commands(hover_descriptor)
            .expect("hover diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &hover_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "×"));
    }

    #[test]
    fn todo_mvc_real_file_completed_item_applies_line_through_style() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let groceries_row =
            find_first_tag_with_text(root, "div", "Buy groceries").expect("todo row should render");
        let groceries_title = find_first_tag_with_text(groceries_row, "span", "Buy groceries")
            .expect("title should render");
        assert!(
            !property_value(&init_batch, groceries_title.id, "style")
                .is_some_and(|value| value.contains("text-decoration:line-through"))
        );

        let groceries_checkbox = groceries_row
            .children
            .iter()
            .find(|child| {
                matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == "button")
                    && child.children.iter().any(|grandchild| {
                        matches!(&grandchild.kind, UiNodeKind::Element { tag, .. } if tag == "div")
                    })
            })
            .expect("checkbox button should render");
        let toggle_port = find_port(&init_batch, groceries_checkbox.id, UiEventKind::Click)
            .expect("checkbox should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let groceries_row = find_first_tag_with_text(root, "div", "Buy groceries")
            .expect("todo row should rerender");
        let groceries_title = find_first_tag_with_text(groceries_row, "span", "Buy groceries")
            .expect("title should rerender");
        assert!(
            property_value(&toggle_batch, groceries_title.id, "style")
                .is_some_and(|value| value.contains("text-decoration:line-through"))
        );
    }

    #[test]
    fn todo_mvc_real_file_completed_item_updates_checkbox_icon_background() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let groceries_row =
            find_first_tag_with_text(root, "div", "Buy groceries").expect("todo row should render");
        let groceries_checkbox = groceries_row
            .children
            .iter()
            .find(|child| {
                matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == "button")
                    && child.children.iter().any(|grandchild| {
                        matches!(&grandchild.kind, UiNodeKind::Element { tag, .. } if tag == "div")
                    })
            })
            .expect("checkbox button should render");
        let checkbox_icon = find_first_tag(groceries_checkbox, "div")
            .expect("checkbox icon container should render");
        assert!(
            property_value(&init_batch, checkbox_icon.id, "style")
                .is_some_and(|value| value.contains("%23949494"))
        );

        let toggle_port = find_port(&init_batch, groceries_checkbox.id, UiEventKind::Click)
            .expect("checkbox should expose Click");
        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let groceries_row = find_first_tag_with_text(root, "div", "Buy groceries")
            .expect("todo row should rerender");
        let groceries_checkbox = groceries_row
            .children
            .iter()
            .find(|child| {
                matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == "button")
                    && child.children.iter().any(|grandchild| {
                        matches!(&grandchild.kind, UiNodeKind::Element { tag, .. } if tag == "div")
                    })
            })
            .expect("checkbox button should rerender");
        let checkbox_icon = find_first_tag(groceries_checkbox, "div")
            .expect("checkbox icon container should rerender");
        let icon_style = property_value(&toggle_batch, checkbox_icon.id, "style");
        assert!(
            icon_style
                .as_deref()
                .is_some_and(|value| value.contains("%2359A193") && value.contains("%233EA390")),
            "expected completed checkbox icon background, got style: {icon_style:?}"
        );
    }

    #[test]
    fn todo_mvc_real_file_checkbox_styles_include_static_dimensions() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let toggle_all = find_first_tag_with_text(root, "button", "❯")
            .expect("toggle-all checkbox should render");
        let toggle_all_style = property_value(&init_batch, toggle_all.id, "style");
        assert!(
            toggle_all_style
                .as_deref()
                .is_some_and(|value| value.contains("width:45px")),
            "expected toggle-all width style, got: {toggle_all_style:?}"
        );

        let toggle_all_icon = find_first_tag_with_text(toggle_all, "div", "❯")
            .expect("toggle-all icon should render");
        let toggle_all_icon_style = property_value(&init_batch, toggle_all_icon.id, "style");
        assert!(
            toggle_all_icon_style.as_deref().is_some_and(
                |value| value.contains("height:34px") && value.contains("font-size:22px")
            ),
            "expected toggle-all icon sizing styles, got: {toggle_all_icon_style:?}"
        );

        let groceries_row =
            find_first_tag_with_text(root, "div", "Buy groceries").expect("todo row should render");
        let groceries_checkbox = groceries_row
            .children
            .iter()
            .find(|child| {
                matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == "button")
                    && child.children.iter().any(|grandchild| {
                        matches!(&grandchild.kind, UiNodeKind::Element { tag, .. } if tag == "div")
                    })
            })
            .expect("checkbox button should render");
        let checkbox_icon = find_first_tag(groceries_checkbox, "div")
            .expect("checkbox icon container should render");
        let checkbox_icon_style = property_value(&init_batch, checkbox_icon.id, "style");
        assert!(
            checkbox_icon_style
                .as_deref()
                .is_some_and(|value| value.contains("width:40px") && value.contains("height:40px")),
            "expected todo checkbox icon sizing styles, got: {checkbox_icon_style:?}"
        );
    }

    #[test]
    fn todo_mvc_real_file_dynamic_added_item_updates_checkbox_icon_background() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("todo_mvc should render an input");
        let input_port = find_port(&init_batch, input.id, UiEventKind::Input)
            .expect("input should expose Input");
        let key_port = find_port(&init_batch, input.id, UiEventKind::KeyDown)
            .expect("input should expose KeyDown");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Test todo".to_string()),
                }],
            }))
            .expect("input event should decode");

        let add_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("add event should decode");
        let add_batch = runtime
            .decode_commands(add_descriptor)
            .expect("add batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let test_row = find_first_tag_with_text(root, "div", "Test todo")
            .expect("dynamic todo row should render");
        let test_checkbox = test_row
            .children
            .iter()
            .find(|child| {
                matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == "button")
                    && child.children.iter().any(|grandchild| {
                        matches!(&grandchild.kind, UiNodeKind::Element { tag, .. } if tag == "div")
                    })
            })
            .expect("dynamic checkbox button should render");
        let toggle_port = find_port(&add_batch, test_checkbox.id, UiEventKind::Click)
            .expect("dynamic checkbox should expose Click");

        let toggle_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle batch should decode");
        let toggle_batch = runtime
            .decode_commands(toggle_descriptor)
            .expect("toggle diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "2 items left"));
        let test_row = find_first_tag_with_text(root, "div", "Test todo")
            .expect("dynamic todo row should rerender");
        let test_checkbox = test_row
            .children
            .iter()
            .find(|child| {
                matches!(&child.kind, UiNodeKind::Element { tag, .. } if tag == "button")
                    && child.children.iter().any(|grandchild| {
                        matches!(&grandchild.kind, UiNodeKind::Element { tag, .. } if tag == "div")
                    })
            })
            .expect("dynamic checkbox button should rerender");
        let checkbox_icon =
            find_first_tag(test_checkbox, "div").expect("dynamic checkbox icon should rerender");
        let icon_style = property_value(&toggle_batch, checkbox_icon.id, "style");
        assert!(
            icon_style
                .as_deref()
                .is_some_and(|value| value.contains("%2359A193") && value.contains("%233EA390")),
            "expected completed dynamic checkbox icon background, got style: {icon_style:?}"
        );
    }

    #[test]
    fn todo_mvc_real_file_single_remaining_item_uses_singular_label() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let toggle_all = find_first_tag_with_text(root, "button", "❯")
            .expect("toggle-all checkbox should render");
        let toggle_all_port = find_port(&init_batch, toggle_all.id, UiEventKind::Click)
            .expect("toggle-all checkbox should expose Click");

        let complete_all_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle-all batch should decode");
        let complete_all_batch = runtime
            .decode_commands(complete_all_descriptor)
            .expect("toggle-all diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) =
            &complete_all_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let clear_completed = find_first_tag_with_text(root, "button", "Clear completed")
            .expect("clear completed button should render");
        let clear_click_port =
            find_port(&complete_all_batch, clear_completed.id, UiEventKind::Click)
                .expect("clear completed button should expose Click");

        let clear_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: clear_click_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("clear completed batch should decode");

        let clear_batch = runtime
            .decode_commands(clear_descriptor)
            .expect("clear completed diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &clear_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let input = find_first_tag(root, "input").expect("todo_mvc should still render the input");
        let input_port = find_port(&clear_batch, input.id, UiEventKind::Input)
            .expect("input should expose Input after clearing");
        let key_port = find_port(&clear_batch, input.id, UiEventKind::KeyDown)
            .expect("input should expose KeyDown after clearing");

        runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: input_port,
                    kind: UiEventKind::Input,
                    payload: Some("Buy milk".to_string()),
                }],
            }))
            .expect("input batch should decode");

        let add_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: key_port,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter".to_string()),
                }],
            }))
            .expect("add batch should decode");
        let add_batch = runtime
            .decode_commands(add_descriptor)
            .expect("add diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &add_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(
            tree_contains_text(root, "1 item left"),
            "expected singular label after re-adding one todo, got {}",
            subtree_text(root)
        );
    }

    #[test]
    fn todo_mvc_real_file_toggle_all_icon_updates_dynamic_color() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let toggle_all = find_first_tag_with_text(root, "button", "❯")
            .expect("toggle-all checkbox should render");
        let toggle_all_icon = find_first_tag_with_text(toggle_all, "div", "❯")
            .expect("toggle-all icon container should render");
        let init_style = property_value(&init_batch, toggle_all_icon.id, "style");
        assert!(
            init_style
                .as_deref()
                .is_some_and(|value| value.contains("color:oklch(66.7% 0 0)")),
            "expected initial dynamic Oklch color, got style: {init_style:?}"
        );

        let toggle_all_port = find_port(&init_batch, toggle_all.id, UiEventKind::Click)
            .expect("toggle-all checkbox should expose Click");
        let toggle_all_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: toggle_all_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("toggle-all batch should decode");
        let toggle_all_batch = runtime
            .decode_commands(toggle_all_descriptor)
            .expect("toggle-all diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &toggle_all_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let toggle_all = find_first_tag_with_text(root, "button", "❯")
            .expect("toggle-all checkbox should rerender");
        let toggle_all_icon = find_first_tag_with_text(toggle_all, "div", "❯")
            .expect("toggle-all icon container should rerender");
        let toggle_style = property_value(&toggle_all_batch, toggle_all_icon.id, "style");
        assert!(
            toggle_style
                .as_deref()
                .is_some_and(|value| value.contains("color:oklch(40% 0 0)")),
            "expected toggled dynamic Oklch color, got style: {toggle_style:?}"
        );
    }

    #[test]
    fn todo_mvc_real_file_selected_filter_applies_outline_style() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let active_button = find_first_tag_with_text(root, "button", "Active")
            .expect("Active button should render");
        assert!(
            !property_value(&init_batch, active_button.id, "style")
                .is_some_and(|value| value.contains("outline:1px solid"))
        );
        let active_port = find_port(&init_batch, active_button.id, UiEventKind::Click)
            .expect("Active button should expose Click");

        let active_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: active_port,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }))
            .expect("active batch should decode");
        let active_batch = runtime
            .decode_commands(active_descriptor)
            .expect("active diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &active_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let active_button = find_first_tag_with_text(root, "button", "Active")
            .expect("Active button should rerender");
        let active_style = property_value(&active_batch, active_button.id, "style");
        assert!(
            active_style
                .as_deref()
                .is_some_and(|value| value.contains("outline:1px solid")),
            "expected Active button outline after selecting filter, got style: {active_style:?}"
        );
    }

    #[test]
    fn todo_mvc_real_file_focus_toggles_new_todo_outline() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let row = find_first_tag_with_child_tag(root, "div", "input")
            .expect("new todo row should render around the input");
        let input = find_first_tag(root, "input").expect("input should render");

        assert!(
            property_value(&init_batch, row.id, "style")
                .is_some_and(|value| value.contains("outline:1px solid"))
        );

        let blur_port =
            find_port(&init_batch, input.id, UiEventKind::Blur).expect("input should expose Blur");

        let blur_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: blur_port,
                    kind: UiEventKind::Blur,
                    payload: None,
                }],
            }))
            .expect("blur batch should decode");
        let blur_batch = runtime
            .decode_commands(blur_descriptor)
            .expect("blur diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &blur_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let row = find_first_tag_with_child_tag(root, "div", "input")
            .expect("new todo row should rerender around the input");
        let input = find_first_tag(root, "input").expect("input should rerender");

        assert!(
            !property_value(&blur_batch, row.id, "style")
                .is_some_and(|value| value.contains("outline:1px solid"))
        );

        let focus_port = find_port(&blur_batch, input.id, UiEventKind::Focus)
            .expect("input should expose Focus");

        let focus_descriptor = runtime
            .dispatch_events(&encode_ui_event_batch(&UiEventBatch {
                events: vec![UiEvent {
                    target: focus_port,
                    kind: UiEventKind::Focus,
                    payload: None,
                }],
            }))
            .expect("focus batch should decode");
        let focus_batch = runtime
            .decode_commands(focus_descriptor)
            .expect("focus diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &focus_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let row = find_first_tag_with_child_tag(root, "div", "input")
            .expect("new todo row should rerender around the input");

        assert!(
            property_value(&focus_batch, row.id, "style")
                .is_some_and(|value| value.contains("outline:1px solid"))
        );
    }

    #[test]
    fn todo_mvc_real_file_hover_underlines_footer_link() {
        let semantic = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let footer_link =
            find_first_tag_with_text(root, "a", "Martin Kavík").expect("footer link should render");
        assert!(!batch_contains_property_fragment(
            &init_batch,
            "style",
            "text-decoration:underline"
        ));

        let hover_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: footer_link.id,
                    kind: UiFactKind::Hovered(true),
                }],
            }))
            .expect("hover fact should decode");
        let hover_batch = runtime
            .decode_commands(hover_descriptor)
            .expect("hover diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &hover_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(batch_contains_property_fragment(
            &hover_batch,
            "style",
            "text-decoration:underline"
        ));

        let footer_link = find_first_tag_with_text(root, "a", "Martin Kavík")
            .expect("footer link should rerender");

        let unhover_descriptor = runtime
            .apply_facts(&encode_ui_fact_batch(&UiFactBatch {
                facts: vec![UiFact {
                    id: footer_link.id,
                    kind: UiFactKind::Hovered(false),
                }],
            }))
            .expect("hover false fact should decode");
        let unhover_batch = runtime
            .decode_commands(unhover_descriptor)
            .expect("hover false diff should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &unhover_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(!batch_contains_property_fragment(
            &unhover_batch,
            "style",
            "text-decoration:underline"
        ));
    }
}
