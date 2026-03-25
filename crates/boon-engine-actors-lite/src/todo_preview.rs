use crate::bridge::{HostInput, HostSnapshot};
use crate::filtered_list_view::filtered_list_with_filter;
use crate::ids::ActorId;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::{
    FunctionInstanceId, MirrorCellId, RetainedNodeKey, SinkPortId, SourcePortId, ViewSiteId,
};
use crate::ir_executor::IrExecutor;
use crate::lower::{TodoProgram, try_lower_todo_mvc};
use crate::metrics::{InteractionMetricsReport, LatencySummary};
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::ActorKind;
use crate::runtime::RuntimeTelemetrySnapshot;
use crate::runtime_backed_domain::RuntimeBackedDomain;
use crate::runtime_backed_preview::RuntimeBackedPreviewState;
use crate::targeted_list_runtime::TargetedListRuntime;
use crate::text_input::{DecodedKeyDown, KEYDOWN_TEXT_SEPARATOR, decode_key_down_payload};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    NodeId, RenderOp, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
};
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TodoItem {
    title: String,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PortPayload {
    Click,
    Focus,
    Text(String),
    KeyDown {
        key: String,
        current_text: Option<String>,
    },
    TargetId(u64),
    IdTitle {
        target_id: u64,
        title: String,
    },
    HoverState {
        target_id: u64,
        hovered: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextInputTarget {
    MainInput,
    EditInput(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TodoUiAction {
    MainInputChange,
    MainInputKeyDown,
    MainInputBlur,
    MainInputFocus,
    FilterAll,
    FilterActive,
    FilterCompleted,
    ToggleAll,
    ClearCompleted,
    TodoCheckbox(u64),
    TodoTitleDoubleClick(u64),
    TodoDelete(u64),
    TodoEditChange(u64),
    TodoEditKeyDown(u64),
    TodoEditBlur(u64),
    TodoEditFocus(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FactTarget {
    MainInput,
    TodoTitle(u64),
    TodoEditInput(u64),
}

pub struct TodoPreview {
    program: TodoProgram,
    ui_state_runtime: PreviewRuntime,
    ui_state_actor: ActorId,
    ui_state_executor: IrExecutor,
    todos: TargetedListRuntime<TodoItem>,
    ui: RuntimeBackedPreviewState<TodoUiAction, FactTarget>,
}

impl InteractivePreview for TodoPreview {
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

impl RuntimeBackedDomain for TodoPreview {
    type Action = TodoUiAction;
    type FactTarget = FactTarget;

    fn preview_state(&mut self) -> &mut RuntimeBackedPreviewState<Self::Action, Self::FactTarget> {
        &mut self.ui
    }

    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        TodoPreview::render_document(self, ops)
    }

    fn handle_event(
        &mut self,
        action: Self::Action,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        TodoPreview::apply_event(self, action, kind, payload)
    }

    fn handle_fact(&mut self, target: Self::FactTarget, kind: UiFactKind) -> bool {
        TodoPreview::apply_fact(self, target, kind)
    }

    fn fact_cell(target: &Self::FactTarget) -> MirrorCellId {
        todo_fact_cell(target)
    }

    fn fact_target(cell: MirrorCellId) -> Option<Self::FactTarget> {
        todo_fact_target(cell)
    }
}

impl TodoPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_todo_mvc(source)?;
        let mut ui_state_runtime = PreviewRuntime::new();
        let ui_state_actor = ui_state_runtime.alloc_actor(ActorKind::SourcePort);
        let ui_state_executor = IrExecutor::new(program.ir.clone())?;
        Ok(Self {
            program,
            ui_state_runtime,
            ui_state_actor,
            ui_state_executor,
            todos: TargetedListRuntime::new(
                [
                    (
                        1,
                        TodoItem {
                            title: "Buy groceries".to_string(),
                            completed: false,
                        },
                    ),
                    (
                        2,
                        TodoItem {
                            title: "Clean room".to_string(),
                            completed: false,
                        },
                    ),
                ],
                3,
            ),
            ui: RuntimeBackedPreviewState::default(),
        })
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        RuntimeBackedDomain::preview_text(self)
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        RuntimeBackedDomain::dispatch_ui_events(self, batch)
    }

    #[must_use]
    pub fn runtime_telemetry_snapshot(&self) -> RuntimeTelemetrySnapshot {
        self.ui_state_runtime.telemetry_snapshot()
    }

    pub fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        RuntimeBackedDomain::dispatch_ui_facts(self, batch)
    }

    fn apply_event(
        &mut self,
        action: TodoUiAction,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        match action {
            TodoUiAction::MainInputChange => {
                self.apply_text_input_event(TextInputTarget::MainInput, kind, payload)
            }
            TodoUiAction::MainInputKeyDown => {
                self.apply_text_input_event(TextInputTarget::MainInput, kind, payload)
            }
            TodoUiAction::MainInputBlur => {
                self.apply_text_input_event(TextInputTarget::MainInput, kind, payload)
            }
            TodoUiAction::MainInputFocus => {
                self.apply_text_input_event(TextInputTarget::MainInput, kind, payload)
            }
            TodoUiAction::FilterAll if kind == UiEventKind::Click => {
                self.apply_filter_click(TodoProgram::FILTER_ALL_PORT)
            }
            TodoUiAction::FilterActive if kind == UiEventKind::Click => {
                self.apply_filter_click(TodoProgram::FILTER_ACTIVE_PORT)
            }
            TodoUiAction::FilterCompleted if kind == UiEventKind::Click => {
                self.apply_filter_click(TodoProgram::FILTER_COMPLETED_PORT)
            }
            TodoUiAction::ToggleAll if kind == UiEventKind::Click => {
                self.apply_todo_list_action(TodoProgram::TOGGLE_ALL_PORT, PortPayload::Click)
            }
            TodoUiAction::ClearCompleted if kind == UiEventKind::Click => {
                self.apply_todo_list_action(TodoProgram::CLEAR_COMPLETED_PORT, PortPayload::Click)
            }
            TodoUiAction::TodoCheckbox(todo_id) if kind == UiEventKind::Click => self
                .apply_todo_list_action(
                    TodoProgram::TODO_TOGGLE_PORT,
                    PortPayload::TargetId(todo_id),
                ),
            TodoUiAction::TodoTitleDoubleClick(todo_id) if kind == UiEventKind::DoubleClick => {
                let before = self
                    .todos
                    .items()
                    .iter()
                    .map(|todo| todo.value.clone())
                    .collect::<Vec<_>>();
                let previous_main_input_focused = self.main_input_focused();
                let previous_main_input_hint = self.main_input_focus_hint();
                let previous_editing = (
                    self.edit_target_id(),
                    self.edit_draft(),
                    self.edit_focus_hint(),
                    self.edit_focused(),
                );
                let edit_draft = self
                    .todos
                    .find(todo_id)
                    .map(|todo| (todo.id, todo.value.title.clone()));
                let editing_changed = edit_draft.is_some_and(|(target_id, title)| {
                    self.apply_edit_session_action(
                        TodoProgram::TODO_BEGIN_EDIT_PORT,
                        PortPayload::IdTitle { target_id, title },
                    )
                });
                self.todos
                    .items()
                    .iter()
                    .map(|todo| todo.value.clone())
                    .collect::<Vec<_>>()
                    != before
                    || self.main_input_focused() != previous_main_input_focused
                    || self.main_input_focus_hint() != previous_main_input_hint
                    || editing_changed
                    || (
                        self.edit_target_id(),
                        self.edit_draft(),
                        self.edit_focus_hint(),
                        self.edit_focused(),
                    ) != previous_editing
            }
            TodoUiAction::TodoDelete(todo_id) if kind == UiEventKind::Click => self
                .apply_todo_list_action(
                    TodoProgram::TODO_DELETE_PORT,
                    PortPayload::TargetId(todo_id),
                ),
            TodoUiAction::TodoEditChange(todo_id) => {
                self.apply_text_input_event(TextInputTarget::EditInput(todo_id), kind, payload)
            }
            TodoUiAction::TodoEditKeyDown(todo_id) => {
                self.apply_text_input_event(TextInputTarget::EditInput(todo_id), kind, payload)
            }
            TodoUiAction::TodoEditBlur(todo_id) => {
                self.apply_text_input_event(TextInputTarget::EditInput(todo_id), kind, payload)
            }
            TodoUiAction::TodoEditFocus(todo_id) => {
                self.apply_text_input_event(TextInputTarget::EditInput(todo_id), kind, payload)
            }
            _ => false,
        }
    }

    fn apply_fact(&mut self, target: FactTarget, kind: UiFactKind) -> bool {
        match (target, kind) {
            (FactTarget::MainInput, kind) => {
                self.apply_text_input_fact(TextInputTarget::MainInput, kind)
            }
            (FactTarget::TodoTitle(todo_id), UiFactKind::Hovered(hovered)) => {
                self.apply_hover_fact(todo_id, hovered)
            }
            (FactTarget::TodoEditInput(todo_id), kind) => {
                self.apply_text_input_fact(TextInputTarget::EditInput(todo_id), kind)
            }
            _ => false,
        }
    }

    fn all_completed(&self) -> bool {
        if self.todos.is_empty() {
            return false;
        }
        match self.ui_state_sink(TodoProgram::ALL_COMPLETED_SINK) {
            Some(KernelValue::Bool(value)) => *value,
            _ => self.todos.all(|todo| todo.value.completed),
        }
    }

    fn active_count(&self) -> usize {
        match self.ui_state_sink(TodoProgram::ACTIVE_COUNT_SINK) {
            Some(KernelValue::Number(number)) if *number >= 0.0 => *number as usize,
            _ => self.todos.count(|todo| !todo.value.completed),
        }
    }

    fn completed_count(&self) -> usize {
        match self.ui_state_sink(TodoProgram::COMPLETED_COUNT_SINK) {
            Some(KernelValue::Number(number)) if *number >= 0.0 => *number as usize,
            _ => self.todos.count(|todo| todo.value.completed),
        }
    }

    fn visible_todo_ids(&self) -> Vec<u64> {
        filtered_list_with_filter(
            self.todos.list(),
            &self.selected_filter(),
            |filter, todo| match *filter {
                Filter::All => true,
                Filter::Active => !todo.value.completed,
                Filter::Completed => todo.value.completed,
            },
        )
        .ids()
    }

    fn selected_filter(&self) -> Filter {
        match self
            .ui_state_executor
            .sink_value(self.program.selected_filter_sink)
        {
            Some(KernelValue::Text(text)) if text == "active" => Filter::Active,
            Some(KernelValue::Text(text)) if text == "completed" => Filter::Completed,
            _ => Filter::All,
        }
    }

    fn ui_state_sink(&self, sink: SinkPortId) -> Option<&KernelValue> {
        self.ui_state_executor.sink_value(sink)
    }

    fn main_input_draft(&self) -> String {
        match self.ui_state_sink(TodoProgram::MAIN_INPUT_TEXT_SINK) {
            Some(KernelValue::Text(text)) => text.clone(),
            _ => String::new(),
        }
    }

    fn main_input_focused(&self) -> bool {
        match self.ui_state_sink(TodoProgram::MAIN_INPUT_FOCUSED_SINK) {
            Some(KernelValue::Bool(focused)) => *focused,
            _ => true,
        }
    }

    fn main_input_focus_hint(&self) -> bool {
        match self.ui_state_sink(TodoProgram::MAIN_INPUT_FOCUS_HINT_SINK) {
            Some(KernelValue::Bool(value)) => *value,
            _ => true,
        }
    }

    fn edit_target_id(&self) -> Option<u64> {
        match self.ui_state_sink(TodoProgram::EDIT_TARGET_SINK) {
            Some(KernelValue::Number(number)) if *number >= 0.0 => Some(*number as u64),
            _ => None,
        }
    }

    fn edit_draft(&self) -> String {
        match self.ui_state_sink(TodoProgram::EDIT_DRAFT_SINK) {
            Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text)) => text.clone(),
            _ => String::new(),
        }
    }

    fn edit_focus_hint(&self) -> bool {
        match self.ui_state_sink(TodoProgram::EDIT_FOCUS_HINT_SINK) {
            Some(KernelValue::Bool(value)) => *value,
            _ => false,
        }
    }

    fn edit_focused(&self) -> bool {
        match self.ui_state_sink(TodoProgram::EDIT_FOCUSED_SINK) {
            Some(KernelValue::Bool(value)) => *value,
            _ => false,
        }
    }

    fn edit_session_state(&self) -> (Option<u64>, String, bool, bool) {
        (
            self.edit_target_id(),
            self.edit_draft(),
            self.edit_focus_hint(),
            self.edit_focused(),
        )
    }

    fn edit_focus_state(&self) -> (bool, bool, bool) {
        (
            self.edit_focused(),
            self.edit_focus_hint(),
            self.main_input_focused(),
        )
    }

    fn main_input_focus_state(&self) -> (bool, bool) {
        (self.main_input_focused(), self.main_input_focus_hint())
    }

    fn selected_filter_with_focus_hint(&self) -> (Filter, bool) {
        (self.selected_filter(), self.main_input_focus_hint())
    }

    fn hovered_todo_id(&self) -> Option<u64> {
        match self.ui_state_sink(TodoProgram::HOVERED_TARGET_SINK) {
            Some(KernelValue::Number(number)) if *number >= 0.0 => Some(*number as u64),
            _ => None,
        }
    }

    fn apply_ui_state_messages(&mut self, messages: Vec<(ActorId, crate::runtime::Msg)>) {
        self.ui_state_executor
            .apply_messages(&messages)
            .expect("todo ui-state IR should execute");
    }

    fn push_text_input_inputs(
        &mut self,
        inputs: &mut Vec<HostInput>,
        cell: MirrorCellId,
        port: SourcePortId,
        text: String,
    ) {
        inputs.push(HostInput::Mirror {
            actor: self.ui_state_actor,
            cell,
            value: KernelValue::from(text.clone()),
            seq: self.ui_state_runtime.causal_seq(inputs.len() as u32),
        });
        inputs.push(HostInput::Pulse {
            actor: self.ui_state_actor,
            port,
            value: Self::port_payload_value(PortPayload::Text(text)),
            seq: self.ui_state_runtime.causal_seq(inputs.len() as u32),
        });
    }

    fn push_pulse_input(
        &mut self,
        inputs: &mut Vec<HostInput>,
        port: SourcePortId,
        value: KernelValue,
    ) {
        inputs.push(HostInput::Pulse {
            actor: self.ui_state_actor,
            port,
            value,
            seq: self.ui_state_runtime.causal_seq(inputs.len() as u32),
        });
    }

    fn dispatch_text_input_inputs_and_observe<T: PartialEq>(
        &mut self,
        target: TextInputTarget,
        text: String,
        mut extra_inputs: Vec<HostInput>,
        pulse: Option<(SourcePortId, PortPayload)>,
        observe: fn(&Self) -> T,
        sync_todos: bool,
    ) -> bool {
        let (cell, port) = Self::text_input_ports(target);
        let mut inputs = Vec::new();
        self.push_text_input_inputs(&mut inputs, cell, port, text);
        inputs.append(&mut extra_inputs);
        if let Some((port, payload)) = pulse {
            self.push_pulse_input(&mut inputs, port, Self::port_payload_value(payload));
        }
        self.dispatch_inputs_and_observe(inputs, observe, sync_todos)
    }

    fn text_input_ports(target: TextInputTarget) -> (MirrorCellId, SourcePortId) {
        match target {
            TextInputTarget::MainInput => (
                TodoProgram::MAIN_INPUT_DRAFT_CELL,
                TodoProgram::MAIN_INPUT_CHANGE_PORT,
            ),
            TextInputTarget::EditInput(_) => (
                TodoProgram::EDIT_TITLE_CELL,
                TodoProgram::TODO_EDIT_CHANGE_PORT,
            ),
        }
    }

    fn text_input_focus_ports(target: TextInputTarget) -> (SourcePortId, SourcePortId) {
        match target {
            TextInputTarget::MainInput => (
                TodoProgram::MAIN_INPUT_FOCUS_PORT,
                TodoProgram::MAIN_INPUT_BLUR_PORT,
            ),
            TextInputTarget::EditInput(_) => (
                TodoProgram::TODO_EDIT_FOCUS_PORT,
                TodoProgram::TODO_EDIT_BLUR_PORT,
            ),
        }
    }

    fn text_input_is_active(&self, target: TextInputTarget) -> bool {
        match target {
            TextInputTarget::MainInput => true,
            TextInputTarget::EditInput(todo_id) => self.edit_target_id() == Some(todo_id),
        }
    }

    fn text_input_draft(&self, target: TextInputTarget) -> String {
        match target {
            TextInputTarget::MainInput => self.main_input_draft(),
            TextInputTarget::EditInput(_) => self.edit_draft(),
        }
    }

    fn apply_text_input_port_value(&mut self, target: TextInputTarget, text: String) -> bool {
        match target {
            TextInputTarget::MainInput => self.dispatch_text_input_inputs_and_observe(
                target,
                text,
                Vec::new(),
                None,
                Self::main_input_draft,
                false,
            ),
            TextInputTarget::EditInput(_) => self.dispatch_text_input_inputs_and_observe(
                target,
                text,
                Vec::new(),
                None,
                Self::edit_draft,
                false,
            ),
        }
    }

    fn apply_text_input_change(&mut self, target: TextInputTarget, draft: &str) -> bool {
        if !self.text_input_is_active(target) || self.text_input_draft(target) == draft {
            return false;
        }
        self.apply_text_input_port_value(target, draft.to_string())
    }

    fn apply_text_input_event(
        &mut self,
        target: TextInputTarget,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        match kind {
            UiEventKind::Input | UiEventKind::Change => {
                self.apply_text_input_change(target, payload.unwrap_or_default())
            }
            UiEventKind::KeyDown => {
                self.apply_text_input_keydown(target, decode_key_down_payload(payload))
            }
            UiEventKind::Blur => self.apply_text_input_focus_change(target, false),
            UiEventKind::Focus => self.apply_text_input_focus_change(target, true),
            _ => false,
        }
    }

    fn apply_text_input_fact(&mut self, target: TextInputTarget, kind: UiFactKind) -> bool {
        match kind {
            UiFactKind::DraftText(text) => self.apply_text_input_change(target, &text),
            UiFactKind::Focused(focused) => self.apply_text_input_focus_change(target, focused),
            _ => false,
        }
    }

    fn sync_keydown_text<F>(
        &mut self,
        current_text: Option<String>,
        current_value: String,
        mut apply_change: F,
    ) -> (bool, String)
    where
        F: FnMut(&mut Self, String) -> bool,
    {
        let mut changed = false;
        let mut latest = current_value;
        if let Some(current_text) = current_text {
            if latest != current_text {
                changed |= apply_change(self, current_text.clone());
            }
            latest = current_text;
        }
        (changed, latest)
    }

    fn apply_text_input_keydown(
        &mut self,
        target: TextInputTarget,
        keydown: DecodedKeyDown,
    ) -> bool {
        if !self.text_input_is_active(target) {
            return false;
        }
        let (changed, latest_draft) = self.sync_keydown_text(
            keydown.current_text,
            self.text_input_draft(target),
            |this, text| this.apply_text_input_port_value(target, text),
        );
        match target {
            TextInputTarget::MainInput => match keydown.key.as_str() {
                "Enter" => self.apply_text_input_enter(target, latest_draft) || changed,
                _ => changed,
            },
            TextInputTarget::EditInput(todo_id) => match keydown.key.as_str() {
                "Enter" => {
                    self.apply_text_input_enter(TextInputTarget::EditInput(todo_id), latest_draft)
                        || changed
                }
                "Escape" => {
                    self.apply_edit_session_action(
                        TodoProgram::TODO_EDIT_CANCEL_PORT,
                        PortPayload::Click,
                    ) || changed
                }
                _ => changed,
            },
        }
    }

    fn kernel_object<const N: usize>(fields: [(&str, KernelValue); N]) -> KernelValue {
        KernelValue::Object(BTreeMap::from_iter(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_string(), value)),
        ))
    }

    fn todo_list_kernel_value(&self) -> KernelValue {
        let items = self
            .todos
            .items()
            .iter()
            .map(|todo| {
                Self::kernel_object([
                    ("id", KernelValue::from(todo.id as f64)),
                    ("title", KernelValue::from(todo.value.title.clone())),
                    ("completed", KernelValue::from(todo.value.completed)),
                ])
            })
            .collect();
        KernelValue::List(items)
    }

    fn next_todo_id(&self) -> u64 {
        self.todos.next_id()
    }

    fn runtime_todos(&self) -> Option<Vec<(u64, String, bool)>> {
        let KernelValue::List(items) = self
            .ui_state_executor
            .sink_value(TodoProgram::TODOS_LIST_SINK)?
        else {
            return None;
        };
        items
            .iter()
            .map(|item| {
                let KernelValue::Object(fields) = item else {
                    return None;
                };
                let id = match fields.get("id") {
                    Some(KernelValue::Number(number)) if *number >= 0.0 => *number as u64,
                    _ => return None,
                };
                let title = match fields.get("title") {
                    Some(KernelValue::Text(text)) | Some(KernelValue::Tag(text)) => text.clone(),
                    _ => return None,
                };
                let completed = match fields.get("completed") {
                    Some(KernelValue::Bool(value)) => *value,
                    _ => return None,
                };
                Some((id, title, completed))
            })
            .collect()
    }

    fn sync_todos_from_runtime(&mut self) -> bool {
        let Some(runtime_todos) = self.runtime_todos() else {
            return false;
        };
        let previous_next_id = self.todos.next_id();
        let current_projection = self
            .todos
            .items()
            .iter()
            .map(|todo| (todo.id, todo.value.title.clone(), todo.value.completed))
            .collect::<Vec<_>>();
        if current_projection == runtime_todos {
            return false;
        }
        let next_id = previous_next_id.max(
            runtime_todos
                .iter()
                .map(|(id, _, _)| *id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.todos.replace_all(
            runtime_todos
                .into_iter()
                .map(|(id, title, completed)| (id, TodoItem { title, completed })),
            next_id,
        );
        self.sync_todo_list_mirrors();
        true
    }

    fn sync_todo_list_mirrors(&mut self) {
        let inputs = self.todo_runtime_inputs(true);
        let messages = self
            .ui_state_runtime
            .dispatch_snapshot(HostSnapshot::new(inputs));
        self.apply_ui_state_messages(messages);
    }

    fn apply_port_action<T: PartialEq>(
        &mut self,
        mut inputs: Vec<HostInput>,
        port: SourcePortId,
        payload: PortPayload,
        observe: fn(&Self) -> T,
        sync_todos: bool,
    ) -> bool {
        self.push_pulse_input(&mut inputs, port, Self::port_payload_value(payload));
        self.dispatch_inputs_and_observe(inputs, observe, sync_todos)
    }

    fn apply_text_input_enter(&mut self, target: TextInputTarget, current_text: String) -> bool {
        match target {
            TextInputTarget::MainInput => {
                let inputs = self.todo_runtime_inputs(true);
                self.dispatch_text_input_inputs_and_observe(
                    target,
                    current_text.clone(),
                    inputs,
                    Some((
                        TodoProgram::MAIN_INPUT_KEY_DOWN_PORT,
                        PortPayload::KeyDown {
                            key: "Enter".to_string(),
                            current_text: Some(current_text),
                        },
                    )),
                    Self::todo_list_projection,
                    true,
                )
            }
            TextInputTarget::EditInput(todo_id) => {
                let inputs = self.todo_runtime_inputs(false);
                self.apply_port_action(
                    inputs,
                    TodoProgram::TODO_EDIT_COMMIT_PORT,
                    PortPayload::IdTitle {
                        target_id: todo_id,
                        title: current_text,
                    },
                    Self::todo_list_projection,
                    true,
                )
            }
        }
    }

    fn apply_edit_session_action(&mut self, port: SourcePortId, payload: PortPayload) -> bool {
        self.apply_port_action(Vec::new(), port, payload, Self::edit_session_state, false)
    }

    fn apply_todo_list_action(&mut self, port: SourcePortId, payload: PortPayload) -> bool {
        let inputs = self.todo_runtime_inputs(false);
        self.apply_port_action(inputs, port, payload, Self::todo_list_projection, true)
    }

    fn port_payload_value(payload: PortPayload) -> KernelValue {
        match payload {
            PortPayload::Click => KernelValue::from("click"),
            PortPayload::Focus => KernelValue::from("focus"),
            PortPayload::Text(text) => KernelValue::from(text),
            PortPayload::KeyDown { key, current_text } => match current_text {
                Some(text) => KernelValue::from(format!("{key}{KEYDOWN_TEXT_SEPARATOR}{text}")),
                None => KernelValue::from(key),
            },
            PortPayload::TargetId(target_id) => {
                Self::kernel_object([("id", KernelValue::from(target_id as f64))])
            }
            PortPayload::IdTitle { target_id, title } => Self::kernel_object([
                ("id", KernelValue::from(target_id as f64)),
                ("title", KernelValue::from(title)),
            ]),
            PortPayload::HoverState { target_id, hovered } => Self::kernel_object([
                ("id", KernelValue::from(target_id as f64)),
                ("hovered", KernelValue::from(hovered)),
            ]),
        }
    }

    fn todo_runtime_inputs(&mut self, include_next_todo_id: bool) -> Vec<HostInput> {
        let mut inputs = vec![HostInput::Mirror {
            actor: self.ui_state_actor,
            cell: TodoProgram::TODOS_LIST_CELL,
            value: self.todo_list_kernel_value(),
            seq: self.ui_state_runtime.causal_seq(0),
        }];
        if include_next_todo_id {
            inputs.push(HostInput::Mirror {
                actor: self.ui_state_actor,
                cell: TodoProgram::NEXT_TODO_ID_CELL,
                value: KernelValue::from(self.next_todo_id() as f64),
                seq: self.ui_state_runtime.causal_seq(inputs.len() as u32),
            });
        }
        inputs
    }

    fn todo_list_projection(&self) -> Vec<(u64, String, bool)> {
        self.todos
            .items()
            .iter()
            .map(|todo| (todo.id, todo.value.title.clone(), todo.value.completed))
            .collect::<Vec<_>>()
    }

    fn dispatch_inputs_and_observe<T: PartialEq>(
        &mut self,
        inputs: Vec<HostInput>,
        observe: fn(&Self) -> T,
        sync_todos: bool,
    ) -> bool {
        let before = observe(self);
        let messages = self
            .ui_state_runtime
            .dispatch_snapshot(HostSnapshot::new(inputs));
        self.apply_ui_state_messages(messages);
        if sync_todos {
            let synced = self.sync_todos_from_runtime();
            synced || before != observe(self)
        } else {
            before != observe(self)
        }
    }

    fn apply_text_input_focus_change(&mut self, target: TextInputTarget, focused: bool) -> bool {
        if !self.text_input_is_active(target) {
            return false;
        }
        let (focus_port, blur_port) = Self::text_input_focus_ports(target);
        match target {
            TextInputTarget::MainInput => self.apply_port_action(
                Vec::new(),
                if focused { focus_port } else { blur_port },
                PortPayload::Focus,
                Self::main_input_focus_state,
                false,
            ),
            TextInputTarget::EditInput(_) => self.apply_port_action(
                Vec::new(),
                if focused { focus_port } else { blur_port },
                PortPayload::Focus,
                Self::edit_focus_state,
                false,
            ),
        }
    }

    fn apply_hover_fact(&mut self, todo_id: u64, hovered: bool) -> bool {
        self.apply_port_action(
            Vec::new(),
            TodoProgram::TODO_HOVER_PORT,
            PortPayload::HoverState {
                target_id: todo_id,
                hovered,
            },
            Self::hovered_todo_id,
            false,
        )
    }

    fn apply_filter_click(&mut self, port: SourcePortId) -> bool {
        self.apply_port_action(
            Vec::new(),
            port,
            PortPayload::Click,
            Self::selected_filter_with_focus_hint,
            false,
        )
    }

    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let active_count = self.active_count();
        let completed_count = self.completed_count();
        let all_completed = self.all_completed();
        let visible_todos = self.visible_todo_ids();

        let mut content_children = vec![self.render_header()];
        content_children.push(self.render_input_row(ops, all_completed));
        if !self.todos.is_empty() {
            content_children.push(self.render_list(ops, &visible_todos));
            content_children.push(self.render_panel_footer(ops, active_count, completed_count));
        }
        content_children.push(self.render_footer_texts());

        let content = self.element_node(ViewSiteId(201), "div", None, content_children);
        self.element_node(ViewSiteId(200), "div", None, vec![content])
    }

    fn render_header(&mut self) -> UiNode {
        self.element_node(ViewSiteId(202), "h1", Some("todos".to_string()), Vec::new())
    }

    fn render_input_row(&mut self, ops: &mut Vec<RenderOp>, all_completed: bool) -> UiNode {
        let mut children = Vec::new();
        if !self.todos.is_empty() {
            children.push(self.render_toggle_all_checkbox(ops, all_completed));
        }
        children.push(self.render_main_input(ops));

        let row = self.element_node(ViewSiteId(203), "div", None, children);
        if self.main_input_focused() {
            ops.push(RenderOp::SetStyle {
                id: row.id,
                name: "outline".to_string(),
                value: Some("1px solid rgb(200, 120, 120)".to_string()),
            });
        }
        row
    }

    fn render_toggle_all_checkbox(
        &mut self,
        ops: &mut Vec<RenderOp>,
        all_completed: bool,
    ) -> UiNode {
        let node = self.element_node(ViewSiteId(204), "input", None, Vec::new());
        self.configure_checkbox(
            ops,
            node.id,
            "cb-toggle-all",
            all_completed,
            TodoProgram::TOGGLE_ALL_PORT,
            TodoUiAction::ToggleAll,
        );
        ops.push(RenderOp::SetProperty {
            id: node.id,
            name: "aria-label".to_string(),
            value: Some("Toggle all".to_string()),
        });
        node
    }

    fn render_main_input(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let node = self.element_node(ViewSiteId(205), "input", None, Vec::new());
        ops.push(RenderOp::SetProperty {
            id: node.id,
            name: "type".to_string(),
            value: Some("text".to_string()),
        });
        ops.push(RenderOp::SetProperty {
            id: node.id,
            name: "placeholder".to_string(),
            value: Some("What needs to be done?".to_string()),
        });
        if self.main_input_focus_hint() {
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "autofocus".to_string(),
                value: Some("true".to_string()),
            });
        }
        ops.push(RenderOp::SetInputValue {
            id: node.id,
            value: self.main_input_draft(),
        });
        self.ui.bind_fact_target(node.id, FactTarget::MainInput);
        self.attach_port(
            ops,
            node.id,
            TodoProgram::MAIN_INPUT_CHANGE_PORT,
            UiEventKind::Input,
            TodoUiAction::MainInputChange,
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::MAIN_INPUT_CHANGE_PORT,
            UiEventKind::Change,
            TodoUiAction::MainInputChange,
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::MAIN_INPUT_KEY_DOWN_PORT,
            UiEventKind::KeyDown,
            TodoUiAction::MainInputKeyDown,
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::MAIN_INPUT_BLUR_PORT,
            UiEventKind::Blur,
            TodoUiAction::MainInputBlur,
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::MAIN_INPUT_FOCUS_PORT,
            UiEventKind::Focus,
            TodoUiAction::MainInputFocus,
        );
        node
    }

    fn render_list(&mut self, ops: &mut Vec<RenderOp>, visible_todos: &[u64]) -> UiNode {
        let visible_items = visible_todos
            .iter()
            .filter_map(|todo_id| self.todos.find(*todo_id).cloned())
            .collect::<Vec<_>>();
        let children = visible_items
            .iter()
            .map(|todo| self.render_todo_item(ops, todo))
            .collect::<Vec<_>>();
        self.element_node(ViewSiteId(206), "div", None, children)
    }

    fn render_todo_item(
        &mut self,
        ops: &mut Vec<RenderOp>,
        todo: &crate::mapped_list_runtime::MappedListItem<TodoItem>,
    ) -> UiNode {
        let mut children = vec![self.render_todo_checkbox(ops, todo)];
        if self.edit_target_id() == Some(todo.id) {
            children.push(self.render_todo_edit_input(ops, todo.id));
        } else {
            children.push(self.render_todo_title(ops, todo));
        }
        if self.hovered_todo_id() == Some(todo.id) {
            children.push(self.render_todo_delete_button(ops, todo));
        }
        self.element_node_with_item(ViewSiteId(300), "div", None, todo.id, children)
    }

    fn render_todo_checkbox(
        &mut self,
        ops: &mut Vec<RenderOp>,
        todo: &crate::mapped_list_runtime::MappedListItem<TodoItem>,
    ) -> UiNode {
        let node = self.element_node_with_item(ViewSiteId(301), "input", None, todo.id, Vec::new());
        self.configure_checkbox(
            ops,
            node.id,
            &format!("cb-{}", todo.id),
            todo.value.completed,
            todo_checkbox_port(todo.id),
            TodoUiAction::TodoCheckbox(todo.id),
        );
        node
    }

    fn render_todo_title(
        &mut self,
        ops: &mut Vec<RenderOp>,
        todo: &crate::mapped_list_runtime::MappedListItem<TodoItem>,
    ) -> UiNode {
        let node = self.element_node_with_item(
            ViewSiteId(302),
            "span",
            Some(todo.value.title.clone()),
            todo.id,
            Vec::new(),
        );
        if todo.value.completed {
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "text-decoration".to_string(),
                value: Some("line-through".to_string()),
            });
        }
        self.ui
            .bind_fact_target(node.id, FactTarget::TodoTitle(todo.id));
        self.attach_port(
            ops,
            node.id,
            todo_title_double_click_port(todo.id),
            UiEventKind::DoubleClick,
            TodoUiAction::TodoTitleDoubleClick(todo.id),
        );
        node
    }

    fn render_todo_edit_input(&mut self, ops: &mut Vec<RenderOp>, todo_id: u64) -> UiNode {
        let node = self.element_node_with_item(ViewSiteId(303), "input", None, todo_id, Vec::new());
        ops.push(RenderOp::SetProperty {
            id: node.id,
            name: "type".to_string(),
            value: Some("text".to_string()),
        });
        ops.push(RenderOp::SetInputValue {
            id: node.id,
            value: self.edit_draft(),
        });
        if self.edit_focus_hint() {
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "autofocus".to_string(),
                value: Some("true".to_string()),
            });
        } else if self.edit_focused() {
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "focused".to_string(),
                value: Some("true".to_string()),
            });
        }
        ops.push(RenderOp::SetStyle {
            id: node.id,
            name: "outline".to_string(),
            value: Some("1px solid rgb(80, 120, 220)".to_string()),
        });
        self.ui
            .bind_fact_target(node.id, FactTarget::TodoEditInput(todo_id));
        self.attach_port(
            ops,
            node.id,
            TodoProgram::TODO_EDIT_CHANGE_PORT,
            UiEventKind::Input,
            TodoUiAction::TodoEditChange(todo_id),
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::TODO_EDIT_CHANGE_PORT,
            UiEventKind::Change,
            TodoUiAction::TodoEditChange(todo_id),
        );
        self.attach_port(
            ops,
            node.id,
            todo_edit_key_down_port(todo_id),
            UiEventKind::KeyDown,
            TodoUiAction::TodoEditKeyDown(todo_id),
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::TODO_EDIT_BLUR_PORT,
            UiEventKind::Blur,
            TodoUiAction::TodoEditBlur(todo_id),
        );
        self.attach_port(
            ops,
            node.id,
            TodoProgram::TODO_EDIT_FOCUS_PORT,
            UiEventKind::Focus,
            TodoUiAction::TodoEditFocus(todo_id),
        );
        node
    }

    fn render_todo_delete_button(
        &mut self,
        ops: &mut Vec<RenderOp>,
        todo: &crate::mapped_list_runtime::MappedListItem<TodoItem>,
    ) -> UiNode {
        let node = self.element_node_with_item(
            ViewSiteId(304),
            "button",
            Some("×".to_string()),
            todo.id,
            Vec::new(),
        );
        self.attach_port(
            ops,
            node.id,
            todo_delete_port(todo.id),
            UiEventKind::Click,
            TodoUiAction::TodoDelete(todo.id),
        );
        node
    }

    fn render_panel_footer(
        &mut self,
        ops: &mut Vec<RenderOp>,
        active_count: usize,
        completed_count: usize,
    ) -> UiNode {
        let count_label = if active_count == 1 {
            "1 item left".to_string()
        } else {
            format!("{active_count} items left")
        };
        let mut children = vec![
            self.element_node(ViewSiteId(207), "span", Some(count_label), Vec::new()),
            self.render_filter_button(
                ops,
                ViewSiteId(208),
                "All",
                Filter::All,
                TodoProgram::FILTER_ALL_PORT,
                TodoUiAction::FilterAll,
            ),
            self.render_filter_button(
                ops,
                ViewSiteId(209),
                "Active",
                Filter::Active,
                TodoProgram::FILTER_ACTIVE_PORT,
                TodoUiAction::FilterActive,
            ),
            self.render_filter_button(
                ops,
                ViewSiteId(210),
                "Completed",
                Filter::Completed,
                TodoProgram::FILTER_COMPLETED_PORT,
                TodoUiAction::FilterCompleted,
            ),
        ];
        if completed_count > 0 {
            let clear = self.element_node(
                ViewSiteId(211),
                "button",
                Some("Clear completed".to_string()),
                Vec::new(),
            );
            self.attach_port(
                ops,
                clear.id,
                TodoProgram::CLEAR_COMPLETED_PORT,
                UiEventKind::Click,
                TodoUiAction::ClearCompleted,
            );
            children.push(clear);
        }
        self.element_node(ViewSiteId(212), "footer", None, children)
    }

    fn render_filter_button(
        &mut self,
        ops: &mut Vec<RenderOp>,
        view_site: ViewSiteId,
        label: &str,
        filter: Filter,
        port: SourcePortId,
        action: TodoUiAction,
    ) -> UiNode {
        let node = self.element_node(view_site, "button", Some(label.to_string()), Vec::new());
        if self.selected_filter() == filter {
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "outline".to_string(),
                value: Some("2px solid rgb(120, 80, 80)".to_string()),
            });
        }
        self.attach_port(ops, node.id, port, UiEventKind::Click, action);
        node
    }

    fn render_footer_texts(&mut self) -> UiNode {
        let help = self.element_node(
            ViewSiteId(214),
            "p",
            Some("Double-click to edit a todo".to_string()),
            Vec::new(),
        );
        let created_by = self.element_node(
            ViewSiteId(215),
            "p",
            Some("Created by Martin Kavík".to_string()),
            Vec::new(),
        );
        let author = self.element_node(
            ViewSiteId(216),
            "p",
            Some("Part of TodoMVC".to_string()),
            Vec::new(),
        );
        self.element_node(ViewSiteId(213), "div", None, vec![help, created_by, author])
    }

    fn configure_checkbox(
        &mut self,
        ops: &mut Vec<RenderOp>,
        node_id: NodeId,
        element_id: &str,
        checked: bool,
        port: SourcePortId,
        action: TodoUiAction,
    ) {
        ops.push(RenderOp::SetProperty {
            id: node_id,
            name: "type".to_string(),
            value: Some("checkbox".to_string()),
        });
        ops.push(RenderOp::SetProperty {
            id: node_id,
            name: "role".to_string(),
            value: Some("checkbox".to_string()),
        });
        ops.push(RenderOp::SetProperty {
            id: node_id,
            name: "id".to_string(),
            value: Some(element_id.to_string()),
        });
        ops.push(RenderOp::SetChecked {
            id: node_id,
            checked,
        });
        self.attach_port(ops, node_id, port, UiEventKind::Click, action);
    }

    fn attach_port(
        &mut self,
        ops: &mut Vec<RenderOp>,
        node_id: NodeId,
        source_port: SourcePortId,
        kind: UiEventKind,
        action: TodoUiAction,
    ) {
        self.ui.attach_port(ops, node_id, source_port, kind, action);
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
                function_instance: Some(FunctionInstanceId(1)),
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
                function_instance: Some(FunctionInstanceId(2)),
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

pub(crate) fn todo_metrics_capture()
-> Result<(InteractionMetricsReport, RuntimeTelemetrySnapshot), String> {
    let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");

    let startup_started = Instant::now();
    let mut preview = TodoPreview::new(source)?;
    let _ = preview.preview_text();
    let startup_millis = startup_started.elapsed().as_secs_f64() * 1000.0;

    let mut add_samples = Vec::new();
    for index in 0..24 {
        let title = format!("Bench todo {index}");
        let started = Instant::now();
        let changed =
            preview.apply_fact(FactTarget::MainInput, UiFactKind::DraftText(title.clone()));
        let committed = preview.apply_event(
            TodoUiAction::MainInputKeyDown,
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}{title}")),
        );
        assert!(changed || committed, "todo add should change state");
        let _ = preview.preview_text();
        add_samples.push(started.elapsed());
    }

    let mut toggle_samples = Vec::new();
    for _ in 0..24 {
        let first_id = preview
            .todos
            .first()
            .map(|todo| todo.id)
            .expect("todo exists");
        let started = Instant::now();
        assert!(preview.apply_event(
            TodoUiAction::TodoCheckbox(first_id),
            UiEventKind::Click,
            None,
        ));
        let _ = preview.preview_text();
        toggle_samples.push(started.elapsed());
    }

    let mut filter_samples = Vec::new();
    for filter in [Filter::Active, Filter::Completed, Filter::All]
        .into_iter()
        .cycle()
        .take(24)
    {
        let action = match filter {
            Filter::All => TodoUiAction::FilterAll,
            Filter::Active => TodoUiAction::FilterActive,
            Filter::Completed => TodoUiAction::FilterCompleted,
        };
        let started = Instant::now();
        assert!(preview.apply_event(action, UiEventKind::Click, None));
        let _ = preview.preview_text();
        filter_samples.push(started.elapsed());
    }

    let mut edit_samples = Vec::new();
    for index in 0..24 {
        let todo_id = preview
            .todos
            .first()
            .map(|todo| todo.id)
            .expect("todo exists");
        let title = format!("Edited todo {index}");
        let started = Instant::now();
        assert!(preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(todo_id),
            UiEventKind::DoubleClick,
            None,
        ));
        assert!(preview.apply_fact(
            FactTarget::TodoEditInput(todo_id),
            UiFactKind::DraftText(title.clone()),
        ));
        assert!(preview.apply_event(
            TodoUiAction::TodoEditKeyDown(todo_id),
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}{title}")),
        ));
        let _ = preview.preview_text();
        edit_samples.push(started.elapsed());
    }

    Ok((
        InteractionMetricsReport {
            startup_millis,
            add_to_paint: LatencySummary::from_durations(&add_samples),
            toggle_to_paint: LatencySummary::from_durations(&toggle_samples),
            filter_to_paint: LatencySummary::from_durations(&filter_samples),
            edit_to_paint: LatencySummary::from_durations(&edit_samples),
        },
        preview.runtime_telemetry_snapshot(),
    ))
}

