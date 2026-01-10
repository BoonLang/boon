// @TODO remove
#![allow(unused_variables)]

use boon::zoon::{eprintln, println, *};
use boon::zoon::{map_ref, Rgba};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::rc::Rc;
use ulid::Ulid;

use boon::platform::browser::{
    bridge::object_with_document_to_element_signal,
    common::{EngineType, default_engine},
    engine::VirtualFilesystem,
    interpreter,
};

// DD engine imports (feature-gated)
#[cfg(feature = "engine-dd")]
use boon::platform::browser::engine_dd::{
    dd_bridge::render_dd_document_reactive_signal,
    dd_interpreter::run_dd_reactive_with_persistence,
};

mod code_editor;
use code_editor::CodeEditor;

static PROJECT_FILES_STORAGE_KEY: &str = "boon-playground-project-files";
static CURRENT_FILE_STORAGE_KEY: &str = "boon-playground-current-file";

static OLD_SOURCE_CODE_STORAGE_KEY: &str = "boon-playground-old-source-code";
static OLD_SPAN_ID_PAIRS_STORAGE_KEY: &str = "boon-playground-span-id-pairs";
static STATES_STORAGE_KEY: &str = "boon-playground-states";
static PANEL_SPLIT_STORAGE_KEY: &str = "boon-playground-panel-split";
static DEBUG_COLLAPSED_STORAGE_KEY: &str = "boon-playground-debug-collapsed";
static CUSTOM_EXAMPLES_STORAGE_KEY: &str = "boon-playground-custom-examples";
static FORCED_PREVIEW_SIZE_STORAGE_KEY: &str = "boon-playground-forced-preview-size";
static PANEL_LAYOUT_STORAGE_KEY: &str = "boon-playground-panel-layout";
static ENGINE_TYPE_STORAGE_KEY: &str = "boon-playground-engine-type";

/// Clear all localStorage keys that match given prefixes.
/// Used to clean up dynamically-keyed persistence data.
fn clear_prefixed_storage_keys(prefixes: &[&str]) {
    let storage = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .expect("localStorage should be available");

    let len = storage.length().unwrap_or(0);
    let mut keys_to_remove = Vec::new();

    for i in 0..len {
        if let Ok(Some(key)) = storage.key(i) {
            for prefix in prefixes {
                if key.starts_with(prefix) {
                    keys_to_remove.push(key.clone());
                    break;
                }
            }
        }
    }

    // Debug logging (uncomment if needed)
    // web_sys::console::log_1(&format!("[DEBUG] clear_prefixed_storage_keys: removing {} keys", keys_to_remove.len()).into());

    for key in keys_to_remove {
        let _ = storage.remove_item(&key);
    }
}

// Number of main examples (rest are debug examples)
const MAIN_EXAMPLES_COUNT: usize = 11;

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

/// Get example name from URL query parameter (?example=name)
fn get_example_from_url() -> Option<String> {
    let window = web_sys::window()?;
    let location = window.location();
    let search = location.search().ok()?;
    if search.is_empty() {
        return None;
    }
    // Parse query string (e.g., "?example=todo_mvc")
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get("example")
}

/// Update URL query parameter without page reload (URL-encodes the name)
fn set_example_in_url(example_name: &str) {
    if let Some(window) = web_sys::window() {
        if let Ok(history) = window.history() {
            // URL-encode the example name to handle spaces and special characters
            let encoded_name = js_sys::encode_uri_component(example_name);
            let new_url = format!("?example={}", encoded_name);
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&new_url));
        }
    }
}

/// Get custom example name from URL query parameter (?custom-example=name)
fn get_custom_example_from_url() -> Option<String> {
    let window = web_sys::window()?;
    let location = window.location();
    let search = location.search().ok()?;
    if search.is_empty() {
        return None;
    }
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get("custom-example")
}

/// Update URL for custom example (uses ?custom-example= to avoid collision with built-in examples)
fn set_custom_example_in_url(example_name: &str) {
    if let Some(window) = web_sys::window() {
        if let Ok(history) = window.history() {
            let encoded_name = js_sys::encode_uri_component(example_name);
            let new_url = format!("?custom-example={}", encoded_name);
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&new_url));
        }
    }
}

/// Get engine type from URL query parameter (?engine=actors or ?engine=dd)
fn get_engine_from_url() -> Option<EngineType> {
    let window = web_sys::window()?;
    let location = window.location();
    let search = location.search().ok()?;
    if search.is_empty() {
        return None;
    }
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    match params.get("engine").as_deref() {
        Some("actors") => Some(EngineType::Actors),
        Some("dd") => Some(EngineType::DifferentialDataflow),
        _ => None,
    }
}

/// Load engine type from localStorage
fn load_engine_from_storage() -> Option<EngineType> {
    let stored = local_storage().get::<String>(ENGINE_TYPE_STORAGE_KEY)?.ok()?;
    match stored.as_str() {
        "Actors" => Some(EngineType::Actors),
        "DD" => Some(EngineType::DifferentialDataflow),
        _ => None,
    }
}

/// Save engine type to localStorage
fn save_engine_to_storage(engine: EngineType) {
    let _ = local_storage().insert(ENGINE_TYPE_STORAGE_KEY, &engine.short_name().to_string());
}

/// Find example data by name (filename without .bn extension)
fn find_example_by_name(name: &str) -> Option<ExampleData> {
    EXAMPLE_DATAS.iter().find(|e| {
        e.filename.trim_end_matches(".bn") == name || e.filename == name
    }).copied()
}

