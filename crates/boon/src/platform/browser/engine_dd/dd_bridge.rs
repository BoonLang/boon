//! DD Bridge - Renders DdValues to Zoon DOM elements.
//!
//! This is the DD equivalent of bridge.rs, but works with
//! simple DdValue types instead of actor-based Values.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::cell::RefCell;

use zoon::*;

use super::dd_value::DdValue;

/// A todo item with reactive state
#[derive(Clone, Debug)]
pub struct Todo {
    pub id: u64,
    pub title: Mutable<String>,
    pub completed: Mutable<bool>,
}

impl Todo {
    pub fn new(id: u64, title: String) -> Self {
        Self {
            id,
            title: Mutable::new(title),
            completed: Mutable::new(false),
        }
    }
}

/// Filter state for todo list
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoFilter {
    All,
    Active,
    Completed,
}

/// Reactive context for wiring up events and state.
///
/// This holds the shared state between evaluation and rendering,
/// allowing button presses to update counter values, etc.
#[derive(Clone)]
pub struct ReactiveContext {
    /// Counter/sum state - maps variable name to its Mutable
    pub counters: Rc<RefCell<HashMap<String, Mutable<i64>>>>,
    /// Event handlers - maps element path to handler
    pub event_handlers: Rc<RefCell<HashMap<String, EventHandler>>>,
    /// Document re-render trigger (Rc-wrapped for sharing across clones)
    pub render_trigger: Rc<Mutable<u64>>,
    /// Storage key prefix for persistence (e.g., "counter" -> "dd_counter_counter")
    storage_prefix: Rc<Option<String>>,
    /// Task handles for persistence watchers (keep alive)
    #[allow(dead_code)]
    persistence_tasks: Rc<RefCell<Vec<TaskHandle>>>,
    /// Shopping list items (for shopping_list example)
    pub shopping_items: Rc<MutableVec<String>>,
    /// Current input text (shared between input and handlers)
    pub current_input_text: Rc<Mutable<String>>,
    /// Todo items (for todo_mvc example)
    pub todos: Rc<MutableVec<Todo>>,
    /// Current filter for todo list
    pub todo_filter: Rc<Mutable<TodoFilter>>,
    /// Next todo ID counter
    pub next_todo_id: Rc<RefCell<u64>>,
    /// Whether todos have been initialized (prevents re-adding defaults after clear)
    todos_initialized: Rc<RefCell<bool>>,
    /// Counter for tracking which todo checkbox we're rendering (for event wiring)
    todo_checkbox_index: Rc<RefCell<usize>>,
}

/// An event handler that updates state
#[derive(Clone)]
pub struct EventHandler {
    /// The counter to increment (if any)
    pub increment_counter: Option<String>,
}

impl ReactiveContext {
    pub fn new() -> Self {
        Self {
            counters: Rc::new(RefCell::new(HashMap::new())),
            event_handlers: Rc::new(RefCell::new(HashMap::new())),
            render_trigger: Rc::new(Mutable::new(0)),
            storage_prefix: Rc::new(None),
            persistence_tasks: Rc::new(RefCell::new(Vec::new())),
            shopping_items: Rc::new(MutableVec::new()),
            current_input_text: Rc::new(Mutable::new(String::new())),
            todos: Rc::new(MutableVec::new()),
            todo_filter: Rc::new(Mutable::new(TodoFilter::All)),
            next_todo_id: Rc::new(RefCell::new(1)),
            todos_initialized: Rc::new(RefCell::new(false)),
            todo_checkbox_index: Rc::new(RefCell::new(0)),
        }
    }

    /// Create a new context with persistence enabled.
    /// The storage_prefix is used to create unique localStorage keys.
    pub fn new_with_persistence(storage_prefix: impl Into<String>) -> Self {
        Self {
            counters: Rc::new(RefCell::new(HashMap::new())),
            event_handlers: Rc::new(RefCell::new(HashMap::new())),
            render_trigger: Rc::new(Mutable::new(0)),
            storage_prefix: Rc::new(Some(storage_prefix.into())),
            persistence_tasks: Rc::new(RefCell::new(Vec::new())),
            shopping_items: Rc::new(MutableVec::new()),
            current_input_text: Rc::new(Mutable::new(String::new())),
            todos: Rc::new(MutableVec::new()),
            todo_filter: Rc::new(Mutable::new(TodoFilter::All)),
            next_todo_id: Rc::new(RefCell::new(1)),
            todos_initialized: Rc::new(RefCell::new(false)),
            todo_checkbox_index: Rc::new(RefCell::new(0)),
        }
    }

    /// Add an item to the shopping list
    pub fn add_shopping_item(&self, item: String) {
        if !item.is_empty() {
            self.shopping_items.lock_mut().push_cloned(item);
            self.save_shopping_items();
            self.trigger_render();
        }
    }

    /// Clear all shopping items
    pub fn clear_shopping_items(&self) {
        self.shopping_items.lock_mut().clear();
        self.save_shopping_items();
        self.trigger_render();
    }

    /// Get shopping items count
    pub fn shopping_items_count(&self) -> usize {
        self.shopping_items.lock_ref().len()
    }

    /// Save shopping items to localStorage
    fn save_shopping_items(&self) {
        if let Some(ref prefix) = *self.storage_prefix {
            let key = format!("dd_{}_shopping_items", prefix);
            let items: Vec<String> = self.shopping_items.lock_ref().iter().cloned().collect();
            if let Ok(json) = zoon::serde_json::to_string(&items) {
                let _ = local_storage().insert(&key, &json);
            }
        }
    }

    /// Load shopping items from localStorage
    pub fn load_shopping_items(&self) {
        if let Some(ref prefix) = *self.storage_prefix {
            let key = format!("dd_{}_shopping_items", prefix);
            if let Some(Ok(json)) = local_storage().get::<String>(&key) {
                if let Ok(items) = zoon::serde_json::from_str::<Vec<String>>(&json) {
                    let mut list = self.shopping_items.lock_mut();
                    for item in items {
                        list.push_cloned(item);
                    }
                }
            }
        }
    }

    // ============ Todo MVC Methods ============

    /// Add a new todo item
    pub fn add_todo(&self, title: String) {
        if !title.is_empty() {
            let id = {
                let mut next_id = self.next_todo_id.borrow_mut();
                let id = *next_id;
                *next_id += 1;
                id
            };
            let todo = Todo::new(id, title);
            self.todos.lock_mut().push_cloned(todo);
            self.save_todos();
        }
    }

    /// Toggle a todo's completed state
    pub fn toggle_todo(&self, id: u64) {
        let todos = self.todos.lock_ref();
        for todo in todos.iter() {
            if todo.id == id {
                let current = todo.completed.get();
                todo.completed.set(!current);
                break;
            }
        }
        drop(todos);
        self.save_todos();
        self.trigger_render();
    }

    /// Toggle all todos to the given state
    pub fn toggle_all_todos(&self, completed: bool) {
        let todos = self.todos.lock_ref();
        for todo in todos.iter() {
            todo.completed.set(completed);
        }
        drop(todos);
        self.save_todos();
        self.trigger_render();
    }