pub fn todo_metrics_snapshot() -> Result<InteractionMetricsReport, String> {
    todo_metrics_capture().map(|(report, _telemetry)| report)
}

fn todo_checkbox_port(todo_id: u64) -> SourcePortId {
    SourcePortId(1_000 + todo_id as u32 * 10)
}

fn todo_title_double_click_port(todo_id: u64) -> SourcePortId {
    SourcePortId(1_001 + todo_id as u32 * 10)
}

fn todo_delete_port(todo_id: u64) -> SourcePortId {
    SourcePortId(1_002 + todo_id as u32 * 10)
}

fn todo_edit_key_down_port(todo_id: u64) -> SourcePortId {
    SourcePortId(1_004 + todo_id as u32 * 10)
}

fn todo_fact_cell(target: &FactTarget) -> MirrorCellId {
    match target {
        FactTarget::MainInput => MirrorCellId(1),
        FactTarget::TodoTitle(todo_id) => MirrorCellId(1_000 + *todo_id as u32),
        FactTarget::TodoEditInput(todo_id) => MirrorCellId(2_000 + *todo_id as u32),
    }
}

fn todo_fact_target(cell: MirrorCellId) -> Option<FactTarget> {
    match cell.0 {
        1 => Some(FactTarget::MainInput),
        1_000..=1_999 => Some(FactTarget::TodoTitle((cell.0 - 1_000) as u64)),
        2_000..=2_999 => Some(FactTarget::TodoEditInput((cell.0 - 2_000) as u64)),
        _ => None,
    }
}

