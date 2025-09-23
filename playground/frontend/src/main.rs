// @TODO remove
#![allow(unused_variables)]

use boon::zoon::{eprintln, println, *};
use boon::zoon::{map_ref, Rgba};
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
const PANEL_DIVIDER_WIDTH: f64 = 10.0;

const APP_BACKGROUND_GRADIENT: &str =
    "linear-gradient(155deg, #231746 0%, #141f33 48%, #0d323f 100%)";

fn shell_surface_color() -> Rgba {
    color!("rgba(13, 18, 30, 0.76)")
}

fn primary_surface_color() -> Rgba {
    color!("rgba(21, 27, 44, 0.92)")
}

fn primary_text_color() -> Rgba {
    color!("#f1f4ff")
}

fn muted_text_color() -> Rgba {
    color!("rgba(226, 232, 255, 0.7)")
}

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
            .layer(
                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .update_raw_el(|raw_el| raw_el.style("background", APP_BACKGROUND_GRADIENT)),
            )
            .update_raw_el({
                let run_command = self.run_command.clone();
                move |raw_el| {
                    let run_command = run_command.clone();
                    raw_el.global_event_handler_with_options(
                        EventOptions::new().preventable().parents_first(),
                        move |event: events::KeyDown| {
                            if event.repeat() {
                                return;
                            }
                            if event.shift_key() && event.key() == "Enter" {
                                event.prevent_default();
                                run_command.set(Some(RunCommand { filename: None }));
                            }
                        },
                    )
                }
            })
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
            .s(Padding::new().x(6).top(8).bottom(10))
            .s(Gap::new().y(8))
            .s(Font::new().color(primary_text_color()))
            .s(Scrollbars::both())
            .item_signal(self.snippet_screenshot_mode.signal().map({
                let this = self.clone();
                move |enabled| if enabled {
                    None
                } else {
                    Some(this.header_bar())
                }
            }))
            .item(
                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .s(Scrollbars::both())
                    .child(self.shell_surface(
                        Column::new()
                            .s(Width::fill())
                            .s(Height::fill())
                            .s(Scrollbars::both())
                            .s(Gap::new().y(8))
                            .item(self.controls_row())
                            .item(self.panels_row()),
                    )),
            )
    }

    fn shell_surface<T: Element>(&self, content: T) -> impl Element + use<T> {
        El::new()
            .s(Width::fill())
            .s(Height::fill())
            .s(Scrollbars::both())
            .s(Background::new().color(shell_surface_color()))
            .s(
                RoundedCorners::new()
                    .top(32)
                    .bottom_signal(
                        self.snippet_screenshot_mode
                            .signal()
                            .map_bool(|| 0, || 32),
                    ),
            )
            .s(Borders::all(
                Border::new().color(color!("rgba(255, 255, 255, 0.05)")).width(1),
            ))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(5, 10, 18, 0.55)"))
                    .y(34)
                    .blur(60)
                    .spread(-18),
            ]))
            .update_raw_el(|raw_el| raw_el.style("backdrop-filter", "blur(24px)"))
            .child(
                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .s(Scrollbars::both())
                    .s(Padding::new().x(10).y(10))
                    .child(content),
            )
    }

    fn header_bar(&self) -> impl Element + use<> {
        El::new()
            .s(Width::fill())
            .s(Background::new().color(shell_surface_color()))
            .s(RoundedCorners::all(28))
            .s(Padding::new().x(18).y(12))
            .s(Borders::all(
                Border::new().color(color!("rgba(255, 255, 255, 0.06)")).width(1),
            ))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(5, 10, 20, 0.45)"))
                    .y(26)
                    .blur(48)
                    .spread(-12),
            ]))
            .update_raw_el(|raw_el| raw_el.style("backdrop-filter", "blur(24px)"))
            .child(
                Row::new()
                    .s(Width::fill())
                    .s(Align::new().center_y())
                    .s(Gap::new().x(12))
                    .item(
                        Paragraph::new()
                            .s(Font::new().size(18).weight(FontWeight::SemiBold).no_wrap())
                            .content("Boon Playground"),
                    )
                    .item(self.example_tabs()),
            )
    }

    fn example_tabs(&self) -> impl Element + use<> {
        Row::new()
            .s(Width::fill())
            .s(Align::new().center_y())
            .s(Gap::new().x(10).y(6))
            .multiline()
            .items(EXAMPLE_DATAS.map(|example_data| self.example_button(example_data)))
    }

    fn controls_row(&self) -> impl Element + use<> {
        Row::new()
            .s(Width::fill())
            .s(Align::new().center_y())
            .s(Gap::new().x(12).y(8))
            .multiline()
            .item(El::new().s(Align::new().left()).child(self.snippet_screenshot_mode_button()))
            .item(El::new().s(Align::new().center_x()).child(self.run_button()))
            .item(El::new().s(Align::new().right()).child(self.clear_saved_states_button()))
    }

    fn primary_panel<T: Element>(&self, content: T) -> impl Element + use<T> {
        El::new()
            .s(Width::fill())
            .s(Height::fill())
            .s(Scrollbars::both())
            .s(Background::new().color(primary_surface_color()))
            .s(RoundedCorners::all(24))
            .s(Borders::all(
                Border::new().color(color!("rgba(255, 255, 255, 0.05)")).width(1),
            ))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(4, 12, 24, 0.32)"))
                    .y(30)
                    .blur(60)
                    .spread(-18),
            ]))
            .update_raw_el(|raw_el| raw_el.style("backdrop-filter", "blur(20px)"))
            .child(content)
    }

    fn panels_row(&self) -> impl Element + use<> {
        Row::new()
            .s(Width::fill())
            .s(Height::fill())
            .s(Align::new().top())
            .s(Scrollbars::both())
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
            .s(Padding::new().right_signal(
                self.snippet_screenshot_mode
                    .signal()
                    .map_bool(|| 0, || 6),
            ))
            .s(Width::with_signal_self(map_ref! {
                let snippet = self.snippet_screenshot_mode.signal(),
                let ratio = self.panel_split_ratio.signal(),
                let container = self.panel_container_width.signal() =>
                if *snippet {
                    Some(Width::fill())
                } else {
                    let container_width = *container as f64;
                    if container_width > PANEL_DIVIDER_WIDTH {
                        let available = container_width - PANEL_DIVIDER_WIDTH;
                        let desired = (available * ratio).clamp(
                            MIN_EDITOR_WIDTH_PX,
                            available - MIN_PREVIEW_WIDTH_PX,
                        );
                        Some(Width::exact(desired.max(0.0) as u32))
                    } else {
                        Some(Width::percent((ratio * 100.0).clamp(0.0, 100.0)))
                    }
                }
            }))
            .child_signal(self.snippet_screenshot_mode.signal().map({
                let this = self.clone();
                move |snippet| {
                    let playground = this.clone();
                    if snippet {
                        Some(Either::Left(playground.snippet_screenshot_surface()))
                    } else {
                        Some(Either::Right(playground.code_editor_panel()))
                    }
                }
            }))
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
                    color!("rgba(140, 196, 255, 0.85)")
                } else if *hovered {
                    color!("rgba(108, 162, 255, 0.75)")
                } else {
                    color!("rgba(72, 108, 176, 0.6)")
                }
            }))
            .s(RoundedCorners::all(18))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(8, 14, 30, 0.55)"))
                    .y(12)
                    .blur(24)
                    .spread(-8),
            ]))
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
            .s(Padding::new().left_signal(
                self.snippet_screenshot_mode
                    .signal()
                    .map_bool(|| 0, || 6),
            ))
            .s(Width::with_signal_self(map_ref! {
                let snippet = self.snippet_screenshot_mode.signal(),
                let ratio = self.panel_split_ratio.signal(),
                let container = self.panel_container_width.signal() =>
                if *snippet {
                    Some(Width::fill())
                } else {
                    let container_width = *container as f64;
                    if container_width > PANEL_DIVIDER_WIDTH {
                        let available = container_width - PANEL_DIVIDER_WIDTH;
                        let editor = (available * ratio).clamp(
                            MIN_EDITOR_WIDTH_PX,
                            available - MIN_PREVIEW_WIDTH_PX,
                        );
                        let preview = (available - editor).max(0.0);
                        Some(Width::exact(preview as u32))
                    } else {
                        Some(Width::percent(((1.0 - ratio) * 100.0).clamp(0.0, 100.0)))
                    }
                }
            }))
            .child(self.primary_panel(self.example_panel()))
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
        let hovered = Mutable::new(false);
        Button::new()
            .s(Padding::new().x(14).y(7))
            .s(RoundedCorners::all(22))
            .s(Font::new().color(color!("#052039")))
            .s(Font::new().weight(FontWeight::SemiBold))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(15, 23, 42, 0.22)"))
                    .y(12)
                    .blur(22)
                    .spread(-8),
            ]))
            .s(Background::new().color_signal(
                hovered
                    .signal()
                    .map_bool(
                        || color!("rgba(140, 196, 255, 0.9)"),
                        || color!("rgba(108, 162, 255, 0.75)"),
                    ),
            ))
            .label(
                Row::new()
                    .s(Align::new().center_y())
                    .s(Gap::new().x(6))
                    .item(
                        El::new()
                            .s(Font::new().size(14).weight(FontWeight::SemiBold).no_wrap())
                            .child("Run"),
                    )
                    .item(
                        Column::new()
                            .s(Gap::new().y(2))
                            .item(
                                El::new()
                                    .s(Font::new().size(13).color(color!("rgba(5, 32, 57, 0.78)")).no_wrap())
                                    .child("Shift + Enter"),
                            ),
                    ),
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
        let hovered = Mutable::new(false);
        Button::new()
            .s(Padding::new().x(12).y(7))
            .s(RoundedCorners::all(22))
            .s(Font::new().size(13).color(primary_text_color()))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(8, 13, 28, 0.26)"))
                    .y(12)
                    .blur(22)
                    .spread(-8),
            ]))
            .s(Background::new().color_signal(map_ref! {
                let hovered = hovered.signal(),
                let active = self.snippet_screenshot_mode.signal() =>
                match (*active, *hovered) {
                    (true, true) => color!("rgba(70, 104, 178, 0.6)"),
                    (true, false) => color!("rgba(60, 94, 168, 0.52)"),
                    (false, true) => color!("rgba(36, 48, 72, 0.44)"),
                    (false, false) => color!("rgba(26, 36, 58, 0.32)"),
                }
            }))
            .label(
                Row::new()
                    .s(Align::new().center_y())
                    .s(Gap::new().x(6))
                    .item(
                        El::new()
                            .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
                            .child("Screenshot mode"),
                    )
                    .item(
                        El::new()
                            .s(Padding::new().x(9).y(3))
                            .s(RoundedCorners::all(999))
                            .s(Font::new().size(11).weight(FontWeight::SemiBold).no_wrap())
                            .s(Background::new().color_signal(map_ref! {
                                let active = self.snippet_screenshot_mode.signal() =>
                                if *active {
                                    color!("rgba(0, 0, 0, 0.18)")
                                } else {
                                    color!("rgba(0, 0, 0, 0.12)")
                                }
                            }))
                            .child_signal(self.snippet_screenshot_mode.signal().map_bool(|| "ON", || "OFF")),
                    ),
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
        let hovered = Mutable::new(false);
        Button::new()
            .s(Padding::new().x(12).y(7))
            .s(RoundedCorners::all(22))
            .s(Borders::all(
                Border::new()
                    .color(color!("rgba(255, 134, 134, 0.45)"))
                    .width(1),
            ))
            .s(Background::new().color_signal(
                hovered
                    .signal()
                    .map_bool(|| color!("rgba(255, 134, 134, 0.12)"), || color!("rgba(255, 134, 134, 0.08)")),
            ))
            .s(Font::new()
                .size(13)
                .weight(FontWeight::Medium)
                .color_signal(hovered.signal().map_bool(
                    || color!("rgba(255, 199, 199, 0.95)"),
                    || color!("rgba(255, 210, 210, 0.85)"),
                )))
            .label(
                El::new()
                    .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
                    .child("Clear saved states"),
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press(|| {
                local_storage().remove(STATES_STORAGE_KEY);
                local_storage().remove(OLD_SOURCE_CODE_STORAGE_KEY);
                local_storage().remove(OLD_SPAN_ID_PAIRS_STORAGE_KEY);
            })
    }

    fn code_editor_panel(&self) -> impl Element + use<> {
        self.primary_panel(self.editor_panel_content())
    }

    fn standard_code_editor_surface(&self) -> impl Element + use<> {
        Stack::new()
            .s(Align::new().top())
            .s(Width::fill())
            .s(Height::fill())
            .layer(
                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .s(RoundedCorners::all(24))
                    .s(Background::new().color(color!("#101a2c")))
                    .s(Shadows::new([
                        Shadow::new()
                            .color(color!("rgba(10, 16, 32, 0.45)"))
                            .y(26)
                            .blur(52)
                            .spread(-20),
                    ])),
            )
            .layer(
                El::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .s(Padding::all(10))
                    .child(
                        self.code_editor_widget()
                            .s(RoundedCorners::all(20))
                            .s(Clip::both())
                            .s(Shadows::new([
                                Shadow::new()
                                    .color(color!("rgba(8, 10, 18, 0.45)"))
                                    .y(18)
                                    .blur(36)
                                    .spread(-12),
                            ]))
                            .s(Background::new().color(color!("#0b1223")))
                            .update_raw_el(|raw_el| {
                                raw_el.style(
                                    "background",
                                    "linear-gradient(120deg, rgba(24,32,52,0.24) 0%, rgba(8,10,18,0.88) 65%)",
                                )
                            }),
                    ),
            )
    }

    fn editor_panel_content(&self) -> impl Element + use<> {
        Column::new()
            .s(Align::new().top())
            .s(Width::fill())
            .s(Height::fill())
            .s(Gap::new().y(8))
            .item(self.standard_code_editor_surface())
    }

    fn snippet_screenshot_surface(&self) -> impl Element + use<> {
        Stack::new()
            .s(Width::fill())
            .s(Height::fill())
            .s(Align::new().center_x().top())
            .s(Padding::new().left(48).right(48).top(64).bottom(64))
            .update_raw_el(|raw_el| {
                raw_el.style(
                    "background",
                    "radial-gradient(120% 120% at 10% 0%, rgba(255,255,255,0.25) 0%, rgba(255,255,255,0) 40%), linear-gradient(135deg, #7c3aed 0%, #4f46e5 40%, #0ea5e9 80%, #14b8a6 100%)",
                )
            })
            .layer(
                Stack::new()
                    .s(Align::center())
                    .s(Width::fill().max(960))
                    .s(Height::fill())
                    .s(Background::new().color(color!("rgba(11, 18, 35, 0.78)")))
                    .s(Shadows::new([
                        Shadow::new().color(color!("rgba(12, 16, 35, 0.55)")).y(40).blur(70),
                        Shadow::new().color(color!("rgba(91, 33, 182, 0.35)")).y(18).blur(36),
                    ]))
                    .s(RoundedCorners::all(28))
                    .s(Clip::both())
                    .s(Borders::all(
                        Border::new().color(color!("rgba(255, 255, 255, 0.05)")).width(1),
                    ))
                    .update_raw_el(|raw_el| raw_el.style("backdrop-filter", "blur(28px)"))
                    .layer(
                        Column::new()
                            .s(Width::fill())
                            .s(Height::fill())
                            .item(self.snippet_window_header())
                            .item(
                                Stack::new()
                                    .s(Width::fill())
                                    .s(Height::fill())
                                    .s(Background::new().color(color!("#101a2c")))
                                    .layer(
                                        El::new()
                                            .s(Width::fill())
                                            .s(Height::fill())
                                            .pointer_handling(PointerHandling::none())
                                            .update_raw_el(|raw_el| {
                                                raw_el.style(
                                                    "background",
                                                    "radial-gradient(140% 140% at 20% 10%, rgba(48,72,112,0.18) 0%, rgba(16,26,44,0.0) 65%)",
                                                )
                                            })
                                    )
                                    .layer(
                                        El::new()
                                            .s(Width::fill())
                                            .s(Height::fill())
                                            .s(Padding::new().x(28).y(24))
                                            .s(Scrollbars::both())
                                            .child(
                                                self.code_editor_widget()
                                                    .s(RoundedCorners::all(20))
                                                    .s(Clip::both())
                                                    .s(Height::fill())
                                                    .s(Scrollbars::both())
                                                    .update_raw_el(|raw_el| {
                                                        raw_el.style(
                                                            "background",
                                                            "linear-gradient(120deg, rgba(24,32,52,0.28) 0%, rgba(8,10,18,0.92) 65%)",
                                                        )
                                                    })
                                                    .s(Background::new().color(color!("#0b1223")))
                                                    .s(Shadows::new([
                                                        Shadow::new()
                                                            .color(color!("rgba(0, 0, 0, 0.25)"))
                                                            .y(22)
                                                            .blur(46),
                                                    ]))
                                            ),
                                    )
                                    .layer(
                                        El::new()
                                            .s(Width::fill())
                                            .s(Height::fill())
                                            .pointer_handling(PointerHandling::none())
                                            .s(Background::new().color(color!("rgba(0, 0, 0, 0.18)")))
                                    )
                            ),
                    ),
            )
    }

    fn snippet_window_header(&self) -> impl Element {
        Stack::new()
            .s(Width::fill())
            .s(Background::new().color(color!("rgba(20, 26, 43, 0.9)")))
            .s(Padding::new().x(20).y(12))
            .s(Borders::new().bottom(
                Border::new().color(color!("rgba(255, 255, 255, 0.06)")).width(1),
            ))
            .layer(
                Row::new()
                    .s(Gap::new().x(12))
                    .items([
                        color!("#3b6dac"),
                        color!("#c9962d"),
                        color!("#3fa869"),
                    ]
                    .into_iter()
                    .map(|color| self.snippet_window_control(color)))
            )
    }

    fn snippet_window_control(&self, color: impl IntoColor) -> impl Element {
        let color = color.into_color();
        El::new()
            .s(Width::exact(14))
            .s(Height::exact(14))
            .s(RoundedCorners::all_max())
            .s(Background::new().color(color))
            .s(Shadows::new([Shadow::new().color(color!("rgba(0,0,0,0.25)")).y(1).blur(2)]))
    }

    fn code_editor_widget(&self) -> CodeEditor {
        CodeEditor::new()
            .s(Width::fill())
            .s(Height::fill())
            .content_signal(self.source_code.signal_cloned())
            .snippet_screenshot_mode_signal(self.snippet_screenshot_mode.signal())
            .on_change({
                let source_code = self.source_code.clone();
                move |content| source_code.set_neq(Rc::new(Cow::from(content)))
            })
    }

    fn example_panel(&self) -> impl Element + use<> {
        Column::new()
            .s(Align::new().top())
            .s(Width::fill())
            .s(Height::fill())
            .s(Gap::new().y(8))
            .item(
                Stack::new()
                    .s(Width::fill())
                    .s(Height::fill())
                    .s(RoundedCorners::all(24))
                    .s(Clip::both())
                    .layer(
                        El::new()
                            .s(Width::fill())
                            .s(Height::fill())
                            .update_raw_el(|raw_el| {
                                raw_el.style(
                                    "background",
                                    "radial-gradient(120% 120% at 84% 0%, rgba(76, 214, 255, 0.16) 0%, rgba(5, 9, 18, 0.0) 55%), linear-gradient(165deg, rgba(9, 13, 24, 0.94) 15%, rgba(5, 8, 14, 0.96) 85%)",
                                )
                            }),
                    )
                    .layer(
                        El::new()
                            .s(Width::fill())
                            .s(Height::fill())
                            .s(Padding::new().x(12).y(12))
                            .s(Scrollbars::both())
                            .child_signal(self.run_command.signal().map({
                                let this = self.clone();
                                move |maybe_run| Some(match maybe_run {
                                    Some(run_command) => Either::Right(this.example_runner(run_command)),
                                    None => Either::Left(this.preview_placeholder()),
                                })
                            })),
                    ),
            )
    }

    fn preview_placeholder(&self) -> impl Element + use<> {
        Stack::new()
            .s(Width::fill())
            .s(Height::fill())
            .layer(
                El::new()
                    .s(Align::new().center_x().center_y())
                    .s(Font::new().size(14).color(muted_text_color()).no_wrap())
                    .child("Run to see preview"),
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
        let hovered = Mutable::new(false);
        let hovered_signal = hovered.signal().broadcast();
        let source_signal = self.source_code.signal_cloned().broadcast();
        Button::new()
            .s(Padding::new().x(14).y(7))
            .s(RoundedCorners::all(24))
            .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
            .s(Background::new().color_signal(map_ref! {
                let hovered = hovered_signal.signal(),
                let source_code = source_signal.signal_cloned() => {
                    let is_active = source_code.as_ref() == example_data.source_code;
                    match (is_active, *hovered) {
                        (true, _) => color!("rgba(80, 112, 188, 0.55)"),
                        (false, true) => color!("rgba(36, 48, 72, 0.45)"),
                        (false, false) => color!("rgba(24, 32, 54, 0.35)"),
                    }
                }
            }))
            .s(Borders::all(
                Border::new().color(color!("rgba(88, 126, 194, 0.4)")).width(1),
            ))
            .s(Font::new().color_signal(map_ref! {
                let hovered = hovered_signal.signal(),
                let source_code = source_signal.signal_cloned() =>
                if source_code.as_ref() == example_data.source_code {
                    color!("#f6f8ff")
                } else if *hovered {
                    color!("rgba(214, 223, 255, 0.86)")
                } else {
                    muted_text_color()
                }
            }))
            .label(
                El::new()
                    .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
                    .child(example_data.filename),
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
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
