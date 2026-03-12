use std::collections::{BTreeMap, BTreeSet, HashMap};

use boon_scene::{RenderDiffBatch, UiEventBatch, UiFactBatch};

use super::abi::{
    BufferRange, decode_render_diff_batch, decode_ui_event_batch, decode_ui_fact_batch,
    encode_render_diff_batch,
};
use super::codegen;
use super::exec_ir::ExecProgram;
use super::semantic_ir::{
    DerivedArithmeticOp, DerivedScalarOperand, DerivedScalarSpec, IntCompareOp, ItemScalarUpdate,
    ItemTextUpdate, ObjectDerivedScalarOperand, ObjectItemActionKind, ObjectItemActionSpec,
    ObjectListFilter, ObjectListItem, ObjectListUpdate, RuntimeModel, ScalarUpdate,
    SemanticAction, SemanticFactBinding, SemanticFactKind, SemanticInputValue, SemanticNode,
    SemanticProgram, SemanticStyleFragment, SemanticTextPart, StateRuntimeModel, TextListFilter,
    TextListUpdate, TextUpdate,
    bootstrap_runtime_scaffold,
};

const KEYDOWN_TEXT_SEPARATOR: char = '\u{1F}';

#[derive(Debug, Default)]
pub struct WasmProRuntime {
    memory: Vec<u8>,
    pending_commands: Option<BufferRange>,
    event_history: Vec<UiEventBatch>,
    fact_history: Vec<UiFactBatch>,
    source_len: usize,
    external_functions: usize,
    persistence_enabled: bool,
    draft_text: String,
    focused: bool,
    active_program: Option<SemanticProgram>,
    active_actions: HashMap<boon_scene::EventPortId, SemanticAction>,
    active_input_bindings: HashMap<boon_scene::EventPortId, String>,
    active_input_nodes: HashMap<boon_scene::NodeId, String>,
    active_fact_bindings: HashMap<(boon_scene::NodeId, SemanticFactKind), String>,
    scalar_values: BTreeMap<String, i64>,
    text_values: BTreeMap<String, String>,
    text_lists: BTreeMap<String, Vec<String>>,
    object_lists: BTreeMap<String, Vec<ObjectListItem>>,
    input_texts: BTreeMap<String, String>,
    derived_scalars: Vec<DerivedScalarSpec>,
    next_object_item_id: u64,
}

impl WasmProRuntime {
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
        self.active_program = Some(SemanticProgram {
            root: program.semantic_root.clone(),
            runtime: program.runtime.clone(),
        });
        self.scalar_values = match &program.runtime {
            RuntimeModel::Scalars(model) => model.values.clone(),
            RuntimeModel::State(StateRuntimeModel {
                scalar_values,
                text_values,
                text_lists,
                object_lists,
                input_texts,
                derived_scalars,
            }) => {
                self.text_values = text_values.clone();
                self.text_lists = text_lists.clone();
                self.object_lists = object_lists.clone();
                self.input_texts = input_texts.clone();
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
            self.derived_scalars.clear();
        }
        self.next_object_item_id = self
            .object_lists
            .values()
            .flatten()
            .map(|item| item.id)
            .max()
            .unwrap_or_default()
            + 1;
        self.refresh_derived_scalars();
        self.render_active_program()
    }

    pub fn dispatch_events(&mut self, bytes: &[u8]) -> Result<u64, serde_json::Error> {
        let batch = decode_ui_event_batch(bytes)?;
        if self.active_program.is_some() {
            let mut changed = false;
            for event in &batch.events {
                if let Some(action) = self.active_actions.get(&event.target).cloned() {
                    changed |= self.apply_action(action, event);
                }
            }
            if changed {
                self.refresh_derived_scalars();
            }
            self.apply_event_effects(&batch);
            self.event_history.push(batch);
            return Ok(if changed {
                self.render_active_program()
            } else {
                0
            });
        }
        self.apply_event_effects(&batch);
        self.event_history.push(batch);
        Ok(self.queue_commands(self.status_batch()))
    }