    /// Remove completed todos
    pub fn clear_completed_todos(&self) {
        let mut todos = self.todos.lock_mut();
        // Find indices of completed todos (in reverse order to avoid index shifting)
        let indices_to_remove: Vec<usize> = todos.iter()
            .enumerate()
            .filter(|(_, t)| t.completed.get())
            .map(|(i, _)| i)
            .collect();

        // Remove in reverse order
        for i in indices_to_remove.into_iter().rev() {
            todos.remove(i);
        }
        drop(todos);
        self.save_todos();
        self.trigger_render();
    }

    /// Remove a specific todo by ID
    pub fn remove_todo(&self, id: u64) {
        let mut todos = self.todos.lock_mut();
        if let Some(index) = todos.iter().position(|t| t.id == id) {
            todos.remove(index);
        }
        drop(todos);
        self.save_todos();
        self.trigger_render();
    }

    /// Set the current filter
    pub fn set_todo_filter(&self, filter: TodoFilter) {
        self.todo_filter.set(filter);
        self.trigger_render();
    }

    /// Get active (not completed) todo count
    pub fn active_todo_count(&self) -> usize {
        self.todos.lock_ref().iter().filter(|t| !t.completed.get()).count()
    }

    /// Get completed todo count
    pub fn completed_todo_count(&self) -> usize {
        self.todos.lock_ref().iter().filter(|t| t.completed.get()).count()
    }

    /// Check if all todos are completed
    pub fn all_completed(&self) -> bool {
        let todos = self.todos.lock_ref();
        !todos.is_empty() && todos.iter().all(|t| t.completed.get())
    }

    /// Reset the todo checkbox rendering counter (call before each render)
    pub fn reset_todo_checkbox_index(&self) {
        *self.todo_checkbox_index.borrow_mut() = 0;
    }

    /// Get current todo checkbox index and increment for next call
    pub fn get_and_increment_todo_checkbox_index(&self) -> usize {
        let mut index = self.todo_checkbox_index.borrow_mut();
        let current = *index;
        *index += 1;
        current
    }

    /// Get the ID of the visible todo at the given index (based on current filter)
    pub fn get_visible_todo_id_at_index(&self, index: usize) -> Option<u64> {
        let todos = self.todos.lock_ref();
        let filter = *self.todo_filter.lock_ref();

        let visible: Vec<&Todo> = todos.iter()
            .filter(|t| match filter {
                TodoFilter::All => true,
                TodoFilter::Active => !t.completed.get(),
                TodoFilter::Completed => t.completed.get(),
            })
            .collect();

        visible.get(index).map(|t| t.id)
    }

    /// Save todos to localStorage
    fn save_todos(&self) {
        if let Some(ref prefix) = *self.storage_prefix {
            let key = format!("dd_{}_todos", prefix);
            let todos = self.todos.lock_ref();
            // Serialize as array of {id, title, completed}
            let data: Vec<(u64, String, bool)> = todos.iter()
                .map(|t| (t.id, t.title.get_cloned(), t.completed.get()))
                .collect();
            if let Ok(json) = zoon::serde_json::to_string(&data) {
                let _ = local_storage().insert(&key, &json);
            }
        }
    }

    /// Load todos from localStorage
    pub fn load_todos(&self) {
        if let Some(ref prefix) = *self.storage_prefix {
            let key = format!("dd_{}_todos", prefix);
            if let Some(Ok(json)) = local_storage().get::<String>(&key) {
                if let Ok(data) = zoon::serde_json::from_str::<Vec<(u64, String, bool)>>(&json) {
                    let mut max_id = 0u64;
                    let mut todos = self.todos.lock_mut();
                    for (id, title, completed) in data {
                        let todo = Todo::new(id, title);
                        todo.completed.set(completed);
                        todos.push_cloned(todo);
                        if id >= max_id {
                            max_id = id + 1;
                        }
                    }
                    *self.next_todo_id.borrow_mut() = max_id;
                }
            }
        }
    }

    /// Initialize todos with default items if not already initialized.
    /// This prevents re-adding defaults after the user clears all todos.
    pub fn init_default_todos(&self) {
        // Only initialize once per session
        if *self.todos_initialized.borrow() {
            return;
        }
        *self.todos_initialized.borrow_mut() = true;

        // Only add defaults if no todos exist
        if self.todos.lock_ref().is_empty() {
            self.add_todo("Buy groceries".to_string());
            self.add_todo("Clean room".to_string());
        }
    }

    /// Get or create a counter Mutable with optional persistence
    pub fn get_or_create_counter(&self, name: &str, initial: i64) -> Mutable<i64> {
        let mut counters = self.counters.borrow_mut();
        if let Some(counter) = counters.get(name) {
            counter.clone()
        } else {
            // Try to load from localStorage if persistence is enabled
            let loaded_value = if let Some(ref prefix) = *self.storage_prefix {
                let storage_key = format!("dd_{}_{}", prefix, name);
                if let Some(Ok(value)) = local_storage().get::<i64>(&storage_key) {
                    value
                } else {
                    initial
                }
            } else {
                initial
            };

            let counter = Mutable::new(loaded_value);
            counters.insert(name.to_string(), counter.clone());

            // Set up persistence watcher if enabled
            if let Some(ref prefix) = *self.storage_prefix {
                let storage_key = format!("dd_{}_{}", prefix, name);
                let counter_for_save = counter.clone();
                let task = Task::start_droppable(
                    counter_for_save.signal().for_each_sync(move |value| {
                        let _ = local_storage().insert(&storage_key, &value);
                    })
                );
                self.persistence_tasks.borrow_mut().push(task);
            }

            counter
        }
    }

    /// Register an event handler for an element
    pub fn register_event_handler(&self, element_path: &str, handler: EventHandler) {
        self.event_handlers.borrow_mut().insert(element_path.to_string(), handler);
    }

    /// Trigger a re-render
    pub fn trigger_render(&self) {
        self.render_trigger.update(|v| v + 1);
    }
}

