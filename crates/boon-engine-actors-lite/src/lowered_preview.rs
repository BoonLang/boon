use crate::bridge::{HostInput, HostViewIr};
use crate::cells_preview::CellsPreview;
use crate::editable_list_actions::{EditableListActionPorts, apply_editable_list_actions};
use crate::editable_mapped_list_preview_runtime::{
    EditableMappedListPreviewRuntime, EditableMappedListProjection,
};
use crate::editable_mapped_list_runtime::EditableMappedListRuntime;
use crate::host_view_preview::{
    HostViewPreviewApp, InteractiveHostViewModel, render_interactive_host_view,
};
use crate::ids::ActorId;
use crate::input_form_runtime::{FormInputBinding, FormInputEvent};
use crate::ir::{IrProgram, MirrorCellId, SinkPortId, SourcePortId};
use crate::ir_executor::IrExecutor;
use crate::list_form_actions::update_selected_from_inputs;
use crate::lower::{
    ButtonHoverTestProgram, ButtonHoverToClickTestProgram, ChainedListRemoveBugProgram,
    CheckboxTestProgram, CircleDrawerProgram, ComplexCounterProgram, CounterProgram, CrudProgram,
    FibonacciProgram, FilterCheckboxBugProgram, FlightBookerProgram, IntervalProgram,
    LatestProgram, LayersProgram, ListMapBlockProgram, ListMapExternalDepProgram,
    ListObjectStateProgram, ListRetainCountProgram, ListRetainReactiveProgram,
    ListRetainRemoveProgram, LoweredProgram, PagesProgram, ShoppingListProgram, StaticProgram,
    SwitchHoldTestProgram, TemperatureConverterProgram, TextInterpolationUpdateProgram,
    ThenProgram, TimerProgram, WhenProgram, WhileFunctionCallProgram, WhileProgram,
};
use crate::mapped_click_runtime::MappedClickRuntime;
use crate::mapped_list_runtime::MappedListItem;
use crate::mapped_list_view_runtime::MappedListViewRuntime;
use crate::preview_runtime::PreviewRuntime;
use crate::slot_projection::{project_slot_values_into_app, project_slot_values_into_map};
use crate::text_filtered_editable_list_preview_runtime::{
    TextFilteredEditableMappedListProjection, dispatch_text_filtered_ui_events, text_filtered_items,
};
use crate::todo_preview::TodoPreview;
use crate::validated_form_runtime::ValidatedFormRuntime;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{NodeId, RenderRoot, UiEvent, UiEventBatch, UiEventKind, UiFactBatch, UiFactKind};
use std::collections::BTreeMap;

pub struct LoweredPreview {
    model: LoweredPreviewModel,
}

enum LoweredPreviewModel {
    Runtime(RuntimeHostViewPreview),
    FormRuntime(FormRuntimeHostViewPreview),
    Todo(TodoPreview),
    Cells(CellsPreview),
    Crud(CrudHostViewPreview),
    Pages(PagesHostViewPreview),
    LocalState(LocalStateHostViewPreview),
    Latest(LatestHostViewPreview),
    Static(StaticHostViewPreview),
}

struct RuntimeHostViewPreview {
    kind: RuntimePreviewKind,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
    /// Persistence program metadata for dirty collection and commit.
    program_ir: IrProgram,
    /// Whether persistence is enabled.
    persistence_enabled: bool,
}

struct FormRuntimeHostViewPreview {
    kind: FormRuntimePreviewKind,
}

struct CrudPerson {
    name: String,
    surname: String,
}

struct CrudProjection {
    program: CrudProgram,
}

struct CrudHostViewPreview {
    program: CrudProgram,
    runtime: EditableMappedListPreviewRuntime<CrudPerson, CrudProjection, 3, 4, 3>,
}

enum RuntimePreviewKind {
    ComplexCounter {
        decrement_port: SourcePortId,
        increment_port: SourcePortId,
        decrement_hovered_cell: MirrorCellId,
        increment_hovered_cell: MirrorCellId,
        counter_sink: SinkPortId,
        decrement_hovered_sink: SinkPortId,
        increment_hovered_sink: SinkPortId,
    },
    Counter {
        press_port: SourcePortId,
        counter_sink: SinkPortId,
    },
    Interval {
        tick_port: SourcePortId,
        value_sink: SinkPortId,
    },
    ListRetainReactive {
        toggle_port: SourcePortId,
        mode_sink: SinkPortId,
        count_sink: SinkPortId,
        items_list_sink: SinkPortId,
        item_sinks: [SinkPortId; 6],
    },
    ListMapExternalDep {
        toggle_port: SourcePortId,
        mode_sink: SinkPortId,
        info_sink: SinkPortId,
        items_list_sink: SinkPortId,
        item_sinks: [SinkPortId; 4],
    },
    AppendList {
        title_sink: Option<SinkPortId>,
        input_sink: SinkPortId,
        count_sinks: Vec<SinkPortId>,
        items_list_sink: SinkPortId,
        item_sinks: Vec<SinkPortId>,
        input_change_port: SourcePortId,
        input_key_down_port: SourcePortId,
        clear_press_port: Option<SourcePortId>,
        item_prefix: &'static str,
    },
    TimedMath {
        input_a_tick_port: SourcePortId,
        input_b_tick_port: SourcePortId,
        addition_press_port: Option<SourcePortId>,
        subtraction_press_port: Option<SourcePortId>,
        input_a_sink: SinkPortId,
        input_b_sink: SinkPortId,
        result_sink: SinkPortId,
    },
    CircleDrawer {
        canvas_click_port: SourcePortId,
        undo_press_port: SourcePortId,
        title_sink: SinkPortId,
        count_sink: SinkPortId,
        circles_sink: SinkPortId,
    },
}

enum FormRuntimePreviewKind {
    TemperatureConverter {
        runtime: PreviewRuntime,
        celsius_actor: ActorId,
        fahrenheit_actor: ActorId,
        program: TemperatureConverterProgram,
        executor: IrExecutor,
        form: ValidatedFormRuntime<2>,
    },
    FlightBooker {
        runtime: PreviewRuntime,
        flight_type_actor: ActorId,
        departure_actor: ActorId,
        return_actor: ActorId,
        book_actor: ActorId,
        program: FlightBookerProgram,
        executor: IrExecutor,
        form: ValidatedFormRuntime<3>,
    },
    Timer {
        runtime: PreviewRuntime,
        duration_actor: ActorId,
        reset_actor: ActorId,
        tick_actor: ActorId,
        program: TimerProgram,
        executor: IrExecutor,
        form: ValidatedFormRuntime<1>,
    },
}

struct LatestHostViewPreview {
    send_press_ports: [SourcePortId; 2],
    value_sink: SinkPortId,
    sum_sink: SinkPortId,
    current_value: i64,
    app: HostViewPreviewApp,
}

struct PagesHostViewPreview {
    program: PagesProgram,
    current_route: String,
    app: HostViewPreviewApp,
}

struct LocalStateHostViewPreview {
    kind: LocalStateProgramKind,
    app: HostViewPreviewApp,
}

struct ChainedListRemoveBugItem {
    name: String,
    completed: bool,
}

enum LocalStateProgramKind {
    TextInterpolationUpdate {
        program: TextInterpolationUpdateProgram,
        value: bool,
    },
    WhileFunctionCall {
        program: WhileFunctionCallProgram,
        value: bool,
    },
    ButtonHoverToClickTest {
        program: ButtonHoverToClickTestProgram,
        clicked: [bool; 3],
    },
    ButtonHoverTest {
        program: ButtonHoverTestProgram,
        hovered: [bool; 3],
    },
    FilterCheckboxBug {
        program: FilterCheckboxBugProgram,
        filter: FilterCheckboxBugMode,
        checked: [bool; 2],
    },
    CheckboxTest {
        program: CheckboxTestProgram,
        checked: [bool; 2],
    },
    ListObjectState {
        program: ListObjectStateProgram,
        counts: [u32; 3],
    },
    ChainedListRemoveBug {
        program: ChainedListRemoveBugProgram,
        clicks: MappedClickRuntime,
        items: MappedListViewRuntime<ChainedListRemoveBugItem>,
    },
    SwitchHoldTest {
        program: SwitchHoldTestProgram,
        show_item_a: bool,
        click_counts: [u32; 2],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterCheckboxBugMode {
    All,
    Active,
}

struct StaticHostViewPreview {
    app: HostViewPreviewApp,
}

const HOME_ROUTE: &str = "/";
const ABOUT_ROUTE: &str = "/about";
const CONTACT_ROUTE: &str = "/contact";

#[derive(Clone)]
struct UiEventPulseBinding {
    source_port: SourcePortId,
    expected_kind: ExpectedUiEventKind,
    value: KernelValue,
}

#[derive(Clone)]
enum ExpectedUiEventKind {
    Exact(UiEventKind),
    AnyCustom,
}

impl EditableMappedListProjection<CrudPerson, 3, 4> for CrudProjection {
    fn host_view(&self) -> &HostViewIr {
        &self.program.host_view
    }

    fn initial_sink_values(
        &self,
        people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
    ) -> BTreeMap<SinkPortId, KernelValue> {
        initial_crud_sink_values(&self.program, people)
    }

    fn refresh_sink_values(
        &self,
        app: &mut HostViewPreviewApp,
        people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
    ) {
        refresh_crud_sink_values(app, &self.program, people);
    }
}

impl TextFilteredEditableMappedListProjection<CrudPerson, 3, 4> for CrudProjection {
    const FILTER_INPUT_INDEX: usize = 0;

    fn item_matches_filter(filter_text: &str, item: &MappedListItem<CrudPerson>) -> bool {
        filter_text.is_empty() || item.value.surname.starts_with(filter_text)
    }
}

impl LoweredPreview {
    pub fn from_program(program: LoweredProgram) -> Result<Self, String> {
        let model = match program {
            LoweredProgram::Counter(program) => {
                LoweredPreviewModel::Runtime(RuntimeHostViewPreview::from_counter_program(program)?)
            }
            LoweredProgram::ComplexCounter(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_complex_counter_program(program)?,
            ),
            LoweredProgram::TemperatureConverter(program) => LoweredPreviewModel::FormRuntime(
                FormRuntimeHostViewPreview::from_temperature_converter_program(program)?,
            ),
            LoweredProgram::FlightBooker(program) => LoweredPreviewModel::FormRuntime(
                FormRuntimeHostViewPreview::from_flight_booker_program(program)?,
            ),
            LoweredProgram::Timer(program) => LoweredPreviewModel::FormRuntime(
                FormRuntimeHostViewPreview::from_timer_program(program)?,
            ),
            LoweredProgram::ListMapExternalDep(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_list_map_external_dep_program(program)?,
            ),
            LoweredProgram::ListMapBlock(program) => LoweredPreviewModel::Static(
                StaticHostViewPreview::from_list_map_block_program(program),
            ),
            LoweredProgram::ListRetainCount(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_list_retain_count_program(program)?,
            ),
            LoweredProgram::ListRetainRemove(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_list_retain_remove_program(program)?,
            ),
            LoweredProgram::ShoppingList(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_shopping_list_program(program)?,
            ),
            LoweredProgram::ListObjectState(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_list_object_state_program(program),
            ),
            LoweredProgram::ChainedListRemoveBug(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_chained_list_remove_bug_program(program),
            ),
            LoweredProgram::TodoMvc(program) => {
                LoweredPreviewModel::Todo(TodoPreview::from_program(program)?)
            }
            LoweredProgram::TodoMvcWithInitialTodos { program, initial_todos } => {
                let todos: Vec<(u64, crate::todo_preview::TodoItem)> = initial_todos
                    .into_iter()
                    .map(|(id, title, completed)| (id, crate::todo_preview::TodoItem { title, completed }))
                    .collect();
                LoweredPreviewModel::Todo(TodoPreview::from_program_with_initial_todos(program, todos)?)
            }
            LoweredProgram::Crud(program) => {
                LoweredPreviewModel::Crud(CrudHostViewPreview::from_program(program))
            }
            LoweredProgram::Interval(program) | LoweredProgram::IntervalHold(program) => {
                LoweredPreviewModel::Runtime(RuntimeHostViewPreview::from_interval_program(
                    program,
                )?)
            }
            LoweredProgram::Fibonacci(program) => {
                LoweredPreviewModel::Static(StaticHostViewPreview::from_fibonacci_program(program))
            }
            LoweredProgram::Layers(program) => {
                LoweredPreviewModel::Static(StaticHostViewPreview::from_layers_program(program))
            }
            LoweredProgram::Pages(program) => {
                LoweredPreviewModel::Pages(PagesHostViewPreview::from_program(program))
            }
            LoweredProgram::Latest(program) => {
                LoweredPreviewModel::Latest(LatestHostViewPreview::from_program(program))
            }
            LoweredProgram::TextInterpolationUpdate(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_text_interpolation_update_program(program),
            ),
            LoweredProgram::ButtonHoverToClickTest(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_button_hover_to_click_test_program(program),
            ),
            LoweredProgram::ButtonHoverTest(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_button_hover_test_program(program),
            ),
            LoweredProgram::FilterCheckboxBug(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_filter_checkbox_bug_program(program),
            ),
            LoweredProgram::CheckboxTest(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_checkbox_test_program(program),
            ),
            LoweredProgram::ListRetainReactive(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_list_retain_reactive_program(program)?,
            ),
            LoweredProgram::Then(program) => {
                LoweredPreviewModel::Runtime(RuntimeHostViewPreview::from_then_program(program)?)
            }
            LoweredProgram::When(program) => {
                LoweredPreviewModel::Runtime(RuntimeHostViewPreview::from_when_program(program)?)
            }
            LoweredProgram::While(program) => {
                LoweredPreviewModel::Runtime(RuntimeHostViewPreview::from_while_program(program)?)
            }
            LoweredProgram::WhileFunctionCall(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_while_function_call_program(program),
            ),
            LoweredProgram::SwitchHoldTest(program) => LoweredPreviewModel::LocalState(
                LocalStateHostViewPreview::from_switch_hold_test_program(program),
            ),
            LoweredProgram::CircleDrawer(program) => LoweredPreviewModel::Runtime(
                RuntimeHostViewPreview::from_circle_drawer_program(program)?,
            ),
            LoweredProgram::Cells(program) => {
                LoweredPreviewModel::Cells(CellsPreview::from_program(program))
            }
            LoweredProgram::StaticDocument(program) => {
                LoweredPreviewModel::Static(StaticHostViewPreview::from_program(program))
            }
        };
        Ok(Self { model })
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn app(&self) -> &HostViewPreviewApp {
        self.model.app()
    }

    /// Enable persistence on this preview.
    ///
    /// When persistence is enabled, the preview will collect dirty HOLD cells
    /// after each dispatch and commit them to the browser's localStorage.
    pub fn with_persistence(mut self) -> Self {
        self.model.enable_persistence();
        self
    }

    /// Inject a restored counter value into the counter sink.
    /// Used to restore persisted counter state after page refresh.
    pub fn inject_restored_counter_value(&mut self, value: i64) {
        self.model.set_counter_sink_value(value);
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.model.preview_text()
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        self.model.dispatch_ui_events(batch)
    }

    #[must_use]
    pub fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        <Self as InteractiveHostViewModel>::render_snapshot(self)
    }
}

impl InteractiveHostViewModel for LoweredPreview {
    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        self.model.app_mut()
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        self.model.dispatch_ui_events(batch)
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        self.model.dispatch_ui_facts(batch)
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        self.model.render_snapshot()
    }
}

pub fn render_lowered_preview(preview: LoweredPreview) -> impl Element {
    render_interactive_host_view(preview)
}

impl LoweredPreviewModel {
    #[cfg(test)]
    fn app(&self) -> &HostViewPreviewApp {
        match self {
            Self::Runtime(preview) => &preview.app,
            Self::FormRuntime(preview) => preview.app(),
            Self::Todo(preview) => preview.app(),
            Self::Cells(preview) => preview.app(),
            Self::Crud(preview) => preview.app(),
            Self::Pages(preview) => &preview.app,
            Self::LocalState(preview) => &preview.app,
            Self::Latest(preview) => &preview.app,
            Self::Static(preview) => &preview.app,
        }
    }

    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        match self {
            Self::Runtime(preview) => &mut preview.app,
            Self::FormRuntime(preview) => preview.app_mut(),
            Self::Todo(preview) => InteractiveHostViewModel::app_mut(preview),
            Self::Cells(preview) => InteractiveHostViewModel::app_mut(preview),
            Self::Crud(preview) => preview.app_mut(),
            Self::Pages(preview) => &mut preview.app,
            Self::LocalState(preview) => &mut preview.app,
            Self::Latest(preview) => &mut preview.app,
            Self::Static(preview) => &mut preview.app,
        }
    }