    pub fn apply_facts(&mut self, bytes: &[u8]) -> Result<u64, serde_json::Error> {
        let batch = decode_ui_fact_batch(bytes)?;
        if self.active_program.is_some() {
            let changed = self.apply_fact_effects(&batch);
            self.fact_history.push(batch);
            return Ok(if changed {
                self.render_active_program()
            } else {
                0
            });
        }
        self.apply_fact_effects(&batch);
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
        self.queue_commands(codegen::emit_render_batch(&exec))
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
                if tag == "input" {
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
                        let value = self
                            .input_texts
                            .get(source_binding)
                            .cloned()
                            .or_else(|| {
                                properties
                                    .iter()
                                    .find(|(name, _)| name == "value")
                                    .map(|(_, value)| value.clone())
                            })
                            .unwrap_or_default();
                        if let Some((_, property_value)) =
                            properties.iter_mut().find(|(name, _)| name == "value")
                        {
                            *property_value = value;
                        } else {
                            properties.push(("value".to_string(), value));
                        }
                    }
                }
                let next_element_scope = local_element_scope(path, None, fact_bindings)
                    .or_else(|| current_element_scope.map(ToString::to_string));
                merge_style_fragments(
                    &mut properties,
                    style_fragments,
                    &self.scalar_values,
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
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
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
                    .enumerate()
                    .map(|(index, item)| {
                        let child_path = child_path(path, index);
                        self.materialize_object_template(
                            binding,
                            &item,
                            item_actions,
                            template.as_ref(),
                            &child_path,
                            None,
                        )
                    })
                    .collect(),
            ),
        }
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
                            let entry = self.scalar_values.entry(binding).or_default();
                            if *entry != value {
                                *entry = value;
                                changed = true;
                            }
                        }
                        ScalarUpdate::Add { binding, delta } => {
                            let entry = self.scalar_values.entry(binding).or_default();
                            let next = *entry + delta;
                            if *entry != next {
                                *entry = next;
                                changed = true;
                            }
                        }
                        ScalarUpdate::ToggleBool { binding } => {
                            let entry = self.scalar_values.entry(binding).or_default();
                            let next = if *entry == 0 { 1 } else { 0 };
                            if *entry != next {
                                *entry = next;
                                changed = true;
                            }
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
                            let entry = self.text_values.entry(binding).or_default();
                            if *entry != value {
                                *entry = value;
                                changed = true;
                            }
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
                            let next = self
                                .input_texts
                                .get(&source_binding)
                                .cloned()
                                .unwrap_or_default();
                            let entry = self.text_values.entry(binding).or_default();
                            if *entry != next {
                                *entry = next;
                                changed = true;
                            }
                        }
                        TextUpdate::SetFromPayload { binding } => {
                            let next = event_primary_payload(event).unwrap_or_default().to_string();
                            let entry = self.text_values.entry(binding).or_default();
                            if *entry != next {
                                *entry = next;
                                changed = true;
                            }
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
                            let mut title = event_primary_payload(event).unwrap_or_default().to_string();
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
                        ObjectListUpdate::RemoveMatching { binding, filter } => {
                            let items = self.object_lists.entry(binding).or_default();
                            let before = items.len();
                            items.retain(|item| {
                                !object_list_item_matches_filter(item, &filter, &self.scalar_values)
                            });
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
                SemanticTextPart::ScalarBinding(binding) => output.push_str(
                    &self
                        .scalar_values
                        .get(binding)
                        .copied()
                        .unwrap_or_default()
                        .to_string(),
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
                SemanticTextPart::ObjectBoolFieldText { .. } => {}
            }
        }
        output
    }

    fn render_input_value(&self, value: &SemanticInputValue) -> String {
        match value {
            SemanticInputValue::Static(value) => value.clone(),
            SemanticInputValue::TextParts { parts, .. } => self.render_text_parts(parts),
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

    fn apply_event_effects(&mut self, batch: &UiEventBatch) {
        if let Some(event) = batch.events.last() {
            match event.kind {
                boon_scene::UiEventKind::Input | boon_scene::UiEventKind::Change => {
                    self.draft_text = event_primary_payload(event).unwrap_or_default().to_string();
                    if let Some(source_binding) = self.active_input_bindings.get(&event.target) {
                        self.input_texts
                            .insert(source_binding.clone(), self.draft_text.clone());
                    }
                }
                boon_scene::UiEventKind::KeyDown => {
                    if let Some(text) = event_keydown_text(event) {
                        self.draft_text = text.to_string();
                        if let Some(source_binding) = self.active_input_bindings.get(&event.target) {
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
                        let entry = self.input_texts.entry(source_binding).or_default();
                        if *entry != *text {
                            *entry = text.clone();
                            changed = true;
                        }
                    }
                }
                boon_scene::UiFactKind::Focused(focused) => {
                    if self.focused != *focused {
                        self.focused = *focused;
                        changed = true;
                    }
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
                    DerivedScalarSpec::Arithmetic {
                        target: _,
                        op,
                        left,
                        right,
                    } => {
                        let left = self.derived_scalar_operand_value(left);
                        let right = self.derived_scalar_operand_value(right);
                        match op {
                            DerivedArithmeticOp::Add => left + right,
                            DerivedArithmeticOp::Subtract => left - right,
                        }
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
                };
                let target = match spec {
                    DerivedScalarSpec::TextListCount { target, .. }
                    | DerivedScalarSpec::ObjectListCount { target, .. }
                    | DerivedScalarSpec::Arithmetic { target, .. }
                    | DerivedScalarSpec::Comparison { target, .. } => target,
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
            DerivedScalarOperand::Literal(value) => *value,
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
                .filter(|item| object_list_item_matches_filter(item, filter, &self.scalar_values))
                .collect(),
        }
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
                if tag == "input" {
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
                            scope_object_source_binding(
                                Some(source_binding),
                                list_binding,
                                item.id,
                            )
                        })
                    {
                        let value = self
                            .input_texts
                            .get(&source_binding)
                            .cloned()
                            .or_else(|| {
                                properties
                                    .iter()
                                    .find(|(name, _)| name == "value")
                                    .map(|(_, value)| value.clone())
                            })
                            .unwrap_or_default();
                        if let Some((_, property_value)) =
                            properties.iter_mut().find(|(name, _)| name == "value")
                        {
                            *property_value = value;
                        } else {
                            properties.push(("value".to_string(), value));
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
                                .and_then(|source| source.strip_prefix("__item__."));
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
                                    ObjectItemActionKind::RemoveSelf => {
                                        Some(ObjectListUpdate::RemoveItem {
                                            binding: list_binding.to_string(),
                                            item_id: item.id,
                                        })
                                    }
                                    ObjectItemActionKind::UpdateBindings { .. } => None,
                                })
                                .collect::<Vec<_>>();
                            let mut actions = Vec::new();
                            if !object_updates.is_empty() {
                                actions.push(SemanticAction::UpdateObjectLists {
                                    updates: object_updates,
                                });
                            }
                            for spec in matched {
                                if let ObjectItemActionKind::UpdateBindings {
                                    scalar_updates,
                                    text_updates,
                                    payload_filter,
                                } = &spec.action
                                {
                                    if !scalar_updates.is_empty() {
                                        actions.push(SemanticAction::UpdateScalars {
                                            updates: scalar_updates
                                                .iter()
                                                .map(|update| match update {
                                                    ItemScalarUpdate::SetStatic { binding, value } => {
                                                        ScalarUpdate::Set {
                                                            binding: binding.clone(),
                                                            value: *value,
                                                        }
                                                    }
                                                    ItemScalarUpdate::SetFromField { binding, field } => {
                                                        ScalarUpdate::Set {
                                                            binding: binding.clone(),
                                                            value: self.object_item_field_scalar_value(item, field),
                                                        }
                                                    }
                                                })
                                                .collect(),
                                        });
                                    }
                                    if !text_updates.is_empty() {
                                        actions.push(SemanticAction::UpdateTexts {
                                            updates: text_updates
                                                .iter()
                                                .map(|update| match update {
                                                    ItemTextUpdate::SetStatic { binding, value } => TextUpdate::SetStatic {
                                                        binding: binding.clone(),
                                                        value: value.clone(),
                                                        payload_filter: payload_filter.clone(),
                                                    },
                                                    ItemTextUpdate::SetFromField { binding, field } => TextUpdate::SetStatic {
                                                        binding: binding.clone(),
                                                        value: self.object_item_field_text_value(item, field),
                                                        payload_filter: payload_filter.clone(),
                                                    },
                                                    ItemTextUpdate::SetFromPayload { binding } => TextUpdate::SetFromPayload {
                                                        binding: binding.clone(),
                                                    },
                                                    ItemTextUpdate::SetFromInputSource {
                                                        binding,
                                                        source_suffix,
                                                    } => TextUpdate::SetFromInput {
                                                        binding: binding.clone(),
                                                        source_binding: scope_object_source_binding(
                                                            Some(&format!("__item__.{source_suffix}")),
                                                            list_binding,
                                                            item.id,
                                                        )
                                                        .unwrap_or_else(|| {
                                                            format!(
                                                                "{}.{}",
                                                                object_item_scope(list_binding, item.id),
                                                                source_suffix
                                                            )
                                                        }),
                                                        payload_filter: payload_filter.clone(),
                                                    },
                                                })
                                                .collect(),
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
                let branch =
                    if self.object_scalar_compare_matches(item, left, op.clone(), right) {
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
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
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
                let nested_list_binding = format!("{}.{}", object_item_scope(list_binding, item.id), field);
                let nested_items = item.object_lists.get(field).cloned().unwrap_or_default();
                let nested_items = match filter {
                    None => nested_items,
                    Some(filter) => nested_items
                        .into_iter()
                        .filter(|nested_item| {
                            object_list_item_matches_filter(nested_item, filter, &self.scalar_values)
                        })
                        .collect(),
                };
                SemanticNode::Fragment(
                    nested_items
                        .into_iter()
                        .enumerate()
                        .map(|(index, nested_item)| {
                            let child_path = child_path(path, index);
                            self.materialize_object_template(
                                &nested_list_binding,
                                &nested_item,
                                item_actions,
                                template.as_ref(),
                                &child_path,
                                current_element_scope,
                            )
                        })
                        .collect(),
                )
            }
        }
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
                SemanticTextPart::ScalarBinding(binding) => output.push_str(
                    &self
                        .scalar_values
                        .get(binding)
                        .copied()
                        .unwrap_or_default()
                        .to_string(),
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
            SemanticInputValue::TextParts { parts, .. } => self.render_object_text_parts(item, parts),
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
            ObjectDerivedScalarOperand::Field(field) => self.object_item_field_scalar_value(item, field),
            ObjectDerivedScalarOperand::Literal(value) => *value,
        }
    }

    fn object_item_field_text_value(&self, item: &ObjectListItem, field: &str) -> String {
        match field {
            "formula_text" => self
                .cell_position(item)
                .map_or_else(String::new, |(column, row)| self.cell_formula_text(column, row)),
            "display_value" => self
                .cell_position(item)
                .map_or_else(String::new, |(column, row)| self.cell_display_value_text(column, row)),
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

    fn cell_value_at(
        &self,
        column: i64,
        row: i64,
        visited: &mut BTreeSet<(i64, i64)>,
    ) -> i64 {
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
    Some(format!("{}.{suffix}", object_item_scope(list_binding, item_id)))
}

fn merge_style_fragments(
    properties: &mut Vec<(String, String)>,
    fragments: &[SemanticStyleFragment],
    scalar_values: &BTreeMap<String, i64>,
    current_element_scope: Option<&str>,
    item: Option<&ObjectListItem>,
) {
    for fragment in fragments {
        if let Some(style) =
            evaluate_style_fragment(fragment, scalar_values, current_element_scope, item)
        {
            merge_style_property(properties, &style);
        }
    }
}

fn evaluate_style_fragment(
    fragment: &SemanticStyleFragment,
    scalar_values: &BTreeMap<String, i64>,
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
            evaluate_style_fragment(branch, scalar_values, current_element_scope, item)
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
                current_element_scope,
            ) {
                truthy
            } else {
                falsy
            };
            evaluate_style_fragment(branch, scalar_values, current_element_scope, item)
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
            evaluate_style_fragment(branch, scalar_values, current_element_scope, item)
        }
    }
}

fn style_scalar_compare_matches(
    left: &DerivedScalarOperand,
    op: IntCompareOp,
    right: &DerivedScalarOperand,
    scalar_values: &BTreeMap<String, i64>,
    current_element_scope: Option<&str>,
) -> bool {
    let left = style_scalar_operand_value(left, scalar_values, current_element_scope);
    let right = style_scalar_operand_value(right, scalar_values, current_element_scope);
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
    current_element_scope: Option<&str>,
) -> i64 {
    match operand {
        DerivedScalarOperand::Literal(value) => *value,
        DerivedScalarOperand::Binding(binding) => {
            let binding = scope_element_binding(binding, current_element_scope);
            scalar_values.get(&binding).copied().unwrap_or_default()
        }
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

fn object_list_item_matches_filter(
    item: &ObjectListItem,
    filter: &ObjectListFilter,
    scalar_values: &BTreeMap<String, i64>,
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
    }
}

pub fn bootstrap_runtime(
    source: &str,
    external_functions: usize,
    persistence_enabled: bool,
) -> WasmProRuntime {
    WasmProRuntime::new(source.len(), external_functions, persistence_enabled)
}

#[cfg(test)]
mod tests {
    use boon_scene::{
        EventPortId, RenderDiffBatch, RenderRoot, UiEvent, UiEventBatch, UiEventKind, UiFact,
        UiFactBatch, UiFactKind, UiNodeKind,
    };

    use super::{WasmProRuntime, bootstrap_runtime};
    use crate::platform::browser::engine_wasm_pro::abi::{
        encode_ui_event_batch, encode_ui_fact_batch,
    };
    use crate::platform::browser::engine_wasm_pro::exec_ir::ExecProgram;
    use crate::platform::browser::engine_wasm_pro::lower::lower_to_semantic;
    use crate::platform::browser::engine_wasm_pro::semantic_ir::{
        RuntimeModel, ScalarRuntimeModel, ScalarUpdate, SemanticAction, SemanticEventBinding,
        SemanticNode, SemanticProgram, SemanticTextPart,
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

    fn find_nth_port(batch: &RenderDiffBatch, kind: UiEventKind, ordinal: usize) -> Option<EventPortId> {
        batch.ops
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
        batch.ops
            .iter()
            .filter_map(|op| match op {
                boon_scene::RenderOp::AttachEventPort {
                    id,
                    kind: op_kind,
                    ..
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

    #[test]
    fn init_queues_replace_root_batch() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "section",
                Some("WasmPro runtime scaffold".to_string()),
                Vec::new(),
                Vec::new(),
                vec![SemanticNode::text("child")],
            ),
            runtime: RuntimeModel::Static,
        });
        let mut runtime = WasmProRuntime::new(12, 0, false);

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
        assert_eq!(text.as_deref(), Some("WasmPro runtime scaffold"));
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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(4, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
                "../../../../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/checkbox_test.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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

        assert_eq!(property_value(&init_batch, source_input_id, "value"), Some("Draft"));
        assert_eq!(property_value(&init_batch, first_row_input_id, "value"), Some("Draft"));
        assert_eq!(property_value(&init_batch, second_row_input_id, "value"), Some("=A1"));

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
                "../../../../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
                "../../../../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
            ),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(tree_contains_text(root, "2 items left"));
        assert!(tree_contains_text(root, "Buy groceries"));
        assert!(tree_contains_text(root, "Clean room"));

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
    fn cells_real_file_runtime_initializes_grid_headers_and_rows() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
    }

    #[test]
    fn cells_real_file_double_click_enters_edit_mode() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());

        let display_cell = find_first_tag_with_text(root, "span", ".")
            .or_else(|| find_first_tag_with_text(root, "div", "."))
            .expect("cells grid should render a display label");
        let double_click_port = find_port(&init_batch, display_cell.id, UiEventKind::DoubleClick)
            .expect("display cell should expose DoubleClick");

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_some());
    }

    #[test]
    fn cells_real_file_change_and_enter_commits_value() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let display_cell = find_first_tag_with_text(root, "span", ".")
            .or_else(|| find_first_tag_with_text(root, "div", "."))
            .expect("cells grid should render a display label");
        let double_click_port = find_port(&init_batch, display_cell.id, UiEventKind::DoubleClick)
            .expect("display cell should expose DoubleClick");

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
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
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &init_batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        let double_click_port =
            find_nth_port(&init_batch, UiEventKind::DoubleClick, 0).expect("A1 should expose DoubleClick");

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
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
        assert_eq!(input_descriptor, 0);
    }

    #[test]
    fn cells_real_file_recomputes_formula_cells_after_edit() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
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

        assert!(tree_contains_text(root, "7"));
        assert!(tree_contains_text(root, "17"));
        assert!(tree_contains_text(root, "32"));
    }

    #[test]
    fn cells_real_file_escape_cancels_edit_without_commit() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port =
            find_nth_port(&init_batch, UiEventKind::DoubleClick, 0).expect("A1 should expose DoubleClick");

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
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
        assert!(tree_contains_text(root, "5"));
        assert!(!tree_contains_text(root, "1234"));
    }

    #[test]
    fn cells_real_file_blur_exits_edit_without_commit() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port =
            find_nth_port(&init_batch, UiEventKind::DoubleClick, 0).expect("A1 should expose DoubleClick");

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &blur_batch.ops[0]
        else {
            panic!("expected ReplaceRoot(UiTree)");
        };

        assert!(find_first_tag(root, "input").is_none());
        assert!(tree_contains_text(root, "5"));
        assert!(!tree_contains_text(root, "2345"));
    }

    #[test]
    fn cells_real_file_formula_commit_updates_formula_cell() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

        let init_descriptor = runtime.init(&exec);
        let init_batch = runtime
            .decode_commands(init_descriptor)
            .expect("init batch should decode");
        let double_click_port =
            find_nth_port(&init_batch, UiEventKind::DoubleClick, 1).expect("B1 should expose DoubleClick");

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
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(root)) = &edit_batch.ops[0]
        else {
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
    fn todo_mvc_real_file_runtime_edits_title() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
    fn todo_mvc_real_file_toggle_all_icon_updates_dynamic_color() {
        let semantic = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let exec = ExecProgram::from_semantic(&semantic);
        let mut runtime = WasmProRuntime::new(0, 0, false);

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