/// Panel layout mode for screenshot and viewing modes
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(crate = "boon::zoon::serde")]
enum PanelLayout {
    /// Both code editor and preview panels visible (default)
    #[default]
    Normal,
    /// Only code editor visible (for code screenshots)
    CodeOnly,
    /// Only preview panel visible (for preview screenshots)
    PreviewOnly,
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

static EXAMPLE_DATAS: [ExampleData; 24] = [
    make_example_data!("minimal"),
    make_example_data!("hello_world"),
    make_example_data!("interval"),
    make_example_data!("interval_hold"),
    make_example_data!("counter"),
    make_example_data!("counter_hold"),
    make_example_data!("fibonacci"),
    make_example_data!("layers"),
    make_example_data!("shopping_list"),
    make_example_data!("pages"),
    make_example_data!("todo_mvc"),
    make_example_data!("list_retain_count"),
    make_example_data!("list_map_block"),
    make_example_data!("list_object_state"),
    make_example_data!("list_retain_reactive"),
    make_example_data!("list_retain_remove"),
    make_example_data!("while_function_call"),
    make_example_data!("list_map_external_dep"),
    make_example_data!("text_interpolation_update"),
    make_example_data!("button_hover_test"),
    make_example_data!("button_hover_to_click_test"),
    make_example_data!("switch_hold_test"),
    make_example_data!("filter_checkbox_bug"),
    make_example_data!("chained_list_remove_bug"),
];

#[derive(Clone, Copy)]
struct RunCommand {
    filename: Option<&'static str>,
}

fn main() {
    start_app("app", Playground::new);
}

const DEFAULT_FILE_NAME: &str = "main.bn";

#[derive(Clone)]
struct Playground {
    /// All files in the project (filename -> content)
    files: Mutable<Rc<BTreeMap<String, String>>>,
    /// Currently selected/edited file name
    current_file: Mutable<String>,
    /// Current file content for the code editor
    source_code: Mutable<Rc<Cow<'static, str>>>,
    run_command: Mutable<Option<RunCommand>>,
    panel_layout: Mutable<PanelLayout>,
    panel_split_ratio: Mutable<f64>,
    panel_container_width: Mutable<u32>,
    is_dragging_panel_split: Mutable<bool>,
    /// Whether debug examples section is collapsed
    debug_collapsed: Mutable<bool>,
    /// Custom user examples (id, name, source_code) - Vec preserves insertion order, id is stable
    custom_examples: Mutable<Rc<Vec<(String, String, String)>>>,
    /// Currently selected custom example (for styling - not based on content matching)
    selected_custom_example: Mutable<Option<String>>,
    /// Currently being renamed custom example (old_name)
    editing_custom_example: Mutable<Option<String>>,
    /// Forced preview size (width, height) - None means auto
    forced_preview_size: Mutable<Option<(u32, u32)>>,
    /// Whether the force size UI is expanded
    force_size_expanded: Mutable<bool>,
    /// Selected engine type (for engine-both feature)
    engine_type: Mutable<EngineType>,
    _store_files_task: Rc<TaskHandle>,
    _store_current_file_task: Rc<TaskHandle>,
    _store_panel_split_task: Rc<TaskHandle>,
    _store_debug_collapsed_task: Rc<TaskHandle>,
    _store_custom_examples_task: Rc<TaskHandle>,
    _store_forced_preview_size_task: Rc<TaskHandle>,
    _store_panel_layout_task: Rc<TaskHandle>,
    _store_engine_type_task: Rc<TaskHandle>,
    _sync_source_to_files_task: Rc<TaskHandle>,
    _sync_source_to_custom_example_task: Rc<TaskHandle>,
}

impl Playground {
    fn new() -> impl Element {
        // Load custom examples from storage first (needed for URL parameter check)
        let custom_examples_value: Vec<(String, String, String)> = local_storage()
            .get::<Vec<(String, String, String)>>(CUSTOM_EXAMPLES_STORAGE_KEY)
            .and_then(Result::ok)
            .unwrap_or_default();

        // Check for ?custom-example= URL parameter first
        let custom_example_from_url = get_custom_example_from_url()
            .and_then(|name| {
                custom_examples_value.iter()
                    .find(|(_, n, _)| n == &name)
                    .map(|(id, name, code)| (id.clone(), name.clone(), code.clone()))
            });

        // Determine initial selected custom example ID (if loading from URL)
        let initial_selected_custom_example = custom_example_from_url.as_ref().map(|(id, _, _)| id.clone());

        // Load files from storage, or initialize with default/URL example
        let (files, current_file, current_content) = if let Some((_, name, code)) = custom_example_from_url {
            // Load custom example from URL
            let filename = format!("{}.bn", name);
            let mut files = BTreeMap::new();
            files.insert(filename.clone(), code.clone());
            (files, filename, code)
        } else if let Some(Ok(stored_files)) =
            local_storage().get::<BTreeMap<String, String>>(PROJECT_FILES_STORAGE_KEY)
        {
            let current = local_storage()
                .get::<String>(CURRENT_FILE_STORAGE_KEY)
                .and_then(Result::ok)
                .unwrap_or_else(|| {
                    stored_files
                        .keys()
                        .next()
                        .cloned()
                        .unwrap_or_else(|| DEFAULT_FILE_NAME.to_string())
                });
            let content = stored_files.get(&current).cloned().unwrap_or_default();
            (stored_files, current, content)
        } else {
            // Check URL for built-in example parameter
            let example_data = get_example_from_url()
                .and_then(|name| find_example_by_name(&name))
                .unwrap_or(EXAMPLE_DATAS[0]);

            let mut files = BTreeMap::new();
            files.insert(
                example_data.filename.to_string(),
                example_data.source_code.to_string(),
            );
            (files, example_data.filename.to_string(), example_data.source_code.to_string())
        };

        let files = Mutable::new(Rc::new(files));
        let current_file = Mutable::new(current_file);
        let source_code = Mutable::new(Rc::new(Cow::from(current_content)));
        let custom_examples = Mutable::new(Rc::new(custom_examples_value));

        let panel_split_ratio_value =
            if let Some(Ok(ratio)) = local_storage().get(PANEL_SPLIT_STORAGE_KEY) {
                ratio
            } else {
                DEFAULT_PANEL_SPLIT_RATIO
            };
        let panel_split_ratio =
            Mutable::new(Self::clamp_panel_split_ratio(panel_split_ratio_value));

        // Auto-save files to storage
        let _store_files_task = Rc::new(Task::start_droppable(
            files.signal_cloned().for_each_sync(|files| {
                if let Err(error) = local_storage().insert(PROJECT_FILES_STORAGE_KEY, files.as_ref())
                {
                    eprintln!("Failed to store project files: {error:#?}");
                }
            }),
        ));

        // Auto-save current file name to storage
        let _store_current_file_task = Rc::new(Task::start_droppable(
            current_file.signal_cloned().for_each_sync(|filename| {
                if let Err(error) = local_storage().insert(CURRENT_FILE_STORAGE_KEY, &filename) {
                    eprintln!("Failed to store current file name: {error:#?}");
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

        // Load debug collapsed state from storage (default: collapsed)
        let debug_collapsed_value = local_storage()
            .get::<bool>(DEBUG_COLLAPSED_STORAGE_KEY)
            .and_then(Result::ok)
            .unwrap_or(true);
        let debug_collapsed = Mutable::new(debug_collapsed_value);

        let _store_debug_collapsed_task = Rc::new(Task::start_droppable(
            debug_collapsed
                .signal()
                .for_each_sync(|collapsed| {
                    if let Err(error) = local_storage().insert(DEBUG_COLLAPSED_STORAGE_KEY, &collapsed) {
                        eprintln!("Failed to store debug collapsed state: {error:#?}");
                    }
                }),
        ));

        // custom_examples already loaded at the start for URL parameter check

        let _store_custom_examples_task = Rc::new(Task::start_droppable(
            custom_examples
                .signal_cloned()
                .for_each_sync(|examples| {
                    if let Err(error) = local_storage().insert(CUSTOM_EXAMPLES_STORAGE_KEY, examples.as_ref()) {
                        eprintln!("Failed to store custom examples: {error:#?}");
                    }
                }),
        ));

        // Load forced preview size from storage
        let forced_preview_size: Mutable<Option<(u32, u32)>> = Mutable::new(
            local_storage()
                .get::<(u32, u32)>(FORCED_PREVIEW_SIZE_STORAGE_KEY)
                .and_then(Result::ok)
        );
        let force_size_expanded = Mutable::new(forced_preview_size.get().is_some());

        let _store_forced_preview_size_task = Rc::new(Task::start_droppable({
            let forced_preview_size = forced_preview_size.clone();
            forced_preview_size
                .signal()
                .for_each_sync(move |size| {
                    if let Some(size) = size {
                        if let Err(error) = local_storage().insert(FORCED_PREVIEW_SIZE_STORAGE_KEY, &size) {
                            eprintln!("Failed to store forced preview size: {error:#?}");
                        }
                    } else {
                        local_storage().remove(FORCED_PREVIEW_SIZE_STORAGE_KEY);
                    }
                })
        }));

        // Load panel layout from storage (default: Normal)
        let panel_layout_value = local_storage()
            .get::<PanelLayout>(PANEL_LAYOUT_STORAGE_KEY)
            .and_then(Result::ok)
            .unwrap_or(PanelLayout::Normal);
        let panel_layout = Mutable::new(panel_layout_value);

        let _store_panel_layout_task = Rc::new(Task::start_droppable({
            let panel_layout = panel_layout.clone();
            panel_layout
                .signal()
                .for_each_sync(move |layout| {
                    if let Err(error) = local_storage().insert(PANEL_LAYOUT_STORAGE_KEY, &layout) {
                        eprintln!("Failed to store panel layout: {error:#?}");
                    }
                })
        }));

        // Load engine type: URL param > localStorage > default
        let engine_type_value = get_engine_from_url()
            .or_else(load_engine_from_storage)
            .unwrap_or_else(default_engine);
        let engine_type = Mutable::new(engine_type_value);

        let _store_engine_type_task = Rc::new(Task::start_droppable({
            let engine_type = engine_type.clone();
            engine_type
                .signal()
                .for_each_sync(move |engine| {
                    save_engine_to_storage(engine);
                })
        }));

        // Sync source_code changes back to files map
        let _sync_source_to_files_task = {
            let files = files.clone();
            let current_file = current_file.clone();
            Rc::new(Task::start_droppable(
                source_code.signal_cloned().for_each_sync(move |content| {
                    let filename = current_file.lock_ref().clone();
                    // Clone the inner BTreeMap (not just the Rc)
                    let mut files_map = (**files.lock_ref()).clone();
                    files_map.insert(filename, content.to_string());
                    files.set(Rc::new(files_map));
                }),
            ))
        };

        // Track currently selected custom example for syncing code changes
        // Initialize with URL parameter value if a custom example was requested
        let selected_custom_example: Mutable<Option<String>> = Mutable::new(initial_selected_custom_example);

        // Sync source_code changes to the currently selected custom example
        let _sync_source_to_custom_example_task = {
            let custom_examples = custom_examples.clone();
            let selected_custom_example = selected_custom_example.clone();
            Rc::new(Task::start_droppable(
                source_code.signal_cloned().for_each_sync(move |content| {
                    // If a custom example is selected, update its code
                    if let Some(ref id) = *selected_custom_example.lock_ref() {
                        let mut examples = (**custom_examples.lock_ref()).clone();
                        if let Some((_, _, code)) = examples.iter_mut().find(|(eid, _, _)| eid == id) {
                            *code = content.to_string();
                            custom_examples.set(Rc::new(examples));
                        }
                    }
                }),
            ))
        };

        Self {
            files,
            current_file,
            source_code,
            run_command: Mutable::new(None),
            panel_layout,
            panel_split_ratio,
            panel_container_width: Mutable::new(0),
            is_dragging_panel_split: Mutable::new(false),
            debug_collapsed,
            custom_examples,
            selected_custom_example,
            editing_custom_example: Mutable::new(None),
            forced_preview_size,
            force_size_expanded,
            _store_files_task,
            _store_current_file_task,
            _store_panel_split_task,
            _store_debug_collapsed_task,
            _store_custom_examples_task,
            _store_forced_preview_size_task,
            _store_panel_layout_task,
            _store_engine_type_task,
            _sync_source_to_files_task,
            _sync_source_to_custom_example_task,
            engine_type,
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
            // Expose window.boonPlayground API for browser automation
            .update_raw_el({
                let run_command = self.run_command.clone();
                let source_code = self.source_code.clone();
                let current_file = self.current_file.clone();
                let forced_preview_size = self.forced_preview_size.clone();
                let panel_layout = self.panel_layout.clone();
                let engine_type = self.engine_type.clone();
                move |raw_el| {
                    use wasm_bindgen::prelude::*;
                    use wasm_bindgen::JsCast;

                    let window = web_sys::window().unwrap();

                    // Create boonPlayground API object
                    let api = js_sys::Object::new();

                    // isReady() - always returns true once API is set up
                    let is_ready = Closure::wrap(Box::new(|| true) as Box<dyn Fn() -> bool>);
                    js_sys::Reflect::set(&api, &"isReady".into(), is_ready.as_ref()).ok();
                    is_ready.forget();

                    // setCode(code) - set editor content
                    let source_code_for_set = source_code.clone();
                    let set_code = Closure::wrap(Box::new(move |code: String| {
                        let source_code_inner = source_code_for_set.clone();
                        Task::start(async move {
                            source_code_inner.set(Rc::new(Cow::from(code)));
                        });
                    }) as Box<dyn Fn(String)>);
                    js_sys::Reflect::set(&api, &"setCode".into(), set_code.as_ref()).ok();
                    set_code.forget();

                    // setCurrentFile(filename) - set current file name (for persistence)
                    let current_file_for_set = current_file.clone();
                    let set_current_file = Closure::wrap(Box::new(move |filename: String| {
                        current_file_for_set.set(filename);
                    }) as Box<dyn Fn(String)>);
                    js_sys::Reflect::set(&api, &"setCurrentFile".into(), set_current_file.as_ref()).ok();
                    set_current_file.forget();

                    // getCode() - get current editor content
                    let source_code_for_get = source_code.clone();
                    let get_code = Closure::wrap(Box::new(move || -> String {
                        source_code_for_get.lock_ref().to_string()
                    }) as Box<dyn Fn() -> String>);
                    js_sys::Reflect::set(&api, &"getCode".into(), get_code.as_ref()).ok();
                    get_code.forget();

                    // run() - trigger code execution
                    let run_command_for_run = run_command.clone();
                    let run_fn = Closure::wrap(Box::new(move || {
                        let run_command_inner = run_command_for_run.clone();
                        Task::start(async move {
                            run_command_inner.set(Some(RunCommand { filename: None }));
                        });
                    }) as Box<dyn Fn()>);
                    js_sys::Reflect::set(&api, &"run".into(), run_fn.as_ref()).ok();
                    run_fn.forget();

                    // getPreview() - get preview panel text content
                    let get_preview = Closure::wrap(Box::new(|| -> String {
                        if let Some(win) = web_sys::window() {
                            if let Some(doc) = win.document() {
                                // Try to find preview panel content
                                if let Some(el) = doc.query_selector(".preview-panel, [data-panel=\"preview\"], #preview").ok().flatten() {
                                    return el.text_content().unwrap_or_default();
                                }
                                // Fallback: get the example panel content
                                if let Some(el) = doc.query_selector(".example-panel").ok().flatten() {
                                    return el.text_content().unwrap_or_default();
                                }
                            }
                        }
                        String::new()
                    }) as Box<dyn Fn() -> String>);
                    js_sys::Reflect::set(&api, &"getPreview".into(), get_preview.as_ref()).ok();
                    get_preview.forget();

                    // setPreviewSize(width, height) - force preview pane to exact pixel dimensions
                    let forced_preview_size_for_set = forced_preview_size.clone();
                    let set_preview_size = Closure::wrap(Box::new(move |width: u32, height: u32| -> js_sys::Object {
                        forced_preview_size_for_set.set(Some((width, height)));
                        let result = js_sys::Object::new();
                        js_sys::Reflect::set(&result, &"success".into(), &true.into()).ok();
                        js_sys::Reflect::set(&result, &"width".into(), &width.into()).ok();
                        js_sys::Reflect::set(&result, &"height".into(), &height.into()).ok();
                        result
                    }) as Box<dyn Fn(u32, u32) -> js_sys::Object>);
                    js_sys::Reflect::set(&api, &"setPreviewSize".into(), set_preview_size.as_ref()).ok();
                    set_preview_size.forget();

                    // resetPreviewSize() - reset preview pane to auto size
                    let forced_preview_size_for_reset = forced_preview_size.clone();
                    let reset_preview_size = Closure::wrap(Box::new(move || {
                        forced_preview_size_for_reset.set(None);
                    }) as Box<dyn Fn()>);
                    js_sys::Reflect::set(&api, &"resetPreviewSize".into(), reset_preview_size.as_ref()).ok();
                    reset_preview_size.forget();

                    // getPreviewSize() - get current preview size setting
                    let forced_preview_size_for_get = forced_preview_size.clone();
                    let get_preview_size = Closure::wrap(Box::new(move || -> JsValue {
                        match forced_preview_size_for_get.get() {
                            Some((w, h)) => {
                                let result = js_sys::Object::new();
                                js_sys::Reflect::set(&result, &"forced".into(), &true.into()).ok();
                                js_sys::Reflect::set(&result, &"width".into(), &w.into()).ok();
                                js_sys::Reflect::set(&result, &"height".into(), &h.into()).ok();
                                result.into()
                            }
                            None => {
                                let result = js_sys::Object::new();
                                js_sys::Reflect::set(&result, &"forced".into(), &false.into()).ok();
                                result.into()
                            }
                        }
                    }) as Box<dyn Fn() -> JsValue>);
                    js_sys::Reflect::set(&api, &"getPreviewSize".into(), get_preview_size.as_ref()).ok();
                    get_preview_size.forget();

                    // setPanelLayout(layout) - set panel layout mode ('normal', 'code', 'preview')
                    let panel_layout_for_set = panel_layout.clone();
                    let set_panel_layout = Closure::wrap(Box::new(move |layout_str: String| -> bool {
                        let new_layout = match layout_str.to_lowercase().as_str() {
                            "normal" | "both" => PanelLayout::Normal,
                            "code" | "codeonly" | "code_only" => PanelLayout::CodeOnly,
                            "preview" | "previewonly" | "preview_only" => PanelLayout::PreviewOnly,
                            _ => return false,
                        };
                        panel_layout_for_set.set(new_layout);
                        true
                    }) as Box<dyn Fn(String) -> bool>);
                    js_sys::Reflect::set(&api, &"setPanelLayout".into(), set_panel_layout.as_ref()).ok();
                    set_panel_layout.forget();

                    // getPanelLayout() - get current panel layout mode
                    let panel_layout_for_get = panel_layout.clone();
                    let get_panel_layout = Closure::wrap(Box::new(move || -> String {
                        match panel_layout_for_get.get() {
                            PanelLayout::Normal => "normal".to_string(),
                            PanelLayout::CodeOnly => "code".to_string(),
                            PanelLayout::PreviewOnly => "preview".to_string(),
                        }
                    }) as Box<dyn Fn() -> String>);
                    js_sys::Reflect::set(&api, &"getPanelLayout".into(), get_panel_layout.as_ref()).ok();
                    get_panel_layout.forget();

                    // getEngine() - get current engine type and switchability
                    let engine_type_for_get = engine_type.clone();
                    let get_engine = Closure::wrap(Box::new(move || -> JsValue {
                        let result = js_sys::Object::new();
                        let engine_name = engine_type_for_get.get().short_name();
                        js_sys::Reflect::set(&result, &"engine".into(), &engine_name.into()).ok();
                        js_sys::Reflect::set(&result, &"switchable".into(), &boon::platform::browser::common::is_engine_switchable().into()).ok();
                        result.into()
                    }) as Box<dyn Fn() -> JsValue>);
                    js_sys::Reflect::set(&api, &"getEngine".into(), get_engine.as_ref()).ok();
                    get_engine.forget();

                    // setEngine(engine) - set engine type and trigger re-run
                    let engine_type_for_set = engine_type.clone();
                    let run_command_for_engine = run_command.clone();
                    let set_engine = Closure::wrap(Box::new(move |engine_str: String| -> JsValue {
                        let result = js_sys::Object::new();
                        let previous = engine_type_for_set.get().short_name().to_string();

                        // Check if switching is available
                        if !boon::platform::browser::common::is_engine_switchable() {
                            js_sys::Reflect::set(&result, &"error".into(), &"Engine switching not available (single engine compiled)".into()).ok();
                            return result.into();
                        }

                        // Parse engine string
                        let new_engine = match engine_str.as_str() {
                            "Actors" => EngineType::Actors,
                            "DD" => EngineType::DifferentialDataflow,
                            _ => {
                                js_sys::Reflect::set(&result, &"error".into(), &format!("Invalid engine '{}'. Use 'Actors' or 'DD'", engine_str).into()).ok();
                                return result.into();
                            }
                        };

                        // Set the engine
                        engine_type_for_set.set(new_engine);

                        // Trigger re-run
                        run_command_for_engine.set(Some(RunCommand { filename: None }));

                        js_sys::Reflect::set(&result, &"engine".into(), &new_engine.short_name().into()).ok();
                        js_sys::Reflect::set(&result, &"previous".into(), &previous.into()).ok();
                        result.into()
                    }) as Box<dyn Fn(String) -> JsValue>);
                    js_sys::Reflect::set(&api, &"setEngine".into(), set_engine.as_ref()).ok();
                    set_engine.forget();

                    // Set window.boonPlayground
                    js_sys::Reflect::set(&window, &"boonPlayground".into(), &api).ok();

                    // Auto-run on startup
                    let run_command_for_autorun = run_command.clone();
                    Task::start(async move {
                        // Small delay to ensure the editor and UI are fully initialized
                        Timer::sleep(100).await;
                        run_command_for_autorun.set(Some(RunCommand { filename: None }));
                    });

                    // Also keep the legacy boon-run event listener for backwards compatibility
                    let run_command_clone = run_command.clone();
                    let closure = Closure::wrap(Box::new(move |_: web_sys::Event| {
                        let run_command_inner = run_command_clone.clone();
                        Task::start(async move {
                            run_command_inner.set(Some(RunCommand { filename: None }));
                        });
                    }) as Box<dyn FnMut(_)>);

                    window
                        .add_event_listener_with_callback("boon-run", closure.as_ref().unchecked_ref())
                        .unwrap();
                    closure.forget();

                    raw_el
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
            .item_signal(self.panel_layout.signal().map({
                let this = self.clone();
                move |layout| if layout != PanelLayout::Normal {
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
                        self.panel_layout
                            .signal()
                            .map(|layout| if layout != PanelLayout::Normal { 0 } else { 32 }),
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
                    .item(self.header_title())
                    .item(self.example_tabs()),
            )
    }

    fn header_title(&self) -> impl Element + use<> {
        Row::new()
            .s(Align::new().center_y())
            .s(
                Font::new()
                    .size(18)
                    .weight(FontWeight::SemiBold)
                    .family([
                        FontFamily::new("JetBrains Mono"),
                        FontFamily::Monospace,
                    ])
                    .no_wrap(),
            )
            .s(Transform::new().move_up(2))
            .item(
                Link::new()
                    .s(
                        Font::new().color(color!("#6cb6ff")).line(
                            FontLine::new()
                                .underline()
                                .color(color!("#6cb6ff"))
                                .offset(4),
                        ),
                    )
                    .label("Boon")
                    .to("https://boon.run"),
            )
            .item(
                El::new()
                    .s(Font::new().color(color!("#d2691e")))
                    .child("/"),
            )
            .item(
                El::new()
                    .s(Font::new().color(color!("#fcbf49")))
                    .child("play"),
            )
    }

    fn example_tabs(&self) -> impl Element + use<> {
        let main_examples = &EXAMPLE_DATAS[..MAIN_EXAMPLES_COUNT];
        let debug_examples = &EXAMPLE_DATAS[MAIN_EXAMPLES_COUNT..];

        Column::new()
            .s(Width::fill())
            .s(Gap::new().y(8))
            .item(
                // Main examples row
                Row::new()
                    .s(Width::fill())
                    .s(Align::new().center_y())
                    .s(Gap::new().x(10).y(6))
                    .multiline()
                    .items(main_examples.iter().map(|&example_data| self.example_button(example_data)))
            )
            .item_signal(
                // Custom examples row (only shown if there are custom examples or always show add button)
                self.custom_examples.signal_cloned().map({
                    let this = self.clone();
                    move |custom_examples| {
                        if custom_examples.is_empty() {
                            // Just show the add button when no custom examples
                            Some(
                                Row::new()
                                    .s(Width::fill())
                                    .s(Align::new().center_y())
                                    .s(Gap::new().x(10).y(6))
                                    .multiline()
                                    .item(this.add_custom_example_button())
                            )
                        } else {
                            // Show custom examples with add button at the end (Vec preserves order)
                            // Pass (id, name) tuples for button creation
                            let id_names: Vec<(String, String)> = custom_examples.iter().map(|(id, name, _)| (id.clone(), name.clone())).collect();
                            Some(
                                Row::new()
                                    .s(Width::fill())
                                    .s(Align::new().center_y())
                                    .s(Gap::new().x(10).y(6))
                                    .multiline()
                                    .items(id_names.into_iter().map(|(id, name)| this.custom_example_button(id, name)))
                                    .item(this.add_custom_example_button())
                            )
                        }
                    }
                })
            )
            .item(
                // Debug section with toggle header
                Column::new()
                    .s(Width::fill())
                    .s(Gap::new().y(6))
                    .item(self.debug_section_header())
                    .item_signal(
                        self.debug_collapsed.signal().map({
                            let this = self.clone();
                            let debug_examples = debug_examples.to_vec();
                            move |collapsed| {
                                if collapsed {
                                    None
                                } else {
                                    Some(
                                        Row::new()
                                            .s(Width::fill())
                                            .s(Align::new().center_y())
                                            .s(Gap::new().x(10).y(6))
                                            .multiline()
                                            .items(debug_examples.iter().map(|&example_data| this.example_button(example_data)))
                                    )
                                }
                            }
                        })
                    )
            )
    }

    fn debug_section_header(&self) -> impl Element + use<> {
        let hovered = Mutable::new(false);
        Button::new()
            .s(Padding::new().x(10).y(5))
            .s(RoundedCorners::all(12))
            .s(Font::new().size(12).weight(FontWeight::Medium))
            .s(Background::new().color_signal(
                hovered.signal().map(|h| {
                    if h {
                        color!("rgba(60, 70, 100, 0.4)")
                    } else {
                        color!("rgba(40, 50, 80, 0.3)")
                    }
                })
            ))
            .s(Font::new().color(muted_text_color()))
            .label_signal(self.debug_collapsed.signal().map(|collapsed| {
                if collapsed {
                    "▶  Debug examples"
                } else {
                    "▼  Debug examples"
                }
            }))
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press({
                let debug_collapsed = self.debug_collapsed.clone();
                move || {
                    debug_collapsed.set(!debug_collapsed.get());
                }
            })
    }

    fn controls_row(&self) -> impl Element + use<> {
        Row::new()
            .s(Width::fill())
            .s(Align::new().center_y())
            .s(Gap::new().x(12).y(8))
            .multiline()
            .item(El::new().s(Align::new().left()).child(self.panel_layout_button()))
            .item(
                El::new()
                    .s(Font::new().size(12).color(color!("rgba(255, 255, 255, 0.5)")))
                    .child("F12 → dev tools for logs & errors")
            )
            .item(self.engine_indicator())
            .item(El::new().s(Align::new().center_x()).child(self.run_button()))
            .item(self.force_size_controls())
            .item(El::new().s(Align::new().right()).child(self.clear_saved_states_button()))
    }

    /// Display the current engine type with tooltip.
    /// When engine-both feature is enabled, this becomes a clickable toggle.
    fn engine_indicator(&self) -> impl Element + use<> {
        let engine_type = self.engine_type.clone();
        let run_command = self.run_command.clone();
        let hovered = Mutable::new(false);

        // Check if engine switching is available (engine-both feature)
        let is_switchable = boon::platform::browser::common::is_engine_switchable();

        El::new()
            .s(Font::new().size(12).color_signal(
                hovered.signal().map(move |h| {
                    if h && is_switchable {
                        color!("rgba(100, 200, 255, 1.0)")
                    } else {
                        color!("rgba(100, 200, 255, 0.8)")
                    }
                })
            ))
            .s(Padding::new().x(8).y(4))
            .s(Background::new().color_signal(
                hovered.signal().map(move |h| {
                    if h && is_switchable {
                        color!("rgba(100, 200, 255, 0.2)")
                    } else {
                        color!("rgba(100, 200, 255, 0.1)")
                    }
                })
            ))
            .s(RoundedCorners::all(4))
            .s(Cursor::new(if is_switchable { CursorIcon::Pointer } else { CursorIcon::Default }))
            .update_raw_el({
                let engine_type = engine_type.clone();
                move |raw_el| {
                    raw_el.attr_signal("title", engine_type.signal().map(move |engine| {
                        if is_switchable {
                            format!("{} (click to switch)", engine.full_name())
                        } else {
                            engine.full_name().to_string()
                        }
                    }))
                }
            })
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_click_event({
                let engine_type = engine_type.clone();
                let run_command = run_command.clone();
                move |_| {
                    if is_switchable {
                        // Toggle engine
                        let new_engine = match engine_type.get() {
                            EngineType::Actors => EngineType::DifferentialDataflow,
                            EngineType::DifferentialDataflow => EngineType::Actors,
                        };
                        engine_type.set(new_engine);
                        // Trigger re-run with new engine
                        run_command.set(Some(RunCommand { filename: None }));
                    }
                }
            })
            .child_signal(engine_type.signal().map(|engine| {
                format!("Engine: {}", engine.short_name())
            }))
    }

    fn primary_panel<T: Element>(&self, content: T) -> impl Element + use<T> {
        El::new()
            .s(Width::fill())
            .s(Height::fill())
            .s(Scrollbars::both())
            .s(Background::new().color(primary_surface_color()))
            .s(RoundedCorners::all_signal(self.panel_layout.signal().map(|layout| {
                match layout {
                    PanelLayout::PreviewOnly => Some(0),  // No rounded corners for screenshots
                    _ => Some(24),
                }
            })))
            .s(Borders::all_signal(self.panel_layout.signal().map(|layout| {
                match layout {
                    PanelLayout::PreviewOnly => Border::new(),  // No border for screenshots
                    _ => Border::new().color(color!("rgba(255, 255, 255, 0.05)")).width(1),
                }
            })))
            .s(Shadows::with_signal_self(self.panel_layout.signal().map(|layout| {
                match layout {
                    PanelLayout::PreviewOnly => None,  // No shadow for screenshots
                    _ => Some(Shadows::new([
                        Shadow::new()
                            .color(color!("rgba(4, 12, 24, 0.32)"))
                            .y(30)
                            .blur(60)
                            .spread(-18),
                    ])),
                }
            })))
            .update_raw_el({
                let panel_layout = self.panel_layout.clone();
                move |raw_el| {
                    raw_el.style_signal("backdrop-filter", panel_layout.signal().map(|layout| {
                        if layout == PanelLayout::PreviewOnly { "none" } else { "blur(20px)" }
                    }))
                }
            })
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
            // Code editor - CSS hide when PreviewOnly (preserves DOM state)
            .item(self.code_editor_panel_container())
            // Panel divider - CSS hide when not Normal
            .item(self.panel_divider())
            // Preview panel - CSS hide when CodeOnly (preserves DOM state)
            .item(self.example_panel_container())
    }

    fn code_editor_panel_container(&self) -> impl Element + use<> {
        El::new()
            .s(Align::new().top())
            .s(Height::fill())
            .s(Padding::new().right_signal(
                self.panel_layout
                    .signal()
                    .map(|layout| if layout == PanelLayout::CodeOnly { 0 } else { 6 }),
            ))
            .s(Width::with_signal_self(map_ref! {
                let layout = self.panel_layout.signal(),
                let ratio = self.panel_split_ratio.signal(),
                let container = self.panel_container_width.signal() =>
                match *layout {
                    // When hidden via display:none, width doesn't matter but we set fill for consistency
                    PanelLayout::PreviewOnly => Some(Width::fill()),
                    PanelLayout::CodeOnly => Some(Width::fill()),
                    PanelLayout::Normal => {
                        let container_width = *container as f64;
                        let min_total = MIN_EDITOR_WIDTH_PX + MIN_PREVIEW_WIDTH_PX + PANEL_DIVIDER_WIDTH;
                        if container_width >= min_total {
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
                }
            }))
            // TODO: Add Display style to MoonZoon (display: none/block/flex/etc.)
            // Using raw style for now to properly hide panel instead of Width::exact(0) antipattern
            .update_raw_el({
                let panel_layout = self.panel_layout.clone();
                move |raw_el| {
                    raw_el.style_signal(
                        "display",
                        panel_layout.signal().map(|layout| {
                            if layout == PanelLayout::PreviewOnly {
                                Some("none")
                            } else {
                                None::<&str> // Remove display style, use default
                            }
                        }),
                    )
                }
            })
            .child_signal(self.panel_layout.signal().map({
                let this = self.clone();
                move |layout| {
                    let playground = this.clone();
                    if layout == PanelLayout::CodeOnly {
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
            // TODO: Add Display style to MoonZoon
            .update_raw_el({
                let panel_layout = self.panel_layout.clone();
                move |raw_el| {
                    raw_el.style_signal(
                        "display",
                        panel_layout.signal().map(|layout| {
                            if layout == PanelLayout::Normal {
                                None::<&str> // Remove display style, use default
                            } else {
                                Some("none")
                            }
                        }),
                    )
                }
            })
    }

    fn example_panel_container(&self) -> impl Element + use<> {
        El::new()
            .s(Align::new().top())
            .s(Height::fill())
            .s(Padding::new().left_signal(
                self.panel_layout
                    .signal()
                    .map(|layout| if layout == PanelLayout::PreviewOnly { 0 } else { 6 }),
            ))
            .s(Width::with_signal_self(map_ref! {
                let layout = self.panel_layout.signal(),
                let ratio = self.panel_split_ratio.signal(),
                let container = self.panel_container_width.signal() =>
                match *layout {
                    // When hidden via display:none, width doesn't matter but we set fill for consistency
                    PanelLayout::CodeOnly => Some(Width::fill()),
                    PanelLayout::PreviewOnly => Some(Width::fill()),
                    PanelLayout::Normal => {
                        let container_width = *container as f64;
                        let min_total = MIN_EDITOR_WIDTH_PX + MIN_PREVIEW_WIDTH_PX + PANEL_DIVIDER_WIDTH;
                        if container_width >= min_total {
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
                }
            }))
            // TODO: Add Display style to MoonZoon
            .update_raw_el({
                let panel_layout = self.panel_layout.clone();
                move |raw_el| {
                    raw_el.style_signal(
                        "display",
                        panel_layout.signal().map(|layout| {
                            if layout == PanelLayout::CodeOnly {
                                Some("none")
                            } else {
                                None::<&str> // Remove display style, use default
                            }
                        }),
                    )
                }
            })
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

    fn panel_layout_button(&self) -> impl Element {
        Row::new()
            .s(RoundedCorners::all(22))
            .s(Background::new().color(color!("rgba(26, 36, 58, 0.32)")))
            .s(Shadows::new([
                Shadow::new()
                    .color(color!("rgba(8, 13, 28, 0.26)"))
                    .y(12)
                    .blur(22)
                    .spread(-8),
            ]))
            .s(Padding::all(3))
            .item(self.layout_segment("Both", PanelLayout::Normal))
            .item(self.layout_segment("Code", PanelLayout::CodeOnly))
            .item(self.layout_segment("Preview", PanelLayout::PreviewOnly))
    }

    fn layout_segment(&self, label: &'static str, layout: PanelLayout) -> impl Element {
        let hovered = Mutable::new(false);
        let hovered_for_signal = hovered.clone();
        Button::new()
            .s(Padding::new().x(10).y(5))
            .s(RoundedCorners::all(18))
            .s(Font::new().size(13).color(primary_text_color()))
            .s(Background::new().color_signal(map_ref! {
                let current = self.panel_layout.signal(),
                let hovered = hovered_for_signal.signal() =>
                {
                    let is_active = *current == layout;
                    match (is_active, *hovered) {
                        (true, true) => color!("rgba(70, 104, 178, 0.7)"),
                        (true, false) => color!("rgba(60, 94, 168, 0.6)"),
                        (false, true) => color!("rgba(50, 68, 108, 0.5)"),
                        (false, false) => color!("transparent"),
                    }
                }
            }))
            .label(
                El::new()
                    .s(Font::new().size(13).weight(FontWeight::Medium).no_wrap())
                    .child(label),
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press({
                let panel_layout = self.panel_layout.clone();
                move || panel_layout.set(layout)
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
                // Clear dynamically-keyed persistence data (list calls, removed sets)
                clear_prefixed_storage_keys(&["list_calls:", "list_removed:"]);
            })
    }

    fn force_size_controls(&self) -> impl Element + use<> {
        let width_input = Mutable::new(
            self.forced_preview_size.get().map(|(w, _)| w.to_string()).unwrap_or_else(|| "700".to_string())
        );
        let height_input = Mutable::new(
            self.forced_preview_size.get().map(|(_, h)| h.to_string()).unwrap_or_else(|| "700".to_string())
        );

        Row::new()
            .s(Gap::new().x(6))
            .s(Align::new().center_y())
            .item_signal(self.force_size_expanded.signal().map({
                let this = self.clone();
                let force_size_expanded = self.force_size_expanded.clone();
                let width_input = width_input.clone();
                let height_input = height_input.clone();
                move |expanded| {
                    if expanded {
                        // Show inputs and Auto button
                        let forced_preview_size = this.forced_preview_size.clone();
                        let force_size_expanded = force_size_expanded.clone();
                        let width_input = width_input.clone();
                        let height_input = height_input.clone();
                        Some(Row::new()
                            .s(Gap::new().x(4))
                            .s(Align::new().center_y())
                            .item(self::force_size_input(width_input.clone(), "W"))
                            .item(El::new().s(Font::new().size(12).color(color!("rgba(255,255,255,0.5)"))).child("×"))
                            .item(self::force_size_input(height_input.clone(), "H"))
                            .item(self::force_size_apply_button(width_input, height_input, forced_preview_size.clone()))
                            .item(self::force_size_auto_button(forced_preview_size, force_size_expanded))
                        )
                    } else {
                        None
                    }
                }
            }))
            .item_signal(self.force_size_expanded.signal().map({
                let force_size_expanded = self.force_size_expanded.clone();
                move |expanded| {
                    if !expanded {
                        // Show "Force size" button
                        let force_size_expanded = force_size_expanded.clone();
                        Some(self::force_size_toggle_button(force_size_expanded))
                    } else {
                        None
                    }
                }
            }))
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
                        Stack::new()
                            .s(Width::fill())
                            .s(Height::fill())
                            .s(Background::new().color(color!("#101a2c")))
                            .s(Borders::new().top(
                                Border::new().color(color!("indigo")).width(4),
                            ))
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
            )
    }

    fn code_editor_widget(&self) -> CodeEditor {
        CodeEditor::new()
            .s(Width::fill())
            .s(Height::fill())
            .content_signal(self.source_code.signal_cloned())
            .snippet_screenshot_mode_signal(
                self.panel_layout.signal().map(|layout| layout == PanelLayout::CodeOnly)
            )
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
                    .s(RoundedCorners::all_signal(self.panel_layout.signal().map(|layout| {
                        match layout {
                            PanelLayout::PreviewOnly => Some(0),  // No rounded corners for screenshots
                            _ => Some(24),
                        }
                    })))
                    .s(Clip::both())
                    .layer(
                        El::new()
                            .s(Width::fill())
                            .s(Height::fill())
                            .update_raw_el({
                                let panel_layout = self.panel_layout.clone();
                                move |raw_el| {
                                    raw_el.style_signal(
                                        "background",
                                        panel_layout.signal().map(|layout| {
                                            if layout == PanelLayout::PreviewOnly {
                                                "transparent"
                                            } else {
                                                "radial-gradient(120% 120% at 84% 0%, rgba(76, 214, 255, 0.16) 0%, rgba(5, 9, 18, 0.0) 55%), linear-gradient(165deg, rgba(9, 13, 24, 0.94) 15%, rgba(5, 8, 14, 0.96) 85%)"
                                            }
                                        })
                                    )
                                }
                            }),
                    )
                    .layer(
                        El::new()
                            // Keep forced_preview_size for actual dimensions
                            .s(Width::with_signal_self(self.forced_preview_size.signal().map(|size| {
                                match size {
                                    Some((w, _)) => Some(Width::exact(w)),
                                    None => Some(Width::fill()),
                                }
                            })))
                            .s(Height::with_signal_self(self.forced_preview_size.signal().map(|size| {
                                match size {
                                    Some((_, h)) => Some(Height::exact(h)),
                                    None => Some(Height::fill()),
                                }
                            })))
                            // Use panel_layout for padding styling
                            .s(Padding::new()
                                .x_signal(self.panel_layout.signal().map(|layout| {
                                    match layout {
                                        PanelLayout::PreviewOnly => Some(0),
                                        _ => Some(12),
                                    }
                                }))
                                .y_signal(self.panel_layout.signal().map(|layout| {
                                    match layout {
                                        PanelLayout::PreviewOnly => Some(0),
                                        _ => Some(12),
                                    }
                                })))
                            .s(Scrollbars::y_and_clip_x())
                            .update_raw_el({
                                let forced_preview_size = self.forced_preview_size.clone();
                                move |raw_el| {
                                    raw_el
                                        .attr("data-boon-panel", "preview")
                                        .style_signal("overflow", forced_preview_size.signal().map(|size| {
                                            if size.is_some() { "hidden" } else { "auto" }
                                        }))
                                }
                            })
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

        // Get all files and current file info
        let files = self.files.lock_ref();
        let current_file_name = self.current_file.lock_ref().clone();
        let filename = run_command.filename.unwrap_or(&current_file_name);
        let source_code = self.source_code.lock_ref();
        let engine_type = self.engine_type.get();

        // Check which engine to use
        #[cfg(feature = "engine-dd")]
        if engine_type == EngineType::DifferentialDataflow {
            println!("[DD Engine] Running with DD engine");

            // Run with DD engine (reactive evaluation)
            let result = run_dd_reactive_with_persistence(
                filename,
                &source_code,
                Some(STATES_STORAGE_KEY),
            );
            drop(source_code);
            drop(files);

            if let Some(dd_result) = result {
                if let Some(document) = dd_result.document {
                    println!("[DD Engine] Document rendered successfully");
                    return render_dd_document_reactive_signal(document, dd_result.context)
                        .unify();
                }
            }

            return El::new()
                .s(Font::new().color(color!("LightCoral")))
                .child("DD Engine: Failed to run. See errors in dev console.")
                .unify();
        }

        // Create VirtualFilesystem with all project files
        let virtual_fs = VirtualFilesystem::with_files(
            files
                .iter()
                .map(|(name, content)| (name.clone(), content.clone()))
                .collect(),
        );

        // Check if BUILD.bn exists and run it first
        let build_source = files.get("BUILD.bn").cloned();
        drop(files);

        // Run BUILD.bn if it exists (to write generated files to VirtualFilesystem)
        if let Some(build_code) = build_source {
            println!("Running BUILD.bn first...");
            let _ = interpreter::run_with_registry(
                "BUILD.bn",
                &build_code,
                "boon-playground-build-states",
                "boon-playground-build-old-code",
                "boon-playground-build-span-id-pairs",
                virtual_fs.clone(),
                None,
            );
            println!("BUILD.bn completed");
        }

        // Run the main file (uses ModuleLoader for imports, no shared registry)
        // We keep reference_connector and link_connector alive to preserve all actors.
        // Dropping them (via after_remove) will trigger cleanup of all actors.
        let evaluation_result = interpreter::run_with_registry(
            filename,
            &source_code,
            STATES_STORAGE_KEY,
            OLD_SOURCE_CODE_STORAGE_KEY,
            OLD_SPAN_ID_PAIRS_STORAGE_KEY,
            virtual_fs,
            None,
        );
        drop(source_code);
        if let Some((object, construct_context, _registry, _module_loader, reference_connector, link_connector, pass_through_connector)) = evaluation_result {
            El::new()
                .s(Width::fill())
                .s(Height::fill())
                .child_signal(object_with_document_to_element_signal(
                    object.clone(),
                    construct_context,
                ))
                .after_remove(move |_| {
                    // Drop object first, then drop connectors to trigger actor cleanup
                    drop(object);
                    drop(reference_connector);
                    drop(link_connector);
                    drop(pass_through_connector);
                })
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
                    .child(example_data.filename.trim_end_matches(".bn")),
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press({
                let files = self.files.clone();
                let current_file = self.current_file.clone();
                let source_code = self.source_code.clone();
                let run_command = self.run_command.clone();
                let custom_examples = self.custom_examples.clone();
                let selected_custom_example = self.selected_custom_example.clone();
                move || {
                    // Check if we're re-selecting the same example
                    let is_same_example = *current_file.lock_ref() == example_data.filename;

                    // Save current code to previously selected custom example before switching
                    let prev_selected_id = selected_custom_example.lock_ref().clone();
                    if let Some(prev_id) = prev_selected_id {
                        let current_code = source_code.lock_ref().to_string();
                        let mut examples = (**custom_examples.lock_ref()).clone();
                        if let Some((_, _, code)) = examples.iter_mut().find(|(id, _, _)| id == &prev_id) {
                            *code = current_code;
                        }
                        custom_examples.set(Rc::new(examples));
                    }

                    // Clear custom example selection
                    selected_custom_example.set(None);

                    // Only clear saved state when switching to a DIFFERENT example.
                    // When re-selecting the same example, preserve state for persistence testing.
                    if !is_same_example {
                        // Clear saved state to prevent "ghost" data from previous examples
                        local_storage().remove(STATES_STORAGE_KEY);
                        local_storage().remove(OLD_SOURCE_CODE_STORAGE_KEY);
                        local_storage().remove(OLD_SPAN_ID_PAIRS_STORAGE_KEY);
                        clear_prefixed_storage_keys(&["list_calls:", "list_removed:"]);
                    }

                    // Update URL to share this example
                    set_example_in_url(example_data.filename.trim_end_matches(".bn"));

                    // Replace project files with just this example
                    let mut new_files = BTreeMap::new();
                    new_files.insert(
                        example_data.filename.to_string(),
                        example_data.source_code.to_string(),
                    );
                    files.set(Rc::new(new_files));
                    current_file.set(example_data.filename.to_string());
                    source_code.set_neq(Rc::new(Cow::from(example_data.source_code)));
                    run_command.set(Some(RunCommand {
                        filename: Some(example_data.filename),
                    }));
                }
            })
    }

    fn add_custom_example_button(&self) -> impl Element + use<> {
        let hovered = Mutable::new(false);
        Button::new()
            .s(Padding::new().x(12).y(7))
            .s(RoundedCorners::all(24))
            .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
            .s(Background::new().color_signal(
                hovered.signal().map(|h| {
                    if h {
                        color!("rgba(60, 140, 100, 0.45)")
                    } else {
                        color!("rgba(40, 100, 70, 0.35)")
                    }
                })
            ))
            .s(Borders::all(
                Border::new().color(color!("rgba(80, 180, 120, 0.5)")).width(1),
            ))
            .s(Font::new().color_signal(
                hovered.signal().map(|h| {
                    if h {
                        color!("rgba(180, 255, 200, 0.95)")
                    } else {
                        color!("rgba(150, 220, 170, 0.85)")
                    }
                })
            ))
            .label(
                El::new()
                    .s(Font::new().size(14).weight(FontWeight::SemiBold).no_wrap())
                    .child("+"),
            )
            .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
            .on_press({
                let custom_examples = self.custom_examples.clone();
                let selected_custom_example = self.selected_custom_example.clone();
                let files = self.files.clone();
                let current_file = self.current_file.clone();
                let source_code = self.source_code.clone();
                let run_command = self.run_command.clone();
                move || {
                    // Save current code to previously selected custom example before creating new one
                    let prev_selected_id = selected_custom_example.lock_ref().clone();
                    if let Some(prev_id) = prev_selected_id {
                        let current_code = source_code.lock_ref().to_string();
                        let mut examples = (**custom_examples.lock_ref()).clone();
                        if let Some((_, _, code)) = examples.iter_mut().find(|(id, _, _)| id == &prev_id) {
                            *code = current_code;
                        }
                        custom_examples.set(Rc::new(examples));
                    }

                    // Generate stable ID and unique name for new custom example
                    let id = Ulid::new().to_string();
                    let examples = custom_examples.lock_ref();
                    let mut counter = 1;
                    let name = loop {
                        let candidate = format!("custom_{}", counter);
                        if !examples.iter().any(|(_, n, _)| n == &candidate) {
                            break candidate;
                        }
                        counter += 1;
                    };
                    drop(examples);

                    // Default code for new custom example
                    let default_code = "-- My custom example\ndocument: TEXT { Hello! } |> Document/new()";

                    // Add to custom examples (push to end to preserve order)
                    let mut new_examples = (**custom_examples.lock_ref()).clone();
                    new_examples.push((id.clone(), name.clone(), default_code.to_string()));
                    custom_examples.set(Rc::new(new_examples));

                    // Set as selected (by ID)
                    selected_custom_example.set(Some(id));

                    // Clear saved state
                    local_storage().remove(STATES_STORAGE_KEY);
                    local_storage().remove(OLD_SOURCE_CODE_STORAGE_KEY);
                    local_storage().remove(OLD_SPAN_ID_PAIRS_STORAGE_KEY);
                    clear_prefixed_storage_keys(&["list_calls:", "list_removed:"]);

                    // Update URL (use custom-example parameter)
                    set_custom_example_in_url(&name);

                    // Set as current file
                    let filename = format!("{}.bn", name);
                    let mut new_files = BTreeMap::new();
                    new_files.insert(filename.clone(), default_code.to_string());
                    files.set(Rc::new(new_files));
                    current_file.set(filename);
                    source_code.set_neq(Rc::new(Cow::from(default_code)));
                    run_command.set(Some(RunCommand { filename: None }));
                }
            })
    }

    fn custom_example_button(&self, id: String, name: String) -> impl Element + use<> {
        let hovered = Mutable::new(false);
        let delete_hovered = Mutable::new(false);
        let hovered_signal = hovered.signal().broadcast();
        let selected_signal = self.selected_custom_example.signal_cloned().broadcast();
        let id_for_bg = id.clone();
        let id_for_font = id.clone();
        let id_for_editing = id.clone();
        let id_for_editing_check = id.clone();
        let custom_examples_signal = self.custom_examples.signal_cloned().broadcast();
        let editing_signal = self.editing_custom_example.signal_cloned().broadcast();
        let edit_text = Mutable::new(name.clone());

        Row::new()
            .s(Align::new().center_y())
            .s(Gap::new().x(0))
            .item_signal(
                editing_signal.signal_cloned().map({
                    let id = id.clone();
                    let name = name.clone();
                    let hovered = hovered.clone();
                    let hovered_signal = hovered_signal.clone();
                    let selected_signal = selected_signal.clone();
                    let custom_examples_signal = custom_examples_signal.clone();
                    let custom_examples = self.custom_examples.clone();
                    let selected_custom_example = self.selected_custom_example.clone();
                    let editing_custom_example = self.editing_custom_example.clone();
                    let files = self.files.clone();
                    let current_file = self.current_file.clone();
                    let source_code = self.source_code.clone();
                    let run_command = self.run_command.clone();
                    let id_for_bg = id_for_bg.clone();
                    let id_for_font = id_for_font.clone();
                    let edit_text = edit_text.clone();
                    move |editing| {
                        // editing_custom_example stores the ID
                        let is_editing = editing.as_ref() == Some(&id_for_editing_check);
                        if is_editing {
                            // Editing mode: show text input
                            let id_for_rename = id.clone();
                            let name_for_rename = name.clone();
                            let custom_examples_for_rename = custom_examples.clone();
                            let editing_custom_example_for_rename = editing_custom_example.clone();
                            let edit_text_for_input = edit_text.clone();

                            TextInput::new()
                                .s(Padding::new().left(14).right(6).y(4))
                                .s(RoundedCorners::new().left(24))
                                .s(Font::new().size(14).weight(FontWeight::Medium).color(color!("#e8ffe8")))
                                .s(Background::new().color(color!("rgba(100, 140, 100, 0.55)")))
                                .s(Borders::new()
                                    .left(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                                    .top(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                                    .bottom(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                                )
                                .s(Width::exact(100))
                                .focus(true)
                                .label_hidden("Rename example")
                                .text_signal(edit_text_for_input.signal_cloned())
                                .on_change({
                                    let edit_text = edit_text.clone();
                                    move |new_text| edit_text.set(new_text)
                                })
                                .update_raw_el({
                                    let id = id_for_rename.clone();
                                    let name = name_for_rename.clone();
                                    let custom_examples = custom_examples_for_rename.clone();
                                    let editing_custom_example = editing_custom_example_for_rename.clone();
                                    let edit_text = edit_text.clone();
                                    move |raw_el| {
                                        raw_el.event_handler(move |event: events::KeyDown| {
                                            if event.key() == "Enter" {
                                                let new_name = edit_text.lock_ref().trim().to_string();
                                                if !new_name.is_empty() && new_name != name {
                                                    // Rename the custom example (in place to preserve order)
                                                    // Find by ID, update name
                                                    let mut new_examples = (**custom_examples.lock_ref()).clone();
                                                    if let Some((_, n, _)) = new_examples.iter_mut().find(|(eid, _, _)| eid == &id) {
                                                        *n = new_name.clone();
                                                        custom_examples.set(Rc::new(new_examples));
                                                        // Update URL to reflect new name
                                                        set_custom_example_in_url(&new_name);
                                                    }
                                                }
                                                editing_custom_example.set(None);
                                            } else if event.key() == "Escape" {
                                                editing_custom_example.set(None);
                                            }
                                        })
                                    }
                                })
                                .on_blur({
                                    let id = id_for_rename;
                                    let name = name_for_rename;
                                    let custom_examples = custom_examples_for_rename;
                                    let editing_custom_example = editing_custom_example_for_rename;
                                    let edit_text = edit_text.clone();
                                    move || {
                                        let new_name = edit_text.lock_ref().trim().to_string();
                                        if !new_name.is_empty() && new_name != name {
                                            // Rename the custom example (in place to preserve order)
                                            // Find by ID, update name
                                            let mut new_examples = (**custom_examples.lock_ref()).clone();
                                            if let Some((_, n, _)) = new_examples.iter_mut().find(|(eid, _, _)| eid == &id) {
                                                *n = new_name.clone();
                                                custom_examples.set(Rc::new(new_examples));
                                                // Update URL to reflect new name
                                                set_custom_example_in_url(&new_name);
                                            }
                                        }
                                        editing_custom_example.set(None);
                                    }
                                })
                                .left_either()
                        } else {
                            // Normal mode: show button
                            let id_for_bg = id_for_bg.clone();
                            let id_for_font = id_for_font.clone();
                            let id_for_click = id.clone();
                            let id_for_dblclick = id.clone();
                            let name_for_click = name.clone();
                            let name_for_dblclick = name.clone();
                            Button::new()
                                .s(Padding::new().left(14).right(6).y(7))
                                .s(RoundedCorners::new().left(24))
                                .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
                                .s(Background::new().color_signal(map_ref! {
                                    let hovered = hovered_signal.signal(),
                                    let selected = selected_signal.signal_cloned() => {
                                        let is_active = selected.as_ref() == Some(&id_for_bg);
                                        match (is_active, *hovered) {
                                            (true, _) => color!("rgba(100, 140, 100, 0.55)"),
                                            (false, true) => color!("rgba(50, 70, 50, 0.45)"),
                                            (false, false) => color!("rgba(35, 50, 35, 0.35)"),
                                        }
                                    }
                                }))
                                .s(Borders::new()
                                    .left(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                                    .top(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                                    .bottom(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                                )
                                .s(Font::new().color_signal(map_ref! {
                                    let hovered = hovered_signal.signal(),
                                    let selected = selected_signal.signal_cloned() =>
                                    {
                                        let is_active = selected.as_ref() == Some(&id_for_font);
                                        if is_active {
                                            color!("#e8ffe8")
                                        } else if *hovered {
                                            color!("rgba(200, 240, 200, 0.86)")
                                        } else {
                                            color!("rgba(150, 200, 150, 0.7)")
                                        }
                                    }
                                }))
                                .label(
                                    El::new()
                                        .s(Font::new().size(14).weight(FontWeight::Medium).no_wrap())
                                        .child(name.clone()),
                                )
                                .on_hovered_change({
                                    let hovered = hovered.clone();
                                    move |is_hovered| hovered.set(is_hovered)
                                })
                                .on_press({
                                    let id = id_for_click;
                                    let name = name_for_click;
                                    let custom_examples = custom_examples.clone();
                                    let selected_custom_example = selected_custom_example.clone();
                                    let files = files.clone();
                                    let current_file = current_file.clone();
                                    let source_code = source_code.clone();
                                    let run_command = run_command.clone();
                                    move || {
                                        // If already selected, do nothing (don't reset code)
                                        if selected_custom_example.lock_ref().as_ref() == Some(&id) {
                                            return;
                                        }

                                        // Save current code to previously selected custom example
                                        let prev_selected_id = selected_custom_example.lock_ref().clone();
                                        if let Some(prev_id) = prev_selected_id {
                                            let current_code = source_code.lock_ref().to_string();
                                            let mut examples = (**custom_examples.lock_ref()).clone();
                                            if let Some((_, _, code)) = examples.iter_mut().find(|(eid, _, _)| eid == &prev_id) {
                                                *code = current_code;
                                            }
                                            custom_examples.set(Rc::new(examples));
                                        }

                                        // Load new example's code
                                        let examples = custom_examples.lock_ref();
                                        if let Some((_, _, code)) = examples.iter().find(|(eid, _, _)| eid == &id) {
                                            let code = code.clone();
                                            drop(examples);

                                            // Clear saved state
                                            local_storage().remove(STATES_STORAGE_KEY);
                                            local_storage().remove(OLD_SOURCE_CODE_STORAGE_KEY);
                                            local_storage().remove(OLD_SPAN_ID_PAIRS_STORAGE_KEY);
                                            clear_prefixed_storage_keys(&["list_calls:", "list_removed:"]);

                                            // Update URL (use custom-example parameter)
                                            set_custom_example_in_url(&name);

                                            // Set as current file
                                            let filename = format!("{}.bn", name);
                                            let mut new_files = BTreeMap::new();
                                            new_files.insert(filename.clone(), code.clone());
                                            files.set(Rc::new(new_files));
                                            current_file.set(filename);
                                            source_code.set(Rc::new(Cow::from(code)));
                                            run_command.set(Some(RunCommand { filename: None }));

                                            // Update selection (by ID)
                                            selected_custom_example.set(Some(id.clone()));
                                        }
                                    }
                                })
                                .update_raw_el({
                                    let editing_custom_example = editing_custom_example.clone();
                                    let edit_text = edit_text.clone();
                                    let name_for_dblclick = name_for_dblclick.clone();
                                    let id_for_dblclick = id_for_dblclick.clone();
                                    move |raw_el| {
                                        raw_el.event_handler(move |_: events::DoubleClick| {
                                            // Start editing on double-click (store ID)
                                            edit_text.set(name_for_dblclick.clone());
                                            editing_custom_example.set(Some(id_for_dblclick.clone()));
                                        })
                                    }
                                })
                                .right_either()
                        }
                    }
                })
            )
            .item(
                // Delete button (×)
                Button::new()
                    .s(Padding::new().left(4).right(10).y(7))
                    .s(RoundedCorners::new().right(24))
                    .s(Font::new().size(12).weight(FontWeight::Bold))
                    .s(Background::new().color_signal(
                        delete_hovered.signal().map(|h| {
                            if h {
                                color!("rgba(180, 80, 80, 0.55)")
                            } else {
                                color!("rgba(35, 50, 35, 0.35)")
                            }
                        })
                    ))
                    .s(Borders::new()
                        .right(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                        .top(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                        .bottom(Border::new().color(color!("rgba(100, 160, 100, 0.4)")).width(1))
                    )
                    .s(Font::new().color_signal(
                        delete_hovered.signal().map(|h| {
                            if h {
                                color!("rgba(255, 200, 200, 0.95)")
                            } else {
                                color!("rgba(150, 200, 150, 0.6)")
                            }
                        })
                    ))
                    .label("×")
                    .on_hovered_change(move |is_hovered| delete_hovered.set(is_hovered))
                    .on_press({
                        let id = id.clone();
                        let custom_examples = self.custom_examples.clone();
                        let selected_custom_example = self.selected_custom_example.clone();
                        move || {
                            // Clear selection if we're deleting the selected example
                            if selected_custom_example.lock_ref().as_ref() == Some(&id) {
                                selected_custom_example.set(None);
                            }
                            // Remove custom example by ID
                            let mut new_examples = (**custom_examples.lock_ref()).clone();
                            if let Some(idx) = new_examples.iter().position(|(eid, _, _)| eid == &id) {
                                new_examples.remove(idx);
                            }
                            custom_examples.set(Rc::new(new_examples));
                        }
                    })
            )
    }

}

// Force size UI helper functions
fn force_size_input(value: Mutable<String>, label_text: &'static str) -> impl Element {
    let focused = Mutable::new(false);
    Row::new()
        .s(Gap::new().x(2))
        .s(Align::new().center_y())
        .item(
            El::new()
                .s(Font::new().size(10).color(color!("rgba(255,255,255,0.4)")))
                .child(label_text)
        )
        .item(
            TextInput::new()
                .s(Width::exact(45))
                .s(Height::exact(22))
                .s(Padding::new().x(4))
                .s(Font::new().size(12).color(color!("rgba(255,255,255,0.9)")))
                .s(Background::new().color_signal(
                    focused.signal().map_bool(
                        || color!("rgba(255,255,255,0.15)"),
                        || color!("rgba(255,255,255,0.08)")
                    )
                ))
                .s(RoundedCorners::all(4))
                .s(Borders::all_signal(
                    focused.signal().map_bool(
                        || Border::new().width(1).color(color!("rgba(100,150,255,0.5)")),
                        || Border::new().width(1).color(color!("rgba(255,255,255,0.1)"))
                    )
                ))
                .label_hidden(label_text)
                .text_signal(value.signal_cloned())
                .on_focused_change(move |is_focused| focused.set(is_focused))
                .on_change({
                    let value = value.clone();
                    move |text| value.set(text)
                })
                .placeholder(Placeholder::new("700"))
        )
}

fn force_size_apply_button(
    width_input: Mutable<String>,
    height_input: Mutable<String>,
    forced_preview_size: Mutable<Option<(u32, u32)>>,
) -> impl Element {
    let hovered = Mutable::new(false);
    Button::new()
        .s(Padding::new().x(8).y(4))
        .s(RoundedCorners::all(4))
        .s(Background::new().color_signal(
            hovered.signal().map_bool(
                || color!("rgba(100,180,100,0.3)"),
                || color!("rgba(100,180,100,0.15)")
            )
        ))
        .s(Font::new().size(11).weight(FontWeight::Medium).color(color!("rgba(180,255,180,0.9)")))
        .label("Apply")
        .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
        .on_press(move || {
            let w = width_input.get_cloned().parse::<u32>().unwrap_or(700);
            let h = height_input.get_cloned().parse::<u32>().unwrap_or(700);
            forced_preview_size.set(Some((w, h)));
        })
}

fn force_size_auto_button(
    forced_preview_size: Mutable<Option<(u32, u32)>>,
    force_size_expanded: Mutable<bool>,
) -> impl Element {
    let hovered = Mutable::new(false);
    Button::new()
        .s(Padding::new().x(8).y(4))
        .s(RoundedCorners::all(4))
        .s(Background::new().color_signal(
            hovered.signal().map_bool(
                || color!("rgba(180,180,255,0.3)"),
                || color!("rgba(180,180,255,0.15)")
            )
        ))
        .s(Font::new().size(11).weight(FontWeight::Medium).color(color!("rgba(200,200,255,0.9)")))
        .label("Auto")
        .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
        .on_press(move || {
            forced_preview_size.set(None);
            force_size_expanded.set(false);
        })
}

fn force_size_toggle_button(force_size_expanded: Mutable<bool>) -> impl Element {
    let hovered = Mutable::new(false);
    Button::new()
        .s(Padding::new().x(10).y(5))
        .s(RoundedCorners::all(4))
        .s(Background::new().color_signal(
            hovered.signal().map_bool(
                || color!("rgba(255,255,255,0.12)"),
                || color!("rgba(255,255,255,0.06)")
            )
        ))
        .s(Font::new().size(12).weight(FontWeight::Medium).color(color!("rgba(255,255,255,0.7)")))
        .label("Force size")
        .on_hovered_change(move |is_hovered| hovered.set(is_hovered))
        .on_press(move || {
            force_size_expanded.set(true);
        })
}