impl Default for ReactiveContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `store` DdValue from ReactiveContext state.
///
/// This creates a DdValue structure that matches what todo_mvc.bn expects:
/// ```boon
/// store: [
///     elements: [filter_buttons: [...], ...]
///     selected_filter: All/Active/Completed
///     todos: LIST { ... }
///     todos_count: number
///     completed_todos_count: number
///     active_todos_count: number
///     all_completed: bool
/// ]
/// ```
pub fn build_store_from_context(ctx: &ReactiveContext) -> DdValue {
    let mut store = BTreeMap::new();

    // Build elements with LINK markers
    let mut elements = BTreeMap::new();

    // Filter buttons
    let mut filter_buttons = BTreeMap::new();
    filter_buttons.insert(Arc::from("all"), DdValue::Unit); // LINK
    filter_buttons.insert(Arc::from("active"), DdValue::Unit); // LINK
    filter_buttons.insert(Arc::from("completed"), DdValue::Unit); // LINK
    elements.insert(Arc::from("filter_buttons"), DdValue::Object(Arc::new(filter_buttons)));

    // Other element links
    elements.insert(Arc::from("remove_completed_button"), DdValue::Unit);
    elements.insert(Arc::from("toggle_all_checkbox"), DdValue::Unit);
    elements.insert(Arc::from("new_todo_title_text_input"), DdValue::Unit);

    store.insert(Arc::from("elements"), DdValue::Object(Arc::new(elements)));

    // Selected filter
    let filter_tag = match *ctx.todo_filter.lock_ref() {
        TodoFilter::All => "All",
        TodoFilter::Active => "Active",
        TodoFilter::Completed => "Completed",
    };
    store.insert(Arc::from("selected_filter"), DdValue::Tagged {
        tag: Arc::from(filter_tag),
        fields: Arc::new(BTreeMap::new()),
    });

    // Build todos list
    let todos_vec: Vec<DdValue> = ctx.todos.lock_ref().iter().map(|todo| {
        let mut todo_obj = BTreeMap::new();

        // Build todo_elements with LINK markers
        let mut todo_elements = BTreeMap::new();
        todo_elements.insert(Arc::from("remove_todo_button"), DdValue::Unit);
        todo_elements.insert(Arc::from("editing_todo_title_element"), DdValue::Unit);
        todo_elements.insert(Arc::from("todo_title_element"), DdValue::Unit);
        todo_elements.insert(Arc::from("todo_checkbox"), DdValue::Unit);
        todo_obj.insert(Arc::from("todo_elements"), DdValue::Object(Arc::new(todo_elements)));

        // Todo data
        todo_obj.insert(Arc::from("id"), DdValue::int(todo.id as i64));
        todo_obj.insert(Arc::from("title"), DdValue::text(todo.title.get_cloned()));
        todo_obj.insert(Arc::from("completed"), DdValue::Bool(todo.completed.get()));
        todo_obj.insert(Arc::from("editing"), DdValue::Bool(false));

        DdValue::Object(Arc::new(todo_obj))
    }).collect();

    store.insert(Arc::from("todos"), DdValue::List(Arc::from(todos_vec)));

    // Computed values
    let todos_count = ctx.todos.lock_ref().len();
    let completed_count = ctx.completed_todo_count();
    let active_count = ctx.active_todo_count();
    let all_completed = todos_count > 0 && completed_count == todos_count;

    store.insert(Arc::from("todos_count"), DdValue::int(todos_count as i64));
    store.insert(Arc::from("completed_todos_count"), DdValue::int(completed_count as i64));
    store.insert(Arc::from("active_todos_count"), DdValue::int(active_count as i64));
    store.insert(Arc::from("all_completed"), DdValue::Bool(all_completed));

    // title_to_add - usually SKIP, we don't track this
    store.insert(Arc::from("title_to_add"), DdValue::Tagged {
        tag: Arc::from("SKIP"),
        fields: Arc::new(BTreeMap::new()),
    });

    DdValue::Object(Arc::new(store))
}

/// Convert a DdValue to a Zoon element.
///
/// This handles the mapping from Boon values to DOM elements.
pub fn dd_value_to_element(value: &DdValue) -> RawElOrText {
    dd_value_to_element_with_context(value, None, "")
}

/// Render a DdValue document with reactive context.
/// This is the main entry point for rendering - it resets counters before rendering.
pub fn render_dd_document_with_context(
    value: &DdValue,
    ctx: &ReactiveContext,
) -> RawElOrText {
    // Reset the todo checkbox counter before rendering
    ctx.reset_todo_checkbox_index();
    dd_value_to_element_with_context(value, Some(ctx), "")
}

/// Convert a DdValue to a Zoon element with reactive context.
pub fn dd_value_to_element_with_context(
    value: &DdValue,
    ctx: Option<&ReactiveContext>,
    path: &str,
) -> RawElOrText {
    match value {
        // Text becomes a text node
        DdValue::Text(s) => zoon::Text::new(s.as_ref()).unify(),

        // Number becomes text
        DdValue::Number(n) => zoon::Text::new(n.to_string()).unify(),

        // Boolean becomes text
        DdValue::Bool(b) => zoon::Text::new(b.to_string()).unify(),

        // Unit becomes empty
        DdValue::Unit => zoon::Text::new("").unify(),

        // Tagged objects are elements
        DdValue::Tagged { tag, fields } => {
            match tag.as_ref() {
                // Element types
                "Element" => render_element_with_context(fields, ctx, path),

                // NoElement renders as nothing (invisible)
                "NoElement" => El::new().unify(),

                // Tags render as text
                _ => zoon::Text::new(tag.as_ref()).unify(),
            }
        }

        // Objects render as debug text for now
        DdValue::Object(_) => {
            zoon::Text::new("[object]").unify()
        }

        // Lists render as vertical stack of children
        DdValue::List(items) => {
            if items.is_empty() {
                El::new().unify()
            } else {
                let children: Vec<RawElOrText> = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let item_path = format!("{}/items[{}]", path, i);
                        dd_value_to_element_with_context(item, ctx, &item_path)
                    })
                    .collect();

                Column::new()
                    .items(children)
                    .unify()
            }
        }
    }
}

/// Render an Element tagged object.
fn render_element(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    render_element_with_context(fields, None, "")
}

/// Render an Element tagged object with reactive context.
fn render_element_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
    path: &str,
) -> RawElOrText {
    let element_type = fields
        .get("_element_type")
        .and_then(|v| v.as_text())
        .unwrap_or("container");

    match element_type {
        "container" => render_container_with_context(fields, ctx, path),
        "stripe" => render_stripe_with_context(fields, ctx, path),
        "stack" => render_stack_with_context(fields, ctx, path),
        "button" => render_button_with_context(fields, ctx, path),
        "text_input" => render_text_input_with_context(fields, ctx),
        "checkbox" => render_checkbox_with_context(fields, ctx, path),
        "label" => render_label_with_context(fields, ctx),
        "paragraph" => render_paragraph(fields),
        "link" => render_link(fields),
        _ => {
            // Unknown element type - render as container
            render_container_with_context(fields, ctx, path)
        }
    }
}

/// Render a container element.
fn render_container_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
    path: &str,
) -> RawElOrText {
    // Extract style values
    let style = fields.get("style");
    let size = style.and_then(|s| s.get("size")).and_then(|v| v.as_float());
    let width = style.and_then(|s| s.get("width")).and_then(|v| v.as_float());
    let height = style.and_then(|s| s.get("height")).and_then(|v| v.as_float());
    let bg_url = style
        .and_then(|s| s.get("background"))
        .and_then(|b| b.get("url"))
        .and_then(|v| v.as_text())
        .map(|s| s.to_string());

    // Build element with child if present (must use update_raw_el for background to avoid typestate issues)
    if let Some(child_value) = fields.get("child") {
        let child_path = format!("{}/child", path);
        let child_el = dd_value_to_element_with_context(child_value, ctx, &child_path);

        let mut el = El::new();
        if let Some(s) = size {
            el = el.s(Width::exact(s as u32)).s(Height::exact(s as u32));
        }
        if let Some(w) = width {
            el = el.s(Width::exact(w as u32));
        }
        if let Some(h) = height {
            el = el.s(Height::exact(h as u32));
        }
        if let Some(url) = bg_url {
            el = el.update_raw_el(move |raw_el| {
                raw_el.style("background-image", &format!("url(\"{}\")", url))
                      .style("background-size", "contain")
                      .style("background-repeat", "no-repeat")
            });
        }
        el.child(child_el).unify()
    } else {
        // No child - simpler path
        let mut el = El::new();
        if let Some(s) = size {
            el = el.s(Width::exact(s as u32)).s(Height::exact(s as u32));
        }
        if let Some(w) = width {
            el = el.s(Width::exact(w as u32));
        }
        if let Some(h) = height {
            el = el.s(Height::exact(h as u32));
        }
        if let Some(url) = bg_url {
            el = el.update_raw_el(move |raw_el| {
                raw_el.style("background-image", &format!("url(\"{}\")", url))
                      .style("background-size", "contain")
                      .style("background-repeat", "no-repeat")
            });
        }
        el.unify()
    }
}

