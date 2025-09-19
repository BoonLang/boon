// @TODO remove
#![allow(unused_variables)]

use boon::zoon::{eprintln, println, *};
use boon::zoon::map_ref;
use std::borrow::Cow;
use std::rc::Rc;

use boon::platform::browser::{bridge::object_with_document_to_element_signal, interpreter};

mod code_editor;
use code_editor::CodeEditor;

static SOURCE_CODE_STORAGE_KEY: &str = "boon-playground-source-code";

static OLD_SOURCE_CODE_STORAGE_KEY: &str = "boon-playground-old-source-code";
static OLD_SPAN_ID_PAIRS_STORAGE_KEY: &str = "boon-playground-span-id-pairs";
static STATES_STORAGE_KEY: &str = "boon-playground-states";
static PANEL_SPLIT_STORAGE_KEY: &str = "boon-playground-panel-split";

const DEFAULT_PANEL_SPLIT_RATIO: f64 = 0.5;
const MIN_PANEL_RATIO: f64 = 0.1;
const MAX_PANEL_RATIO: f64 = 0.9;
const MIN_EDITOR_WIDTH_PX: f64 = 260.0;
const MIN_PREVIEW_WIDTH_PX: f64 = 260.0;

#[derive(Clone, Copy)]
struct ExampleData {
    filename: &'static str,
    source_code: &'static str,
}

macro_rules! make_example_data {
    ($name:literal) => {{
        ExampleData {
            filename: concat!($name, ".bn"),
            source_code: include_str!(concat!("examples/", $name, "/", $name, ".bn")),
        }
    }};
}

static EXAMPLE_DATAS: [ExampleData; 4] = [
    make_example_data!("minimal"),
    make_example_data!("hello_world"),
    make_example_data!("interval"),
    make_example_data!("counter"),
];

#[derive(Clone, Copy)]
struct RunCommand {
    filename: Option<&'static str>,
}

fn main() {
    start_app("app", Playground::new);
}

#[derive(Clone)]
struct Playground {
    source_code: Mutable<Rc<Cow<'static, str>>>,
    run_command: Mutable<Option<RunCommand>>,
    snippet_screenshot_mode: Mutable<bool>,
    panel_split_ratio: Mutable<f64>,
    panel_container_width: Mutable<u32>,
    is_dragging_panel_split: Mutable<bool>,
    _store_source_code_task: Rc<TaskHandle>,
    _store_panel_split_task: Rc<TaskHandle>,
}

impl Playground {
    fn new() -> impl Element {
        let source_code =
            if let Some(Ok(source_code)) = local_storage().get(SOURCE_CODE_STORAGE_KEY) {
                Cow::Owned(source_code)
            } else {
                Cow::Borrowed(EXAMPLE_DATAS[0].source_code)
            };
        let source_code = Mutable::new(Rc::new(source_code));
        let panel_split_ratio_value =
            if let Some(Ok(ratio)) = local_storage().get(PANEL_SPLIT_STORAGE_KEY) {
                ratio
            } else {
                DEFAULT_PANEL_SPLIT_RATIO
            };
        let panel_split_ratio =
            Mutable::new(Self::clamp_panel_split_ratio(panel_split_ratio_value));
        let _store_source_code_task = Rc::new(Task::start_droppable(
            source_code.signal_cloned().for_each_sync(|source_code| {
                if let Err(error) = local_storage().insert(SOURCE_CODE_STORAGE_KEY, &source_code)
                {
                    eprintln!("Failed to store source code: {error:#?}");
                }
            }),
        ));
        let _store_panel_split_task = Rc::new(Task::start_droppable(
            panel_split_ratio
                .signal_cloned()
                .for_each_sync(|ratio| {
                    if let Err(error) = local_storage().insert(PANEL_SPLIT_STORAGE_KEY, &ratio) {
                        eprintln!("Failed to store panel split ratio: {error:#?}");
                    }
                }),
        ));
        Self {
            source_code,
            run_command: Mutable::new(None),
            snippet_screenshot_mode: Mutable::new(false),
            panel_split_ratio,
            panel_container_width: Mutable::new(0),
            is_dragging_panel_split: Mutable::new(false),
            _store_source_code_task,
            _store_panel_split_task,
        }
        .root()
    }