    /// Enable persistence on the underlying preview.
    fn enable_persistence(&mut self) {
        match self {
            Self::Runtime(preview) => preview.enable_persistence(),
            Self::FormRuntime(_) => {}
            Self::Todo(preview) => preview.enable_persistence(),
            Self::Cells(preview) => preview.enable_persistence(),
            Self::Crud(_) => {}
            Self::Pages(_) => {}
            Self::LocalState(_) => {}
            Self::Latest(_) => {}
            Self::Static(_) => {}
        }
    }

    /// Set the counter sink value for persistence restoration.
    fn set_counter_sink_value(&mut self, value: i64) {
        match self {
            Self::Runtime(preview) => preview.set_counter_sink_value(value),
            _ => {}
        }
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        match self {
            Self::Runtime(preview) => preview.dispatch_ui_events(batch),
            Self::FormRuntime(preview) => preview.dispatch_ui_events(batch),
            Self::Todo(preview) => preview.dispatch_ui_events(batch),
            Self::Cells(preview) => preview.dispatch_ui_events(batch),
            Self::Crud(preview) => preview.dispatch_ui_events(batch),
            Self::Pages(preview) => preview.dispatch_ui_events(batch),
            Self::LocalState(preview) => preview.dispatch_ui_events(batch),
            Self::Latest(preview) => preview.dispatch_ui_events(batch),
            Self::Static(_) => false,
        }
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        match self {
            Self::Runtime(preview) => preview.dispatch_ui_facts(batch),
            Self::FormRuntime(_) => false,
            Self::Todo(preview) => preview.dispatch_ui_facts(batch),
            Self::Cells(preview) => preview.dispatch_ui_facts(batch),
            Self::Crud(_) => false,
            Self::Pages(_) => false,
            Self::LocalState(preview) => preview.dispatch_ui_facts(batch),
            Self::Latest(_) => false,
            Self::Static(_) => false,
        }
    }

    fn preview_text(&mut self) -> String {
        match self {
            Self::Runtime(preview) => preview.app.preview_text(),
            Self::FormRuntime(preview) => preview.preview_text(),
            Self::Todo(preview) => preview.preview_text(),
            Self::Cells(preview) => preview.preview_text(),
            Self::Crud(preview) => preview.preview_text(),
            Self::Pages(preview) => preview.app.preview_text(),
            Self::LocalState(preview) => preview.app.preview_text(),
            Self::Latest(preview) => preview.app.preview_text(),
            Self::Static(preview) => preview.app.preview_text(),
        }
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        match self {
            Self::Runtime(preview) => {
                let (root, state) = preview.app.render_snapshot();
                (RenderRoot::UiTree(root), state)
            }
            Self::FormRuntime(preview) => preview.render_snapshot(),
            Self::Todo(preview) => InteractiveHostViewModel::render_snapshot(preview),
            Self::Cells(preview) => InteractiveHostViewModel::render_snapshot(preview),
            Self::Crud(preview) => {
                let (root, state) = preview.render_snapshot();
                (RenderRoot::UiTree(root), state)
            }
            Self::Pages(preview) => {
                let (root, state) = preview.app.render_snapshot();
                (RenderRoot::UiTree(root), state)
            }
            Self::LocalState(preview) => {
                let (root, state) = preview.app.render_snapshot();
                (RenderRoot::UiTree(root), state)
            }
            Self::Latest(preview) => {
                let (root, state) = preview.app.render_snapshot();
                (RenderRoot::UiTree(root), state)
            }
            Self::Static(preview) => {
                let (root, state) = preview.app.render_snapshot();
                (RenderRoot::UiTree(root), state)
            }
        }
    }
}

impl CrudHostViewPreview {
    fn from_program(program: CrudProgram) -> Self {
        let people = EditableMappedListRuntime::new(
            [
                (
                    0,
                    CrudPerson {
                        name: "Hans".to_string(),
                        surname: "Emil".to_string(),
                    },
                ),
                (
                    1,
                    CrudPerson {
                        name: "Max".to_string(),
                        surname: "Mustermann".to_string(),
                    },
                ),
                (
                    2,
                    CrudPerson {
                        name: "Roman".to_string(),
                        surname: "Tansen".to_string(),
                    },
                ),
            ],
            3,
            [
                program.filter_change_port,
                program.name_change_port,
                program.surname_change_port,
            ],
            program.row_press_ports,
        );
        let runtime = EditableMappedListPreviewRuntime::new(
            CrudProjection {
                program: program.clone(),
            },
            people,
            [
                program.create_press_port,
                program.update_press_port,
                program.delete_press_port,
            ],
        );

        Self { program, runtime }
    }

    #[cfg(test)]
    fn app(&self) -> &HostViewPreviewApp {
        self.runtime.app()
    }

    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        self.runtime.app_mut()
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let program = self.program.clone();
        dispatch_text_filtered_ui_events(&mut self.runtime, batch, move |people, clicked| {
            apply_crud_button_clicks(&program, people, clicked)
        })
    }

    fn preview_text(&mut self) -> String {
        self.runtime.preview_text()
    }

    fn render_snapshot(&mut self) -> (boon_scene::UiNode, FakeRenderState) {
        self.runtime.render_snapshot()
    }
}

fn apply_crud_button_clicks(
    program: &CrudProgram,
    people: &mut EditableMappedListRuntime<CrudPerson, 3, 4>,
    clicked: Vec<SourcePortId>,
) -> bool {
    apply_editable_list_actions(
        EditableListActionPorts {
            create_port: Some(program.create_press_port),
            update_port: Some(program.update_press_port),
            delete_port: Some(program.delete_press_port),
        },
        people,
        clicked,
        |people| {
            Some(CrudPerson {
                name: people.input(1).to_string(),
                surname: people.input(2).to_string(),
            })
        },
        &[1, 2],
        |people| {
            update_selected_from_inputs(
                people,
                |people| Some((people.input(1).to_string(), people.input(2).to_string())),
                |person, (name, surname)| {
                    let changed = person.name != name || person.surname != surname;
                    person.name = name;
                    person.surname = surname;
                    changed
                },
            )
        },
    )
}

fn initial_crud_sink_values(
    program: &CrudProgram,
    people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(program.title_sink, KernelValue::from("CRUD"));
    refresh_crud_sink_values_into(&mut sink_values, program, people);
    sink_values
}

fn refresh_crud_sink_values(
    app: &mut HostViewPreviewApp,
    program: &CrudProgram,
    people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
) {
    let visible_people = text_filtered_items::<CrudPerson, CrudProjection, 3, 4>(people);
    app.set_sink_value(program.title_sink, KernelValue::from("CRUD"));
    app.set_sink_value(
        program.filter_input_sink,
        KernelValue::from(people.input(0).to_string()),
    );
    app.set_sink_value(
        program.name_input_sink,
        KernelValue::from(people.input(1).to_string()),
    );
    app.set_sink_value(
        program.surname_input_sink,
        KernelValue::from(people.input(2).to_string()),
    );
    visible_people.project_into_app(
        app,
        &program.row_label_sinks,
        |item| {
            let prefix = if Some(item.id) == people.selected_id() {
                "\u{25BA} "
            } else {
                ""
            };
            KernelValue::from(format!(
                "{prefix}{}, {}",
                item.value.surname, item.value.name
            ))
        },
        KernelValue::from(""),
    );
    visible_people.project_into_app(
        app,
        &program.row_selected_sinks,
        |item| KernelValue::Bool(Some(item.id) == people.selected_id()),
        KernelValue::Bool(false),
    );
}

fn refresh_crud_sink_values_into(
    sink_values: &mut BTreeMap<SinkPortId, KernelValue>,
    program: &CrudProgram,
    people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
) {
    let visible_people = text_filtered_items::<CrudPerson, CrudProjection, 3, 4>(people);
    sink_values.insert(
        program.filter_input_sink,
        KernelValue::from(people.input(0).to_string()),
    );
    sink_values.insert(
        program.name_input_sink,
        KernelValue::from(people.input(1).to_string()),
    );
    sink_values.insert(
        program.surname_input_sink,
        KernelValue::from(people.input(2).to_string()),
    );
    visible_people.project_into_map(
        sink_values,
        &program.row_label_sinks,
        |item| {
            let prefix = if Some(item.id) == people.selected_id() {
                "\u{25BA} "
            } else {
                ""
            };
            KernelValue::from(format!(
                "{prefix}{}, {}",
                item.value.surname, item.value.name
            ))
        },
        KernelValue::from(""),
    );
    visible_people.project_into_map(
        sink_values,
        &program.row_selected_sinks,
        |item| KernelValue::Bool(Some(item.id) == people.selected_id()),
        KernelValue::Bool(false),
    );
}