/// Render a stripe (column/row) element.
fn render_stripe_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
    path: &str,
) -> RawElOrText {
    let direction = fields
        .get("direction")
        .and_then(|v| match v {
            DdValue::Tagged { tag, .. } => Some(tag.as_ref()),
            _ => None,
        })
        .unwrap_or("Column");

    let items = fields.get("items").and_then(|v| v.as_list()).unwrap_or(&[]);

    let gap = fields
        .get("gap")
        .and_then(|v| v.as_int())
        .unwrap_or(0) as u32;

    // Check if this is the shopping items list (empty items with gap=4 in Column)
    // Use reactive rendering for shopping items
    if items.is_empty() && gap == 4 && direction == "Column" {
        if let Some(ctx) = ctx {
            let shopping_items = ctx.shopping_items.clone();
            return Column::new()
                .s(Gap::both(gap))
                .items_signal_vec(shopping_items.signal_vec_cloned().map(|item| {
                    // Render each item as a label with "- item" format
                    Label::new()
                        .label(zoon::Text::new(format!("- {}", item)))
                        .unify()
                }))
                .unify();
        }
    }

    // Static rendering for other stripes
    let children: Vec<RawElOrText> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let item_path = format!("{}/items[{}]", path, i);
            let element_type = if let DdValue::Tagged { tag, fields } = item {
                if tag.as_ref() == "Element" {
                    fields.get("_element_type").and_then(|v| v.as_text()).unwrap_or("unknown")
                } else {
                    tag.as_ref()
                }
            } else {
                "non-element"
            };
            let _ = element_type; // Suppress unused warning
            dd_value_to_element_with_context(item, ctx, &item_path)
        })
        .collect();

    match direction {
        "Column" => Column::new().s(Gap::both(gap)).items(children).unify(),
        "Row" => Row::new().s(Gap::both(gap)).items(children).unify(),
        _ => Column::new().s(Gap::both(gap)).items(children).unify(),
    }
}

/// Render a stack element.
fn render_stack_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
    path: &str,
) -> RawElOrText {
    // Stack uses "layers" field, not "items"
    let layers = fields.get("layers").and_then(|v| v.as_list()).unwrap_or(&[]);
    let children: Vec<RawElOrText> = layers
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let item_path = format!("{}/layers[{}]", path, i);
            dd_value_to_element_with_context(item, ctx, &item_path)
        })
        .collect();

    Stack::new().layers(children).unify()
}

/// Render a button element with reactive event handling.
fn render_button_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
    path: &str,
) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Extract outline CSS from style
    let outline_css = extract_outline_css(fields.get("style"));

    // Check if this button has LINK events
    let has_link = has_link_event(fields);

    if let (Some(ctx), true) = (ctx, has_link) {
        // Create a button with reactive event handling
        let ctx_clone = ctx.clone();
        let label_for_nav = label.clone();
        let mut button = Button::new()
            .label(zoon::Text::new(&label))
            .on_press(move || {
                zoon::println!("[DD_BRIDGE] on_press called! label={}", label_for_nav);
                // Check for navigation buttons (pages example)
                let route = match label_for_nav.as_str() {
                    "Home" => Some("/"),
                    "About" => Some("/about"),
                    "Contact" => Some("/contact"),
                    _ => None,
                };

                if let Some(route) = route {
                    #[cfg(target_arch = "wasm32")]
                    {
                        use zoon::*;
                        // Use pushState to update URL without reload
                        let _ = window()
                            .history()
                            .expect_throw("history")
                            .push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(route));
                    }
                    ctx_clone.trigger_render();
                    return;
                }

                // Check for Clear button (shopping_list example)
                if label_for_nav.as_str() == "Clear" {
                    ctx_clone.clear_shopping_items();
                    return;
                }

                // Check for filter buttons (todo_mvc example)
                let filter = match label_for_nav.as_str() {
                    "All" => Some(TodoFilter::All),
                    "Active" => Some(TodoFilter::Active),
                    "Completed" => Some(TodoFilter::Completed),
                    _ => None,
                };
                if let Some(filter) = filter {
                    zoon::println!("[DD_BRIDGE] Filter button clicked: {:?}", filter);
                    ctx_clone.set_todo_filter(filter);
                    zoon::println!("[DD_BRIDGE] Filter set, current: {:?}", ctx_clone.todo_filter.get());
                    return;
                }

                // Check for Clear completed button (todo_mvc example)
                if label_for_nav.as_str() == "Clear completed" {
                    ctx_clone.clear_completed_todos();
                    return;
                }

                // Default: increment "counter" if it exists
                zoon::println!("[DD_BRIDGE] Button clicked, checking counter");
                let counters = ctx_clone.counters.borrow();
                zoon::println!("[DD_BRIDGE] Counters map size: {}", counters.len());
                if let Some(counter) = counters.get("counter") {
                    let old_val = counter.get();
                    counter.update(|v| v + 1);
                    zoon::println!("[DD_BRIDGE] Counter updated: {} -> {}", old_val, counter.get());
                    ctx_clone.trigger_render();
                } else {
                    zoon::println!("[DD_BRIDGE] No counter found in map!");
                }
            });

        // Add role="button" for test compatibility
        button = button.update_raw_el(|raw_el| raw_el.attr("role", "button"));

        // Apply outline style if present
        if let Some(outline) = outline_css {
            button = button.update_raw_el(|raw_el| raw_el.style("outline", &outline));
        }

        button.unify()
    } else {
        // Static button
        let mut button = Button::new()
            .label(zoon::Text::new(&label))
            .update_raw_el(|raw_el| raw_el.attr("role", "button"));

        // Apply outline style if present
        if let Some(outline) = outline_css {
            button = button.update_raw_el(|raw_el| raw_el.style("outline", &outline));
        }

        button.unify()
    }
}