    fn root(&self) -> impl Element + use<> {
        Stack::new()
            .s(Width::fill())
            .s(Height::fill())
            .on_pointer_up({
                let this = self.clone();
                move || this.stop_panel_drag()
            })
            .layer(self.main_layout())
            .layer_signal(self.is_dragging_panel_split.signal().map_bool(
                {
                    let this = self.clone();
                    move || Some(this.panel_drag_overlay())
                },
                || None,
            ))
    }

    fn main_layout(&self) -> impl Element + use<> {
        Column::new()
            .s(Width::fill())
            .s(Height::fill())
            .s(Font::new().color(color!("oklch(0.8 0 0)")))
            .s(Scrollbars::both())
            .item(
                Row::new()
                    .item(
                        Row::new().s(Gap::new().x(20)).multiline().items(
                            EXAMPLE_DATAS.map(|example_data| self.example_button(example_data)),
                        ),
                    )
                    .item(self.clear_saved_states_button()),
            )
            .item(
                Row::new()
                    .s(Gap::both(20))
                    .s(Align::new().center_x())
                    .item(self.run_button())
                    .item(self.snippet_screenshot_mode_button()),
            )
            .item(self.panels_row())
    }

    fn panels_row(&self) -> impl Element + use<> {
        Row::new()
            .s(Padding::new().top(5))
            .s(Width::fill())
            .s(Height::fill())
            .s(Scrollbars::both())
            .update_raw_el(|raw_el| raw_el.class("panels-row"))
            .on_viewport_size_change({
                let panel_container_width = self.panel_container_width.clone();
                let panel_split_ratio = self.panel_split_ratio.clone();
                move |width, _| {
                    panel_container_width.set_neq(width);
                    let current_ratio = *panel_split_ratio.lock_ref();
                    let clamped = Self::clamp_panel_split_ratio_for_width(current_ratio, width);
                    panel_split_ratio.set_neq(clamped);
                }
            })
            .item(self.code_editor_panel_container())
            .item_signal(self.snippet_screenshot_mode.signal().map_bool(
                || None,
                {
                    let this = self.clone();
                    move || Some(this.panel_divider())
                },
            ))
            .item_signal(self.snippet_screenshot_mode.signal().map_bool(
                || None,
                {
                    let this = self.clone();
                    move || Some(this.example_panel_container())
                },
            ))
    }

    fn code_editor_panel_container(&self) -> impl Element + use<> {
        El::new()
            .s(Align::new().top())
            .s(Height::fill())
            .s(Width::with_signal_self(map_ref! {
                let snippet = self.snippet_screenshot_mode.signal(),
                let ratio = self.panel_split_ratio.signal() =>
                if *snippet {
                    Some(Width::fill())
                } else {
                    Some(Width::percent((ratio * 100.0).clamp(0.0, 100.0)))
                }
            }))
            .child(self.code_editor_panel())
    }

    fn panel_divider(&self) -> impl Element + use<> {
        let hovered = Mutable::new(false);
        let hovered_for_signal = hovered.clone();
        El::new()
            .s(Align::new().top())
            .s(Width::exact(10))
            .s(Height::fill())
            .s(Cursor::new(CursorIcon::ColumnResize))
            .s(Background::new().color_signal(map_ref! {
                let hovered = hovered_for_signal.signal(),
                let dragging = self.is_dragging_panel_split.signal() =>
                if *dragging {
                    color!("#4c566a")
                } else if *hovered {
                    color!("#3b404a")
                } else {
                    color!("#2c3038")
                }
            }))
            .s(Borders::new()
                .left(Border::new().color(color!("#1d2026")).width(1))
                .right(Border::new().color(color!("#1d2026")).width(1)))
            .on_hovered_change(move |is_hovered| hovered.set_neq(is_hovered))
            .text_content_selecting(TextContentSelecting::none())
            .on_pointer_down_event({
                let this = self.clone();
                move |event| this.start_panel_drag(event)
            })
    }

    fn example_panel_container(&self) -> impl Element + use<> {
        El::new()
            .s(Align::new().top())
            .s(Height::fill())
            .s(Width::percent_signal::<f64>(
                self.panel_split_ratio
                    .signal_cloned()
                    .map(|ratio| Some((1.0 - ratio).clamp(0.0, 1.0) * 100.0)),
            ))
            .child(self.example_panel())
    }