pub fn render_todo_preview(preview: TodoPreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_acceptance::{TodoAcceptanceAction, todo_edit_save_acceptance_sequences};

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

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum OracleFilter {
        All,
        Active,
        Completed,
    }

    #[derive(Clone)]
    struct OracleTodo {
        id: u64,
        title: String,
        completed: bool,
    }

    #[derive(Clone)]
    struct OracleTodoState {
        todos: Vec<OracleTodo>,
        next_id: u64,
        filter: OracleFilter,
        main_draft: String,
        editing: Option<(u64, String)>,
    }

    impl OracleTodoState {
        fn new() -> Self {
            Self {
                todos: vec![
                    OracleTodo {
                        id: 1,
                        title: "Buy groceries".to_string(),
                        completed: false,
                    },
                    OracleTodo {
                        id: 2,
                        title: "Clean room".to_string(),
                        completed: false,
                    },
                ],
                next_id: 3,
                filter: OracleFilter::All,
                main_draft: String::new(),
                editing: None,
            }
        }

        fn visible_titles(&self) -> Vec<String> {
            self.todos
                .iter()
                .filter(|todo| match self.filter {
                    OracleFilter::All => true,
                    OracleFilter::Active => !todo.completed,
                    OracleFilter::Completed => todo.completed,
                })
                .map(|todo| todo.title.clone())
                .collect()
        }

        fn active_count(&self) -> usize {
            self.todos.iter().filter(|todo| !todo.completed).count()
        }

        fn completed_count(&self) -> usize {
            self.todos.iter().filter(|todo| todo.completed).count()
        }
    }

    fn todo_id_by_title(preview: &TodoPreview, title: &str) -> u64 {
        preview
            .todos
            .items()
            .iter()
            .find(|todo| todo.value.title == title)
            .map(|todo| todo.id)
            .expect("todo id by title")
    }

    fn seed_buy_milk_only(preview: &mut TodoPreview) {
        preview.todos = TargetedListRuntime::new(
            [(
                3,
                TodoItem {
                    title: "Buy milk".to_string(),
                    completed: false,
                },
            )],
            4,
        );
    }
    #[test]
    fn todo_preview_renders_initial_state() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        let text = preview.preview_text();
        assert!(text.contains("2 items left"));
        assert!(text.contains("Buy groceries"));
        assert!(text.contains("Clean room"));
    }

    #[test]
    fn todo_preview_adds_new_todo_from_main_input() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        preview.apply_fact(
            FactTarget::MainInput,
            UiFactKind::DraftText("Test todo".to_string()),
        );
        preview.apply_event(
            TodoUiAction::MainInputKeyDown,
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}Test todo")),
        );
        let text = preview.preview_text();
        assert!(text.contains("3 items left"));
        assert!(text.contains("Test todo"));
        assert!(preview.main_input_draft().is_empty());
        assert!(preview.main_input_focused());
        assert!(preview.main_input_focus_hint());
    }

    #[test]
    fn todo_preview_can_enter_edit_mode_and_escape_without_saving() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(1),
            UiEventKind::DoubleClick,
            None,
        );
        assert_eq!(preview.edit_target_id(), Some(1));
        preview.apply_fact(
            FactTarget::TodoEditInput(1),
            UiFactKind::DraftText("Changed".to_string()),
        );
        preview.apply_event(
            TodoUiAction::TodoEditKeyDown(1),
            UiEventKind::KeyDown,
            Some("Escape"),
        );
        assert_eq!(preview.edit_target_id(), None);
        assert_eq!(
            preview.todos.first().expect("first todo").value.title,
            "Buy groceries"
        );
    }

    #[test]
    fn todo_preview_hover_reveals_delete_button() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        preview.apply_fact(FactTarget::TodoTitle(1), UiFactKind::Hovered(true));
        assert!(preview.preview_text().contains("×"));
    }

    #[test]
    fn todo_preview_double_click_clears_hovered_delete_button() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        preview.apply_fact(FactTarget::TodoTitle(1), UiFactKind::Hovered(true));
        assert!(preview.preview_text().contains("×"));

        preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(1),
            UiEventKind::DoubleClick,
            None,
        );

        let text = preview.preview_text();
        assert!(!text.contains("×"));
        assert_eq!(preview.hovered_todo_id(), None);
        assert_eq!(preview.edit_target_id(), Some(1));
    }

    #[test]
    fn todo_preview_filter_click_clears_main_input_focus_hint() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        assert!(preview.main_input_focus_hint());
        preview.apply_event(TodoUiAction::FilterActive, UiEventKind::Click, None);
        assert!(!preview.main_input_focus_hint());
    }

    #[test]
    fn todo_preview_main_input_focus_and_blur_events_flow_through_ports() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_event(TodoUiAction::MainInputBlur, UiEventKind::Blur, None);
        assert!(!preview.main_input_focused());
        assert!(preview.main_input_focus_hint());

        preview.apply_event(TodoUiAction::MainInputFocus, UiEventKind::Focus, None);
        assert!(preview.main_input_focused());
        assert!(!preview.main_input_focus_hint());
    }

    #[test]
    fn todo_preview_main_input_focus_and_blur_facts_flow_through_ports() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_fact(FactTarget::MainInput, UiFactKind::Focused(false));
        assert!(!preview.main_input_focused());
        assert!(preview.main_input_focus_hint());

        preview.apply_fact(FactTarget::MainInput, UiFactKind::Focused(true));
        assert!(preview.main_input_focused());
        assert!(!preview.main_input_focus_hint());
    }

    #[test]
    fn todo_preview_restores_all_items_after_filter_roundtrip() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_fact(
            FactTarget::MainInput,
            UiFactKind::DraftText("Test todo".to_string()),
        );
        preview.apply_event(
            TodoUiAction::MainInputKeyDown,
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}Test todo")),
        );
        preview.apply_event(TodoUiAction::TodoCheckbox(1), UiEventKind::Click, None);
        preview.apply_event(TodoUiAction::FilterActive, UiEventKind::Click, None);
        assert!(preview.preview_text().contains("Clean room"));
        assert!(!preview.preview_text().contains("Buy groceries"));

        preview.apply_event(TodoUiAction::FilterCompleted, UiEventKind::Click, None);
        assert!(preview.preview_text().contains("Buy groceries"));
        assert!(!preview.preview_text().contains("Clean room"));

        preview.apply_event(TodoUiAction::FilterAll, UiEventKind::Click, None);
        let text = preview.preview_text();
        assert!(text.contains("Buy groceries"));
        assert!(text.contains("Clean room"));
        assert!(text.contains("Test todo"));
    }

    #[test]
    fn todo_preview_edit_mode_clears_main_input_focus_and_escape_exits() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_event(
            TodoUiAction::MainInputKeyDown,
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}Buy milk")),
        );
        assert!(preview.main_input_focused());

        let buy_milk_id = preview
            .todos
            .iter()
            .find(|todo| todo.value.title == "Buy milk")
            .map(|todo| todo.id)
            .expect("buy milk todo id");

        preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(buy_milk_id),
            UiEventKind::DoubleClick,
            None,
        );
        assert_eq!(preview.edit_target_id(), Some(buy_milk_id));
        preview.apply_fact(
            FactTarget::TodoEditInput(buy_milk_id),
            UiFactKind::Focused(true),
        );
        assert!(!preview.main_input_focused());

        preview.apply_event(
            TodoUiAction::TodoEditKeyDown(buy_milk_id),
            UiEventKind::KeyDown,
            Some(&format!("Escape{KEYDOWN_TEXT_SEPARATOR}Buy milk")),
        );

        let todo = preview
            .todos
            .iter()
            .find(|todo| todo.id == buy_milk_id)
            .expect("todo after escape");
        assert_eq!(preview.edit_target_id(), None);
        assert_eq!(todo.value.title, "Buy milk");
    }

    #[test]
    fn todo_preview_edit_save_uses_latest_draft_across_batches() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(1),
            UiEventKind::DoubleClick,
            None,
        );
        preview.apply_fact(
            FactTarget::TodoEditInput(1),
            UiFactKind::DraftText("edited title".to_string()),
        );
        preview.apply_event(
            TodoUiAction::TodoEditKeyDown(1),
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}edited title")),
        );

        let todo = preview
            .todos
            .iter()
            .find(|todo| todo.id == 1)
            .expect("todo after edit save");
        assert_eq!(todo.value.title, "edited title");
        assert_eq!(preview.edit_target_id(), None);
    }

    #[test]
    fn todo_preview_shared_edit_save_acceptance_sequences_behave_as_expected() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        seed_buy_milk_only(&mut preview);

        for sequence in todo_edit_save_acceptance_sequences() {
            for action in &sequence.actions {
                match action {
                    TodoAcceptanceAction::DblClickText { text } => {
                        let todo_id = todo_id_by_title(&preview, text);
                        preview.apply_event(
                            TodoUiAction::TodoTitleDoubleClick(todo_id),
                            UiEventKind::DoubleClick,
                            None,
                        );
                        preview.apply_fact(
                            FactTarget::TodoEditInput(todo_id),
                            UiFactKind::Focused(true),
                        );
                    }
                    TodoAcceptanceAction::AssertFocused { index } => {
                        assert_eq!(*index, 1, "shared Todo edit-save trace expects edit input");
                        assert!(preview.edit_focused(), "{}", sequence.description);
                    }
                    TodoAcceptanceAction::AssertInputTypeable { index } => {
                        assert_eq!(*index, 1, "shared Todo edit-save trace expects edit input");
                        assert_eq!(
                            preview.edit_target_id(),
                            Some(3),
                            "{}",
                            sequence.description
                        );
                    }
                    TodoAcceptanceAction::TypeText { text } => {
                        let todo_id = preview.edit_target_id().expect("edit target");
                        let next_draft = format!("{}{}", preview.edit_draft(), text);
                        preview.apply_fact(
                            FactTarget::TodoEditInput(todo_id),
                            UiFactKind::DraftText(next_draft),
                        );
                    }
                    TodoAcceptanceAction::FocusInput { index } => {
                        assert_eq!(*index, 1, "shared Todo edit-save trace expects edit input");
                        let todo_id = preview.edit_target_id().expect("edit target");
                        preview.apply_fact(
                            FactTarget::TodoEditInput(todo_id),
                            UiFactKind::Focused(true),
                        );
                    }
                    TodoAcceptanceAction::Key { key } => {
                        let todo_id = preview.edit_target_id().expect("edit target");
                        let payload = if *key == "Enter" {
                            format!("Enter{KEYDOWN_TEXT_SEPARATOR}{}", preview.edit_draft())
                        } else {
                            (*key).to_string()
                        };
                        preview.apply_event(
                            TodoUiAction::TodoEditKeyDown(todo_id),
                            UiEventKind::KeyDown,
                            Some(payload.as_str()),
                        );
                    }
                }
            }

            assert!(
                preview.preview_text().contains(sequence.expect),
                "{}",
                sequence.description
            );
        }
    }

    #[test]
    fn todo_preview_edit_input_focus_and_blur_events_flow_through_ports() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(1),
            UiEventKind::DoubleClick,
            None,
        );
        assert_eq!(preview.edit_target_id(), Some(1));
        assert!(preview.edit_focus_hint());
        assert!(!preview.edit_focused());

        preview.apply_event(TodoUiAction::TodoEditFocus(1), UiEventKind::Focus, None);
        assert!(preview.edit_focused());
        assert!(!preview.edit_focus_hint());
        assert!(!preview.main_input_focused());

        preview.apply_event(TodoUiAction::TodoEditBlur(1), UiEventKind::Blur, None);
        assert!(!preview.edit_focused());
        assert!(!preview.edit_focus_hint());
    }

    #[test]
    fn todo_preview_edit_input_focus_and_blur_facts_flow_through_ports() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");

        preview.apply_event(
            TodoUiAction::TodoTitleDoubleClick(1),
            UiEventKind::DoubleClick,
            None,
        );
        assert_eq!(preview.edit_target_id(), Some(1));

        preview.apply_fact(FactTarget::TodoEditInput(1), UiFactKind::Focused(true));
        assert!(preview.edit_focused());
        assert!(!preview.edit_focus_hint());
        assert!(!preview.main_input_focused());

        preview.apply_fact(FactTarget::TodoEditInput(1), UiFactKind::Focused(false));
        assert!(!preview.edit_focused());
        assert!(!preview.edit_focus_hint());
    }

    #[test]
    fn todo_preview_randomized_trace_matches_oracle_subset() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let mut preview = TodoPreview::new(source).expect("todo preview");
        let mut oracle = OracleTodoState::new();
        let mut rng = TraceRng::new(0xA11CE);

        for step in 0..120u32 {
            let op = rng.next_range(9);
            match op {
                0 => {
                    let draft = format!("todo-{step}");
                    preview.apply_fact(FactTarget::MainInput, UiFactKind::DraftText(draft.clone()));
                    preview.apply_event(
                        TodoUiAction::MainInputKeyDown,
                        UiEventKind::KeyDown,
                        Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}{draft}")),
                    );
                    if !draft.trim().is_empty() {
                        oracle.todos.push(OracleTodo {
                            id: oracle.next_id,
                            title: draft.clone(),
                            completed: false,
                        });
                        oracle.next_id += 1;
                        oracle.main_draft.clear();
                    }
                }
                1 if !oracle.todos.is_empty() => {
                    let index = rng.next_range(oracle.todos.len() as u32) as usize;
                    let todo_id = oracle.todos[index].id;
                    preview.apply_event(
                        TodoUiAction::TodoCheckbox(todo_id),
                        UiEventKind::Click,
                        None,
                    );
                    oracle.todos[index].completed = !oracle.todos[index].completed;
                }
                2 => {
                    preview.apply_event(TodoUiAction::FilterAll, UiEventKind::Click, None);
                    oracle.filter = OracleFilter::All;
                }
                3 => {
                    preview.apply_event(TodoUiAction::FilterActive, UiEventKind::Click, None);
                    oracle.filter = OracleFilter::Active;
                }
                4 => {
                    preview.apply_event(TodoUiAction::FilterCompleted, UiEventKind::Click, None);
                    oracle.filter = OracleFilter::Completed;
                }
                5 if !oracle.todos.is_empty() => {
                    let index = rng.next_range(oracle.todos.len() as u32) as usize;
                    let todo_id = oracle.todos[index].id;
                    preview.apply_event(
                        TodoUiAction::TodoTitleDoubleClick(todo_id),
                        UiEventKind::DoubleClick,
                        None,
                    );
                    let next_title = format!("edited-{step}");
                    preview.apply_fact(
                        FactTarget::TodoEditInput(todo_id),
                        UiFactKind::DraftText(next_title.clone()),
                    );
                    preview.apply_event(
                        TodoUiAction::TodoEditKeyDown(todo_id),
                        UiEventKind::KeyDown,
                        Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}{next_title}")),
                    );
                    oracle.todos[index].title = next_title;
                    oracle.editing = None;
                }
                6 if !oracle.todos.is_empty() => {
                    let index = rng.next_range(oracle.todos.len() as u32) as usize;
                    let todo_id = oracle.todos[index].id;
                    preview.apply_event(
                        TodoUiAction::TodoTitleDoubleClick(todo_id),
                        UiEventKind::DoubleClick,
                        None,
                    );
                    preview.apply_fact(
                        FactTarget::TodoEditInput(todo_id),
                        UiFactKind::DraftText(format!("discard-{step}")),
                    );
                    preview.apply_event(
                        TodoUiAction::TodoEditKeyDown(todo_id),
                        UiEventKind::KeyDown,
                        Some("Escape"),
                    );
                    oracle.editing = None;
                }
                7 => {
                    preview.apply_event(TodoUiAction::ClearCompleted, UiEventKind::Click, None);
                    oracle.todos.retain(|todo| !todo.completed);
                }
                8 if !oracle.todos.is_empty() => {
                    let index = rng.next_range(oracle.todos.len() as u32) as usize;
                    let todo_id = oracle.todos[index].id;
                    preview.apply_fact(FactTarget::TodoTitle(todo_id), UiFactKind::Hovered(true));
                    preview.apply_event(
                        TodoUiAction::TodoDelete(todo_id),
                        UiEventKind::Click,
                        None,
                    );
                    oracle.todos.remove(index);
                }
                _ => {}
            }

            let text = preview.preview_text();
            for title in oracle.visible_titles() {
                assert!(
                    text.contains(&title),
                    "missing visible title `{title}` at step {step}"
                );
            }
            if oracle.todos.is_empty() {
                assert!(
                    !text.contains("item left"),
                    "unexpected count footer at step {step}: {text}"
                );
            } else {
                assert!(
                    text.contains(&format!("{} item", oracle.active_count()))
                        || text.contains(&format!("{} items", oracle.active_count())),
                    "wrong active count at step {step}: {text}"
                );
            }
            if oracle.completed_count() == 0 {
                assert!(
                    !text.contains("Clear completed"),
                    "unexpected clear button at step {step}"
                );
            }
        }
    }
}
