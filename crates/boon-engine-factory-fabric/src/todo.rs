use crate::depot::{
    FunctionInstanceId, ListDepot, ListHandleId, ListItemId, ListMapInstanceTable, MapperSiteId,
};
use crate::host_view::{HostViewNode, HostViewTree};
use crate::lower::TodoProgram;
use crate::metrics::{InteractionMetricsReport, LatencySummary};
use crate::{FabricValue, RegionId, RuntimeCore};
use boon_renderer_zoon::FakeRenderState;
use boon_scene::RenderDiffBatch;
use boon_scene::{EventPortId, NodeId, UiEvent, UiEventKind};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const OUTLINE_STYLE: &str = "1px solid rgb(210, 140, 120)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoFilter {
    All,
    Active,
    Completed,
}

#[derive(Debug)]
pub struct TodoState {
    pub region: RegionId,
    pub program: TodoProgram,
    depot: ListDepot,
    list_handle: ListHandleId,
    instances: ListMapInstanceTable,
    ui: TodoUi,
    items: BTreeMap<ListItemId, TodoItemState>,
    selected_filter: TodoFilter,
    last_recreated_mapped_scopes: usize,
}

#[derive(Debug)]
struct TodoUi {
    root: NodeId,
    heading: NodeId,
    input_row: NodeId,
    toggle_all: TodoCheckboxUi,
    main_input: TodoInputUi,
    list_root: NodeId,
    footer_row: NodeId,
    count_text: NodeId,
    filters_row: NodeId,
    filter_all: TodoButtonUi,
    filter_active: TodoButtonUi,
    filter_completed: TodoButtonUi,
    clear_completed: TodoButtonUi,
    hint_one: NodeId,
    hint_two: NodeId,
    hint_three: NodeId,
    item_views: BTreeMap<ListItemId, TodoItemUi>,
}

#[derive(Debug)]
struct TodoItemUi {
    _instance: FunctionInstanceId,
    root: NodeId,
    checkbox: TodoCheckboxUi,
    title_label: TodoButtonUi,
    remove_button: TodoButtonUi,
    edit_input: TodoInputUi,
}

#[derive(Debug)]
struct TodoCheckboxUi {
    node: NodeId,
    click_port: EventPortId,
}

#[derive(Debug)]
struct TodoInputUi {
    node: NodeId,
    key_down_port: EventPortId,
    change_port: EventPortId,
    blur_port: EventPortId,
    focus_port: EventPortId,
}

#[derive(Debug)]
struct TodoButtonUi {
    node: NodeId,
    click_port: EventPortId,
}

#[derive(Debug, Clone)]
struct TodoItemState {
    title: String,
    completed: bool,
    editing: bool,
}

impl TodoState {
    pub fn new(program: TodoProgram, runtime: &mut RuntimeCore) -> Self {
        let region = runtime.alloc_region();
        let mut depot = ListDepot::new();
        let list_handle = depot.list_literal(
            program
                .initial_titles
                .iter()
                .cloned()
                .map(FabricValue::from),
        );
        let mut items = BTreeMap::new();
        if let Some(list) = depot.list(list_handle) {
            for entry in list.entries() {
                items.insert(
                    entry.id,
                    TodoItemState {
                        title: match &entry.value {
                            FabricValue::Text(text) => text.clone(),
                            _ => String::new(),
                        },
                        completed: false,
                        editing: false,
                    },
                );
            }
        }

        let mut state = Self {
            region,
            program,
            depot,
            list_handle,
            instances: ListMapInstanceTable::new(MapperSiteId(1)),
            ui: TodoUi::new(),
            items,
            selected_filter: TodoFilter::All,
            last_recreated_mapped_scopes: 0,
        };
        state.sync_instances();
        state
    }