    fn panel_drag_overlay(&self) -> impl Element + use<> {
        El::new()
            .s(Align::new().top())
            .s(Width::fill())
            .s(Height::fill())
            .s(Cursor::new(CursorIcon::ColumnResize))
            .s(Background::new().color(color!("rgba(0, 0, 0, 0)")))
            .text_content_selecting(TextContentSelecting::none())
            .on_pointer_move_event({
                let this = self.clone();
                move |event| this.adjust_panel_split(&event)
            })
            .on_pointer_up({
                let this = self.clone();
                move || this.stop_panel_drag()
            })
            .on_pointer_leave({
                let this = self.clone();
                move || this.stop_panel_drag()
            })
    }

    fn start_panel_drag(&self, pointer_event: PointerEvent) {
        if !*self.is_dragging_panel_split.lock_ref() {
            if let RawPointerEvent::PointerDown(raw_event) = &pointer_event.raw_event {
                raw_event.prevent_default();
                if let Some(target) = raw_event.dyn_target::<web_sys::Element>() {
                    if let Ok(Some(container)) = target.closest(".panels-row") {
                        let width = container.get_bounding_client_rect().width();
                        if width.is_finite() && width > 0.0 {
                            self.panel_container_width
                                .set_neq(width.round().max(1.0) as u32);
                        }
                    }
                }
            }
        }
        if *self.is_dragging_panel_split.lock_ref() {
            return;
        }
        self.is_dragging_panel_split.set_neq(true);
    }

    fn stop_panel_drag(&self) {
        if !*self.is_dragging_panel_split.lock_ref() {
            return;
        }
        self.is_dragging_panel_split.set_neq(false);
        let width = *self.panel_container_width.lock_ref();
        if width > 0 {
            let current_ratio = *self.panel_split_ratio.lock_ref();
            let clamped = Self::clamp_panel_split_ratio_for_width(current_ratio, width);
            self.panel_split_ratio.set_neq(clamped);
        }
    }

    fn adjust_panel_split(&self, pointer_event: &PointerEvent) {
        if !*self.is_dragging_panel_split.lock_ref() {
            return;
        }
        let delta_x = pointer_event.movement_x();
        if delta_x == 0 {
            return;
        }
        let width = *self.panel_container_width.lock_ref();
        if width == 0 {
            return;
        }
        let current_ratio = *self.panel_split_ratio.lock_ref();
        let ratio_delta = f64::from(delta_x) / width as f64;
        if ratio_delta == 0.0 {
            return;
        }
        let new_ratio = Self::clamp_panel_split_ratio_for_width(current_ratio + ratio_delta, width);
        self.panel_split_ratio.set_neq(new_ratio);
    }

    fn clamp_panel_split_ratio(ratio: f64) -> f64 {
        ratio.clamp(MIN_PANEL_RATIO, MAX_PANEL_RATIO)
    }

    fn clamp_panel_split_ratio_for_width(ratio: f64, width: u32) -> f64 {
        if width == 0 {
            return Self::clamp_panel_split_ratio(ratio);
        }
        let width = width as f64;
        if width <= (MIN_EDITOR_WIDTH_PX + MIN_PREVIEW_WIDTH_PX) {
            return Self::clamp_panel_split_ratio(ratio);
        }
        let min_ratio = (MIN_EDITOR_WIDTH_PX / width).max(MIN_PANEL_RATIO);
        let max_ratio = (1.0 - (MIN_PREVIEW_WIDTH_PX / width)).min(MAX_PANEL_RATIO);
        if min_ratio > max_ratio {
            return Self::clamp_panel_split_ratio(ratio);
        }
        ratio.clamp(min_ratio, max_ratio)
    }