impl RuntimeHostViewPreview {
    fn from_complex_counter_program(program: ComplexCounterProgram) -> Result<Self, String> {
        let ComplexCounterProgram {
            ir,
            host_view,
            decrement_port,
            increment_port,
            decrement_hovered_cell,
            increment_hovered_cell,
            counter_sink,
            decrement_hovered_sink,
            increment_hovered_sink,
            ..
        } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(host_view, executor.sink_values());
        Ok(Self {
            kind: RuntimePreviewKind::ComplexCounter {
                decrement_port,
                increment_port,
                decrement_hovered_cell,
                increment_hovered_cell,
                counter_sink,
                decrement_hovered_sink,
                increment_hovered_sink,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_counter_program(program: CounterProgram) -> Result<Self, String> {
        let CounterProgram {
            ir,
            host_view,
            press_port,
            counter_sink,
            ..
        } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(host_view, executor.sink_values());
        #[cfg(target_arch = "wasm32")]
        {
            let msg = format!("PERSIST: from_counter_program: {} persistence entries, {} nodes", program_ir.persistence.len(), program_ir.nodes.len());
            crate::browser_debug::set_debug_marker(&msg);
            for entry in &program_ir.persistence {
                let entry_msg = format!("PERSIST: persistence entry: node={:?}, policy={:?}", entry.node, entry.policy);
                crate::browser_debug::set_debug_marker(&entry_msg);
            }
        }
        Ok(Self {
            kind: RuntimePreviewKind::Counter {
                press_port,
                counter_sink,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_list_retain_reactive_program(
        program: ListRetainReactiveProgram,
    ) -> Result<Self, String> {
        let ListRetainReactiveProgram {
            ir,
            host_view,
            toggle_port,
            mode_sink,
            count_sink,
            items_list_sink,
            item_sinks,
        } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(
            host_view,
            initial_list_retain_reactive_sinks(
                mode_sink,
                count_sink,
                items_list_sink,
                &item_sinks,
                &executor,
            ),
        );
        Ok(Self {
            kind: RuntimePreviewKind::ListRetainReactive {
                toggle_port,
                mode_sink,
                count_sink,
                items_list_sink,
                item_sinks,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_list_map_external_dep_program(
        program: ListMapExternalDepProgram,
    ) -> Result<Self, String> {
        let ListMapExternalDepProgram {
            ir,
            host_view,
            toggle_port,
            mode_sink,
            info_sink,
            items_list_sink,
            item_sinks,
        } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(
            host_view,
            initial_list_map_external_dep_sinks(
                mode_sink,
                info_sink,
                items_list_sink,
                &item_sinks,
                &executor,
            ),
        );
        Ok(Self {
            kind: RuntimePreviewKind::ListMapExternalDep {
                toggle_port,
                mode_sink,
                info_sink,
                items_list_sink,
                item_sinks,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_list_retain_count_program(program: ListRetainCountProgram) -> Result<Self, String> {
        let ListRetainCountProgram {
            ir,
            host_view,
            input_sink,
            all_count_sink,
            retain_count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            item_sinks,
        } = program;
        Self::from_append_list_parts(
            ir,
            host_view,
            None,
            input_sink,
            vec![all_count_sink, retain_count_sink],
            items_list_sink,
            item_sinks.into_iter().collect(),
            input_change_port,
            input_key_down_port,
            None,
            "",
        )
    }

    fn from_list_retain_remove_program(program: ListRetainRemoveProgram) -> Result<Self, String> {
        let ListRetainRemoveProgram {
            ir,
            host_view,
            title_sink,
            input_sink,
            count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            item_sinks,
        } = program;
        Self::from_append_list_parts(
            ir,
            host_view,
            Some(title_sink),
            input_sink,
            vec![count_sink],
            items_list_sink,
            item_sinks.into_iter().collect(),
            input_change_port,
            input_key_down_port,
            None,
            "- ",
        )
    }

    fn from_shopping_list_program(program: ShoppingListProgram) -> Result<Self, String> {
        let ShoppingListProgram {
            ir,
            host_view,
            title_sink,
            input_sink,
            count_sink,
            items_list_sink,
            input_change_port,
            input_key_down_port,
            clear_press_port,
            item_sinks,
        } = program;
        Self::from_append_list_parts(
            ir,
            host_view,
            Some(title_sink),
            input_sink,
            vec![count_sink],
            items_list_sink,
            item_sinks.into_iter().collect(),
            input_change_port,
            input_key_down_port,
            Some(clear_press_port),
            "- ",
        )
    }

    fn from_append_list_parts(
        ir: crate::ir::IrProgram,
        host_view: HostViewIr,
        title_sink: Option<SinkPortId>,
        input_sink: SinkPortId,
        count_sinks: Vec<SinkPortId>,
        items_list_sink: SinkPortId,
        item_sinks: Vec<SinkPortId>,
        input_change_port: SourcePortId,
        input_key_down_port: SourcePortId,
        clear_press_port: Option<SourcePortId>,
        item_prefix: &'static str,
    ) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(
            host_view,
            initial_append_list_sinks(
                title_sink,
                input_sink,
                &count_sinks,
                items_list_sink,
                &item_sinks,
                item_prefix,
                &executor,
            ),
        );
        Ok(Self {
            kind: RuntimePreviewKind::AppendList {
                title_sink,
                input_sink,
                count_sinks,
                items_list_sink,
                item_sinks,
                input_change_port,
                input_key_down_port,
                clear_press_port,
                item_prefix,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_interval_program(program: IntervalProgram) -> Result<Self, String> {
        let IntervalProgram {
            ir,
            host_view,
            value_sink,
            tick_port,
            ..
        } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(host_view, executor.sink_values());
        Ok(Self {
            kind: RuntimePreviewKind::Interval {
                tick_port,
                value_sink,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_then_program(program: ThenProgram) -> Result<Self, String> {
        let ThenProgram {
            ir,
            host_view,
            input_a_tick_port,
            input_b_tick_port,
            addition_press_port,
            input_a_sink,
            input_b_sink,
            result_sink,
        } = program;
        Self::from_timed_math_parts(
            ir,
            host_view,
            input_a_tick_port,
            input_b_tick_port,
            Some(addition_press_port),
            None,
            input_a_sink,
            input_b_sink,
            result_sink,
        )
    }

    fn from_when_program(program: WhenProgram) -> Result<Self, String> {
        let WhenProgram {
            ir,
            host_view,
            input_a_tick_port,
            input_b_tick_port,
            addition_press_port,
            subtraction_press_port,
            input_a_sink,
            input_b_sink,
            result_sink,
        } = program;
        Self::from_timed_math_parts(
            ir,
            host_view,
            input_a_tick_port,
            input_b_tick_port,
            Some(addition_press_port),
            Some(subtraction_press_port),
            input_a_sink,
            input_b_sink,
            result_sink,
        )
    }

    fn from_while_program(program: WhileProgram) -> Result<Self, String> {
        let WhileProgram {
            ir,
            host_view,
            input_a_tick_port,
            input_b_tick_port,
            addition_press_port,
            subtraction_press_port,
            input_a_sink,
            input_b_sink,
            result_sink,
        } = program;
        Self::from_timed_math_parts(
            ir,
            host_view,
            input_a_tick_port,
            input_b_tick_port,
            Some(addition_press_port),
            Some(subtraction_press_port),
            input_a_sink,
            input_b_sink,
            result_sink,
        )
    }

    fn from_timed_math_parts(
        ir: crate::ir::IrProgram,
        host_view: crate::bridge::HostViewIr,
        input_a_tick_port: SourcePortId,
        input_b_tick_port: SourcePortId,
        addition_press_port: Option<SourcePortId>,
        subtraction_press_port: Option<SourcePortId>,
        input_a_sink: SinkPortId,
        input_b_sink: SinkPortId,
        result_sink: SinkPortId,
    ) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(host_view, executor.sink_values());
        Ok(Self {
            kind: RuntimePreviewKind::TimedMath {
                input_a_tick_port,
                input_b_tick_port,
                addition_press_port,
                subtraction_press_port,
                input_a_sink,
                input_b_sink,
                result_sink,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn from_circle_drawer_program(program: CircleDrawerProgram) -> Result<Self, String> {
        let CircleDrawerProgram {
            ir,
            host_view,
            title_sink,
            count_sink,
            circles_sink,
            canvas_click_port,
            undo_press_port,
        } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let program_ir = ir.clone();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(host_view, executor.sink_values());
        Ok(Self {
            kind: RuntimePreviewKind::CircleDrawer {
                canvas_click_port,
                undo_press_port,
                title_sink,
                count_sink,
                circles_sink,
            },
            runtime,
            host_actor,
            executor,
            app,
            program_ir,
            persistence_enabled: false,
        })
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let inputs = match &self.kind {
            RuntimePreviewKind::ComplexCounter {
                decrement_port,
                increment_port,
                ..
            } => pulse_inputs_from_events(
                &self.app,
                self.host_actor,
                &self.runtime,
                &[
                    UiEventPulseBinding {
                        source_port: *decrement_port,
                        expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                        value: KernelValue::from("press"),
                    },
                    UiEventPulseBinding {
                        source_port: *increment_port,
                        expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                        value: KernelValue::from("press"),
                    },
                ],
                batch.events,
            ),
            RuntimePreviewKind::Counter { press_port, .. } => pulse_inputs_from_events(
                &self.app,
                self.host_actor,
                &self.runtime,
                &[UiEventPulseBinding {
                    source_port: *press_port,
                    expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                    value: KernelValue::from("press"),
                }],
                batch.events,
            ),
            RuntimePreviewKind::Interval { tick_port, .. } => pulse_inputs_from_events(
                &self.app,
                self.host_actor,
                &self.runtime,
                &[UiEventPulseBinding {
                    source_port: *tick_port,
                    expected_kind: ExpectedUiEventKind::AnyCustom,
                    value: KernelValue::from("tick"),
                }],
                batch.events,
            ),
            RuntimePreviewKind::ListRetainReactive { toggle_port, .. } => pulse_inputs_from_events(
                &self.app,
                self.host_actor,
                &self.runtime,
                &[UiEventPulseBinding {
                    source_port: *toggle_port,
                    expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                    value: KernelValue::from("press"),
                }],
                batch.events,
            ),
            RuntimePreviewKind::ListMapExternalDep { toggle_port, .. } => pulse_inputs_from_events(
                &self.app,
                self.host_actor,
                &self.runtime,
                &[UiEventPulseBinding {
                    source_port: *toggle_port,
                    expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                    value: KernelValue::from("press"),
                }],
                batch.events,
            ),
            RuntimePreviewKind::AppendList {
                input_change_port,
                input_key_down_port,
                clear_press_port,
                ..
            } => append_list_inputs_from_events(
                &self.app,
                self.host_actor,
                &self.runtime,
                *input_change_port,
                *input_key_down_port,
                *clear_press_port,
                batch.events,
            ),
            RuntimePreviewKind::TimedMath {
                input_a_tick_port,
                input_b_tick_port,
                addition_press_port,
                subtraction_press_port,
                ..
            } => {
                let mut bindings = vec![
                    UiEventPulseBinding {
                        source_port: *input_a_tick_port,
                        expected_kind: ExpectedUiEventKind::AnyCustom,
                        value: KernelValue::from("tick"),
                    },
                    UiEventPulseBinding {
                        source_port: *input_b_tick_port,
                        expected_kind: ExpectedUiEventKind::AnyCustom,
                        value: KernelValue::from("tick"),
                    },
                ];
                if let Some(port) = addition_press_port {
                    bindings.push(UiEventPulseBinding {
                        source_port: *port,
                        expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                        value: KernelValue::from("press"),
                    });
                }
                if let Some(port) = subtraction_press_port {
                    bindings.push(UiEventPulseBinding {
                        source_port: *port,
                        expected_kind: ExpectedUiEventKind::Exact(UiEventKind::Click),
                        value: KernelValue::from("press"),
                    });
                }
                pulse_inputs_from_events(
                    &self.app,
                    self.host_actor,
                    &self.runtime,
                    &bindings,
                    batch.events,
                )
            }
            RuntimePreviewKind::CircleDrawer {
                canvas_click_port,
                undo_press_port,
                ..
            } => {
                let _ = self.app.render_root();
                let canvas_port = self.app.event_port_for_source(*canvas_click_port);
                let undo_port = self.app.event_port_for_source(*undo_press_port);
                let mut inputs = Vec::new();

                for (seq, event) in batch.events.into_iter().enumerate() {
                    if Some(event.target) == canvas_port && matches!(event.kind, UiEventKind::Click)
                    {
                        inputs.push(HostInput::Pulse {
                            actor: self.host_actor,
                            port: *canvas_click_port,
                            value: parse_canvas_click_payload(event.payload.as_deref()),
                            seq: self.runtime.causal_seq(seq as u32),
                        });
                        continue;
                    }
                    if Some(event.target) == undo_port && matches!(event.kind, UiEventKind::Click) {
                        inputs.push(HostInput::Pulse {
                            actor: self.host_actor,
                            port: *undo_press_port,
                            value: KernelValue::from("press"),
                            seq: self.runtime.causal_seq(seq as u32),
                        });
                    }
                }

                inputs
            }
        };
        self.apply_messages(inputs)
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        let (decrement_hovered_cell, increment_hovered_cell) = match &self.kind {
            RuntimePreviewKind::ComplexCounter {
                decrement_hovered_cell,
                increment_hovered_cell,
                ..
            } => (*decrement_hovered_cell, *increment_hovered_cell),
            _ => return false,
        };
        let Some((decrement_id, increment_id)) = self.complex_counter_button_ids() else {
            return false;
        };

        let mut inputs = Vec::new();
        for fact in batch.facts {
            let UiFactKind::Hovered(hovered) = fact.kind else {
                continue;
            };
            let cell = if fact.id == decrement_id {
                decrement_hovered_cell
            } else if fact.id == increment_id {
                increment_hovered_cell
            } else {
                continue;
            };
            inputs.push(HostInput::Mirror {
                actor: self.host_actor,
                cell,
                value: KernelValue::Bool(hovered),
                seq: self.runtime.causal_seq(inputs.len() as u32),
            });
        }

        self.apply_messages(inputs)
    }

    fn apply_messages(&mut self, inputs: Vec<HostInput>) -> bool {
        if inputs.is_empty() {
            return false;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let msg = format!("PERSIST: apply_messages called, persistence_enabled={}", self.persistence_enabled);
            crate::browser_debug::set_debug_marker(&msg);
        }
        let Self {
            runtime, executor, ..
        } = self;
        runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
            executor
                .apply_pure_messages_owned(messages.drain(..))
                .expect("lowered IR should execute");
        });
        self.refresh_sink_values();

        // Collect dirty persistence and commit if enabled
        if self.persistence_enabled {
            self.collect_and_commit_persistence();
        }

        true
    }

    /// Collect dirty persistence entries and commit to the adapter.
    #[cfg(target_arch = "wasm32")]
    fn collect_and_commit_persistence(&mut self) {
        use crate::ir::IrNodeKind;
        use crate::persist::{PersistedRecord, PersistenceAdapter};
        use crate::persist_browser::BrowserLocalStorage;
        use boon::parser::PersistenceId;

        crate::browser_debug::set_debug_marker("collect_persist:start");

        let adapter = BrowserLocalStorage::instance();
        let all_sinks = self.executor.sink_values();
        crate::browser_debug::set_debug_marker(&format!("collect_persist:sinks:{}", all_sinks.len()));

        // Build mappings from node ID to SinkPortId
        let mut node_to_sink: std::collections::BTreeMap<
            crate::ir::NodeId,
            crate::ir::SinkPortId,
        > = std::collections::BTreeMap::new();
        for node in &self.program_ir.nodes {
            if let IrNodeKind::SinkPort { port, input } = node.kind {
                node_to_sink.insert(node.id, port);
                node_to_sink.insert(input, port);
            }
        }
        crate::browser_debug::set_debug_marker(&format!("collect_persist:node_map:{}", node_to_sink.len()));
        crate::browser_debug::set_debug_marker(&format!("collect_persist:persist_entries:{}", self.program_ir.persistence.len()));

        // Collect dirty entries
        let mut writes = Vec::new();
        for entry in &self.program_ir.persistence {
            if let crate::ir::PersistPolicy::Durable {
                root_key,
                local_slot,
                persist_kind,
            } = entry.policy
            {
                crate::browser_debug::set_debug_marker(&format!("collect_persist:entry:{root_key:?}:{local_slot}:{persist_kind:?}"));
                if matches!(persist_kind, crate::ir::PersistKind::Hold | crate::ir::PersistKind::ListStore) {
                    if let Some(sink_id) = node_to_sink.get(&entry.node) {
                        crate::browser_debug::set_debug_marker(&format!("collect_persist:sink_found:{sink_id:?}"));
                        if let Some(value) = all_sinks.get(sink_id) {
                            crate::browser_debug::set_debug_marker(&format!("collect_persist:value:{value:?}"));
                            let value_json = crate::persistence::kernel_value_to_json(value);
                            writes.push(PersistedRecord::Hold {
                                root_key: root_key.to_string(),
                                local_slot: local_slot,
                                value: value_json,
                            });
                        } else {
                            crate::browser_debug::set_debug_marker(&format!("collect_persist:no_value_for_sink:{sink_id:?}"));
                        }
                    } else {
                        crate::browser_debug::set_debug_marker(&format!("collect_persist:no_sink_for_node:{:?}", entry.node));
                    }
                }
            }
        }

        crate::browser_debug::set_debug_marker(&format!("collect_persist:writes:{}", writes.len()));

        // Commit if there are writes
        if !writes.is_empty() {
            match adapter.apply_batch(&writes, &[]) {
                Ok(()) => {
                    crate::browser_debug::set_debug_marker("collect_persist:commit_ok");
                }
                Err(e) => {
                    crate::browser_debug::set_debug_marker(&format!("collect_persist:commit_err:{e}"));
                }
            }
        } else {
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn collect_and_commit_persistence(&mut self) {
        // No-op on non-wasm targets
    }

    /// Enable persistence on this preview.
    pub fn enable_persistence(&mut self) {
        self.persistence_enabled = true;
        crate::browser_debug::set_debug_marker("persistence:enabled");
    }

    /// Set the counter sink value for persistence restoration.
    pub fn set_counter_sink_value(&mut self, value: i64) {
        if let RuntimePreviewKind::Counter { counter_sink, .. } = &self.kind {
            self.app.set_sink_value(*counter_sink, KernelValue::Number(value as f64));
            crate::browser_debug::set_debug_marker(&format!("persistence:counter_restored:{value}"));
        }
    }

    fn refresh_sink_values(&mut self) {
        match &self.kind {
            RuntimePreviewKind::ComplexCounter {
                counter_sink,
                decrement_hovered_sink,
                increment_hovered_sink,
                ..
            } => {
                sync_sink_values(
                    &mut self.app,
                    &self.executor,
                    &[
                        *counter_sink,
                        *decrement_hovered_sink,
                        *increment_hovered_sink,
                    ],
                );
            }
            RuntimePreviewKind::Counter { counter_sink, .. } => {
                sync_sink_values(&mut self.app, &self.executor, &[*counter_sink]);
            }
            RuntimePreviewKind::Interval { value_sink, .. } => {
                sync_sink_values(&mut self.app, &self.executor, &[*value_sink]);
            }
            RuntimePreviewKind::ListRetainReactive {
                mode_sink,
                count_sink,
                items_list_sink,
                item_sinks,
                ..
            } => {
                self.app.set_sink_value(
                    *mode_sink,
                    self.executor
                        .sink_value(*mode_sink)
                        .cloned()
                        .unwrap_or_else(|| KernelValue::from("show_even: False")),
                );
                self.app.set_sink_value(
                    *count_sink,
                    self.executor
                        .sink_value(*count_sink)
                        .cloned()
                        .unwrap_or_else(|| KernelValue::from("Filtered count: 6")),
                );
                project_slot_values_into_app(
                    &mut self.app,
                    item_sinks,
                    list_sink_items(&self.executor, *items_list_sink),
                    KernelValue::from(""),
                );
            }
            RuntimePreviewKind::ListMapExternalDep {
                mode_sink,
                info_sink,
                items_list_sink,
                item_sinks,
                ..
            } => {
                self.app.set_sink_value(
                    *mode_sink,
                    self.executor
                        .sink_value(*mode_sink)
                        .cloned()
                        .unwrap_or_else(|| KernelValue::from("show_filtered: False")),
                );
                self.app.set_sink_value(
                    *info_sink,
                    self.executor
                        .sink_value(*info_sink)
                        .cloned()
                        .unwrap_or_else(|| {
                            KernelValue::from(
                                "Expected: When True, show Apple and Cherry. When False, show all.",
                            )
                        }),
                );
                project_slot_values_into_app(
                    &mut self.app,
                    item_sinks,
                    list_sink_items(&self.executor, *items_list_sink)
                        .into_iter()
                        .filter(|value| !matches!(value, KernelValue::Skip)),
                    KernelValue::from(""),
                );
            }
            RuntimePreviewKind::AppendList {
                title_sink,
                input_sink,
                count_sinks,
                items_list_sink,
                item_sinks,
                item_prefix,
                ..
            } => {
                if let Some(title_sink) = title_sink {
                    sync_sink_values(&mut self.app, &self.executor, &[*title_sink]);
                }
                sync_sink_values(&mut self.app, &self.executor, &[*input_sink]);
                sync_sink_values(&mut self.app, &self.executor, count_sinks);
                project_slot_values_into_app(
                    &mut self.app,
                    item_sinks,
                    list_sink_items(&self.executor, *items_list_sink)
                        .into_iter()
                        .filter_map(|value| append_list_item_value(value, item_prefix)),
                    KernelValue::from(""),
                );
            }
            RuntimePreviewKind::TimedMath {
                input_a_sink,
                input_b_sink,
                result_sink,
                ..
            } => {
                sync_sink_values(
                    &mut self.app,
                    &self.executor,
                    &[*input_a_sink, *input_b_sink, *result_sink],
                );
            }
            RuntimePreviewKind::CircleDrawer {
                title_sink,
                count_sink,
                circles_sink,
                ..
            } => {
                sync_sink_values(
                    &mut self.app,
                    &self.executor,
                    &[*title_sink, *count_sink, *circles_sink],
                );
            }
        }
    }

    fn complex_counter_button_ids(&mut self) -> Option<(NodeId, NodeId)> {
        let RuntimePreviewKind::ComplexCounter { .. } = &self.kind else {
            return None;
        };
        let root = self.app.render_root();
        let stripe = root.children.first()?;
        let decrement = stripe.children.first()?;
        let increment = stripe.children.get(2)?;
        Some((decrement.id, increment.id))
    }
}

impl FormRuntimeHostViewPreview {
    fn from_temperature_converter_program(
        program: TemperatureConverterProgram,
    ) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let celsius_actor = runtime.alloc_actor();
        let fahrenheit_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let bindings = [
            FormInputBinding {
                change_port: program.celsius_change_port,
                key_down_port: Some(program.celsius_key_down_port),
            },
            FormInputBinding {
                change_port: program.fahrenheit_change_port,
                key_down_port: Some(program.fahrenheit_key_down_port),
            },
        ];
        let form = ValidatedFormRuntime::new(
            program.host_view.clone(),
            executor.sink_values(),
            bindings,
            [],
        );

        let mut preview = Self {
            kind: FormRuntimePreviewKind::TemperatureConverter {
                runtime,
                celsius_actor,
                fahrenheit_actor,
                program,
                executor,
                form,
            },
        };
        preview.sync_temperature_converter_inputs();
        Ok(preview)
    }

    fn from_flight_booker_program(program: FlightBookerProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let flight_type_actor = runtime.alloc_actor();
        let departure_actor = runtime.alloc_actor();
        let return_actor = runtime.alloc_actor();
        let book_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let form = ValidatedFormRuntime::new(
            program.host_view.clone(),
            executor.sink_values(),
            [
                FormInputBinding {
                    change_port: program.flight_type_change_port,
                    key_down_port: None,
                },
                FormInputBinding {
                    change_port: program.departure_change_port,
                    key_down_port: None,
                },
                FormInputBinding {
                    change_port: program.return_change_port,
                    key_down_port: None,
                },
            ],
            [program.book_press_port],
        );

        Ok(Self {
            kind: FormRuntimePreviewKind::FlightBooker {
                runtime,
                flight_type_actor,
                departure_actor,
                return_actor,
                book_actor,
                program,
                executor,
                form,
            },
        })
    }

    fn from_timer_program(program: TimerProgram) -> Result<Self, String> {
        let mut runtime = PreviewRuntime::new();
        let duration_actor = runtime.alloc_actor();
        let reset_actor = runtime.alloc_actor();
        let tick_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(program.ir.clone())?;
        let form = ValidatedFormRuntime::new(
            program.host_view.clone(),
            executor.sink_values(),
            [FormInputBinding {
                change_port: program.duration_change_port,
                key_down_port: None,
            }],
            [program.reset_press_port],
        );

        Ok(Self {
            kind: FormRuntimePreviewKind::Timer {
                runtime,
                duration_actor,
                reset_actor,
                tick_actor,
                program,
                executor,
                form,
            },
        })
    }

    #[cfg(test)]
    fn app(&self) -> &HostViewPreviewApp {
        match &self.kind {
            FormRuntimePreviewKind::TemperatureConverter { form, .. } => form.app(),
            FormRuntimePreviewKind::FlightBooker { form, .. } => form.app(),
            FormRuntimePreviewKind::Timer { form, .. } => form.app(),
        }
    }

    fn app_mut(&mut self) -> &mut HostViewPreviewApp {
        match &mut self.kind {
            FormRuntimePreviewKind::TemperatureConverter { form, .. } => form.app_mut(),
            FormRuntimePreviewKind::FlightBooker { form, .. } => form.app_mut(),
            FormRuntimePreviewKind::Timer { form, .. } => form.app_mut(),
        }
    }

    fn preview_text(&mut self) -> String {
        match &mut self.kind {
            FormRuntimePreviewKind::TemperatureConverter { form, .. } => form.preview_text(),
            FormRuntimePreviewKind::FlightBooker { form, .. } => form.preview_text(),
            FormRuntimePreviewKind::Timer { form, .. } => form.preview_text(),
        }
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = match &mut self.kind {
            FormRuntimePreviewKind::TemperatureConverter { form, .. } => form.render_snapshot(),
            FormRuntimePreviewKind::FlightBooker { form, .. } => form.render_snapshot(),
            FormRuntimePreviewKind::Timer { form, .. } => form.render_snapshot(),
        };
        (RenderRoot::UiTree(root), state)
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        match &mut self.kind {
            FormRuntimePreviewKind::TemperatureConverter {
                runtime,
                celsius_actor,
                fahrenheit_actor,
                program,
                executor,
                form,
            } => {
                let dispatch = form.dispatch_ui_events(batch);
                let inputs = dispatch
                    .input_events
                    .iter()
                    .enumerate()
                    .filter_map(|(index, event)| match event {
                        FormInputEvent::Changed { index: 0 } => Some(HostInput::Pulse {
                            actor: *celsius_actor,
                            port: program.celsius_change_port,
                            value: KernelValue::from(form.input(0).to_string()),
                            seq: runtime.causal_seq(index as u32),
                        }),
                        FormInputEvent::Changed { index: 1 } => Some(HostInput::Pulse {
                            actor: *fahrenheit_actor,
                            port: program.fahrenheit_change_port,
                            value: KernelValue::from(form.input(1).to_string()),
                            seq: runtime.causal_seq(index as u32),
                        }),
                        FormInputEvent::KeyDown { .. } | FormInputEvent::Changed { .. } => None,
                    })
                    .collect::<Vec<_>>();

                let changed = apply_form_runtime_messages(runtime, executor, form, inputs);
                if changed {
                    sync_temperature_converter_inputs(program, form);
                }
                changed
            }
            FormRuntimePreviewKind::FlightBooker {
                runtime,
                flight_type_actor,
                departure_actor,
                return_actor,
                book_actor,
                program,
                executor,
                form,
            } => {
                let dispatch = form.dispatch_ui_events(batch);
                let mut inputs = dispatch
                    .input_events
                    .iter()
                    .enumerate()
                    .filter_map(|(seq, event)| match event {
                        FormInputEvent::Changed { index: 0 } => Some(HostInput::Pulse {
                            actor: *flight_type_actor,
                            port: program.flight_type_change_port,
                            value: KernelValue::from(form.input(0).to_string()),
                            seq: runtime.causal_seq(seq as u32),
                        }),
                        FormInputEvent::Changed { index: 1 } => Some(HostInput::Pulse {
                            actor: *departure_actor,
                            port: program.departure_change_port,
                            value: KernelValue::from(form.input(1).to_string()),
                            seq: runtime.causal_seq(seq as u32),
                        }),
                        FormInputEvent::Changed { index: 2 } => Some(HostInput::Pulse {
                            actor: *return_actor,
                            port: program.return_change_port,
                            value: KernelValue::from(form.input(2).to_string()),
                            seq: runtime.causal_seq(seq as u32),
                        }),
                        FormInputEvent::KeyDown { .. } | FormInputEvent::Changed { .. } => None,
                    })
                    .collect::<Vec<_>>();

                let input_seq_base = inputs.len() as u32;
                inputs.extend(dispatch.clicked_ports.into_iter().enumerate().filter_map(
                    |(offset, port)| {
                        (port == program.book_press_port).then_some(HostInput::Pulse {
                            actor: *book_actor,
                            port,
                            value: KernelValue::from("press"),
                            seq: runtime.causal_seq(input_seq_base + offset as u32),
                        })
                    },
                ));

                apply_form_runtime_messages(runtime, executor, form, inputs)
            }
            FormRuntimePreviewKind::Timer {
                runtime,
                duration_actor,
                reset_actor,
                tick_actor,
                program,
                executor,
                form,
            } => {
                let tick_batch = batch.clone();
                let dispatch = form.dispatch_ui_events(batch);
                let tick_event_port = form.app().event_port_for_source(program.tick_port);
                let mut inputs = dispatch
                    .input_events
                    .iter()
                    .enumerate()
                    .filter_map(|(seq, event)| match event {
                        FormInputEvent::Changed { index: 0 } => Some(HostInput::Pulse {
                            actor: *duration_actor,
                            port: program.duration_change_port,
                            value: KernelValue::from(form.input(0).to_string()),
                            seq: runtime.causal_seq(seq as u32),
                        }),
                        FormInputEvent::Changed { .. } | FormInputEvent::KeyDown { .. } => None,
                    })
                    .collect::<Vec<_>>();

                let mut next_seq = inputs.len() as u32;
                for port in dispatch.clicked_ports {
                    if port == program.reset_press_port {
                        inputs.push(HostInput::Pulse {
                            actor: *reset_actor,
                            port,
                            value: KernelValue::from("press"),
                            seq: runtime.causal_seq(next_seq),
                        });
                        next_seq += 1;
                    }
                }

                for event in &tick_batch.events {
                    if Some(event.target) == tick_event_port
                        && matches!(event.kind, UiEventKind::Custom(_))
                    {
                        inputs.push(HostInput::Pulse {
                            actor: *tick_actor,
                            port: program.tick_port,
                            value: KernelValue::from("tick"),
                            seq: runtime.causal_seq(next_seq),
                        });
                        next_seq += 1;
                    }
                }

                apply_form_runtime_messages(runtime, executor, form, inputs)
            }
        }
    }

    fn sync_temperature_converter_inputs(&mut self) {
        if let FormRuntimePreviewKind::TemperatureConverter { program, form, .. } = &mut self.kind {
            sync_temperature_converter_inputs(program, form);
        }
    }
}

impl LatestHostViewPreview {
    fn from_program(program: LatestProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view,
            BTreeMap::from([
                (program.value_sink, KernelValue::from("3")),
                (program.sum_sink, KernelValue::from("3")),
            ]),
        );
        Self {
            send_press_ports: program.send_press_ports,
            value_sink: program.value_sink,
            sum_sink: program.sum_sink,
            current_value: 3,
            app,
        }
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let first = self.app.event_port_for_source(self.send_press_ports[0]);
        let second = self.app.event_port_for_source(self.send_press_ports[1]);
        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            if Some(event.target) == first {
                return self.set_value(1);
            }
            if Some(event.target) == second {
                return self.set_value(2);
            }
        }
        false
    }

    fn set_value(&mut self, value: i64) -> bool {
        if self.current_value == value {
            return false;
        }
        self.current_value = value;
        let value = KernelValue::from(value.to_string());
        self.app.set_sink_value(self.value_sink, value.clone());
        self.app.set_sink_value(self.sum_sink, value);
        true
    }
}

impl PagesHostViewPreview {
    fn from_program(program: PagesProgram) -> Self {
        let current_route = current_pages_route();
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            pages_sink_values_for_route(&program, &current_route),
        );
        Self {
            program,
            current_route,
            app,
        }
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let home_port = self
            .app
            .event_port_for_source(self.program.nav_press_ports[0]);
        let about_port = self
            .app
            .event_port_for_source(self.program.nav_press_ports[1]);
        let contact_port = self
            .app
            .event_port_for_source(self.program.nav_press_ports[2]);

        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            if Some(event.target) == home_port {
                return self.set_route(HOME_ROUTE);
            }
            if Some(event.target) == about_port {
                return self.set_route(ABOUT_ROUTE);
            }
            if Some(event.target) == contact_port {
                return self.set_route(CONTACT_ROUTE);
            }
        }

        false
    }

    fn set_route(&mut self, route: &str) -> bool {
        let normalized = normalize_pages_route(route);
        if normalized == self.current_route {
            return false;
        }
        self.current_route = normalized.clone();
        push_pages_route_to_browser(&normalized);
        for (sink, value) in pages_sink_values_for_route(&self.program, &normalized) {
            self.app.set_sink_value(sink, value);
        }
        true
    }
}

impl LocalStateHostViewPreview {
    fn from_text_interpolation_update_program(program: TextInterpolationUpdateProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            text_interpolation_update_sinks(&program, false),
        );
        Self {
            kind: LocalStateProgramKind::TextInterpolationUpdate {
                program,
                value: false,
            },
            app,
        }
    }

    fn from_while_function_call_program(program: WhileFunctionCallProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            while_function_call_sinks(&program, false),
        );
        Self {
            kind: LocalStateProgramKind::WhileFunctionCall {
                program,
                value: false,
            },
            app,
        }
    }

    fn from_button_hover_to_click_test_program(program: ButtonHoverToClickTestProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            button_hover_to_click_sinks(&program, [false; 3]),
        );
        Self {
            kind: LocalStateProgramKind::ButtonHoverToClickTest {
                program,
                clicked: [false; 3],
            },
            app,
        }
    }

    fn from_button_hover_test_program(program: ButtonHoverTestProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            button_hover_sinks(&program, [false; 3]),
        );
        Self {
            kind: LocalStateProgramKind::ButtonHoverTest {
                program,
                hovered: [false; 3],
            },
            app,
        }
    }

    fn from_filter_checkbox_bug_program(program: FilterCheckboxBugProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            filter_checkbox_bug_sinks(&program, FilterCheckboxBugMode::All, [false; 2]),
        );
        Self {
            kind: LocalStateProgramKind::FilterCheckboxBug {
                program,
                filter: FilterCheckboxBugMode::All,
                checked: [false; 2],
            },
            app,
        }
    }

    fn from_checkbox_test_program(program: CheckboxTestProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            checkbox_test_sinks(&program, [false; 2]),
        );
        Self {
            kind: LocalStateProgramKind::CheckboxTest {
                program,
                checked: [false; 2],
            },
            app,
        }
    }

    fn from_list_object_state_program(program: ListObjectStateProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            list_object_state_sinks(&program, [0; 3]),
        );
        Self {
            kind: LocalStateProgramKind::ListObjectState {
                program,
                counts: [0; 3],
            },
            app,
        }
    }

    fn from_chained_list_remove_bug_program(program: ChainedListRemoveBugProgram) -> Self {
        let items = MappedListViewRuntime::new(
            [
                (
                    0,
                    ChainedListRemoveBugItem {
                        name: "Item A".to_string(),
                        completed: false,
                    },
                ),
                (
                    1,
                    ChainedListRemoveBugItem {
                        name: "Item B".to_string(),
                        completed: false,
                    },
                ),
            ],
            2,
        );
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            chained_list_remove_bug_sinks(&program, &items),
        );
        let clicks = MappedClickRuntime::new(
            [program.add_press_port, program.clear_completed_port]
                .into_iter()
                .chain(program.checkbox_ports)
                .chain(program.remove_ports),
        );
        Self {
            kind: LocalStateProgramKind::ChainedListRemoveBug {
                program,
                clicks,
                items,
            },
            app,
        }
    }

    fn from_switch_hold_test_program(program: SwitchHoldTestProgram) -> Self {
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            switch_hold_test_sinks(&program, true, [0, 0]),
        );
        Self {
            kind: LocalStateProgramKind::SwitchHoldTest {
                program,
                show_item_a: true,
                click_counts: [0, 0],
            },
            app,
        }
    }

    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let events = batch.events;
        let sink_values = match &mut self.kind {
            LocalStateProgramKind::TextInterpolationUpdate { program, value } => {
                let toggle_port = self.app.event_port_for_source(program.toggle_press_port);
                let mut sink_values = None;
                for event in &events {
                    if event.kind == UiEventKind::Click && Some(event.target) == toggle_port {
                        *value = !*value;
                        sink_values = Some(text_interpolation_update_sinks(program, *value));
                        break;
                    }
                }
                sink_values
            }
            LocalStateProgramKind::WhileFunctionCall { program, value } => {
                let toggle_port = self.app.event_port_for_source(program.toggle_press_port);
                let mut sink_values = None;
                for event in &events {
                    if event.kind == UiEventKind::Click && Some(event.target) == toggle_port {
                        *value = !*value;
                        sink_values = Some(while_function_call_sinks(program, *value));
                        break;
                    }
                }
                sink_values
            }
            LocalStateProgramKind::ButtonHoverToClickTest { program, clicked } => {
                let button_ports = program
                    .button_press_ports
                    .map(|port| self.app.event_port_for_source(port));
                let mut sink_values = None;
                for event in &events {
                    if event.kind != UiEventKind::Click {
                        continue;
                    }
                    for (index, port) in button_ports.iter().enumerate() {
                        if Some(event.target) == *port {
                            clicked[index] = !clicked[index];
                            sink_values = Some(button_hover_to_click_sinks(program, *clicked));
                            break;
                        }
                    }
                    if sink_values.is_some() {
                        break;
                    }
                }
                sink_values
            }
            LocalStateProgramKind::ButtonHoverTest { .. } => None,
            LocalStateProgramKind::FilterCheckboxBug {
                program,
                filter,
                checked,
            } => {
                let all_port = self.app.event_port_for_source(program.filter_all_port);
                let active_port = self.app.event_port_for_source(program.filter_active_port);
                let checkbox_ports = program
                    .checkbox_ports
                    .map(|port| self.app.event_port_for_source(port));
                let mut changed = false;
                for event in &events {
                    if event.kind != UiEventKind::Click {
                        continue;
                    }
                    if Some(event.target) == all_port {
                        changed |= *filter != FilterCheckboxBugMode::All;
                        *filter = FilterCheckboxBugMode::All;
                        continue;
                    }
                    if Some(event.target) == active_port {
                        changed |= *filter != FilterCheckboxBugMode::Active;
                        *filter = FilterCheckboxBugMode::Active;
                        continue;
                    }
                    for (index, port) in checkbox_ports.iter().enumerate() {
                        if Some(event.target) == *port {
                            checked[index] = !checked[index];
                            changed = true;
                            break;
                        }
                    }
                }
                changed.then(|| filter_checkbox_bug_sinks(program, *filter, *checked))
            }
            LocalStateProgramKind::CheckboxTest { program, checked } => {
                let checkbox_ports = program
                    .checkbox_ports
                    .map(|port| self.app.event_port_for_source(port));
                let mut changed = false;
                for event in &events {
                    if event.kind != UiEventKind::Click {
                        continue;
                    }
                    for (index, port) in checkbox_ports.iter().enumerate() {
                        if Some(event.target) == *port {
                            checked[index] = !checked[index];
                            changed = true;
                            break;
                        }
                    }
                }
                changed.then(|| checkbox_test_sinks(program, *checked))
            }
            LocalStateProgramKind::ListObjectState { program, counts } => {
                let button_ports = program
                    .press_ports
                    .map(|port| self.app.event_port_for_source(port));
                let mut changed = false;
                for event in &events {
                    if event.kind != UiEventKind::Click {
                        continue;
                    }
                    for (index, port) in button_ports.iter().enumerate() {
                        if Some(event.target) == *port {
                            counts[index] += 1;
                            changed = true;
                            break;
                        }
                    }
                }
                changed.then(|| list_object_state_sinks(program, *counts))
            }
            LocalStateProgramKind::ChainedListRemoveBug {
                program,
                clicks,
                items,
            } => {
                let clicked = clicks.dispatch_clicks(&self.app, UiEventBatch { events });
                if !clicked.is_empty()
                    && apply_chained_list_remove_bug_clicks(program, items, clicked)
                {
                    Some(chained_list_remove_bug_sinks(program, items))
                } else {
                    None
                }
            }
            LocalStateProgramKind::SwitchHoldTest {
                program,
                show_item_a,
                click_counts,
            } => {
                let toggle_port = self.app.event_port_for_source(program.toggle_press_port);
                let item_ports = program
                    .item_press_ports
                    .map(|port| self.app.event_port_for_source(port));
                let mut sink_values = None;
                for event in &events {
                    if event.kind != UiEventKind::Click {
                        continue;
                    }
                    if Some(event.target) == toggle_port {
                        *show_item_a = !*show_item_a;
                        sink_values =
                            Some(switch_hold_test_sinks(program, *show_item_a, *click_counts));
                        break;
                    }
                    if Some(event.target) == item_ports[0] && *show_item_a {
                        click_counts[0] += 1;
                        sink_values =
                            Some(switch_hold_test_sinks(program, *show_item_a, *click_counts));
                        break;
                    }
                    if Some(event.target) == item_ports[1] && !*show_item_a {
                        click_counts[1] += 1;
                        sink_values =
                            Some(switch_hold_test_sinks(program, *show_item_a, *click_counts));
                        break;
                    }
                }
                sink_values
            }
        };

        sink_values.map_or(false, |sink_values| self.sync_sinks(sink_values))
    }

    fn dispatch_ui_facts(&mut self, batch: UiFactBatch) -> bool {
        let Some(button_ids) = self.button_ids() else {
            return false;
        };
        let sink_values = match &mut self.kind {
            LocalStateProgramKind::ButtonHoverTest { program, hovered } => {
                let mut changed = false;
                for fact in batch.facts {
                    let UiFactKind::Hovered(is_hovered) = fact.kind else {
                        continue;
                    };
                    for (index, id) in button_ids.iter().enumerate() {
                        if fact.id == *id && hovered[index] != is_hovered {
                            hovered[index] = is_hovered;
                            changed = true;
                        }
                    }
                }
                if changed {
                    Some(button_hover_sinks(program, *hovered))
                } else {
                    None
                }
            }
            _ => None,
        };

        sink_values.map_or(false, |sink_values| self.sync_sinks(sink_values))
    }

    fn button_ids(&mut self) -> Option<[NodeId; 3]> {
        let LocalStateProgramKind::ButtonHoverTest { .. } = &self.kind else {
            return None;
        };
        let root = self.app.render_root();
        let stripe = root.children.first()?;
        let button_row = stripe.children.get(1)?;
        Some([
            button_row.children.first()?.id,
            button_row.children.get(1)?.id,
            button_row.children.get(2)?.id,
        ])
    }

    fn sync_sinks(&mut self, sink_values: BTreeMap<SinkPortId, KernelValue>) -> bool {
        for (sink, value) in sink_values {
            self.app.set_sink_value(sink, value);
        }
        true
    }
}

impl StaticHostViewPreview {
    fn from_parts(host_view: HostViewIr, sink_values: BTreeMap<SinkPortId, KernelValue>) -> Self {
        Self {
            app: HostViewPreviewApp::new(host_view, sink_values),
        }
    }

    fn from_program(program: StaticProgram) -> Self {
        let StaticProgram {
            host_view,
            sink_values,
        } = program;
        Self::from_parts(host_view, sink_values)
    }

    fn from_fibonacci_program(program: FibonacciProgram) -> Self {
        let FibonacciProgram {
            host_view,
            sink_values,
        } = program;
        Self::from_parts(host_view, sink_values)
    }

    fn from_layers_program(program: LayersProgram) -> Self {
        let LayersProgram {
            host_view,
            sink_values,
        } = program;
        Self::from_parts(host_view, sink_values)
    }

    fn from_list_map_block_program(program: ListMapBlockProgram) -> Self {
        let ListMapBlockProgram {
            host_view,
            mode_sink,
            direct_item_sinks,
            block_item_sinks,
        } = program;
        Self::from_parts(
            host_view,
            initial_list_map_block_sinks(mode_sink, &direct_item_sinks, &block_item_sinks),
        )
    }
}

fn pages_sink_values_for_route(
    program: &PagesProgram,
    route: &str,
) -> BTreeMap<SinkPortId, KernelValue> {
    let normalized = normalize_pages_route(route);
    let (title, description) = match normalized.as_str() {
        HOME_ROUTE => (
            "Welcome Home",
            "This is the home page. Use the navigation above to explore.",
        ),
        ABOUT_ROUTE => (
            "About",
            "A multi-page Boon app demonstrating Router/route and Router/go_to.",
        ),
        CONTACT_ROUTE => (
            "Contact",
            "Get in touch! URL-driven state and navigation demo.",
        ),
        _ => (
            "404 - Not Found",
            "The page you're looking for doesn't exist.",
        ),
    };

    BTreeMap::from([
        (
            program.current_page_sink,
            match normalized.as_str() {
                HOME_ROUTE => KernelValue::Tag("Home".to_string()),
                ABOUT_ROUTE => KernelValue::Tag("About".to_string()),
                CONTACT_ROUTE => KernelValue::Tag("Contact".to_string()),
                _ => KernelValue::Tag("NotFound".to_string()),
            },
        ),
        (program.title_sink, KernelValue::from(title)),
        (program.description_sink, KernelValue::from(description)),
        (
            program.nav_active_sinks[0],
            KernelValue::Bool(normalized.as_str() == HOME_ROUTE),
        ),
        (
            program.nav_active_sinks[1],
            KernelValue::Bool(normalized.as_str() == ABOUT_ROUTE),
        ),
        (
            program.nav_active_sinks[2],
            KernelValue::Bool(normalized.as_str() == CONTACT_ROUTE),
        ),
    ])
}

fn bool_text(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

fn text_interpolation_update_sinks(
    program: &TextInterpolationUpdateProgram,
    value: bool,
) -> BTreeMap<SinkPortId, KernelValue> {
    let value_text = bool_text(value);
    BTreeMap::from([
        (
            program.button_label_sink,
            KernelValue::from(format!("Toggle (value: {value_text})")),
        ),
        (
            program.label_sink,
            KernelValue::from(format!("Label shows: {value_text}")),
        ),
        (program.while_sink, KernelValue::from(value)),
    ])
}

fn while_function_call_sinks(
    program: &WhileFunctionCallProgram,
    show_greeting: bool,
) -> BTreeMap<SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.toggle_label_sink,
            KernelValue::from(format!("Toggle (show: {})", bool_text(show_greeting))),
        ),
        (program.content_sink, KernelValue::from(show_greeting)),
    ])
}

fn button_hover_to_click_sinks(
    program: &ButtonHoverToClickTestProgram,
    clicked: [bool; 3],
) -> BTreeMap<SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.intro_sink,
            KernelValue::from("Click each button - clicked ones turn darker with outline"),
        ),
        (
            program.button_active_sinks[0],
            KernelValue::Bool(clicked[0]),
        ),
        (
            program.button_active_sinks[1],
            KernelValue::Bool(clicked[1]),
        ),
        (
            program.button_active_sinks[2],
            KernelValue::Bool(clicked[2]),
        ),
        (
            program.state_sink,
            KernelValue::from(format!(
                "States - A: {}, B: {}, C: {}",
                bool_text(clicked[0]),
                bool_text(clicked[1]),
                bool_text(clicked[2])
            )),
        ),
    ])
}

fn button_hover_sinks(
    program: &ButtonHoverTestProgram,
    hovered: [bool; 3],
) -> BTreeMap<SinkPortId, KernelValue> {
    BTreeMap::from([
        (
            program.intro_sink,
            KernelValue::from("Hover each button - only hovered one should show border"),
        ),
        (program.button_hover_sinks[0], KernelValue::Bool(hovered[0])),
        (program.button_hover_sinks[1], KernelValue::Bool(hovered[1])),
        (program.button_hover_sinks[2], KernelValue::Bool(hovered[2])),
    ])
}

fn filter_checkbox_bug_sinks(
    program: &FilterCheckboxBugProgram,
    filter: FilterCheckboxBugMode,
    checked: [bool; 2],
) -> BTreeMap<SinkPortId, KernelValue> {
    let filter_text = match filter {
        FilterCheckboxBugMode::All => "Filter: All",
        FilterCheckboxBugMode::Active => "Filter: Active",
    };
    let view_label = match filter {
        FilterCheckboxBugMode::All => "ALL",
        FilterCheckboxBugMode::Active => "ACTIVE",
    };

    BTreeMap::from([
        (program.filter_sink, KernelValue::from(filter_text)),
        (program.checkbox_sinks[0], KernelValue::Bool(checked[0])),
        (program.checkbox_sinks[1], KernelValue::Bool(checked[1])),
        (
            program.item_label_sinks[0],
            KernelValue::from(format!("Item A ({view_label}) - checked: {}", checked[0])),
        ),
        (
            program.item_label_sinks[1],
            KernelValue::from(format!("Item B ({view_label}) - checked: {}", checked[1])),
        ),
        (
            program.footer_sink,
            KernelValue::from("Test: Click Active, All, then checkbox 3x"),
        ),
    ])
}

fn checkbox_test_sinks(
    program: &CheckboxTestProgram,
    checked: [bool; 2],
) -> BTreeMap<SinkPortId, KernelValue> {
    BTreeMap::from([
        (program.label_sinks[0], KernelValue::from("Item A")),
        (program.label_sinks[1], KernelValue::from("Item B")),
        (program.checkbox_sinks[0], KernelValue::Bool(checked[0])),
        (program.checkbox_sinks[1], KernelValue::Bool(checked[1])),
        (
            program.status_sinks[0],
            KernelValue::from(if checked[0] {
                "(checked)"
            } else {
                "(unchecked)"
            }),
        ),
        (
            program.status_sinks[1],
            KernelValue::from(if checked[1] {
                "(checked)"
            } else {
                "(unchecked)"
            }),
        ),
    ])
}

fn list_object_state_sinks(
    program: &ListObjectStateProgram,
    counts: [u32; 3],
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        SinkPortId(89),
        KernelValue::from("Click each button - counts should be independent"),
    );
    for (sink, count) in program.count_sinks.iter().zip(counts) {
        sink_values.insert(*sink, KernelValue::from(format!("Count: {count}")));
    }
    sink_values
}

fn chained_list_remove_bug_sinks(
    program: &ChainedListRemoveBugProgram,
    items: &MappedListViewRuntime<ChainedListRemoveBugItem>,
) -> BTreeMap<SinkPortId, KernelValue> {
    let active_count = items.iter().filter(|item| !item.value.completed).count();
    let completed_count = items.iter().filter(|item| item.value.completed).count();
    let mut sink_values = BTreeMap::from([
        (
            program.title_sink,
            KernelValue::from("Chained List/remove Bug Test"),
        ),
        (
            program.counts_sink,
            KernelValue::from(format!(
                "Active: {active_count}, Completed: {completed_count}"
            )),
        ),
    ]);
    items.project_visible_into_map(
        &mut sink_values,
        &program.checkbox_sinks,
        |_| true,
        |item| KernelValue::Bool(item.value.completed),
        KernelValue::Bool(false),
    );
    items.project_visible_into_map(
        &mut sink_values,
        &program.row_label_sinks,
        |_| true,
        |item| KernelValue::from(format!("{} (id={})", item.value.name, item.id)),
        KernelValue::from(""),
    );
    sink_values
}

fn apply_chained_list_remove_bug_clicks(
    program: &ChainedListRemoveBugProgram,
    items: &mut MappedListViewRuntime<ChainedListRemoveBugItem>,
    clicked: Vec<SourcePortId>,
) -> bool {
    let mut changed = false;

    for port in clicked {
        if port == program.add_press_port {
            items.append(ChainedListRemoveBugItem {
                name: "New Item".to_string(),
                completed: false,
            });
            changed = true;
            continue;
        }
        if port == program.clear_completed_port {
            changed |= items.retain(|item| !item.value.completed);
            continue;
        }
        if let Some(index) = program
            .checkbox_ports
            .iter()
            .position(|candidate| *candidate == port)
        {
            changed |= items.update_visible(
                index,
                |_| true,
                |item| {
                    item.value.completed = !item.value.completed;
                },
            );
            continue;
        }
        if let Some(index) = program
            .remove_ports
            .iter()
            .position(|candidate| *candidate == port)
        {
            changed |= items.remove_visible(index, |_| true);
        }
    }

    changed
}

fn switch_hold_test_sinks(
    program: &SwitchHoldTestProgram,
    show_item_a: bool,
    click_counts: [u32; 2],
) -> BTreeMap<SinkPortId, KernelValue> {
    let (item_name, count, disabled) = if show_item_a {
        ("Item A", click_counts[0], [false, true])
    } else {
        ("Item B", click_counts[1], [true, false])
    };
    BTreeMap::from([
        (program.show_item_a_sink, KernelValue::Bool(show_item_a)),
        (
            program.item_count_sinks[0],
            KernelValue::from(click_counts[0] as f64),
        ),
        (
            program.item_count_sinks[1],
            KernelValue::from(click_counts[1] as f64),
        ),
        (
            program.current_item_sink,
            KernelValue::from(format!("Showing: {item_name}")),
        ),
        (
            program.current_count_sink,
            KernelValue::from(format!("{item_name} clicks: {count}")),
        ),
        (
            program.item_disabled_sinks[0],
            KernelValue::Bool(disabled[0]),
        ),
        (
            program.item_disabled_sinks[1],
            KernelValue::Bool(disabled[1]),
        ),
        (
            program.footer_sink,
            KernelValue::from(
                "Test: Click button, toggle view, click again. Counts should increment correctly.",
            ),
        ),
    ])
}

fn normalize_pages_route(route: &str) -> String {
    match route {
        "" | HOME_ROUTE => HOME_ROUTE.to_string(),
        ABOUT_ROUTE => ABOUT_ROUTE.to_string(),
        CONTACT_ROUTE => CONTACT_ROUTE.to_string(),
        other if other.starts_with('/') => other.to_string(),
        other => format!("/{other}"),
    }
}

fn current_pages_route() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(pathname) = window.location().pathname() {
                return normalize_pages_route(&pathname);
            }
        }
    }