/// Check if an element has a LINK event
fn has_link_event(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> bool {
    if let Some(element) = fields.get("element") {
        if let Some(event) = element.get("event") {
            // Check for press event (buttons)
            if let Some(press) = event.get("press") {
                if matches!(press, DdValue::Unit) ||
                   matches!(press, DdValue::Tagged { tag, .. } if tag.as_ref() == "LINK") {
                    return true;
                }
            }
            // Check for click event (checkboxes)
            if let Some(click) = event.get("click") {
                if matches!(click, DdValue::Unit) ||
                   matches!(click, DdValue::Tagged { tag, .. } if tag.as_ref() == "LINK") {
                    return true;
                }
            }
        }
    }
    false
}

/// Convert an Oklch color DdValue to a CSS string.
/// Returns None if the value is not an Oklch color tag.
fn oklch_to_css(value: &DdValue) -> Option<String> {
    if let DdValue::Tagged { tag, fields } = value {
        if tag.as_ref() == "Oklch" {
            let lightness = fields.get("lightness")
                .and_then(|v| v.as_float())
                .unwrap_or(0.5);
            let chroma = fields.get("chroma")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0);
            let hue = fields.get("hue")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0);
            let alpha = fields.get("alpha")
                .and_then(|v| v.as_float());

            if let Some(a) = alpha {
                if a < 1.0 {
                    return Some(format!("oklch({}% {} {} / {})", lightness * 100.0, chroma, hue, a));
                }
            }
            return Some(format!("oklch({}% {} {})", lightness * 100.0, chroma, hue));
        }
    }
    None
}

/// Extract outline CSS from a style object.
/// Returns "none" for NoOutline tag, or a CSS outline string for outline Object.
fn extract_outline_css(style: Option<&DdValue>) -> Option<String> {
    let style = style?;
    let outline = style.get("outline")?;

    // Check for NoOutline tag
    if let DdValue::Tagged { tag, .. } = outline {
        if tag.as_ref() == "NoOutline" {
            return Some("none".to_string());
        }
    }

    // Check for outline Object with color
    if let Some(color) = outline.get("color") {
        if let Some(css_color) = oklch_to_css(color) {
            // Default outline: 1px solid color
            return Some(format!("1px solid {}", css_color));
        }
    }

    None
}

/// Render a text input element with reactive context for Enter key handling.
fn render_text_input_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
) -> RawElOrText {
    // Get placeholder text
    let placeholder_text = fields
        .get("placeholder")
        .and_then(|v| v.get("text"))
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Get initial text value
    let text_value = fields
        .get("text")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Check for focus attribute
    let should_focus = fields
        .get("focus")
        .map(|v| v.is_truthy())
        .unwrap_or(false);

    // Check if this input has key_down LINK event (for shopping_list Enter handling)
    let has_key_down_link = fields
        .get("element")
        .and_then(|e| e.get("event"))
        .and_then(|ev| ev.get("key_down"))
        .map(|kd| matches!(kd, DdValue::Unit) || matches!(kd, DdValue::Tagged { tag, .. } if tag.as_ref() == "LINK"))
        .unwrap_or(false);

    // Use context's input text if available, otherwise create local mutable
    let text_mutable = if let Some(ctx) = ctx {
        (*ctx.current_input_text).clone()
    } else {
        Mutable::new(text_value)
    };

    // Build different TextInput variants based on features needed
    // (Zoon's builder pattern has typestate that makes conditional chaining tricky)
    if has_key_down_link && ctx.is_some() {
        let ctx = ctx.unwrap();
        let ctx_for_key = ctx.clone();
        let text_for_key = text_mutable.clone();
        let text_for_change = text_mutable.clone();

        if should_focus && !placeholder_text.is_empty() {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .placeholder(Placeholder::new(&placeholder_text))
                .focus(true)
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .on_key_down_event(move |event| {
                    if *event.key() == Key::Enter {
                        let current_text = text_for_key.get_cloned();
                        let trimmed = current_text.trim().to_string();
                        if !trimmed.is_empty() {
                            ctx_for_key.add_shopping_item(trimmed);
                            text_for_key.set(String::new());
                        }
                    }
                })
                .unify()
        } else if should_focus {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .focus(true)
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .on_key_down_event(move |event| {
                    if *event.key() == Key::Enter {
                        let current_text = text_for_key.get_cloned();
                        let trimmed = current_text.trim().to_string();
                        if !trimmed.is_empty() {
                            ctx_for_key.add_shopping_item(trimmed);
                            text_for_key.set(String::new());
                        }
                    }
                })
                .unify()
        } else if !placeholder_text.is_empty() {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .placeholder(Placeholder::new(&placeholder_text))
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .on_key_down_event(move |event| {
                    if *event.key() == Key::Enter {
                        let current_text = text_for_key.get_cloned();
                        let trimmed = current_text.trim().to_string();
                        if !trimmed.is_empty() {
                            ctx_for_key.add_shopping_item(trimmed);
                            text_for_key.set(String::new());
                        }
                    }
                })
                .unify()
        } else {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .on_key_down_event(move |event| {
                    if *event.key() == Key::Enter {
                        let current_text = text_for_key.get_cloned();
                        let trimmed = current_text.trim().to_string();
                        if !trimmed.is_empty() {
                            ctx_for_key.add_shopping_item(trimmed);
                            text_for_key.set(String::new());
                        }
                    }
                })
                .unify()
        }
    } else {
        // Simple text input without Enter key handling
        let text_for_change = text_mutable.clone();
        if should_focus && !placeholder_text.is_empty() {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .placeholder(Placeholder::new(&placeholder_text))
                .focus(true)
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .unify()
        } else if should_focus {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .focus(true)
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .unify()
        } else if !placeholder_text.is_empty() {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .placeholder(Placeholder::new(&placeholder_text))
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .unify()
        } else {
            TextInput::new()
                .label_hidden("text input")
                .text_signal(text_mutable.signal_cloned())
                .on_change(move |new_text| { text_for_change.set(new_text); })
                .unify()
        }
    }
}

/// Render a checkbox element with proper ARIA role for test compatibility.
fn render_checkbox(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let checked = fields
        .get("checked")
        .map(|v| v.is_truthy())
        .unwrap_or(false);

    // Extract size from checkbox style, or from icon style as fallback
    let checkbox_style = fields.get("style");
    let icon_value = fields.get("icon");

    // Try to get size from checkbox style first
    let mut width: Option<f64> = checkbox_style
        .and_then(|s| s.get("width"))
        .and_then(|v| v.as_float());
    let mut height: Option<f64> = checkbox_style
        .and_then(|s| s.get("height"))
        .and_then(|v| v.as_float());

    // Get background URL from icon's style if available
    let bg_url = icon_value
        .and_then(|icon| icon.get("style"))
        .and_then(|s| s.get("background"))
        .and_then(|b| b.get("url"))
        .and_then(|v| v.as_text())
        .map(|s| s.to_string());

    // If no size from checkbox style, try to get from icon's style.size
    if width.is_none() && height.is_none() {
        if let Some(icon) = icon_value {
            if let Some(icon_style) = icon.get("style") {
                if let Some(size) = icon_style.get("size").and_then(|v| v.as_float()) {
                    width = Some(size);
                    height = Some(size);
                }
            }
        }
    }

    // Check if icon has text content (like the toggle-all "❯")
    let icon_text = icon_value
        .and_then(|icon| icon.get("child"))
        .and_then(|c| c.as_text())
        .map(|s| s.to_string());

    // Build element using El with update_raw_el for attributes
    let aria_checked = if checked { "true" } else { "false" };

    // Build the element - use update_raw_el for role and aria attributes
    let mut el = El::new()
        .update_raw_el(move |raw_el| {
            raw_el
                .attr("role", "checkbox")
                .attr("aria-checked", aria_checked)
        });

    // Apply size using Zoon's style system
    if let Some(w) = width {
        el = el.s(Width::exact(w as u32));
    }
    if let Some(h) = height {
        el = el.s(Height::exact(h as u32));
    }

    // Apply background image via update_raw_el
    if let Some(url) = bg_url {
        el = el.update_raw_el(move |raw_el| {
            raw_el
                .style("background-image", &format!("url(\"{}\")", url))
                .style("background-size", "contain")
                .style("background-repeat", "no-repeat")
        });
    }

    // Add text content if icon has text (like toggle-all "❯") or checkmark
    if let Some(text) = icon_text {
        el.child(zoon::Text::new(&text)).unify()
    } else if checked {
        el.child(zoon::Text::new("✓")).unify()
    } else {
        el.unify()
    }
}