    fn run_button(&self) -> impl Element {
        let (hovered, hovered_signal) = Mutable::new_and_signal(false);
        Button::new()
            .s(Padding::all(5))
            .label(
                Paragraph::new()
                    .s(Font::new().color_signal(
                        hovered_signal
                            .map_bool(|| color!("MediumSpringGreen"), || color!("LimeGreen")),
                    ))
                    .content("Run (")
                    .content(
                        El::new()
                            .s(Font::new().weight(FontWeight::Bold))
                            .child("Shift + Enter"),
                    )
                    .content(" in editor)"),
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press({
                let run_command = self.run_command.clone();
                move || {
                    run_command.set(Some(RunCommand { filename: None }));
                }
            })
    }

    fn snippet_screenshot_mode_button(&self) -> impl Element {
        let (hovered, hovered_signal) = Mutable::new_and_signal(false);
        Button::new()
            .s(Padding::all(5))
            .label(
                El::new()
                    .s(Font::new().color_signal(
                        hovered_signal
                            .map_bool(|| color!("DarkGrey"), || color!("Grey")),
                    ))
                    .child("Snippet screenshot mode")
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press({
                let snippet_screenshot_mode = self.snippet_screenshot_mode.clone();
                move || {
                    snippet_screenshot_mode.update(|mode| not(mode));
                }
            })
    }

    fn clear_saved_states_button(&self) -> impl Element {
        let (hovered, hovered_signal) = Mutable::new_and_signal(false);
        Button::new()
            .s(Padding::new().x(10).y(5))
            .s(Font::new()
                .color_signal(hovered_signal.map_bool(|| color!("Coral"), || color!("LightCoral"))))
            .label("Clear saved states")
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press(|| {
                local_storage().remove(STATES_STORAGE_KEY);
                local_storage().remove(OLD_SOURCE_CODE_STORAGE_KEY);
                local_storage().remove(OLD_SPAN_ID_PAIRS_STORAGE_KEY);
            })
    }

    fn code_editor_panel(&self) -> impl Element {
        El::new()
            .s(Align::new().top())
            .s(Width::fill())
            .s(Height::fill())
            .s(Padding::all_signal(self.snippet_screenshot_mode.signal().map_bool(|| 100, || 5)))
            .s(Scrollbars::both())
            .child(
                CodeEditor::new()
                    .s(RoundedCorners::all(10))
                    .s(Scrollbars::both())
                    .on_key_down_event_with_options(
                        EventOptions::new().preventable().parents_first(),
                        {
                            let run_command = self.run_command.clone();
                            move |keyboard_event| {
                                let RawKeyboardEvent::KeyDown(raw_event) =
                                    &keyboard_event.raw_event;
                                if keyboard_event.key() == &Key::Enter && raw_event.shift_key() {
                                    keyboard_event.pass_to_parent(false);
                                    raw_event.prevent_default();
                                    run_command.set(Some(RunCommand { filename: None }));
                                }
                            }
                        },
                    )
                    .content_signal(self.source_code.signal_cloned())
                    .snippet_screenshot_mode_signal(self.snippet_screenshot_mode.signal())
                    .on_change({
                        let source_code = self.source_code.clone();
                        move |content| source_code.set_neq(Rc::new(Cow::from(content)))
                    }),
            )
    }

    fn example_panel(&self) -> impl Element + use<> {
        El::new()
            .s(Align::new().top())
            .s(Width::fill())
            .s(Height::fill())
            .s(Padding::all(5))
            .child(
                El::new()
                    .s(RoundedCorners::all(10))
                    .s(Clip::both())
                    .s(Borders::all(
                        Border::new().color(color!("#282c34")).width(4),
                    ))
                    .child_signal(self.run_command.signal().map_some({
                        let this = self.clone();
                        move |run_command| this.example_runner(run_command)
                    })),
            )
    }

    fn example_runner(&self, run_command: RunCommand) -> impl Element + use<> {
        println!("Command to run example received!");
        let filename = run_command.filename.unwrap_or("custom code");
        let source_code = self.source_code.lock_ref();
        let object_and_construct_context = interpreter::run(
            filename,
            &source_code,
            STATES_STORAGE_KEY,
            OLD_SOURCE_CODE_STORAGE_KEY,
            OLD_SPAN_ID_PAIRS_STORAGE_KEY,
        );
        drop(source_code);
        if let Some((object, construct_context)) = object_and_construct_context {
            El::new()
                .child_signal(object_with_document_to_element_signal(
                    object.clone(),
                    construct_context,
                ))
                .after_remove(move |_| drop(object))
                .unify()
        } else {
            El::new()
                .s(Font::new().color(color!("LightCoral")))
                .child("Failed to run the example. See errors in dev console.")
                .unify()
        }
    }

    fn example_button(&self, example_data: ExampleData) -> impl Element {
        Button::new()
            .s(Padding::new().x(10).y(5))
            .s(Font::new().line(FontLine::new().underline().offset(3)))
            .label(example_data.filename)
            .on_press({
                let source_code = self.source_code.clone();
                let run_command = self.run_command.clone();
                move || {
                    source_code.set_neq(Rc::new(Cow::from(example_data.source_code)));
                    run_command.set(Some(RunCommand {
                        filename: Some(example_data.filename),
                    }));
                }
            })
    }
}