    HOME_ROUTE.to_string()
}

fn push_pages_route_to_browser(_route: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(history) = window.history() {
                let search = window.location().search().unwrap_or_default();
                let target = format!("{}{}", normalize_pages_route(_route), search);
                let _ =
                    history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&target));
            }
        }
    }
}

fn pulse_inputs_from_events(
    app: &HostViewPreviewApp,
    host_actor: ActorId,
    runtime: &PreviewRuntime,
    bindings: &[UiEventPulseBinding],
    events: Vec<UiEvent>,
) -> Vec<HostInput> {
    let resolved = bindings
        .iter()
        .filter_map(|binding| {
            app.event_port_for_source(binding.source_port)
                .map(|event_port| (event_port, binding.clone()))
        })
        .collect::<Vec<_>>();

    events
        .into_iter()
        .enumerate()
        .filter_map(|(index, event)| {
            resolved.iter().find_map(|(event_port, binding)| {
                if *event_port != event.target || !binding.expected_kind.matches(&event.kind) {
                    return None;
                }
                Some(HostInput::Pulse {
                    actor: host_actor,
                    port: binding.source_port,
                    value: binding.value.clone(),
                    seq: runtime.causal_seq(index as u32),
                })
            })
        })
        .collect()
}

fn append_list_inputs_from_events(
    app: &HostViewPreviewApp,
    host_actor: ActorId,
    runtime: &PreviewRuntime,
    input_change_port: SourcePortId,
    input_key_down_port: SourcePortId,
    clear_press_port: Option<SourcePortId>,
    events: Vec<UiEvent>,
) -> Vec<HostInput> {
    let change_port = app.event_port_for_source(input_change_port);
    let key_port = app.event_port_for_source(input_key_down_port);
    let clear_port = clear_press_port.and_then(|port| app.event_port_for_source(port));

    events
        .into_iter()
        .enumerate()
        .filter_map(|(index, event)| match event.kind {
            UiEventKind::Input | UiEventKind::Change if Some(event.target) == change_port => {
                Some(HostInput::Pulse {
                    actor: host_actor,
                    port: input_change_port,
                    value: KernelValue::from(event.payload.unwrap_or_default()),
                    seq: runtime.causal_seq(index as u32),
                })
            }
            UiEventKind::KeyDown if Some(event.target) == key_port => Some(HostInput::Pulse {
                actor: host_actor,
                port: input_key_down_port,
                value: KernelValue::from(event.payload.unwrap_or_default()),
                seq: runtime.causal_seq(index as u32),
            }),
            UiEventKind::Click if Some(event.target) == clear_port => Some(HostInput::Pulse {
                actor: host_actor,
                port: clear_press_port.expect("clear port"),
                value: KernelValue::from("press"),
                seq: runtime.causal_seq(index as u32),
            }),
            _ => None,
        })
        .collect()
}