    pub fn handle_event(&mut self, runtime: &mut RuntimeCore, event: &UiEvent) -> bool {
        runtime.schedule_region(self.region);
        let _ = runtime.pop_ready_region();

        if event.target == self.ui.toggle_all.click_port && event.kind == UiEventKind::Click {
            self.toggle_all();
            return true;
        }
        if event.target == self.ui.filter_all.click_port && event.kind == UiEventKind::Click {
            self.selected_filter = TodoFilter::All;
            return true;
        }
        if event.target == self.ui.filter_active.click_port && event.kind == UiEventKind::Click {
            self.selected_filter = TodoFilter::Active;
            return true;
        }
        if event.target == self.ui.filter_completed.click_port && event.kind == UiEventKind::Click {
            self.selected_filter = TodoFilter::Completed;
            return true;
        }
        if event.target == self.ui.clear_completed.click_port && event.kind == UiEventKind::Click {
            self.clear_completed();
            return true;
        }
        if event.target == self.ui.main_input.key_down_port && event.kind == UiEventKind::KeyDown {
            return self.handle_main_input_key(event.payload.as_deref().unwrap_or_default());
        }

        for (item_id, item_ui) in &self.ui.item_views {
            if event.target == item_ui.checkbox.click_port && event.kind == UiEventKind::Click {
                if let Some(item) = self.items.get_mut(item_id) {
                    item.completed = !item.completed;
                    return true;
                }
            }
            if event.target == item_ui.title_label.click_port
                && event.kind == UiEventKind::DoubleClick
            {
                if let Some(item) = self.items.get_mut(item_id) {
                    item.editing = true;
                    return true;
                }
            }
            if event.target == item_ui.remove_button.click_port && event.kind == UiEventKind::Click
            {
                let _ = self.depot.remove_item(self.list_handle, *item_id);
                self.items.remove(item_id);
                self.sync_instances();
                return true;
            }
            if event.target == item_ui.edit_input.key_down_port
                && event.kind == UiEventKind::KeyDown
            {
                return self
                    .handle_edit_key(*item_id, event.payload.as_deref().unwrap_or_default());
            }
            if event.target == item_ui.edit_input.blur_port && event.kind == UiEventKind::Blur {
                self.commit_edit_from_payload(
                    *item_id,
                    event.payload.as_deref().unwrap_or_default(),
                );
                return true;
            }
        }

        false
    }

    pub fn view_tree(&self) -> HostViewTree {
        let any_editing = self.items.values().any(|item| item.editing);
        let visible_items = self.visible_item_ids();
        let active_count = self.active_count();
        let completed_count = self.completed_count();
        let all_completed = !self.items.is_empty() && active_count == 0;

        let input_row = HostViewNode::element(self.ui.input_row, "div").with_children(vec![
            HostViewNode::element(self.ui.toggle_all.node, "input")
                .with_property("type", "checkbox")
                .with_checked(all_completed)
                .with_event_port(self.ui.toggle_all.click_port, UiEventKind::Click),
            HostViewNode::element(self.ui.main_input.node, "input")
                .with_property("type", "text")
                .with_property("placeholder", self.program.placeholder.clone())
                .with_property("autofocus", (!any_editing).to_string())
                .with_input_value(String::new())
                .with_event_port(self.ui.main_input.key_down_port, UiEventKind::KeyDown)
                .with_event_port(self.ui.main_input.change_port, UiEventKind::Change)
                .with_event_port(self.ui.main_input.blur_port, UiEventKind::Blur)
                .with_event_port(self.ui.main_input.focus_port, UiEventKind::Focus),
        ]);

        let list_children = visible_items
            .iter()
            .filter_map(|item_id| self.view_for_item(*item_id))
            .collect::<Vec<_>>();
        let list_root = HostViewNode::element(self.ui.list_root, "ul").with_children(list_children);

        let filter_buttons = vec![
            self.filter_button_view(
                &self.ui.filter_all,
                &self.program.filter_labels[0],
                TodoFilter::All,
            ),
            self.filter_button_view(
                &self.ui.filter_active,
                &self.program.filter_labels[1],
                TodoFilter::Active,
            ),
            self.filter_button_view(
                &self.ui.filter_completed,
                &self.program.filter_labels[2],
                TodoFilter::Completed,
            ),
        ];

        let mut footer_children = vec![
            HostViewNode::element(self.ui.count_text, "span")
                .with_text(item_count_text(active_count)),
            HostViewNode::element(self.ui.filters_row, "div").with_children(filter_buttons),
        ];
        if completed_count > 0 {
            footer_children.push(
                HostViewNode::element(self.ui.clear_completed.node, "button")
                    .with_text(self.program.clear_completed_label.clone())
                    .with_event_port(self.ui.clear_completed.click_port, UiEventKind::Click),
            );
        }
        let footer_row =
            HostViewNode::element(self.ui.footer_row, "footer").with_children(footer_children);

        HostViewTree::from_root(
            HostViewNode::element(self.ui.root, "div").with_children(vec![
                HostViewNode::element(self.ui.heading, "h1").with_text(self.program.title.clone()),
                input_row,
                list_root,
                footer_row,
                HostViewNode::element(self.ui.hint_one, "p")
                    .with_text(self.program.footer_hints[0].clone()),
                HostViewNode::element(self.ui.hint_two, "p")
                    .with_text(self.program.footer_hints[1].clone()),
                HostViewNode::element(self.ui.hint_three, "p")
                    .with_text(self.program.footer_hints[2].clone()),
            ]),
        )
    }