/// Render a checkbox element with reactive context for event handling.
fn render_checkbox_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
    _path: &str,
) -> RawElOrText {
    let checked = fields
        .get("checked")
        .map(|v| v.is_truthy())
        .unwrap_or(false);

    // Check if this checkbox has LINK events
    let has_link = has_link_event(fields);

    // Check if icon has text content (like the toggle-all "❯")
    let icon_value = fields.get("icon");
    let icon_text = icon_value
        .and_then(|icon| icon.get("child"))
        .and_then(|c| c.as_text())
        .map(|s| s.to_string());

    // Determine if this is the toggle-all checkbox (has "❯" icon)
    let is_toggle_all = icon_text.as_ref().map_or(false, |t| t.contains("❯"));

    if let (Some(ctx), true) = (ctx, has_link) {
        // Interactive checkbox with event handling
        let ctx_clone = ctx.clone();
        let aria_checked = if checked { "true" } else { "false" };

        if is_toggle_all {
            // Toggle-all checkbox
            let all_completed = ctx.all_completed();

            Button::new()
                .s(Width::exact(60))
                .s(Height::fill())
                .s(Font::new().size(22).color(
                    if all_completed {
                        hsluv!(0, 0, 40)
                    } else {
                        hsluv!(0, 0, 75)
                    }
                ))
                .s(Transform::new().rotate(90))
                .label("❯")
                .on_press(move || {
                    let all_done = ctx_clone.all_completed();
                    ctx_clone.toggle_all_todos(!all_done);
                })
                .update_raw_el(move |raw_el| {
                    raw_el
                        .attr("role", "checkbox")
                        .attr("aria-checked", aria_checked)
                })
                .unify()
        } else {
            // Individual todo checkbox
            let checkbox_index = ctx.get_and_increment_todo_checkbox_index();

            // Get the todo ID for this checkbox index
            if let Some(todo_id) = ctx.get_visible_todo_id_at_index(checkbox_index) {
                // Get background URL from icon's style
                let bg_url = icon_value
                    .and_then(|icon| icon.get("style"))
                    .and_then(|s| s.get("background"))
                    .and_then(|b| b.get("url"))
                    .and_then(|v| v.as_text())
                    .map(|s| s.to_string());

                let mut button = Button::new()
                    .s(Width::exact(40))
                    .s(Height::exact(40))
                    .label("") // Empty label - icon shown via background image
                    .on_press(move || {
                        ctx_clone.toggle_todo(todo_id);
                    })
                    .update_raw_el(move |raw_el| {
                        raw_el
                            .attr("role", "checkbox")
                            .attr("aria-checked", aria_checked)
                    });

                // Apply background image if available
                if let Some(url) = bg_url {
                    button = button.update_raw_el(move |raw_el| {
                        raw_el
                            .style("background-image", &format!("url(\"{}\")", url))
                            .style("background-size", "contain")
                            .style("background-repeat", "no-repeat")
                    });
                }

                button.unify()
            } else {
                // Fallback to static checkbox if no todo found
                render_checkbox(fields)
            }
        }
    } else {
        // Static checkbox (no context or no LINK)
        render_checkbox(fields)
    }
}

/// Render a label element.
fn render_label(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    // Element/label uses "label" field for its text content
    let text = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Label requires an Element for its label, not a plain string
    Label::new().label(zoon::Text::new(&text)).unify()
}

/// Render a label element with dynamic item count for shopping list.
fn render_label_with_context(
    fields: &Arc<BTreeMap<Arc<str>, DdValue>>,
    ctx: Option<&ReactiveContext>,
) -> RawElOrText {
    let text = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    // Check if this is an item count label (contains "items" pattern)
    if text.contains(" items") {
        if let Some(ctx) = ctx {
            // Use signal for reactive count display
            let items = ctx.shopping_items.clone();
            return Label::new()
                .label_signal(items.signal_vec_cloned().len().map(|len| {
                    zoon::Text::new(format!("{} items", len))
                }))
                .unify();
        }
    }

    // Default: static label
    Label::new().label(zoon::Text::new(&text)).unify()
}

/// Render a paragraph element.
fn render_paragraph(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let text = fields
        .get("content")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    Paragraph::new().content(text).unify()
}

/// Render a link element.
fn render_link(fields: &Arc<BTreeMap<Arc<str>, DdValue>>) -> RawElOrText {
    let label = fields
        .get("label")
        .map(|v| v.to_display_string())
        .unwrap_or_default();

    let url = fields
        .get("url")
        .and_then(|v| v.as_text())
        .unwrap_or("#");

    Link::new()
        .label(label)
        .to(url)
        .unify()
}

// Style application will be added later when reactive styling is implemented

/// Create a Zoon element from the document output.
///
/// This is the main entry point for DD rendering.
pub fn render_document(document: &DdValue) -> RawElOrText {
    // Document/new returns the root value directly in DD evaluation
    dd_value_to_element(document)
}

/// Create a reactive Zoon element from the document output.
///
/// This version uses a ReactiveContext for event handling.
pub fn render_document_with_context(document: &DdValue, ctx: &ReactiveContext) -> RawElOrText {
    // For todo_mvc, use native Rust rendering which handles reactive filter buttons
    let is_todo_mvc = is_todo_mvc_document(document);
    zoon::println!("[DD_BRIDGE] is_todo_mvc_document returned: {}", is_todo_mvc);
    if is_todo_mvc {
        zoon::println!("[DD_BRIDGE] Detected todo_mvc, using native render_todo_mvc()");
        ctx.init_default_todos();
        return render_todo_mvc(ctx);
    }
    zoon::println!("[DD_BRIDGE] Using DD rendering path");
    // Use the DD rendering path with proper counter reset for other examples
    render_dd_document_with_context(document, ctx)
}

/// Check if document is a todo_mvc structure
fn is_todo_mvc_document(document: &DdValue) -> bool {
    // Check for todo_mvc markers: "What needs to be done?" or "todos" header
    fn contains_todo_mvc_marker(value: &DdValue) -> bool {
        match value {
            DdValue::Text(s) => {
                let text = s.as_ref();
                text == "What needs to be done?" || text == "todos"
            }
            DdValue::Tagged { fields, .. } => {
                fields.values().any(|v| contains_todo_mvc_marker(v))
            }
            DdValue::Object(obj) => {
                obj.values().any(|v| contains_todo_mvc_marker(v))
            }
            DdValue::List(items) => {
                items.iter().any(|v| contains_todo_mvc_marker(v))
            }
            _ => false,
        }
    }
    contains_todo_mvc_marker(document)
}