impl ExpectedUiEventKind {
    fn matches(&self, actual: &UiEventKind) -> bool {
        match self {
            Self::Exact(expected) => *actual == *expected,
            Self::AnyCustom => matches!(actual, UiEventKind::Custom(_)),
        }
    }
}

fn initial_list_retain_reactive_sinks(
    mode_sink: SinkPortId,
    count_sink: SinkPortId,
    items_list_sink: SinkPortId,
    item_sinks: &[SinkPortId],
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        mode_sink,
        executor
            .sink_value(mode_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("show_even: False")),
    );
    sink_values.insert(
        count_sink,
        executor
            .sink_value(count_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("Filtered count: 6")),
    );
    project_slot_values_into_map(
        &mut sink_values,
        item_sinks,
        list_sink_items(executor, items_list_sink),
        KernelValue::from(""),
    );
    sink_values
}

fn initial_list_map_external_dep_sinks(
    mode_sink: SinkPortId,
    info_sink: SinkPortId,
    items_list_sink: SinkPortId,
    item_sinks: &[SinkPortId],
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(
        mode_sink,
        executor
            .sink_value(mode_sink)
            .cloned()
            .unwrap_or_else(|| KernelValue::from("show_filtered: False")),
    );
    sink_values.insert(
        info_sink,
        executor.sink_value(info_sink).cloned().unwrap_or_else(|| {
            KernelValue::from("Expected: When True, show Apple and Cherry. When False, show all.")
        }),
    );
    project_slot_values_into_map(
        &mut sink_values,
        item_sinks,
        list_sink_items(executor, items_list_sink)
            .into_iter()
            .filter(|value| !matches!(value, KernelValue::Skip)),
        KernelValue::from(""),
    );
    sink_values
}