    fn filter_button_view(
        &self,
        ui: &TodoButtonUi,
        label: &str,
        filter: TodoFilter,
    ) -> HostViewNode {
        let mut button = HostViewNode::element(ui.node, "button")
            .with_text(label.to_string())
            .with_event_port(ui.click_port, UiEventKind::Click);
        if self.selected_filter == filter {
            button = button.with_style("outline", OUTLINE_STYLE);
        }
        button
    }

    fn view_for_item(&self, item_id: ListItemId) -> Option<HostViewNode> {
        let item = self.items.get(&item_id)?;
        let ui = self.ui.item_views.get(&item_id)?;

        let mut children = vec![
            HostViewNode::element(ui.checkbox.node, "input")
                .with_property("type", "checkbox")
                .with_checked(item.completed)
                .with_event_port(ui.checkbox.click_port, UiEventKind::Click),
        ];
        if item.editing {
            children.push(
                HostViewNode::element(ui.edit_input.node, "input")
                    .with_property("type", "text")
                    .with_property("focused", "true")
                    .with_input_value(item.title.clone())
                    .with_event_port(ui.edit_input.key_down_port, UiEventKind::KeyDown)
                    .with_event_port(ui.edit_input.change_port, UiEventKind::Change)
                    .with_event_port(ui.edit_input.blur_port, UiEventKind::Blur),
            );
        } else {
            children.push(
                HostViewNode::element(ui.title_label.node, "button")
                    .with_text(item.title.clone())
                    .with_event_port(ui.title_label.click_port, UiEventKind::DoubleClick),
            );
        }
        children.push(
            HostViewNode::element(ui.remove_button.node, "button")
                .with_text("×")
                .with_event_port(ui.remove_button.click_port, UiEventKind::Click),
        );

        Some(HostViewNode::element(ui.root, "li").with_children(children))
    }

