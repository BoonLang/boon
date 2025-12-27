//! Bridge between engine_v2 and Zoon UI.
//!
//! This module provides two main types:
//! - BridgeContext: Read-only access to EventLoop for value lookup
//! - ReactiveEventLoop: Mutable wrapper for interactive UI with version signals

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use zoon::*;

/// Global generation counter for ReactiveEventLoop instances.
/// Used to cancel timers from old event loops when a new one is created.
static CURRENT_GENERATION: AtomicU64 = AtomicU64::new(0);

use crate::engine_v2::{
    arena::SlotId,
    event_loop::EventLoop,
    message::Payload,
    node::NodeKind,
    address::Port,
};

/// Read-only context for accessing EventLoop values.
pub struct BridgeContext<'a> {
    event_loop: &'a EventLoop,
}

impl<'a> BridgeContext<'a> {
    pub fn new(event_loop: &'a EventLoop) -> Self {
        Self { event_loop }
    }

    /// Get the current value of a slot.
    pub fn get_slot_value(&self, slot: SlotId) -> Option<&Payload> {
        self.event_loop.arena.get(slot)?
            .extension.as_ref()?
            .current_value.as_ref()
    }

    /// Check if a payload is a simple scalar (not a compound element).
    pub fn is_scalar(&self, payload: &Payload) -> bool {
        match payload {
            Payload::Number(_) | Payload::Text(_) | Payload::Bool(_) | Payload::Unit => true,
            Payload::Tag(_) => true,
            Payload::TaggedObject { tag, .. } => {
                let tag_name = self.event_loop.arena.get_tag_name(*tag);
                !matches!(tag_name.map(|s| s.as_ref()),
                    Some("ElementContainer") | Some("ElementStripe") |
                    Some("ElementButton") | Some("ElementCheckbox") |
                    Some("ElementTextInput") | Some("ElementLabel") |
                    Some("ElementParagraph") | Some("ElementLink") |
                    Some("ElementStack") | Some("Document"))
            }
            Payload::ListHandle(_) | Payload::ObjectHandle(_) => false,
            Payload::Flushed(inner) => self.is_scalar(inner),
            Payload::ListDelta(_) | Payload::ObjectDelta(_) => true,
        }
    }

    /// Render a scalar payload to a string.
    pub fn render_scalar(&self, payload: &Payload) -> String {
        match payload {
            Payload::Number(n) => n.to_string(),
            Payload::Text(s) => s.to_string(),
            Payload::Bool(b) => b.to_string(),
            Payload::Unit => String::new(),
            Payload::Tag(id) => {
                self.event_loop.arena.get_tag_name(*id)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Tag({})", id))
            }
            Payload::TaggedObject { tag, .. } => {
                self.event_loop.arena.get_tag_name(*tag)
                    .map(|s| format!("{}[...]", s))
                    .unwrap_or_else(|| format!("TaggedObject({})", tag))
            }
            Payload::ListHandle(_) => "[list]".to_string(),
            Payload::ObjectHandle(_) => "{object}".to_string(),
            Payload::Flushed(inner) => format!("Error: {}", self.render_scalar(inner)),
            Payload::ListDelta(_) => "[delta]".to_string(),
            Payload::ObjectDelta(_) => "{delta}".to_string(),
        }
    }
}

/// Reactive wrapper for EventLoop with version tracking.
#[derive(Clone)]
pub struct ReactiveEventLoop {
    inner: Rc<RefCell<EventLoop>>,
    root_slot: Option<SlotId>,
    pub version: Mutable<u64>,
    /// Generation ID - used to cancel timers when a new ReactiveEventLoop is created.
    generation: u64,
}

impl ReactiveEventLoop {
    pub fn new(event_loop: EventLoop, root_slot: Option<SlotId>) -> Self {
        // Increment generation - this invalidates all timers from previous ReactiveEventLoops
        let generation = CURRENT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

        #[cfg(target_arch = "wasm32")]
        zoon::println!("ReactiveEventLoop::new generation={}", generation);

        let this = Self {
            inner: Rc::new(RefCell::new(event_loop)),
            root_slot,
            version: Mutable::new(0),
            generation,
        };
        // Schedule any pending real timers
        this.schedule_pending_timers();
        this
    }