fn initial_list_map_block_sinks(
    mode_sink: SinkPortId,
    direct_item_sinks: &[SinkPortId; 5],
    block_item_sinks: &[SinkPortId; 5],
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(mode_sink, KernelValue::from("Mode: All"));
    for (sink, value) in direct_item_sinks.iter().zip(1..=5) {
        sink_values.insert(*sink, KernelValue::from(value as f64));
    }
    for (sink, value) in block_item_sinks.iter().zip(1..=5) {
        sink_values.insert(*sink, KernelValue::from(value as f64));
    }
    sink_values
}

fn initial_append_list_sinks(
    title_sink: Option<SinkPortId>,
    input_sink: SinkPortId,
    count_sinks: &[SinkPortId],
    items_list_sink: SinkPortId,
    item_sinks: &[SinkPortId],
    item_prefix: &str,
    executor: &IrExecutor,
) -> BTreeMap<SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    if let Some(title_sink) = title_sink {
        if let Some(value) = executor.sink_value(title_sink).cloned() {
            sink_values.insert(title_sink, value);
        }
    }
    if let Some(value) = executor.sink_value(input_sink).cloned() {
        sink_values.insert(input_sink, value);
    }
    for sink in count_sinks {
        if let Some(value) = executor.sink_value(*sink).cloned() {
            sink_values.insert(*sink, value);
        }
    }
    project_slot_values_into_map(
        &mut sink_values,
        item_sinks,
        list_sink_items(executor, items_list_sink)
            .into_iter()
            .filter_map(|value| append_list_item_value(value, item_prefix)),
        KernelValue::from(""),
    );
    sink_values
}