    fn visible_item_ids(&self) -> Vec<ListItemId> {
        self.depot
            .list(self.list_handle)
            .map(|list| {
                list.entries()
                    .iter()
                    .filter_map(|entry| {
                        let item = self.items.get(&entry.id)?;
                        match self.selected_filter {
                            TodoFilter::All => Some(entry.id),
                            TodoFilter::Active if !item.completed => Some(entry.id),
                            TodoFilter::Completed if item.completed => Some(entry.id),
                            _ => None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn active_count(&self) -> usize {
        self.items.values().filter(|item| !item.completed).count()
    }

    fn completed_count(&self) -> usize {
        self.items.values().filter(|item| item.completed).count()
    }

    fn toggle_all(&mut self) {
        let should_complete = self.active_count() > 0;
        for item in self.items.values_mut() {
            item.completed = should_complete;
        }
    }

    fn clear_completed(&mut self) {
        let completed = self
            .items
            .iter()
            .filter_map(|(item_id, item)| item.completed.then_some(*item_id))
            .collect::<Vec<_>>();
        for item_id in completed {
            let _ = self.depot.remove_item(self.list_handle, item_id);
            self.items.remove(&item_id);
        }
        self.sync_instances();
    }

    fn handle_main_input_key(&mut self, payload: &str) -> bool {
        let (key, text) = decode_key_payload(payload);
        if key != "Enter" {
            return false;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        let item_id = self
            .depot
            .append(self.list_handle, FabricValue::from(trimmed.to_string()))
            .expect("live todo list should accept append");
        self.items.insert(
            item_id,
            TodoItemState {
                title: trimmed.to_string(),
                completed: false,
                editing: false,
            },
        );
        self.sync_instances();
        true
    }

    fn handle_edit_key(&mut self, item_id: ListItemId, payload: &str) -> bool {
        let (key, text) = decode_key_payload(payload);
        match key {
            "Enter" => {
                self.commit_edit(item_id, text);
                true
            }
            "Escape" => {
                if let Some(item) = self.items.get_mut(&item_id) {
                    item.editing = false;
                }
                true
            }
            _ => false,
        }
    }

    fn commit_edit_from_payload(&mut self, item_id: ListItemId, payload: &str) {
        let (_, text) = decode_key_payload(payload);
        self.commit_edit(item_id, text);
    }

    fn commit_edit(&mut self, item_id: ListItemId, text: String) {
        if let Some(item) = self.items.get_mut(&item_id) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                item.title = trimmed.to_string();
            }
            item.editing = false;
        }
    }

    fn sync_instances(&mut self) {
        let ids = self
            .depot
            .list(self.list_handle)
            .map(|list| {
                list.entries()
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let sync = self.instances.sync_items(&ids);
        self.last_recreated_mapped_scopes = sync.created.len();
        for dropped in sync.dropped {
            self.ui.item_views.remove(&dropped);
        }
        for (item_id, instance) in sync.created {
            self.ui
                .item_views
                .insert(item_id, TodoItemUi::new(instance));
        }
    }

    pub(crate) const fn last_recreated_mapped_scopes(&self) -> usize {
        self.last_recreated_mapped_scopes
    }
}

impl TodoUi {
    fn new() -> Self {
        Self {
            root: NodeId::new(),
            heading: NodeId::new(),
            input_row: NodeId::new(),
            toggle_all: TodoCheckboxUi::new(),
            main_input: TodoInputUi::new(),
            list_root: NodeId::new(),
            footer_row: NodeId::new(),
            count_text: NodeId::new(),
            filters_row: NodeId::new(),
            filter_all: TodoButtonUi::new(),
            filter_active: TodoButtonUi::new(),
            filter_completed: TodoButtonUi::new(),
            clear_completed: TodoButtonUi::new(),
            hint_one: NodeId::new(),
            hint_two: NodeId::new(),
            hint_three: NodeId::new(),
            item_views: BTreeMap::new(),
        }
    }
}

impl TodoItemUi {
    fn new(instance: FunctionInstanceId) -> Self {
        Self {
            _instance: instance,
            root: NodeId::new(),
            checkbox: TodoCheckboxUi::new(),
            title_label: TodoButtonUi::new(),
            remove_button: TodoButtonUi::new(),
            edit_input: TodoInputUi::new(),
        }
    }
}

impl TodoCheckboxUi {
    fn new() -> Self {
        Self {
            node: NodeId::new(),
            click_port: EventPortId::new(),
        }
    }
}

impl TodoInputUi {
    fn new() -> Self {
        Self {
            node: NodeId::new(),
            key_down_port: EventPortId::new(),
            change_port: EventPortId::new(),
            blur_port: EventPortId::new(),
            focus_port: EventPortId::new(),
        }
    }
}

impl TodoButtonUi {
    fn new() -> Self {
        Self {
            node: NodeId::new(),
            click_port: EventPortId::new(),
        }
    }
}

fn decode_key_payload(payload: &str) -> (&str, String) {
    match payload.split_once('\u{1f}') {
        Some((key, text)) => (key, text.to_string()),
        None => (payload, String::new()),
    }
}

fn item_count_text(active_count: usize) -> String {
    if active_count == 1 {
        "1 item left".to_string()
    } else {
        format!("{active_count} items left")
    }
}

pub(crate) fn todo_metrics_capture(
    program: TodoProgram,
) -> Result<InteractionMetricsReport, String> {
    let startup_started = Instant::now();
    let mut runtime = RuntimeCore::new();
    let mut state = TodoState::new(program, &mut runtime);
    let mut render = FakeRenderState::default();
    let mut previous_view = state.view_tree();
    let (initial_batch, _) = previous_view.into_render_batch_with_stats();
    render
        .apply_batch(&initial_batch)
        .map_err(|error| format!("FactoryFabric todo metrics render error: {error:?}"))?;
    let startup_millis = startup_started.elapsed().as_secs_f64() * 1000.0;

    let add_to_paint =
        collect_todo_add_samples(&mut state, &mut runtime, &mut render, &mut previous_view)?;
    let toggle_to_paint =
        collect_todo_toggle_samples(&mut state, &mut runtime, &mut render, &mut previous_view)?;
    let filter_to_paint =
        collect_todo_filter_samples(&mut state, &mut runtime, &mut render, &mut previous_view)?;
    let edit_to_paint =
        collect_todo_edit_samples(&mut state, &mut runtime, &mut render, &mut previous_view)?;

    Ok(InteractionMetricsReport {
        startup_millis,
        add_to_paint: LatencySummary::from_durations(&add_to_paint),
        toggle_to_paint: LatencySummary::from_durations(&toggle_to_paint),
        filter_to_paint: LatencySummary::from_durations(&filter_to_paint),
        edit_to_paint: LatencySummary::from_durations(&edit_to_paint),
    })
}

fn collect_todo_add_samples(
    state: &mut TodoState,
    runtime: &mut RuntimeCore,
    render: &mut FakeRenderState,
    previous_view: &mut HostViewTree,
) -> Result<Vec<Duration>, String> {
    let mut samples = Vec::new();
    for index in 0..24 {
        let started = Instant::now();
        let handled = state.handle_event(
            runtime,
            &UiEvent {
                target: state.ui.main_input.key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter\u{1f}Factory item {index}")),
            },
        );
        if !handled {
            return Err("FactoryFabric todo metrics add event was not handled".to_string());
        }
        apply_todo_diff(state, render, previous_view)?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

fn collect_todo_toggle_samples(
    state: &mut TodoState,
    runtime: &mut RuntimeCore,
    render: &mut FakeRenderState,
    previous_view: &mut HostViewTree,
) -> Result<Vec<Duration>, String> {
    let mut samples = Vec::new();
    let visible = state.visible_item_ids();
    let Some(target) = visible.first().copied() else {
        return Err("FactoryFabric todo metrics toggle requires a visible item".to_string());
    };
    let port = state.ui.item_views[&target].checkbox.click_port;
    for _ in 0..24 {
        let started = Instant::now();
        let handled = state.handle_event(
            runtime,
            &UiEvent {
                target: port,
                kind: UiEventKind::Click,
                payload: None,
            },
        );
        if !handled {
            return Err("FactoryFabric todo metrics toggle event was not handled".to_string());
        }
        apply_todo_diff(state, render, previous_view)?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

fn collect_todo_filter_samples(
    state: &mut TodoState,
    runtime: &mut RuntimeCore,
    render: &mut FakeRenderState,
    previous_view: &mut HostViewTree,
) -> Result<Vec<Duration>, String> {
    let mut samples = Vec::new();
    let filter_ports = [
        state.ui.filter_active.click_port,
        state.ui.filter_completed.click_port,
        state.ui.filter_all.click_port,
    ];
    for port in filter_ports.into_iter().cycle().take(24) {
        let started = Instant::now();
        let handled = state.handle_event(
            runtime,
            &UiEvent {
                target: port,
                kind: UiEventKind::Click,
                payload: None,
            },
        );
        if !handled {
            return Err("FactoryFabric todo metrics filter event was not handled".to_string());
        }
        apply_todo_diff(state, render, previous_view)?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

fn collect_todo_edit_samples(
    state: &mut TodoState,
    runtime: &mut RuntimeCore,
    render: &mut FakeRenderState,
    previous_view: &mut HostViewTree,
) -> Result<Vec<Duration>, String> {
    let mut samples = Vec::new();
    let visible = state.visible_item_ids();
    let Some(target) = visible.first().copied() else {
        return Err("FactoryFabric todo metrics edit requires a visible item".to_string());
    };
    for index in 0..24 {
        let title_port = state.ui.item_views[&target].title_label.click_port;
        let edit_port = state.ui.item_views[&target].edit_input.key_down_port;
        let handled = state.handle_event(
            runtime,
            &UiEvent {
                target: title_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            },
        );
        if !handled {
            return Err("FactoryFabric todo metrics edit activation was not handled".to_string());
        }
        apply_todo_diff(state, render, previous_view)?;

        let started = Instant::now();
        let handled = state.handle_event(
            runtime,
            &UiEvent {
                target: edit_port,
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter\u{1f}Factory item edited {index}")),
            },
        );
        if !handled {
            return Err("FactoryFabric todo metrics edit commit was not handled".to_string());
        }
        apply_todo_diff(state, render, previous_view)?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

fn apply_todo_diff(
    state: &TodoState,
    render: &mut FakeRenderState,
    previous_view: &mut HostViewTree,
) -> Result<RenderDiffBatch, String> {
    let next_view = state.view_tree();
    let (batch, _) = previous_view.diff_with_stats(&next_view);
    render
        .apply_batch(&batch)
        .map_err(|error| format!("FactoryFabric todo metrics render error: {error:?}"))?;
    *previous_view = next_view;
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::{TodoFilter, TodoState};
    use crate::RuntimeCore;
    use crate::lower::TodoProgram;
    use boon_renderer_zoon::FakeRenderState;
    use boon_scene::{UiEvent, UiEventKind, UiNodeKind};
    use std::collections::BTreeMap;

    fn sample_program() -> TodoProgram {
        TodoProgram {
            title: "todos".to_string(),
            placeholder: "What needs to be done?".to_string(),
            initial_titles: vec!["Buy groceries".to_string(), "Clean room".to_string()],
            filter_labels: [
                "All".to_string(),
                "Active".to_string(),
                "Completed".to_string(),
            ],
            clear_completed_label: "Clear completed".to_string(),
            footer_hints: [
                "Double-click to edit a todo".to_string(),
                "Created by Martin Kavík".to_string(),
                "Part of TodoMVC".to_string(),
            ],
        }
    }

    fn flatten_text(root: &boon_scene::UiNode, out: &mut String) {
        match &root.kind {
            UiNodeKind::Element { text, .. } => {
                if let Some(text) = text {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(text);
                }
            }
            UiNodeKind::Text { text } => {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(text);
            }
        }
        for child in &root.children {
            flatten_text(child, out);
        }
    }

    #[test]
    fn add_toggle_filter_edit_and_remove_flow_updates_view() {
        let mut runtime = RuntimeCore::new();
        let mut state = TodoState::new(sample_program(), &mut runtime);
        let mut render_state = FakeRenderState::default();
        render_state
            .apply_batch(&state.view_tree().into_render_batch())
            .expect("initial batch should apply");

        let main_input_port = state.ui.main_input.key_down_port;
        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: main_input_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1f}Walk the dog".to_string()),
            },
        ));
        render_state
            .apply_batch(&state.view_tree().into_render_batch())
            .expect("updated batch should apply");

        let all_ids = state.visible_item_ids();
        let added = *all_ids.last().expect("new todo should exist");
        let checkbox_port = state.ui.item_views[&added].checkbox.click_port;
        let title_port = state.ui.item_views[&added].title_label.click_port;
        let edit_port = state.ui.item_views[&added].edit_input.key_down_port;
        let remove_port = state.ui.item_views[&added].remove_button.click_port;

        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: checkbox_port,
                kind: UiEventKind::Click,
                payload: None,
            },
        ));
        state.selected_filter = TodoFilter::Completed;
        let visible = state.visible_item_ids();
        assert!(visible.contains(&added));

        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: title_port,
                kind: UiEventKind::DoubleClick,
                payload: None,
            },
        ));
        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: edit_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1f}Walk the dog EDITED".to_string()),
            },
        ));
        assert!(state.handle_event(
            &mut runtime,
            &UiEvent {
                target: remove_port,
                kind: UiEventKind::Click,
                payload: None,
            },
        ));

        let root = match state.view_tree().root {
            Some(root) => root.to_ui_node(),
            None => panic!("expected root"),
        };
        let mut text = String::new();
        flatten_text(&root, &mut text);
        assert!(!text.contains("Walk the dog"));
        assert!(text.contains("Double-click to edit a todo"));
    }

    #[test]
    fn unaffected_items_keep_view_identity_after_append_and_remove() {
        let mut runtime = RuntimeCore::new();
        let mut state = TodoState::new(sample_program(), &mut runtime);
        let original_ids = state
            .visible_item_ids()
            .iter()
            .map(|item_id| (*item_id, state.ui.item_views[item_id].root))
            .collect::<BTreeMap<_, _>>();

        let _ = state.handle_event(
            &mut runtime,
            &UiEvent {
                target: state.ui.main_input.key_down_port,
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1f}Test todo".to_string()),
            },
        );
        let new_item = *state.visible_item_ids().last().expect("new item");
        let _ = state.handle_event(
            &mut runtime,
            &UiEvent {
                target: state.ui.item_views[&new_item].remove_button.click_port,
                kind: UiEventKind::Click,
                payload: None,
            },
        );

        for (item_id, root_id) in original_ids {
            assert_eq!(state.ui.item_views[&item_id].root, root_id);
        }
    }