    /// Take pending real timers and schedule them with actual setTimeout.
    fn schedule_pending_timers(&self) {
        let pending = {
            let mut el = self.inner.borrow_mut();
            el.take_pending_timers()
        };

        #[cfg(target_arch = "wasm32")]
        if !pending.is_empty() {
            zoon::println!("schedule_pending_timers: gen={} scheduling {} timer(s)", self.generation, pending.len());
        }

        for (node_id, interval_ms) in pending {
            #[cfg(target_arch = "wasm32")]
            zoon::println!("  timer {:?} interval_ms={}", node_id, interval_ms);

            let reactive_el = self.clone();
            let my_generation = self.generation;
            Task::start(async move {
                // Real wait using browser's setTimeout via Zoon's Timer
                let sleep_ms = interval_ms.round().max(0.0).min(u32::MAX as f64) as u32;
                #[cfg(target_arch = "wasm32")]
                let start = web_sys::window().and_then(|w| Some(w.performance()?.now())).unwrap_or(0.0);
                #[cfg(target_arch = "wasm32")]
                zoon::println!("Timer::sleep({}) starting at {:.0}ms", sleep_ms, start);
                Timer::sleep(sleep_ms).await;
                #[cfg(target_arch = "wasm32")]
                {
                    let end = web_sys::window().and_then(|w| Some(w.performance()?.now())).unwrap_or(0.0);
                    zoon::println!("Timer::sleep({}) completed at {:.0}ms (elapsed: {:.0}ms)", sleep_ms, end, end - start);
                }

                // Check if this timer is still current (not from an old ReactiveEventLoop)
                let current_gen = CURRENT_GENERATION.load(Ordering::SeqCst);
                if current_gen != my_generation {
                    // Stale timer from old code - don't fire
                    return;
                }

                // Fire the timer
                {
                    let mut el = reactive_el.inner.borrow_mut();
                    el.fire_timer(node_id);
                }

                // Process the event - tick() will schedule any new pending timers
                reactive_el.tick();
            });
        }
    }

    /// Run a tick and increment version if anything changed.
    pub fn tick(&self) {
        let mut el = self.inner.borrow_mut();
        let had_work = !el.dirty_nodes.is_empty() || !el.timer_queue.is_empty();
        el.run_tick();

        // Check for pending route change and update browser URL
        if let Some(route_payload) = el.pending_route_change.take() {
            drop(el); // Release borrow before accessing window
            self.update_browser_url(&route_payload);
        } else {
            drop(el);
        }

        if had_work {
            // update() takes closure that returns new value, not in-place mutation
            self.version.update(|v| v + 1);
        }
        // Schedule any new pending timers that may have been created during this tick
        self.schedule_pending_timers();
    }