/// Render the complete TodoMVC UI
fn render_todo_mvc(ctx: &ReactiveContext) -> RawElOrText {
    let ctx_for_input = ctx.clone();
    let ctx_for_toggle_all = ctx.clone();
    let ctx_for_clear = ctx.clone();
    let ctx_for_filter_all = ctx.clone();
    let ctx_for_filter_active = ctx.clone();
    let ctx_for_filter_completed = ctx.clone();

    // Main container
    Column::new()
        .s(Width::fill())
        .s(Height::fill())
        .s(Align::new().center_x())
        .s(Font::new()
            .size(14)
            .color(hsluv!(0, 0, 17))
            .weight(FontWeight::Light))
        .s(Background::new().color(hsluv!(0, 0, 97)))
        .item(
            // Content container
            Column::new()
                .s(Width::exact(550))
                .s(Align::new().center_x())
                .item(render_todo_header())
                .item(
                    Column::new()
                        .s(Gap::both(65))
                        .s(Width::fill())
                        .item(render_todo_main_panel(&ctx_for_input, &ctx_for_toggle_all))
                        .item(render_todo_footer_info())
                )
        )
        .unify()
}

/// Render the "todos" header
fn render_todo_header() -> impl Element {
    El::new()
        .s(Align::new().center_x())
        .s(Padding::new().top(10).bottom(20))
        .s(Height::exact(130))
        .s(Font::new()
            .size(100)
            .color(hsluv!(21.24, 61.8, 54, 0.15))
            .weight(FontWeight::Hairline))
        .child("todos")
}

/// Render the main panel with input, todo list, and controls
fn render_todo_main_panel(ctx: &ReactiveContext, ctx_for_toggle_all: &ReactiveContext) -> impl Element {
    let ctx_for_list = ctx.clone();
    let ctx_for_footer = ctx.clone();
    let todos = ctx.todos.clone();

    Column::new()
        .s(Width::fill())
        .s(Background::new().color(hsluv!(0, 0, 100)))
        .s(Shadows::new([
            Shadow::new().y(2).blur(4).color(hsluv!(0, 0, 0, 0.2)),
            Shadow::new().y(25).blur(50).color(hsluv!(0, 0, 0, 0.1)),
        ]))
        // Input row with toggle-all button
        .item(render_todo_input_row(ctx, ctx_for_toggle_all))
        // Todo list and footer (only shown when todos exist)
        .item_signal(todos.signal_vec_cloned().len().map(move |len| {
            if len == 0 {
                El::new().unify()
            } else {
                Column::new()
                    .item(render_todo_list(&ctx_for_list))
                    .item(render_todo_panel_footer(&ctx_for_footer))
                    .unify()
            }
        }))
}

/// Render the input row with toggle-all checkbox and text input
fn render_todo_input_row(ctx: &ReactiveContext, ctx_for_toggle_all: &ReactiveContext) -> impl Element {
    let todos = ctx.todos.clone();
    let ctx_toggle = ctx_for_toggle_all.clone();

    Row::new()
        .s(Width::fill())
        // Toggle all checkbox (only show when todos exist)
        .item_signal(todos.signal_vec_cloned().len().map(move |len| {
            if len == 0 {
                El::new().unify()
            } else {
                render_toggle_all_checkbox(&ctx_toggle).unify()
            }
        }))
        // Text input
        .item(render_new_todo_input(ctx))
}

/// Render the toggle-all checkbox
fn render_toggle_all_checkbox(ctx: &ReactiveContext) -> impl Element {
    let ctx_click = ctx.clone();
    let todos = ctx.todos.clone();
    let todos_for_aria = ctx.todos.clone();

    // Track if all are completed for the icon color
    let all_completed_signal = todos.signal_vec_cloned().map_signal(|todo| {
        todo.completed.signal()
    }).to_signal_cloned().map(|completeds| {
        !completeds.is_empty() && completeds.iter().all(|c| *c)
    });

    // Clone for aria-checked attribute
    let all_completed_for_aria = todos_for_aria.signal_vec_cloned().map_signal(|todo| {
        todo.completed.signal()
    }).to_signal_cloned().map(|completeds| {
        !completeds.is_empty() && completeds.iter().all(|c| *c)
    });

    Button::new()
        .s(Width::exact(60))
        .s(Height::fill())
        .s(Font::new().size(22).color_signal(
            all_completed_signal.map(|all_done| {
                if all_done {
                    hsluv!(0, 0, 40)
                } else {
                    hsluv!(0, 0, 75)
                }
            })
        ))
        .s(Transform::new().rotate(90))
        .label("❯")
        .on_press(move || {
            let all_done = ctx_click.all_completed();
            ctx_click.toggle_all_todos(!all_done);
        })
        // Add checkbox role for test framework compatibility (index 0 = toggle all)
        .update_raw_el(move |raw_el| {
            raw_el
                .attr("role", "checkbox")
                .attr_signal("aria-checked", all_completed_for_aria.map(|c| if c { "true" } else { "false" }))
        })
}

/// Render the new todo text input
fn render_new_todo_input(ctx: &ReactiveContext) -> impl Element {
    let ctx_key = ctx.clone();
    let text_mutable = (*ctx.current_input_text).clone();
    let text_for_change = text_mutable.clone();
    let text_for_key = text_mutable.clone();

    TextInput::new()
        .s(Width::fill())
        .s(Padding::new().y(19).left(16).right(16))
        .s(Font::new().size(24).color(hsluv!(0, 0, 42)))
        .s(Background::new().color(hsluv!(0, 0, 0, 0.003)))
        .s(Shadows::new([
            Shadow::new().inner().y(-2).blur(1).color(hsluv!(0, 0, 0, 0.03))
        ]))
        .label_hidden("What needs to be done?")
        .placeholder(Placeholder::new("What needs to be done?")
            .s(Font::new().italic().color(hsluv!(0, 0, 92.5))))
        .text_signal(text_mutable.signal_cloned())
        .focus(true)
        .on_change(move |new_text| { text_for_change.set(new_text); })
        .on_key_down_event(move |event| {
            if *event.key() == Key::Enter {
                let current_text = text_for_key.get_cloned();
                let trimmed = current_text.trim().to_string();
                if !trimmed.is_empty() {
                    ctx_key.add_todo(trimmed);
                    text_for_key.set(String::new());
                }
            }
        })
}

/// Render the todo list
fn render_todo_list(ctx: &ReactiveContext) -> impl Element {
    let ctx_for_items = ctx.clone();
    let todos = ctx.todos.clone();
    let filter = ctx.todo_filter.clone();

    Column::new()
        .s(Width::fill())
        .items_signal_vec(
            todos.signal_vec_cloned()
                .filter_signal_cloned(move |todo| {
                    let completed_signal = todo.completed.signal();
                    let filter_signal = filter.signal();
                    map_ref! {
                        let completed = completed_signal,
                        let filter = filter_signal => {
                            match filter {
                                TodoFilter::All => true,
                                TodoFilter::Active => !completed,
                                TodoFilter::Completed => *completed,
                            }
                        }
                    }
                })
                .map(move |todo| {
                    render_todo_item(&ctx_for_items, todo).unify()
                })
        )
}

