use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::ir::{FunctionInstanceId, MirrorCellId, RetainedNodeKey, SourcePortId, ViewSiteId};
use crate::list_form_actions::append_trimmed_text_input;
use crate::lower::{TodoPhysicalProgram, try_lower_todo_mvc_physical};
use crate::runtime_backed_domain::RuntimeBackedDomain;
use crate::runtime_backed_preview::RuntimeBackedPreviewState;
use crate::selected_list_filter::SelectedListFilter;
use crate::targeted_list_runtime::TargetedListRuntime;
use crate::text_input::{TextInputState, decode_key_down_payload};
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{
    NodeId, RenderOp, RenderRoot, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind, UiNode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Theme {
    Professional,
    Glass,
    Brutalist,
    Neumorphic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TodoItem {
    title: String,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TodoPhysicalUiAction {
    MainInputChange,
    MainInputKeyDown,
    MainInputBlur,
    MainInputFocus,
    FilterAll,
    FilterActive,
    FilterCompleted,
    ToggleAll,
    ClearCompleted,
    ThemeProfessional,
    ThemeGlass,
    ThemeBrutalist,
    ThemeNeumorphic,
    ToggleMode,
    TodoCheckbox(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FactTarget {
    MainInput,
}

pub struct TodoPhysicalPreview {
    _program: TodoPhysicalProgram,
    todos: TargetedListRuntime<TodoItem>,
    selected_filter: SelectedListFilter<Filter>,
    selected_theme: SelectedListFilter<Theme>,
    dark_mode: bool,
    main_input: TextInputState,
    ui: RuntimeBackedPreviewState<TodoPhysicalUiAction, FactTarget>,
}

impl InteractivePreview for TodoPhysicalPreview {
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

impl RuntimeBackedDomain for TodoPhysicalPreview {
    type Action = TodoPhysicalUiAction;
    type FactTarget = FactTarget;

    fn preview_state(&mut self) -> &mut RuntimeBackedPreviewState<Self::Action, Self::FactTarget> {
        &mut self.ui
    }

    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        TodoPhysicalPreview::render_document(self, ops)
    }

    fn handle_event(
        &mut self,
        action: Self::Action,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        TodoPhysicalPreview::apply_event(self, action, kind, payload)
    }

    fn handle_fact(&mut self, target: Self::FactTarget, kind: UiFactKind) -> bool {
        TodoPhysicalPreview::apply_fact(self, target, kind)
    }

    fn fact_cell(target: &Self::FactTarget) -> MirrorCellId {
        todo_physical_fact_cell(target)
    }

    fn fact_target(cell: MirrorCellId) -> Option<Self::FactTarget> {
        todo_physical_fact_target(cell)
    }
}

impl TodoPhysicalPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_todo_mvc_physical(source)?;
        Ok(Self {
            _program: program,
            todos: TargetedListRuntime::new([], 1),
            selected_filter: SelectedListFilter::new(Filter::All),
            selected_theme: SelectedListFilter::new(Theme::Professional),
            dark_mode: false,
            main_input: TextInputState::focused_with_hint(String::new()),
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

    fn apply_event(
        &mut self,
        action: TodoPhysicalUiAction,
        kind: UiEventKind,
        payload: Option<&str>,
    ) -> bool {
        match action {
            TodoPhysicalUiAction::MainInputChange => {
                if matches!(kind, UiEventKind::Input | UiEventKind::Change) {
                    return self.main_input.set_draft(payload.unwrap_or_default());
                }
                false
            }
            TodoPhysicalUiAction::MainInputKeyDown => {
                if kind == UiEventKind::KeyDown {
                    let mut changed = false;
                    let keydown = decode_key_down_payload(payload);
                    if let Some(current_text) = keydown.current_text {
                        changed |= self.main_input.set_draft(current_text);
                    }
                    if keydown.key == "Enter" {
                        return self.commit_main_input() || changed;
                    }
                    return changed;
                }
                false
            }
            TodoPhysicalUiAction::MainInputBlur => {
                if kind == UiEventKind::Blur {
                    return self.main_input.blur();
                }
                false
            }
            TodoPhysicalUiAction::MainInputFocus => {
                if kind == UiEventKind::Focus {
                    return self.main_input.apply_focus(true);
                }
                false
            }
            TodoPhysicalUiAction::FilterAll if kind == UiEventKind::Click => self
                .selected_filter
                .select_and_clear_focus_hint(Filter::All, &mut self.main_input),
            TodoPhysicalUiAction::FilterActive if kind == UiEventKind::Click => self
                .selected_filter
                .select_and_clear_focus_hint(Filter::Active, &mut self.main_input),
            TodoPhysicalUiAction::FilterCompleted if kind == UiEventKind::Click => self
                .selected_filter
                .select_and_clear_focus_hint(Filter::Completed, &mut self.main_input),
            TodoPhysicalUiAction::ToggleAll if kind == UiEventKind::Click => {
                let target = !self.all_completed();
                let changed = self
                    .todos
                    .items()
                    .iter()
                    .any(|todo| todo.value.completed != target)
                    || self.main_input.focus_hint;
                for todo in self.todos.items_mut() {
                    todo.value.completed = target;
                }
                self.main_input.clear_focus_hint();
                changed
            }
            TodoPhysicalUiAction::ClearCompleted if kind == UiEventKind::Click => {
                let before = self.todos.len();
                let retained = self.todos.retain(|todo| !todo.value.completed);
                let changed = self.todos.len() != before || self.main_input.focus_hint || retained;
                self.main_input.clear_focus_hint();
                changed
            }
            TodoPhysicalUiAction::ThemeProfessional if kind == UiEventKind::Click => self
                .selected_theme
                .select_and_clear_focus_hint(Theme::Professional, &mut self.main_input),
            TodoPhysicalUiAction::ThemeGlass if kind == UiEventKind::Click => self
                .selected_theme
                .select_and_clear_focus_hint(Theme::Glass, &mut self.main_input),
            TodoPhysicalUiAction::ThemeBrutalist if kind == UiEventKind::Click => self
                .selected_theme
                .select_and_clear_focus_hint(Theme::Brutalist, &mut self.main_input),
            TodoPhysicalUiAction::ThemeNeumorphic if kind == UiEventKind::Click => self
                .selected_theme
                .select_and_clear_focus_hint(Theme::Neumorphic, &mut self.main_input),
            TodoPhysicalUiAction::ToggleMode if kind == UiEventKind::Click => {
                self.dark_mode = !self.dark_mode;
                true
            }
            TodoPhysicalUiAction::TodoCheckbox(todo_id) if kind == UiEventKind::Click => {
                self.todos.update_by_id(todo_id, |todo| {
                    todo.value.completed = !todo.value.completed;
                })
            }
            _ => false,
        }
    }

    fn apply_fact(&mut self, target: FactTarget, kind: UiFactKind) -> bool {
        match (target, kind) {
            (FactTarget::MainInput, UiFactKind::DraftText(text)) => self.main_input.set_draft(text),
            (FactTarget::MainInput, UiFactKind::Focused(focused)) => {
                self.main_input.apply_focus(focused)
            }
            _ => false,
        }
    }

    fn commit_main_input(&mut self) -> bool {
        append_trimmed_text_input(&mut self.todos, &mut self.main_input, |title| TodoItem {
            title,
            completed: false,
        })
    }

    fn all_completed(&self) -> bool {
        !self.todos.is_empty() && self.todos.all(|todo| todo.value.completed)
    }

    fn active_count(&self) -> usize {
        self.todos.count(|todo| !todo.value.completed)
    }

    fn visible_todo_ids(&self) -> Vec<u64> {
        self.todos
            .items()
            .iter()
            .filter(|todo| match self.selected_filter.current() {
                Filter::All => true,
                Filter::Active => !todo.value.completed,
                Filter::Completed => todo.value.completed,
            })
            .map(|todo| todo.id)
            .collect()
    }

    fn render_document(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let children = vec![
            self.render_header(),
            self.render_theme_row(ops),
            self.render_input_row(ops),
            self.render_list(ops),
            self.render_panel_footer(ops),
            self.render_footer_texts(),
        ];
        self.element_node(ViewSiteId(500), "div", None, children)
    }

    fn render_header(&mut self) -> UiNode {
        self.element_node(ViewSiteId(501), "h1", Some("todos".to_string()), Vec::new())
    }

    fn render_theme_row(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let children = vec![
            self.render_button(
                ops,
                ViewSiteId(503),
                "Professional",
                TodoPhysicalProgram::THEME_PROFESSIONAL_PORT,
                TodoPhysicalUiAction::ThemeProfessional,
                self.selected_theme.is(Theme::Professional),
            ),
            self.render_button(
                ops,
                ViewSiteId(504),
                "Glass",
                TodoPhysicalProgram::THEME_GLASS_PORT,
                TodoPhysicalUiAction::ThemeGlass,
                self.selected_theme.is(Theme::Glass),
            ),
            self.render_button(
                ops,
                ViewSiteId(505),
                "Brutalist",
                TodoPhysicalProgram::THEME_BRUTALIST_PORT,
                TodoPhysicalUiAction::ThemeBrutalist,
                self.selected_theme.is(Theme::Brutalist),
            ),
            self.render_button(
                ops,
                ViewSiteId(506),
                "Neumorphic",
                TodoPhysicalProgram::THEME_NEUMORPHIC_PORT,
                TodoPhysicalUiAction::ThemeNeumorphic,
                self.selected_theme.is(Theme::Neumorphic),
            ),
            self.render_button(
                ops,
                ViewSiteId(507),
                if self.dark_mode {
                    "Light mode"
                } else {
                    "Dark mode"
                },
                TodoPhysicalProgram::TOGGLE_MODE_PORT,
                TodoPhysicalUiAction::ToggleMode,
                false,
            ),
        ];
        self.element_node(ViewSiteId(502), "div", None, children)
    }

    fn render_input_row(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let mut children = Vec::new();
        if !self.todos.is_empty() {
            children.push(self.render_toggle_all_control(ops));
        }
        children.push(self.render_main_input(ops));
        self.element_node(ViewSiteId(509), "div", None, children)
    }

    fn render_toggle_all_control(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let checkbox = self.element_node(ViewSiteId(508), "input", None, Vec::new());
        self.configure_checkbox(
            ops,
            checkbox.id,
            "toggle-all",
            self.all_completed(),
            TodoPhysicalProgram::TOGGLE_ALL_PORT,
            TodoPhysicalUiAction::ToggleAll,
        );
        let label = self.element_node(ViewSiteId(525), "span", Some(">".to_string()), Vec::new());
        self.attach_port(
            ops,
            label.id,
            TodoPhysicalProgram::TOGGLE_ALL_PORT,
            UiEventKind::Click,
            TodoPhysicalUiAction::ToggleAll,
        );
        self.element_node(ViewSiteId(526), "div", None, vec![checkbox, label])
    }

    fn render_main_input(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let node = self.element_node(ViewSiteId(510), "input", None, Vec::new());
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
        if self.main_input.focus_hint {
            ops.push(RenderOp::SetProperty {
                id: node.id,
                name: "autofocus".to_string(),
                value: Some("true".to_string()),
            });
        }
        ops.push(RenderOp::SetInputValue {
            id: node.id,
            value: self.main_input.draft.clone(),
        });
        self.ui.bind_fact_target(node.id, FactTarget::MainInput);
        self.attach_port(
            ops,
            node.id,
            TodoPhysicalProgram::MAIN_INPUT_CHANGE_PORT,
            UiEventKind::Input,
            TodoPhysicalUiAction::MainInputChange,
        );
        self.attach_port(
            ops,
            node.id,
            TodoPhysicalProgram::MAIN_INPUT_CHANGE_PORT,
            UiEventKind::Change,
            TodoPhysicalUiAction::MainInputChange,
        );
        self.attach_port(
            ops,
            node.id,
            TodoPhysicalProgram::MAIN_INPUT_KEY_DOWN_PORT,
            UiEventKind::KeyDown,
            TodoPhysicalUiAction::MainInputKeyDown,
        );
        self.attach_port(
            ops,
            node.id,
            TodoPhysicalProgram::MAIN_INPUT_BLUR_PORT,
            UiEventKind::Blur,
            TodoPhysicalUiAction::MainInputBlur,
        );
        self.attach_port(
            ops,
            node.id,
            TodoPhysicalProgram::MAIN_INPUT_FOCUS_PORT,
            UiEventKind::Focus,
            TodoPhysicalUiAction::MainInputFocus,
        );
        node
    }

    fn render_list(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let visible_items = self
            .visible_todo_ids()
            .into_iter()
            .filter_map(|id| self.todos.find(id).cloned())
            .collect::<Vec<_>>();
        let children = visible_items
            .iter()
            .map(|todo| self.render_todo_row(ops, todo.clone()))
            .collect();
        self.element_node(ViewSiteId(511), "div", None, children)
    }

    fn render_todo_row(
        &mut self,
        ops: &mut Vec<RenderOp>,
        todo: crate::mapped_list_runtime::MappedListItem<TodoItem>,
    ) -> UiNode {
        let checkbox =
            self.element_node_with_item(ViewSiteId(512), "input", None, todo.id, Vec::new());
        self.configure_checkbox(
            ops,
            checkbox.id,
            &format!("todo-{}", todo.id),
            todo.value.completed,
            todo_checkbox_port(todo.id),
            TodoPhysicalUiAction::TodoCheckbox(todo.id),
        );
        let title = self.element_node_with_item(
            ViewSiteId(513),
            "span",
            Some(todo.value.title.clone()),
            todo.id,
            Vec::new(),
        );
        if todo.value.completed {
            ops.push(RenderOp::SetStyle {
                id: title.id,
                name: "text-decoration".to_string(),
                value: Some("line-through".to_string()),
            });
        }
        self.element_node_with_item(ViewSiteId(514), "div", None, todo.id, vec![checkbox, title])
    }

    fn render_panel_footer(&mut self, ops: &mut Vec<RenderOp>) -> UiNode {
        let active_count = self.active_count();
        let completed_count = self.todos.count(|todo| todo.value.completed);
        let count_label = if active_count == 1 {
            "1 item left".to_string()
        } else {
            format!("{active_count} items left")
        };
        let mut children = vec![
            self.element_node(ViewSiteId(515), "span", Some(count_label), Vec::new()),
            self.render_button(
                ops,
                ViewSiteId(516),
                "All",
                TodoPhysicalProgram::FILTER_ALL_PORT,
                TodoPhysicalUiAction::FilterAll,
                self.selected_filter.is(Filter::All),
            ),
            self.render_button(
                ops,
                ViewSiteId(517),
                "Active",
                TodoPhysicalProgram::FILTER_ACTIVE_PORT,
                TodoPhysicalUiAction::FilterActive,
                self.selected_filter.is(Filter::Active),
            ),
            self.render_button(
                ops,
                ViewSiteId(518),
                "Completed",
                TodoPhysicalProgram::FILTER_COMPLETED_PORT,
                TodoPhysicalUiAction::FilterCompleted,
                self.selected_filter.is(Filter::Completed),
            ),
        ];
        if completed_count > 0 {
            children.push(self.render_button(
                ops,
                ViewSiteId(519),
                "Clear completed",
                TodoPhysicalProgram::CLEAR_COMPLETED_PORT,
                TodoPhysicalUiAction::ClearCompleted,
                false,
            ));
        }
        self.element_node(ViewSiteId(520), "div", None, children)
    }

    fn render_footer_texts(&mut self) -> UiNode {
        let children = vec![
            self.element_node(
                ViewSiteId(522),
                "p",
                Some("Double-click to edit a todo".to_string()),
                Vec::new(),
            ),
            self.element_node(
                ViewSiteId(523),
                "p",
                Some("Created by Martin Kavík".to_string()),
                Vec::new(),
            ),
            self.element_node(
                ViewSiteId(524),
                "p",
                Some("Part of TodoMVC".to_string()),
                Vec::new(),
            ),
        ];
        self.element_node(ViewSiteId(521), "div", None, children)
    }

    fn render_button(
        &mut self,
        ops: &mut Vec<RenderOp>,
        view_site: ViewSiteId,
        label: &str,
        port: SourcePortId,
        action: TodoPhysicalUiAction,
        selected: bool,
    ) -> UiNode {
        let node = self.element_node(view_site, "button", Some(label.to_string()), Vec::new());
        if selected {
            ops.push(RenderOp::SetStyle {
                id: node.id,
                name: "outline".to_string(),
                value: Some("2px solid rgb(120, 80, 80)".to_string()),
            });
        }
        self.attach_port(ops, node.id, port, UiEventKind::Click, action);
        node
    }

    fn configure_checkbox(
        &mut self,
        ops: &mut Vec<RenderOp>,
        node_id: NodeId,
        element_id: &str,
        checked: bool,
        port: SourcePortId,
        action: TodoPhysicalUiAction,
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
        action: TodoPhysicalUiAction,
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

fn todo_checkbox_port(todo_id: u64) -> SourcePortId {
    SourcePortId(8_000 + todo_id as u32)
}

fn todo_physical_fact_cell(target: &FactTarget) -> MirrorCellId {
    match target {
        FactTarget::MainInput => MirrorCellId(81),
    }
}

fn todo_physical_fact_target(cell: MirrorCellId) -> Option<FactTarget> {
    match cell.0 {
        81 => Some(FactTarget::MainInput),
        _ => None,
    }
}

pub fn render_todo_physical_preview(preview: TodoPhysicalPreview) -> impl Element {
    render_interactive_preview(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_input::KEYDOWN_TEXT_SEPARATOR;

    #[test]
    fn todo_physical_preview_supports_add_filter_toggle_and_theme_mode() {
        let source =
            include_str!("../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn");
        let mut preview = TodoPhysicalPreview::new(source).expect("todo physical preview");
        assert!(preview.preview_text().contains("Professional"));
        assert!(preview.preview_text().contains("Dark mode"));

        preview.apply_fact(
            FactTarget::MainInput,
            UiFactKind::DraftText("Buy groceries".to_string()),
        );
        preview.apply_event(
            TodoPhysicalUiAction::MainInputKeyDown,
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}Buy groceries")),
        );
        preview.apply_fact(
            FactTarget::MainInput,
            UiFactKind::DraftText("Clean room".to_string()),
        );
        preview.apply_event(
            TodoPhysicalUiAction::MainInputKeyDown,
            UiEventKind::KeyDown,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}Clean room")),
        );
        assert!(preview.preview_text().contains("2 items left"));

        let first_id = preview.todos.first().expect("todo").id;
        preview.apply_event(
            TodoPhysicalUiAction::TodoCheckbox(first_id),
            UiEventKind::Click,
            None,
        );
        assert!(preview.preview_text().contains("1 item left"));

        preview.apply_event(TodoPhysicalUiAction::FilterActive, UiEventKind::Click, None);
        let text = preview.preview_text();
        assert!(!text.contains("Buy groceries"));
        assert!(text.contains("Clean room"));

        preview.apply_event(TodoPhysicalUiAction::ToggleMode, UiEventKind::Click, None);
        assert!(preview.preview_text().contains("Light mode"));
    }
}