    /// Update the browser URL using history.pushState.
    fn update_browser_url(&self, route_payload: &Payload) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Payload::Text(route) = route_payload {
                let route_str = route.as_ref();
                zoon::println!("ReactiveEventLoop: updating browser URL to {}", route_str);

                // Use history.pushState to update URL without page reload
                if let Some(window) = web_sys::window() {
                    if let Ok(history) = window.history() {
                        let _ = history.push_state_with_url(
                            &wasm_bindgen::JsValue::NULL,
                            "",
                            Some(route_str),
                        );
                    }
                }
            }
        }
    }

    /// Inject an event and run tick.
    pub fn inject_event(&self, target_slot: SlotId, payload: Payload) {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("inject_event: target={:?} payload={:?}", target_slot, payload);
        {
            let mut el = self.inner.borrow_mut();
            // Put payload in inbox so IOPad can forward it to subscribers
            el.inbox.insert((target_slot, Port::Input(0)), payload.clone());
            el.mark_dirty(target_slot, Port::Input(0));
        }
        self.tick();
    }

    /// Render the root element.
    pub fn render_element(&self) -> RawElOrText {
        #[cfg(target_arch = "wasm32")]
        zoon::println!("render_element called, current version={}", self.version.get());
        let el = self.inner.borrow();

        if let Some(root_slot) = self.root_slot {
            if let Some(node) = el.arena.get(root_slot) {
                if let Some(ext) = &node.extension {
                    if let Some(payload) = &ext.current_value {
                        let payload = payload.clone();
                        drop(el);
                        return self.render_payload(payload);
                    }
                }
            }
        }

        El::new().unify()
    }

    /// Render a payload to a Zoon element.
    fn render_payload(&self, payload: Payload) -> RawElOrText {
        match payload {
            Payload::Text(text) => zoon::Text::new(text.as_ref()).unify(),
            Payload::Number(n) => zoon::Text::new(n.to_string()).unify(),
            Payload::Bool(b) => zoon::Text::new(b.to_string()).unify(),
            Payload::Unit => El::new().unify(),
            Payload::Tag(tag_id) => {
                let el = self.inner.borrow();
                let name = el.arena.get_tag_name(tag_id)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                drop(el);
                if name == "NoElement" {
                    El::new().unify()
                } else {
                    zoon::Text::new(name).unify()
                }
            }
            Payload::TaggedObject { tag, fields } => {
                let el = self.inner.borrow();
                let tag_name = el.arena.get_tag_name(tag)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                drop(el);
                self.render_tagged_object(&tag_name, fields)
            }
            Payload::ListHandle(bus_slot) => self.render_list(bus_slot),
            Payload::ObjectHandle(_) => zoon::Text::new("{object}").unify(),
            Payload::Flushed(inner) => {
                let el = self.inner.borrow();
                let ctx = BridgeContext::new(&el);
                let msg = format!("Error: {}", ctx.render_scalar(&inner));
                drop(el);
                El::new()
                    .s(Font::new().color(hsluv!(0, 70, 60)))
                    .child(msg)
                    .unify()
            }
            Payload::ListDelta(_) | Payload::ObjectDelta(_) => El::new().unify(),
        }
    }

    /// Render a tagged object based on its tag name.
    fn render_tagged_object(&self, tag_name: &str, fields_slot: SlotId) -> RawElOrText {
        match tag_name {
            "Document" => self.render_document(fields_slot),
            "ElementContainer" => self.render_container(fields_slot),
            "ElementStripe" => self.render_stripe(fields_slot),
            "ElementButton" => self.render_button(fields_slot),
            "ElementCheckbox" => self.render_checkbox(fields_slot),
            "ElementTextInput" => self.render_text_input(fields_slot),
            "ElementLabel" => self.render_label(fields_slot),
            "ElementParagraph" => self.render_paragraph(fields_slot),
            "ElementStack" => self.render_stack(fields_slot),
            _ => zoon::Text::new(format!("{}[...]", tag_name)).unify(),
        }
    }

    /// Render a Document.
    fn render_document(&self, fields_slot: SlotId) -> RawElOrText {
        if let Some(payload) = self.get_field_value(fields_slot, "root_element") {
            return self.render_payload(payload);
        }
        El::new().unify()
    }

    /// Render an ElementContainer.
    fn render_container(&self, fields_slot: SlotId) -> RawElOrText {
        let child_payload = self.get_nested_value(fields_slot, &["settings", "child"]);
        let padding = self.get_nested_number(fields_slot, &["settings", "style", "padding"]);

        if let Some(child) = child_payload {
            let mut container = El::new()
                .s(Width::fill())
                .s(Height::fill())
                .child(self.render_payload(child));

            if let Some(p) = padding {
                container = container.s(Padding::all(p as u32));
            }

            container.unify()
        } else {
            let mut container = El::new()
                .s(Width::fill())
                .s(Height::fill());

            if let Some(p) = padding {
                container = container.s(Padding::all(p as u32));
            }

            container.unify()
        }
    }

    /// Render an ElementStripe (horizontal/vertical layout).
    fn render_stripe(&self, fields_slot: SlotId) -> RawElOrText {
        let direction = self.get_nested_value(fields_slot, &["settings", "direction"]);
        let gap = self.get_nested_number(fields_slot, &["settings", "gap"]);

        let is_column = match direction {
            Some(Payload::Tag(tag_id)) => {
                let el = self.inner.borrow();
                let is_col = el.arena.get_tag_name(tag_id)
                    .map(|s| s.as_ref() == "Column")
                    .unwrap_or(true);
                drop(el);
                is_col
            }
            _ => true,
        };

        // Get items from Bus
        let items_slot = self.get_nested_slot(fields_slot, &["settings", "items"]);

        let children = items_slot
            .map(|slot| self.collect_bus_items(slot))
            .unwrap_or_default();

        #[cfg(target_arch = "wasm32")]
        zoon::println!("render_stripe: is_column={} children.len()={}", is_column, children.len());
        #[cfg(target_arch = "wasm32")]
        for (i, c) in children.iter().enumerate() {
            zoon::println!("  child[{}] = {:?}", i, std::mem::discriminant(c));
        }

        if children.is_empty() {
            return El::new().unify();
        }

        // Render all children to elements
        let rendered: Vec<_> = children.into_iter()
            .map(|p| self.render_payload(p))
            .collect();

        if is_column {
            let mut col = Column::new()
                .s(Width::fill())
                .s(Align::new().left())
                .items(rendered);

            if let Some(g) = gap {
                col = col.s(Gap::both(g as u32));
            }

            col.unify()
        } else {
            let mut row = Row::new()
                .s(Height::fill())
                .s(Align::new().top())
                .items(rendered);

            if let Some(g) = gap {
                row = row.s(Gap::both(g as u32));
            }

            row.unify()
        }
    }

    /// Render an ElementButton.
    fn render_button(&self, fields_slot: SlotId) -> RawElOrText {
        let label = self.get_nested_value(fields_slot, &["settings", "label"]);
        // The press event is at path ["event", "press"] since element: [event: [press: LINK]]
        let press_slot = self.get_nested_slot(fields_slot, &["event", "press"]);

        let label_text = match label {
            Some(Payload::Text(s)) => s.to_string(),
            Some(Payload::Number(n)) => n.to_string(),
            _ => "Button".to_string(),
        };

        let reactive_el = self.clone();
        let press_slot = press_slot.unwrap_or(SlotId::INVALID);

        Button::new()
            .label(label_text)
            .on_press(move || {
                if press_slot.is_valid() {
                    zoon::println!("Button pressed! Injecting event to slot {:?}", press_slot);
                    reactive_el.inject_event(press_slot, Payload::Unit);
                } else {
                    zoon::println!("Button pressed but no valid press slot!");
                }
            })
            .unify()
    }

    /// Render an ElementCheckbox.
    fn render_checkbox(&self, fields_slot: SlotId) -> RawElOrText {
        let checked = self.get_nested_value(fields_slot, &["settings", "checked"]);
        let checked_bool = matches!(checked, Some(Payload::Bool(true)));
        // Click event is at ["event", "click"], like button's press is at ["event", "press"]
        let click_slot = self.get_nested_slot(fields_slot, &["event", "click"]);
        let label = self.get_nested_value(fields_slot, &["settings", "label"]);

        let reactive_el = self.clone();
        let click_slot = click_slot.unwrap_or(SlotId::INVALID);

        // Need unique ID for each checkbox - use the fields_slot index
        let checkbox_id = format!("cb-{}", fields_slot.index);
        let checkbox_el = Checkbox::new()
            .id(checkbox_id)
            .label_hidden("checkbox")
            .icon(|checked| {
                // Unicode checkbox icons: ☐ (unchecked) and ☑ (checked)
                let icon_char = if checked.get() { "☑" } else { "☐" };
                zoon::Text::new(icon_char)
            })
            .checked(checked_bool)
            .on_click(move || {
                if click_slot.is_valid() {
                    reactive_el.inject_event(click_slot, Payload::Bool(!checked_bool));
                }
            });

        // Add label if present
        match label {
            Some(Payload::Text(label_text)) => {
                Row::new()
                    .s(Gap::both(8))
                    .item(checkbox_el)
                    .item(zoon::Text::new(label_text.as_ref()))
                    .unify()
            }
            _ => checkbox_el.unify()
        }
    }

    /// Render an ElementTextInput.
    fn render_text_input(&self, fields_slot: SlotId) -> RawElOrText {
        let text = self.get_nested_value(fields_slot, &["settings", "text"]);
        let text_str = match text {
            Some(Payload::Text(s)) => s.to_string(),
            _ => String::new(),
        };

        // Placeholder can be either direct text or an object with a "text" field
        // Try direct text first, then try nested text field
        let placeholder_str = match self.get_nested_value(fields_slot, &["settings", "placeholder"]) {
            Some(Payload::Text(s)) => s.to_string(),
            _ => {
                // Try object with "text" field: [text: "..."]
                match self.get_nested_value(fields_slot, &["settings", "placeholder", "text"]) {
                    Some(Payload::Text(s)) => s.to_string(),
                    _ => String::new(),
                }
            }
        };

        // Get focus setting - check if it's true (Bool) or the Tag "True"
        let focus = self.get_nested_value(fields_slot, &["settings", "focus"]);
        #[cfg(target_arch = "wasm32")]
        zoon::println!("render_text_input: focus value = {:?}", focus);
        let should_focus = match focus {
            Some(Payload::Bool(b)) => b,
            Some(Payload::Tag(tag_id)) => {
                let el = self.inner.borrow();
                el.arena.get_tag_name(tag_id).map(|n| n.as_ref() == "True").unwrap_or(false)
            }
            _ => false,
        };
        #[cfg(target_arch = "wasm32")]
        zoon::println!("render_text_input: should_focus = {}", should_focus);

        // Get separate event slots for key_down and change
        let key_down_slot = self.get_nested_slot(fields_slot, &["event", "key_down"]);
        let change_slot = self.get_nested_slot(fields_slot, &["event", "change"]);

        #[cfg(target_arch = "wasm32")]
        zoon::println!("render_text_input: key_down_slot={:?} change_slot={:?}", key_down_slot, change_slot);

        let reactive_el_keydown = self.clone();
        let reactive_el_change = self.clone();
        let key_down_slot_val = key_down_slot.unwrap_or(SlotId::INVALID);
        let change_slot_val = change_slot.unwrap_or(SlotId::INVALID);

        #[cfg(target_arch = "wasm32")]
        zoon::println!("render_text_input: key_down valid={} change valid={}", key_down_slot_val.is_valid(), change_slot_val.is_valid());

        // Need unique ID for text input
        let input_id = format!("ti-{}", fields_slot.index);
        TextInput::new()
            .id(input_id)
            .label_hidden("text input")
            .s(Width::fill())
            .s(Padding::all(8))
            .placeholder(Placeholder::new(placeholder_str))
            .text(&text_str)
            .update_raw_el(move |raw_el| {
                if should_focus {
                    // Create a Mutable that changes from false to true
                    // This triggers dominator's focused_signal to actually apply focus
                    let focus_mutable = Mutable::new(false);
                    let focus_mutable_clone = focus_mutable.clone();

                    raw_el
                        // Add data attribute for test detection (document.activeElement
                        // doesn't work in automated Chrome without OS-level window focus)
                        .attr("data-boon-focused", "true")
                        .focus_signal(focus_mutable.signal())
                        .after_insert(move |_| {
                            // Set to true after element is in DOM
                            focus_mutable_clone.set(true);
                            #[cfg(target_arch = "wasm32")]
                            zoon::println!("Focus mutable set to true in after_insert");
                        })
                        .after_remove(move |_| {
                            // Keep focus_mutable alive until element is removed
                            drop(focus_mutable);
                        })
                } else {
                    raw_el
                }
            })
            .on_change(move |new_text| {
                #[cfg(target_arch = "wasm32")]
                zoon::println!("on_change TRIGGERED! text='{}' slot_valid={}", new_text, change_slot_val.is_valid());
                if change_slot_val.is_valid() {
                    // Create change event payload: TaggedObject { text: new_text }
                    let payload = reactive_el_change.create_change_event_payload(&new_text);
                    reactive_el_change.inject_event(change_slot_val, payload);
                }
            })
            .on_key_down_event(move |event| {
                #[cfg(target_arch = "wasm32")]
                zoon::println!("on_key_down_event TRIGGERED! slot_valid={}", key_down_slot_val.is_valid());
                if key_down_slot_val.is_valid() {
                    // Convert key to string for tag lookup
                    let key_name = match event.key() {
                        zoon::Key::Enter => "Enter",
                        zoon::Key::Escape => "Escape",
                        zoon::Key::Other(k) => k.as_str(),
                    };
                    #[cfg(target_arch = "wasm32")]
                    zoon::println!("TextInput key_down: {} injecting to {:?}", key_name, key_down_slot_val);
                    // Create key_down event payload: TaggedObject { key: Tag(key_name) }
                    let payload = reactive_el_keydown.create_key_event_payload(key_name);
                    reactive_el_keydown.inject_event(key_down_slot_val, payload);
                }
            })
            .unify()
    }

    /// Create a key event payload: an object with a `key` field containing the key tag.
    fn create_key_event_payload(&self, key_name: &str) -> Payload {
        // Get or create the tag ID for this key name
        let tag_id = {
            let mut el = self.inner.borrow_mut();
            el.arena.intern_tag(key_name)
        };

        // Create a simple object structure with a "key" field
        // For simplicity, we create a TaggedObject where the fields Router has a "key" field
        let (fields_slot, key_field_slot) = {
            let mut el = self.inner.borrow_mut();

            // Allocate slots for the structure
            let key_field_slot = el.arena.alloc();
            let fields_slot = el.arena.alloc();

            // Set up the key field as a Producer with the Tag value
            if let Some(node) = el.arena.get_mut(key_field_slot) {
                node.set_kind(NodeKind::Producer { value: Some(Payload::Tag(tag_id)) });
                node.extension_mut().current_value = Some(Payload::Tag(tag_id));
            }

            // Set up the fields Router with the "key" field
            let key_field_id = el.arena.intern_field("key");
            let mut fields = std::collections::HashMap::new();
            fields.insert(key_field_id, key_field_slot);
            if let Some(node) = el.arena.get_mut(fields_slot) {
                node.set_kind(NodeKind::Router { fields });
            }

            (fields_slot, key_field_slot)
        };

        // Return an ObjectHandle pointing to the fields Router
        Payload::ObjectHandle(fields_slot)
    }

    /// Create a change event payload: an object with a `text` field.
    fn create_change_event_payload(&self, text: &str) -> Payload {
        let (fields_slot, _text_field_slot) = {
            let mut el = self.inner.borrow_mut();

            // Allocate slots for the structure
            let text_field_slot = el.arena.alloc();
            let fields_slot = el.arena.alloc();

            // Set up the text field as a Producer
            let text_arc: std::sync::Arc<str> = text.into();
            if let Some(node) = el.arena.get_mut(text_field_slot) {
                node.set_kind(NodeKind::Producer { value: Some(Payload::Text(text_arc.clone())) });
                node.extension_mut().current_value = Some(Payload::Text(text_arc));
            }

            // Set up the fields Router with the "text" field
            let text_field_id = el.arena.intern_field("text");
            let mut fields = std::collections::HashMap::new();
            fields.insert(text_field_id, text_field_slot);
            if let Some(node) = el.arena.get_mut(fields_slot) {
                node.set_kind(NodeKind::Router { fields });
            }

            (fields_slot, text_field_slot)
        };

        Payload::ObjectHandle(fields_slot)
    }

    /// Render an ElementLabel with full styling support.
    fn render_label(&self, fields_slot: SlotId) -> RawElOrText {
        // Label content is stored as "label" field, not "content"
        let label = self.get_nested_value(fields_slot, &["settings", "label"]);

        let label_text = match label {
            Some(Payload::Text(s)) => s.to_string(),
            Some(Payload::Number(n)) => n.to_string(),
            _ => String::new(),
        };

        // Get style properties
        let style_slot = self.get_nested_slot(fields_slot, &["settings", "style"]);

        // Extract style values
        let padding = style_slot.and_then(|s| self.get_nested_number(s, &["padding"]));
        let width = style_slot.and_then(|s| self.get_nested_number(s, &["width"]));
        let rounded_corners = style_slot.and_then(|s| self.get_nested_number(s, &["rounded_corners"]));
        let bg_color = style_slot.and_then(|s| self.get_nested_value(s, &["background", "color"]));
        let font_color = style_slot.and_then(|s| self.get_nested_value(s, &["font", "color"]));

        // Build the styled element
        let mut el = El::new();

        // Apply padding
        if let Some(p) = padding {
            el = el.s(Padding::all(p as u32));
        }

        // Apply width
        if let Some(w) = width {
            el = el.s(Width::exact(w as u32));
        }

        // Apply rounded corners
        if let Some(r) = rounded_corners {
            el = el.s(RoundedCorners::all(r as u32));
        }

        // Apply background color via CSS
        if let Some(color_payload) = bg_color {
            if let Some(css_color) = self.payload_to_css_color(&color_payload) {
                // Use Zoon's style system with raw CSS color
                el = el.update_raw_el(|raw_el| {
                    raw_el.style("background-color", &css_color)
                });
            }
        }

        // Apply font color via CSS
        if let Some(color_payload) = font_color {
            if let Some(css_color) = self.payload_to_css_color(&color_payload) {
                el = el.update_raw_el(|raw_el| {
                    raw_el.style("color", &css_color)
                });
            }
        }

        el.child(label_text).unify()
    }

    /// Convert a Payload (Oklch tagged object or named color tag) to a CSS color string.
    fn payload_to_css_color(&self, payload: &Payload) -> Option<String> {
        match payload {
            Payload::TaggedObject { tag, fields } => {
                let el = self.inner.borrow();
                let tag_name = el.arena.get_tag_name(*tag)?;
                if tag_name.as_ref() == "Oklch" {
                    // Extract Oklch components
                    let lightness = self.get_nested_number(*fields, &["lightness"]).unwrap_or(0.0);
                    let chroma = self.get_nested_number(*fields, &["chroma"]).unwrap_or(0.0);
                    let hue = self.get_nested_number(*fields, &["hue"]).unwrap_or(0.0);
                    drop(el);
                    // Generate oklch() CSS color string
                    // oklch(lightness chroma hue) where lightness is 0-100%
                    Some(format!("oklch({}% {} {})", lightness * 100.0, chroma, hue))
                } else {
                    drop(el);
                    None
                }
            }
            Payload::Tag(tag_id) => {
                let el = self.inner.borrow();
                let tag_name = el.arena.get_tag_name(*tag_id)?;
                let color = match tag_name.as_ref() {
                    "White" => Some("white".to_string()),
                    "Black" => Some("black".to_string()),
                    "Red" => Some("red".to_string()),
                    "Green" => Some("green".to_string()),
                    "Blue" => Some("blue".to_string()),
                    _ => None,
                };
                drop(el);
                color
            }
            _ => None,
        }
    }

    /// Render an ElementParagraph.
    fn render_paragraph(&self, fields_slot: SlotId) -> RawElOrText {
        let content = self.get_nested_value(fields_slot, &["settings", "content"]);

        let text = match content {
            Some(Payload::Text(s)) => s.to_string(),
            Some(Payload::Number(n)) => n.to_string(),
            _ => String::new(),
        };

        Paragraph::new()
            .content(text)
            .unify()
    }

    /// Render an ElementStack (stacked layers).
    fn render_stack(&self, fields_slot: SlotId) -> RawElOrText {
        // Get style properties
        let width = self.get_nested_number(fields_slot, &["settings", "style", "width"]);
        let height = self.get_nested_number(fields_slot, &["settings", "style", "height"]);
        let bg_color = self.get_nested_value(fields_slot, &["settings", "style", "background", "color"]);

        // Get layers from Bus
        let layers_slot = self.get_nested_slot(fields_slot, &["settings", "layers"]);

        let layers = layers_slot
            .map(|slot| self.collect_bus_items(slot))
            .unwrap_or_default();

        // Render all layers to elements
        let rendered: Vec<_> = layers.into_iter()
            .map(|p| self.render_payload(p))
            .collect();

        // Convert bg_color to CSS string once (avoid borrow issues)
        let bg_css = bg_color.and_then(|c| self.payload_to_css_color(&c));

        // Use Stack with layers if we have any, otherwise empty El
        if rendered.is_empty() {
            let mut el = El::new()
                .s(Align::new().center_x().center_y())
                .s(if let Some(w) = width { Width::exact(w as u32) } else { Width::fill() })
                .s(if let Some(h) = height { Height::exact(h as u32) } else { Height::fill() });

            if let Some(ref css_color) = bg_css {
                el = el.update_raw_el(|raw_el| raw_el.style("background-color", css_color));
            }
            el.unify()
        } else {
            let mut stack = Stack::new()
                .s(Align::new().center_x().center_y())
                .s(if let Some(w) = width { Width::exact(w as u32) } else { Width::fill() })
                .s(if let Some(h) = height { Height::exact(h as u32) } else { Height::fill() });

            if let Some(ref css_color) = bg_css {
                stack = stack.update_raw_el(|raw_el| raw_el.style("background-color", css_color));
            }
            stack.layers(rendered).unify()
        }
    }

    /// Render a list as a column.
    fn render_list(&self, bus_slot: SlotId) -> RawElOrText {
        let children = self.collect_bus_items(bus_slot);

        if children.is_empty() {
            return El::new().unify();
        }

        // Render all children to elements first
        let rendered: Vec<_> = children.into_iter()
            .map(|p| self.render_payload(p))
            .collect();

        Column::new()
            .s(Width::fill())
            .items(rendered)
            .unify()
    }

    /// Collect items from a Bus node.
    fn collect_bus_items(&self, bus_slot: SlotId) -> Vec<Payload> {
        let el = self.inner.borrow();
        let mut items = vec![];

        if let Some(node) = el.arena.get(bus_slot) {
            if let Some(NodeKind::Bus { items: bus_items, .. }) = node.kind() {
                for (_, item_slot) in bus_items {
                    // Check visibility condition
                    if let Some(&cond_slot) = el.visibility_conditions.get(item_slot) {
                        if let Some(cond_node) = el.arena.get(cond_slot) {
                            if let Some(ext) = &cond_node.extension {
                                if let Some(Payload::Bool(false)) = &ext.current_value {
                                    continue;
                                }
                            }
                        }
                    }

                    // Get item value
                    if let Some(item_node) = el.arena.get(*item_slot) {
                        if let Some(ext) = &item_node.extension {
                            if let Some(payload) = &ext.current_value {
                                items.push(payload.clone());
                            }
                        }
                    }
                }
            }
        }

        items
    }

    /// Get a field value from a Router.
    fn get_field_value(&self, router_slot: SlotId, field_name: &str) -> Option<Payload> {
        let el = self.inner.borrow();
        let field_id = el.arena.get_field_id(field_name)?;

        if let Some(node) = el.arena.get(router_slot) {
            if let Some(NodeKind::Router { fields }) = node.kind() {
                let field_slot = *fields.get(&field_id)?;
                if let Some(field_node) = el.arena.get(field_slot) {
                    if let Some(ext) = &field_node.extension {
                        return ext.current_value.clone();
                    }
                }
            }
        }

        None
    }

    /// Get a nested field value by path.
    fn get_nested_value(&self, start_slot: SlotId, path: &[&str]) -> Option<Payload> {
        let el = self.inner.borrow();
        let mut current_slot = start_slot;

        for &field_name in path {
            let field_id = el.arena.get_field_id(field_name)?;

            if let Some(node) = el.arena.get(current_slot) {
                if let Some(NodeKind::Router { fields }) = node.kind() {
                    current_slot = *fields.get(&field_id)?;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }

        if let Some(node) = el.arena.get(current_slot) {
            if let Some(ext) = &node.extension {
                return ext.current_value.clone();
            }
        }

        None
    }

    /// Get a nested slot by path.
    fn get_nested_slot(&self, start_slot: SlotId, path: &[&str]) -> Option<SlotId> {
        let el = self.inner.borrow();
        let mut current_slot = start_slot;

        for &field_name in path {
            let field_id = el.arena.get_field_id(field_name)?;

            if let Some(node) = el.arena.get(current_slot) {
                if let Some(NodeKind::Router { fields }) = node.kind() {
                    current_slot = *fields.get(&field_id)?;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }

        Some(current_slot)
    }

    /// Get a numeric value from a nested path.
    fn get_nested_number(&self, start_slot: SlotId, path: &[&str]) -> Option<f64> {
        match self.get_nested_value(start_slot, path)? {
            Payload::Number(n) => Some(n),
            _ => None,
        }
    }
}