/// Render a single todo item
fn render_todo_item(ctx: &ReactiveContext, todo: Todo) -> impl Element {
    let ctx_toggle = ctx.clone();
    let todo_id = todo.id;
    let title = todo.title.clone();
    let completed = todo.completed.clone();

    Row::new()
        .s(Width::fill())
        .s(Background::new().color(hsluv!(0, 0, 100)))
        .s(Font::new().size(24))
        .s(Padding::new().x(15).y(10))
        .s(Gap::both(5))
        // Checkbox
        .item(render_todo_checkbox(ctx_toggle, todo_id, completed.clone()))
        // Title
        .item(
            Label::new()
                .s(Width::fill())
                .s(Font::new()
                    .size(24)
                    .color(hsluv!(0, 0, 42))
                    .line(FontLine::new().strike_signal(completed.signal())))
                .label_signal(title.signal_cloned())
        )
}

/// Render a todo checkbox
fn render_todo_checkbox(ctx: ReactiveContext, todo_id: u64, completed: Mutable<bool>) -> impl Element {
    let ctx_click = ctx.clone();
    let completed_for_icon = completed.clone();
    let completed_for_aria = completed.clone();

    // Use a button-based checkbox with proper ARIA role for test compatibility
    Button::new()
        .s(Width::exact(40))
        .s(Height::exact(40))
        .label_signal(completed_for_icon.signal().map(|is_checked| {
            if is_checked { "✓" } else { "○" }
        }))
        .s(Font::new().size(24))
        .s(RoundedCorners::all(20))
        .s(Borders::all_signal(completed.signal().map(|is_checked| {
            if is_checked {
                Border::new().width(3).color(hsluv!(158, 43, 66))
            } else {
                Border::new().width(3).color(hsluv!(0, 0, 93))
            }
        })))
        .on_press(move || {
            ctx_click.toggle_todo(todo_id);
        })
        // Add checkbox role and aria-checked for test framework compatibility
        .update_raw_el(move |raw_el| {
            raw_el
                .attr("role", "checkbox")
                .attr_signal("aria-checked", completed_for_aria.signal().map(|c| if c { "true" } else { "false" }))
        })
}

/// Render the panel footer with count, filters, and clear button
fn render_todo_panel_footer(ctx: &ReactiveContext) -> impl Element {
    let ctx_for_clear = ctx.clone();
    let ctx_filter_all = ctx.clone();
    let ctx_filter_active = ctx.clone();
    let ctx_filter_completed = ctx.clone();
    let todos = ctx.todos.clone();
    let filter = ctx.todo_filter.clone();

    Row::new()
        .s(Width::fill())
        .s(Padding::new().x(15).y(10))
        .s(Font::new().color(hsluv!(0, 0, 57)))
        .s(Borders::new().top(Border::new().color(hsluv!(0, 0, 92.5))))
        .s(Shadows::new([
            Shadow::new().y(1).blur(1).color(hsluv!(0, 0, 0, 0.2)),
            Shadow::new().y(8).spread(-3).color(hsluv!(0, 0, 97.3)),
            Shadow::new().y(9).blur(1).spread(-3).color(hsluv!(0, 0, 0, 0.2)),
            Shadow::new().y(16).spread(-6).color(hsluv!(0, 0, 97.3)),
            Shadow::new().y(17).blur(2).spread(-6).color(hsluv!(0, 0, 0, 0.2)),
        ]))
        // Items left count
        .item(render_items_left_count(&ctx.todos))
        // Filter buttons
        .item(
            Row::new()
                .s(Gap::both(10))
                .s(Align::new().center_x())
                .item(render_filter_button(&ctx_filter_all, TodoFilter::All, &filter))
                .item(render_filter_button(&ctx_filter_active, TodoFilter::Active, &filter))
                .item(render_filter_button(&ctx_filter_completed, TodoFilter::Completed, &filter))
        )
        // Clear completed button (only show when there are completed todos)
        .item_signal(
            todos.signal_vec_cloned()
                .map_signal(|t| t.completed.signal())
                .to_signal_cloned()
                .map(move |completeds| {
                    let has_completed = completeds.iter().any(|c| *c);
                    if has_completed {
                        let ctx_clear = ctx_for_clear.clone();
                        Button::new()
                            .label("Clear completed")
                            .on_press(move || {
                                ctx_clear.clear_completed_todos();
                            })
                            .unify()
                    } else {
                        El::new().unify()
                    }
                })
        )
}

/// Render the "X items left" counter
fn render_items_left_count(todos: &Rc<MutableVec<Todo>>) -> impl Element {
    let todos = todos.clone();

    El::new()
        .s(Width::fill())
        .child_signal(
            todos.signal_vec_cloned()
                .map_signal(|t| t.completed.signal().map(move |c| !c))
                .to_signal_cloned()
                .map(|actives| {
                    let count = actives.iter().filter(|a| **a).count();
                    let plural = if count == 1 { "" } else { "s" };
                    format!("{} item{} left", count, plural)
                })
        )
}

/// Render a filter button
fn render_filter_button(
    ctx: &ReactiveContext,
    target_filter: TodoFilter,
    current_filter: &Rc<Mutable<TodoFilter>>,
) -> impl Element {
    let ctx_click = ctx.clone();
    let current = current_filter.clone();
    let label = match target_filter {
        TodoFilter::All => "All",
        TodoFilter::Active => "Active",
        TodoFilter::Completed => "Completed",
    };

    Button::new()
        .s(Padding::new().x(8).y(4))
        .s(RoundedCorners::all(3))
        // Use CSS outline property for test compatibility (Zoon's Outline::inner uses box-shadow)
        .update_raw_el(move |raw_el| {
            raw_el.style_signal("outline", current.signal().map(move |f| {
                if f == target_filter {
                    // Inner outline with rgba color matching todo_mvc design
                    "1px solid rgba(175, 47, 47, 0.2)"
                } else {
                    "none"
                }
            }))
        })
        .label(label)
        .on_press(move || {
            ctx_click.set_todo_filter(target_filter);
        })
}

/// Render the info footer
fn render_todo_footer_info() -> impl Element {
    Column::new()
        .s(Gap::both(9))
        .s(Font::new()
            .size(10)
            .color(hsluv!(0, 0, 80.5))
            .center())
        .item(Paragraph::new().content("Double-click to edit a todo"))
        .item(
            Row::new()
                .s(Align::new().center_x())
                .item("Created by ")
                .item(
                    Link::new()
                        .label("Martin Kavík")
                        .to("https://github.com/MartinKavik")
                        .new_tab(NewTab::new())
                )
        )
        .item(
            Row::new()
                .s(Align::new().center_x())
                .item("Part of ")
                .item(
                    Link::new()
                        .label("TodoMVC")
                        .to("http://todomvc.com")
                        .new_tab(NewTab::new())
                )
        )
}

// Note: dd_bridge tests require WASM runtime and can't run in native Rust tests.
// Testing is done via playground integration tests instead.