fn list_sink_items(executor: &IrExecutor, sink: SinkPortId) -> Vec<KernelValue> {
    executor
        .sink_value(sink)
        .and_then(|value| match value {
            KernelValue::List(items) => Some(items.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn sync_sink_values(app: &mut HostViewPreviewApp, executor: &IrExecutor, sinks: &[SinkPortId]) {
    for sink in sinks {
        if let Some(value) = executor.sink_value(*sink).cloned() {
            app.set_sink_value(*sink, value);
        }
    }
}

fn append_list_item_value(value: KernelValue, item_prefix: &str) -> Option<KernelValue> {
    match value {
        KernelValue::Text(text) | KernelValue::Tag(text) => {
            Some(KernelValue::from(format!("{item_prefix}{text}")))
        }
        _ => None,
    }
}

fn apply_form_runtime_messages<const N: usize>(
    runtime: &mut PreviewRuntime,
    executor: &mut IrExecutor,
    form: &mut ValidatedFormRuntime<N>,
    inputs: Vec<HostInput>,
) -> bool {
    if inputs.is_empty() {
        return false;
    }
    runtime.dispatch_inputs_batches(inputs.as_slice(), |messages| {
        executor
            .apply_pure_messages_owned(messages.drain(..))
            .expect("lowered IR should execute");
    });
    for (sink, value) in executor.sink_values() {
        form.set_sink_value(sink, value);
    }
    true
}

fn sync_temperature_converter_inputs(
    program: &TemperatureConverterProgram,
    form: &mut ValidatedFormRuntime<2>,
) {
    let celsius = form
        .sink_value(program.celsius_input_sink)
        .map(render_kernel_value)
        .unwrap_or_default();
    let fahrenheit = form
        .sink_value(program.fahrenheit_input_sink)
        .map(render_kernel_value)
        .unwrap_or_default();
    let _ = form.set_input(0, celsius);
    let _ = form.set_input(1, fahrenheit);
}

fn render_kernel_value(value: &KernelValue) -> String {
    match value {
        KernelValue::Number(number) if number.fract() == 0.0 => format!("{}", *number as i64),
        KernelValue::Number(number) => number.to_string(),
        KernelValue::Text(text) | KernelValue::Tag(text) => text.clone(),
        KernelValue::Bool(value) => value.to_string(),
        KernelValue::Skip => String::new(),
        KernelValue::Object(_) | KernelValue::List(_) => format!("{value:?}"),
    }
}

fn parse_canvas_click_payload(payload: Option<&str>) -> KernelValue {
    let Some(payload) = payload else {
        return KernelValue::Skip;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return KernelValue::Skip;
    };
    let Some(object) = value.as_object() else {
        return KernelValue::Skip;
    };
    let mut fields = BTreeMap::new();
    for (name, value) in object {
        if let Some(number) = value.as_f64() {
            fields.insert(name.clone(), KernelValue::from(number));
        }
    }
    if fields.is_empty() {
        KernelValue::Skip
    } else {
        KernelValue::Object(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::lower_program;
    use crate::text_input::KEYDOWN_TEXT_SEPARATOR;
    use boon_scene::{UiEvent, UiFact};

    #[test]
    fn shared_lowered_preview_renders_counter_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = lower_program(source).expect("lowered counter");
        let press_port = match &program {
            LoweredProgram::Counter(program) => program.press_port,
            _ => panic!("expected counter program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");
        assert_eq!(preview.preview_text(), "0+");

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(press_port)
                    .expect("counter press port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });

        assert_eq!(preview.preview_text(), "1+");
    }

    #[test]
    fn shared_lowered_preview_renders_complex_counter_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/complex_counter/complex_counter.bn"
        );
        let program = lower_program(source).expect("lowered complex_counter");
        let (decrement_port, increment_port) = match &program {
            LoweredProgram::ComplexCounter(program) => {
                (program.decrement_port, program.increment_port)
            }
            _ => panic!("expected complex_counter program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "-0+");

        let (root, _) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let stripe = &root.children[0];
        let decrement_id = stripe.children[0].id;
        let increment_id = stripe.children[2].id;

        for source_port in [increment_port, increment_port, decrement_port] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("complex_counter click port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert_eq!(preview.preview_text(), "-1+");

        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![UiFact {
                id: decrement_id,
                kind: UiFactKind::Hovered(true),
            }],
        });
        let (root, state) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let stripe = &root.children[0];
        assert_eq!(
            state.style_value(stripe.children[0].id, "background"),
            Some("oklch(0.85 0.07 320)")
        );
        assert_eq!(
            state.style_value(stripe.children[2].id, "background"),
            Some("oklch(0.75 0.07 320)")
        );

        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![
                UiFact {
                    id: decrement_id,
                    kind: UiFactKind::Hovered(false),
                },
                UiFact {
                    id: increment_id,
                    kind: UiFactKind::Hovered(true),
                },
            ],
        });
        let (root, state) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let stripe = &root.children[0];
        assert_eq!(
            state.style_value(stripe.children[0].id, "background"),
            Some("oklch(0.75 0.07 320)")
        );
        assert_eq!(
            state.style_value(stripe.children[2].id, "background"),
            Some("oklch(0.85 0.07 320)")
        );
    }

    #[test]
    fn shared_lowered_preview_renders_latest_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/latest/latest.bn");
        let program = lower_program(source).expect("lowered latest");
        let send_port = match &program {
            LoweredProgram::Latest(program) => program.send_press_ports[1],
            _ => panic!("expected latest program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");
        assert_eq!(preview.preview_text(), "Send 1Send 23Sum: 3");

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(send_port)
                    .expect("latest send port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });

        assert_eq!(preview.preview_text(), "Send 1Send 22Sum: 2");
    }

    #[test]
    fn shared_lowered_preview_renders_fibonacci_from_generic_lowered_entry() {
        let source =
            include_str!("../../../playground/frontend/src/examples/fibonacci/fibonacci.bn");
        let program = lower_program(source).expect("lowered fibonacci");
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "10. Fibonacci number is 55");
        assert!(!preview.dispatch_ui_events(UiEventBatch { events: vec![] }));
    }

    #[test]
    fn shared_lowered_preview_renders_layers_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/layers/layers.bn");
        let program = lower_program(source).expect("lowered layers");
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "Red CardGreen CardBlue Card");
        assert!(!preview.dispatch_ui_events(UiEventBatch { events: vec![] }));
    }

    fn tick_interval(preview: &mut LoweredPreview, tick_port: SourcePortId, interval_ms: u32) {
        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(tick_port)
                    .expect("interval tick port"),
                kind: UiEventKind::Custom(format!("timer:{interval_ms}")),
                payload: None,
            }],
        });
    }

    #[test]
    fn shared_lowered_preview_renders_interval_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/interval/interval.bn");
        let program = lower_program(source).expect("lowered interval");
        let (tick_port, interval_ms) = match &program {
            LoweredProgram::Interval(program) => (program.tick_port, program.interval_ms),
            _ => panic!("expected interval program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "");
        tick_interval(&mut preview, tick_port, interval_ms);
        assert_eq!(preview.preview_text(), "1");
        tick_interval(&mut preview, tick_port, interval_ms);
        tick_interval(&mut preview, tick_port, interval_ms);
        assert_eq!(preview.preview_text(), "3");
    }

    #[test]
    fn shared_lowered_preview_renders_interval_hold_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/interval_hold/interval_hold.bn"
        );
        let program = lower_program(source).expect("lowered interval_hold");
        let (tick_port, interval_ms) = match &program {
            LoweredProgram::IntervalHold(program) => (program.tick_port, program.interval_ms),
            _ => panic!("expected interval_hold program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "");
        tick_interval(&mut preview, tick_port, interval_ms);
        assert_eq!(preview.preview_text(), "1");
        tick_interval(&mut preview, tick_port, interval_ms);
        tick_interval(&mut preview, tick_port, interval_ms);
        assert_eq!(preview.preview_text(), "3");
    }

    #[test]
    fn shared_lowered_preview_renders_pages_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/pages/pages.bn");
        let program = lower_program(source).expect("lowered pages");
        let (about_port, contact_port) = match &program {
            LoweredProgram::Pages(program) => {
                (program.nav_press_ports[1], program.nav_press_ports[2])
            }
            _ => panic!("expected pages program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("Welcome Home"));
        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(about_port)
                    .expect("about port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("A multi-page Boon app"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(contact_port)
                    .expect("contact port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(
            preview
                .preview_text()
                .contains("URL-driven state and navigation demo.")
        );
    }

    #[test]
    fn shared_lowered_preview_renders_text_interpolation_update_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/text_interpolation_update/text_interpolation_update.bn"
        );
        let program = lower_program(source).expect("lowered text_interpolation_update");
        let toggle_port = match &program {
            LoweredProgram::TextInterpolationUpdate(program) => program.toggle_press_port,
            _ => panic!("expected text_interpolation_update program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Toggle (value: False)Label shows: FalseWHILE says: False"
        );
        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(toggle_port)
                    .expect("toggle port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(
            preview.preview_text(),
            "Toggle (value: True)Label shows: TrueWHILE says: True"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_button_hover_to_click_test_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/button_hover_to_click_test/button_hover_to_click_test.bn"
        );
        let program = lower_program(source).expect("lowered button_hover_to_click_test");
        let button_press_ports = match &program {
            LoweredProgram::ButtonHoverToClickTest(program) => program.button_press_ports,
            _ => panic!("expected button_hover_to_click_test program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Click each button - clicked ones turn darker with outlineButton AButton BButton CStates - A: False, B: False, C: False"
        );
        let _ = preview.render_snapshot();
        for source_port in [button_press_ports[0], button_press_ports[2]] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("button port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }
        assert_eq!(
            preview.preview_text(),
            "Click each button - clicked ones turn darker with outlineButton AButton BButton CStates - A: True, B: False, C: True"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_button_hover_test_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/button_hover_test/button_hover_test.bn"
        );
        let program = lower_program(source).expect("lowered button_hover_test");
        assert!(matches!(program, LoweredProgram::ButtonHoverTest(_)));
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Hover each button - only hovered one should show borderButton AButton BButton C"
        );

        let (root, _) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let button_row = &root.children[0].children[1];
        let button_ids = [
            button_row.children[0].id,
            button_row.children[1].id,
            button_row.children[2].id,
        ];

        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![UiFact {
                id: button_ids[1],
                kind: UiFactKind::Hovered(true),
            }],
        });

        let (root, state) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let button_row = &root.children[0].children[1];
        assert_eq!(
            state.style_value(button_row.children[0].id, "outline"),
            Some("none")
        );
        assert_eq!(
            state.style_value(button_row.children[1].id, "outline"),
            Some("2px solid oklch(0.6 0.2 250)")
        );
        assert_eq!(
            state.style_value(button_row.children[2].id, "outline"),
            Some("none")
        );
    }

    #[test]
    fn shared_lowered_preview_renders_filter_checkbox_bug_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/filter_checkbox_bug/filter_checkbox_bug.bn"
        );
        let program = lower_program(source).expect("lowered filter_checkbox_bug");
        let (filter_active_port, filter_all_port, checkbox_port) = match &program {
            LoweredProgram::FilterCheckboxBug(program) => (
                program.filter_active_port,
                program.filter_all_port,
                program.checkbox_ports[0],
            ),
            _ => panic!("expected filter_checkbox_bug program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Filter: AllAllActiveItem A (ALL) - checked: falseItem B (ALL) - checked: falseTest: Click Active, All, then checkbox 3x"
        );

        let _ = preview.render_snapshot();
        for source_port in [
            filter_active_port,
            filter_all_port,
            checkbox_port,
            checkbox_port,
            checkbox_port,
        ] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("filter_checkbox_bug port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert_eq!(
            preview.preview_text(),
            "Filter: AllAllActiveItem A (ALL) - checked: trueItem B (ALL) - checked: falseTest: Click Active, All, then checkbox 3x"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_checkbox_test_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/checkbox_test/checkbox_test.bn"
        );
        let program = lower_program(source).expect("lowered checkbox_test");
        let checkbox_ports = match &program {
            LoweredProgram::CheckboxTest(program) => program.checkbox_ports,
            _ => panic!("expected checkbox_test program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "Item A(unchecked)Item B(unchecked)");

        let _ = preview.render_snapshot();
        for source_port in checkbox_ports {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("checkbox_test port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert_eq!(preview.preview_text(), "Item A(checked)Item B(checked)");
    }

    #[test]
    fn shared_lowered_preview_renders_list_map_external_dep_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_map_external_dep/list_map_external_dep.bn"
        );
        let program = lower_program(source).expect("lowered list_map_external_dep");
        let toggle_port = match &program {
            LoweredProgram::ListMapExternalDep(program) => program.toggle_port,
            _ => panic!("expected list_map_external_dep program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "show_filtered: FalseToggle filterExpected: When True, show Apple and Cherry. When False, show all.AppleBananaCherryDate"
        );

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(toggle_port)
                    .expect("toggle port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });

        assert_eq!(
            preview.preview_text(),
            "show_filtered: TrueToggle filterExpected: When True, show Apple and Cherry. When False, show all.AppleCherry"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_list_map_block_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_map_block/list_map_block.bn"
        );
        let program = lower_program(source).expect("lowered list_map_block");
        assert!(matches!(program, LoweredProgram::ListMapBlock(_)));
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "Mode: All1234512345");
    }

    #[test]
    fn shared_lowered_preview_renders_list_retain_count_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
        );
        let program = lower_program(source).expect("lowered list_retain_count");
        let (input_change_port, input_key_down_port) = match &program {
            LoweredProgram::ListRetainCount(program) => {
                (program.input_change_port, program.input_key_down_port)
            }
            _ => panic!("expected list_retain_count program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "All count: 1Retain count: 1Initial");

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(input_change_port)
                    .expect("change port"),
                kind: UiEventKind::Change,
                payload: Some("Apple".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(input_key_down_port)
                    .expect("key port"),
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter\u{1F}{}", "Apple")),
            }],
        });

        assert_eq!(
            preview.preview_text(),
            "All count: 2Retain count: 2InitialApple"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_list_object_state_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
        );
        let program = lower_program(source).expect("lowered list_object_state");
        let button_press_ports = match &program {
            LoweredProgram::ListObjectState(program) => program.press_ports,
            _ => panic!("expected list_object_state program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Click each button - counts should be independentClick meCount: 0Click meCount: 0Click meCount: 0"
        );

        let _ = preview.render_snapshot();
        for source_port in [
            button_press_ports[0],
            button_press_ports[1],
            button_press_ports[1],
            button_press_ports[2],
        ] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("button port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert_eq!(
            preview.preview_text(),
            "Click each button - counts should be independentClick meCount: 1Click meCount: 2Click meCount: 1"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_chained_list_remove_bug_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/chained_list_remove_bug/chained_list_remove_bug.bn"
        );
        let program = lower_program(source).expect("lowered chained_list_remove_bug");
        let (checkbox_port, clear_port, add_port, remove_port) = match &program {
            LoweredProgram::ChainedListRemoveBug(program) => (
                program.checkbox_ports[0],
                program.clear_completed_port,
                program.add_press_port,
                program.remove_ports[1],
            ),
            _ => panic!("expected chained_list_remove_bug program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("Item A (id=0)"));
        assert!(preview.preview_text().contains("Item B (id=1)"));

        let _ = preview.render_snapshot();
        for source_port in [checkbox_port, clear_port, add_port, remove_port] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("chained_list_remove_bug port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert!(!preview.preview_text().contains("Item A (id=0)"));
        assert!(preview.preview_text().contains("Item B (id=1)"));
    }

    #[test]
    fn shared_lowered_preview_renders_crud_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let program = lower_program(source).expect("lowered crud");
        let (
            filter_change_port,
            name_change_port,
            surname_change_port,
            create_press_port,
            update_press_port,
            delete_press_port,
            row_press_port,
        ) = match &program {
            LoweredProgram::Crud(program) => (
                program.filter_change_port,
                program.name_change_port,
                program.surname_change_port,
                program.create_press_port,
                program.update_press_port,
                program.delete_press_port,
                program.row_press_ports[2],
            ),
            _ => panic!("expected crud program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("Emil, Hans"));
        assert!(preview.preview_text().contains("Mustermann, Max"));
        assert!(preview.preview_text().contains("Tansen, Roman"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(filter_change_port)
                    .expect("filter port"),
                kind: UiEventKind::Change,
                payload: Some("M".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("Mustermann, Max"));
        assert!(!preview.preview_text().contains("Emil, Hans"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(filter_change_port)
                        .expect("filter port"),
                    kind: UiEventKind::Change,
                    payload: Some(String::new()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(name_change_port)
                        .expect("name port"),
                    kind: UiEventKind::Change,
                    payload: Some("John".to_string()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(surname_change_port)
                        .expect("surname port"),
                    kind: UiEventKind::Change,
                    payload: Some("Doe".to_string()),
                },
            ],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(create_press_port)
                    .expect("create port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("Doe, John"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(row_press_port)
                    .expect("row port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("\u{25BA} Tansen, Roman"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(name_change_port)
                        .expect("name port"),
                    kind: UiEventKind::Change,
                    payload: Some("Rita".to_string()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(surname_change_port)
                        .expect("surname port"),
                    kind: UiEventKind::Change,
                    payload: Some("Tester".to_string()),
                },
            ],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(update_press_port)
                    .expect("update port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("\u{25BA} Tester, Rita"));
        assert!(!preview.preview_text().contains("Tansen, Roman"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(delete_press_port)
                    .expect("delete port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(!preview.preview_text().contains("Tester, Rita"));
        assert!(preview.preview_text().contains("Doe, John"));
    }

    #[test]
    fn shared_lowered_preview_renders_circle_drawer_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
        );
        let program = lower_program(source).expect("lowered circle_drawer");
        let (canvas_click_port, undo_press_port) = match &program {
            LoweredProgram::CircleDrawer(program) => {
                (program.canvas_click_port, program.undo_press_port)
            }
            _ => panic!("expected circle_drawer program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("Circle Drawer"));
        assert!(preview.preview_text().contains("Circles: 0"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(canvas_click_port)
                    .expect("canvas click port"),
                kind: UiEventKind::Click,
                payload: Some("{\"x\":120,\"y\":80}".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("Circles: 1"));
        let (root, _) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let canvas = &root.children[0].children[2];
        assert_eq!(canvas.children.len(), 1);
        assert_eq!(canvas.children[0].children.len(), 1);

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(undo_press_port)
                    .expect("undo press port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("Circles: 0"));
        let (root, _) = preview.render_snapshot();
        let RenderRoot::UiTree(root) = root else {
            panic!("expected ui tree render root");
        };
        let canvas = &root.children[0].children[2];
        assert_eq!(canvas.children.len(), 1);
        assert!(canvas.children[0].children.is_empty());
    }

    #[test]
    fn shared_lowered_preview_renders_list_retain_remove_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_retain_remove/list_retain_remove.bn"
        );
        let program = lower_program(source).expect("lowered list_retain_remove");
        let (input_change_port, input_key_down_port) = match &program {
            LoweredProgram::ListRetainRemove(program) => {
                (program.input_change_port, program.input_key_down_port)
            }
            _ => panic!("expected list_retain_remove program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Add items with EnterCount: 3- Apple- Banana- Cherry"
        );

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(input_change_port)
                    .expect("change port"),
                kind: UiEventKind::Change,
                payload: Some("  Orange  ".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(input_key_down_port)
                    .expect("key port"),
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1F}  Orange  ".to_string()),
            }],
        });

        assert_eq!(
            preview.preview_text(),
            "Add items with EnterCount: 4- Apple- Banana- Cherry- Orange"
        );
    }

    #[test]
    fn shared_lowered_preview_renders_shopping_list_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
        );
        let program = lower_program(source).expect("lowered shopping_list");
        let (input_change_port, input_key_down_port, clear_press_port) = match &program {
            LoweredProgram::ShoppingList(program) => (
                program.input_change_port,
                program.input_key_down_port,
                program.clear_press_port,
            ),
            _ => panic!("expected shopping_list program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("0 items"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(input_change_port)
                    .expect("change port"),
                kind: UiEventKind::Change,
                payload: Some("Milk".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(input_key_down_port)
                    .expect("key port"),
                kind: UiEventKind::KeyDown,
                payload: Some("Enter\u{1F}Milk".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("1 items"));
        assert!(preview.preview_text().contains("- Milk"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(clear_press_port)
                    .expect("clear port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("0 items"));
        assert!(!preview.preview_text().contains("- Milk"));
    }

    #[test]
    fn shared_lowered_preview_renders_temperature_converter_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
        );
        let program = lower_program(source).expect("lowered temperature_converter");
        let (celsius_change_port, fahrenheit_input_sink) = match &program {
            LoweredProgram::TemperatureConverter(program) => {
                (program.celsius_change_port, program.fahrenheit_input_sink)
            }
            _ => panic!("expected temperature_converter program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");
        assert!(preview.preview_text().contains("Temperature Converter"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(celsius_change_port)
                    .expect("celsius port"),
                kind: UiEventKind::Input,
                payload: Some("100".to_string()),
            }],
        });

        assert_eq!(
            preview.app().sink_value(fahrenheit_input_sink),
            Some(&KernelValue::from(212.0))
        );
    }

    #[test]
    fn shared_lowered_preview_renders_flight_booker_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
        );
        let program = lower_program(source).expect("lowered flight_booker");
        let (flight_type_change_port, book_press_port) = match &program {
            LoweredProgram::FlightBooker(program) => {
                (program.flight_type_change_port, program.book_press_port)
            }
            _ => panic!("expected flight_booker program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");
        assert!(preview.preview_text().contains("Flight Booker"));
        assert!(preview.preview_text().contains("One-way flight"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(book_press_port)
                    .expect("book port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(
            preview
                .preview_text()
                .contains("Booked one-way flight on 2026-03-03")
        );

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(flight_type_change_port)
                    .expect("flight type port"),
                kind: UiEventKind::Input,
                payload: Some("return".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(book_press_port)
                    .expect("book port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(
            preview
                .preview_text()
                .contains("Booked return flight: 2026-03-03 to 2026-03-03")
        );
    }

    #[test]
    fn shared_lowered_preview_renders_timer_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let program = lower_program(source).expect("lowered timer");
        let (tick_port, duration_change_port, reset_press_port) = match &program {
            LoweredProgram::Timer(program) => (
                program.tick_port,
                program.duration_change_port,
                program.reset_press_port,
            ),
            _ => panic!("expected timer program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("Timer"));
        assert!(preview.preview_text().contains("15s"));

        let _ = preview.render_snapshot();
        let tick_target = preview
            .app()
            .event_port_for_source(tick_port)
            .expect("tick port");
        for _ in 0..5 {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: tick_target,
                    kind: UiEventKind::Custom("timer:100".to_string()),
                    payload: None,
                }],
            });
        }
        assert!(preview.preview_text().contains("0.5s"));
        assert!(preview.preview_text().contains("3%"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(duration_change_port)
                    .expect("duration port"),
                kind: UiEventKind::Input,
                payload: Some("2".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("2s"));
        assert!(preview.preview_text().contains("25%"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(reset_press_port)
                    .expect("reset port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("0s"));
    }

    #[test]
    fn shared_lowered_preview_renders_todo_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let program = lower_program(source).expect("lowered todo_mvc");
        let (main_input_change_port, main_input_key_down_port) = match &program {
            LoweredProgram::TodoMvc(_) => (
                crate::lower::TodoProgram::MAIN_INPUT_CHANGE_PORT,
                crate::lower::TodoProgram::MAIN_INPUT_KEY_DOWN_PORT,
            ),
            _ => panic!("expected todo_mvc program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");
        assert!(preview.preview_text().contains("Buy groceries"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(main_input_change_port)
                        .expect("todo main input change port"),
                    kind: UiEventKind::Input,
                    payload: Some("Shared lowered todo".to_string()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(main_input_key_down_port)
                        .expect("todo main input keydown port"),
                    kind: UiEventKind::KeyDown,
                    payload: Some(format!("Enter{KEYDOWN_TEXT_SEPARATOR}Shared lowered todo")),
                },
            ],
        });

        assert!(preview.preview_text().contains("Shared lowered todo"));
    }

    #[test]
    fn shared_lowered_preview_renders_cells_from_generic_lowered_entry() {
        let source = include_str!("../../../playground/frontend/src/examples/cells/cells.bn");
        let program = lower_program(source).expect("lowered cells");
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert!(preview.preview_text().contains("Cells"));
        assert!(preview.preview_text().contains("15"));

        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(SourcePortId(10_101))
                    .expect("cells double-click port"),
                kind: UiEventKind::DoubleClick,
                payload: None,
            }],
        });
        let _ = preview.render_snapshot();
        preview.dispatch_ui_facts(UiFactBatch {
            facts: vec![boon_scene::UiFact {
                id: *preview
                    .app()
                    .retained_nodes()
                    .get(&crate::ir::RetainedNodeKey {
                        view_site: crate::ir::ViewSiteId(433),
                        function_instance: Some(crate::ir::FunctionInstanceId(11)),
                        mapped_item_identity: Some(1_001),
                    })
                    .expect("cells editing input retained node"),
                kind: UiFactKind::DraftText("11".to_string()),
            }],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(SourcePortId(201_012))
                    .expect("cells edit keydown port"),
                kind: UiEventKind::KeyDown,
                payload: Some(format!("Enter{KEYDOWN_TEXT_SEPARATOR}11")),
            }],
        });

        assert!(preview.preview_text().contains("11"));
        assert!(preview.preview_text().contains("21"));
    }

    #[test]
    fn shared_lowered_preview_renders_while_function_call_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/while_function_call/while_function_call.bn"
        );
        let program = lower_program(source).expect("lowered while_function_call");
        let toggle_port = match &program {
            LoweredProgram::WhileFunctionCall(program) => program.toggle_press_port,
            _ => panic!("expected while_function_call program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(preview.preview_text(), "Toggle (show: False)Hidden");
        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(toggle_port)
                    .expect("toggle port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert_eq!(preview.preview_text(), "Toggle (show: True)Hello, World!");
    }

    #[test]
    fn shared_lowered_preview_renders_switch_hold_test_from_generic_lowered_entry() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/switch_hold_test/switch_hold_test.bn"
        );
        let program = lower_program(source).expect("lowered switch_hold_test");
        let (toggle_press_port, item_press_ports) = match &program {
            LoweredProgram::SwitchHoldTest(program) => {
                (program.toggle_press_port, program.item_press_ports)
            }
            _ => panic!("expected switch_hold_test program"),
        };
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");

        assert_eq!(
            preview.preview_text(),
            "Showing: Item AToggle ViewItem A clicks: 0Click Item ATest: Click button, toggle view, click again. Counts should increment correctly."
        );

        let _ = preview.render_snapshot();
        for source_port in [item_press_ports[0], toggle_press_port, item_press_ports[1]] {
            preview.dispatch_ui_events(UiEventBatch {
                events: vec![UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(source_port)
                        .expect("switch_hold_test port"),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            });
        }

        assert_eq!(
            preview.preview_text(),
            "Showing: Item BToggle ViewItem B clicks: 1Click Item BTest: Click button, toggle view, click again. Counts should increment correctly."
        );
    }

    #[test]
    fn shared_lowered_preview_renders_static_document_from_generic_lowered_entry() {
        let source =
            include_str!("../../../playground/frontend/src/examples/hello_world/hello_world.bn");
        let program = lower_program(source).expect("lowered static document");
        let mut preview = LoweredPreview::from_program(program).expect("shared lowered preview");
        assert_eq!(preview.preview_text(), "Hello world!");
        assert!(!preview.dispatch_ui_events(UiEventBatch { events: vec![] }));
    }
}