    #[test]
    fn initial_view_contains_expected_text_and_count() {
        let mut runtime = RuntimeCore::new();
        let state = TodoState::new(sample_program(), &mut runtime);
        let root = match state.view_tree().root {
            Some(root) => root.to_ui_node(),
            None => panic!("expected root"),
        };
        let mut text = String::new();
        flatten_text(&root, &mut text);
        assert!(text.contains("todos"));
        assert!(text.contains("2 items left"));
        assert!(text.contains("Double-click to edit a todo"));
    }

    #[test]
    fn randomized_todo_trace_matches_oracle_subset() {
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

        #[derive(Clone)]
        struct OracleTodo {
            title: String,
            completed: bool,
        }

        let mut runtime = RuntimeCore::new();
        let mut state = TodoState::new(sample_program(), &mut runtime);
        let mut rng = TraceRng::new(0xA11CE_u64);
        let mut oracle = vec![
            OracleTodo {
                title: "Buy groceries".to_string(),
                completed: false,
            },
            OracleTodo {
                title: "Clean room".to_string(),
                completed: false,
            },
        ];
        let mut filter = TodoFilter::All;

        for step in 0..64 {
            let before_ids = state
                .items
                .keys()
                .copied()
                .map(|item_id| (item_id, state.ui.item_views[&item_id].root))
                .collect::<BTreeMap<_, _>>();

            match rng.next_range(6) {
                0 => {
                    let title = format!("Task {step}");
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: state.ui.main_input.key_down_port,
                            kind: UiEventKind::KeyDown,
                            payload: Some(format!("Enter\u{1f}{title}")),
                        },
                    );
                    oracle.push(OracleTodo {
                        title,
                        completed: false,
                    });
                }
                1 if !oracle.is_empty() => {
                    let index = rng.next_range(oracle.len() as u32) as usize;
                    let item_id = *state.items.keys().nth(index).unwrap_or_else(|| {
                        state.items.keys().next().expect("todo id should exist")
                    });
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: state.ui.item_views[&item_id].checkbox.click_port,
                            kind: UiEventKind::Click,
                            payload: None,
                        },
                    );
                    oracle[index].completed = !oracle[index].completed;
                }
                2 => {
                    filter = match rng.next_range(3) {
                        0 => TodoFilter::All,
                        1 => TodoFilter::Active,
                        _ => TodoFilter::Completed,
                    };
                    let port = match filter {
                        TodoFilter::All => state.ui.filter_all.click_port,
                        TodoFilter::Active => state.ui.filter_active.click_port,
                        TodoFilter::Completed => state.ui.filter_completed.click_port,
                    };
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: port,
                            kind: UiEventKind::Click,
                            payload: None,
                        },
                    );
                }
                3 if !oracle.is_empty() => {
                    let index = rng.next_range(oracle.len() as u32) as usize;
                    let item_id = *state.items.keys().nth(index).unwrap_or_else(|| {
                        state.items.keys().next().expect("todo id should exist")
                    });
                    let new_title = format!("Edited {step}");
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: state.ui.item_views[&item_id].title_label.click_port,
                            kind: UiEventKind::DoubleClick,
                            payload: None,
                        },
                    );
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: state.ui.item_views[&item_id].edit_input.key_down_port,
                            kind: UiEventKind::KeyDown,
                            payload: Some(format!("Enter\u{1f}{new_title}")),
                        },
                    );
                    oracle[index].title = new_title;
                }
                4 if oracle.iter().any(|item| item.completed) => {
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: state.ui.clear_completed.click_port,
                            kind: UiEventKind::Click,
                            payload: None,
                        },
                    );
                    oracle.retain(|item| !item.completed);
                }
                _ if !oracle.is_empty() => {
                    let index = rng.next_range(oracle.len() as u32) as usize;
                    let item_id = *state.items.keys().nth(index).unwrap_or_else(|| {
                        state.items.keys().next().expect("todo id should exist")
                    });
                    let _ = state.handle_event(
                        &mut runtime,
                        &UiEvent {
                            target: state.ui.item_views[&item_id].remove_button.click_port,
                            kind: UiEventKind::Click,
                            payload: None,
                        },
                    );
                    oracle.remove(index);
                }
                _ => {}
            }

            let expected_titles = oracle
                .iter()
                .filter(|item| match filter {
                    TodoFilter::All => true,
                    TodoFilter::Active => !item.completed,
                    TodoFilter::Completed => item.completed,
                })
                .map(|item| item.title.clone())
                .collect::<Vec<_>>();
            let actual_titles = state
                .visible_item_ids()
                .into_iter()
                .filter_map(|item_id| state.items.get(&item_id).map(|item| item.title.clone()))
                .collect::<Vec<_>>();
            assert_eq!(actual_titles, expected_titles);

            for (item_id, root_id) in before_ids {
                if state.items.contains_key(&item_id) {
                    assert_eq!(state.ui.item_views[&item_id].root, root_id);
                }
            }
        }
    }
}
