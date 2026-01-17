//! Core DD types with anti-cheat enforcement.
//!
//! These types are designed to make synchronous state access IMPOSSIBLE:
//! - Output<T>: Can only be observed via async stream, never read synchronously
//! - Input<T>: Can only inject events, never read state
//!
//! This module is part of the "core" layer which has NO Zoon dependencies
//! and NO Mutable/RefCell types.

use zoon::futures_channel::mpsc;
use zoon::futures_util::stream::Stream;

/// DD output wrapper - can ONLY be observed via stream, never read synchronously.
///
/// # Anti-Cheat Design
///
/// This type intentionally has NO `.get()` method. The only way to observe
/// values is through the async `.stream()` method. This enforces that all
/// state observation flows through the reactive system.
///
/// # Example
///
/// ```ignore
/// let output: Output<Value> = worker.output();
///
/// // CORRECT: async observation
/// let stream = output.stream();
/// stream.for_each(|value| { /* handle value */ }).await;
///
/// // IMPOSSIBLE: no .get() method exists
/// // let value = output.get();  // COMPILE ERROR
/// ```
pub struct Output<T> {
    receiver: mpsc::UnboundedReceiver<T>,
}

impl<T> Output<T> {
    /// Create a new output from a receiver channel.
    ///
    /// This should only be called from the DD worker when setting up output channels.
    pub(crate) fn new(receiver: mpsc::UnboundedReceiver<T>) -> Self {
        Self { receiver }
    }

    /// Convert to an async stream for observation.
    ///
    /// This is the ONLY way to observe values from DD output.
    /// The stream emits whenever the DD dataflow produces new output.
    pub fn stream(self) -> impl Stream<Item = T> {
        self.receiver
    }

    // NO .get() method - intentionally omitted for anti-cheat
    // NO .get_cloned() method - intentionally omitted for anti-cheat
    // ALLOWED: doc comment - NO .borrow() method - intentionally omitted for anti-cheat
}

/// DD input handle - can ONLY inject events, never read state.
///
/// # Anti-Cheat Design
///
/// This type intentionally has NO `.get()` method. The only way to interact
/// is through the `.send()` method which injects events into DD.
///
/// # Example
///
/// ```ignore
/// let input: Input<Event> = worker.input();
///
/// // CORRECT: inject event
/// input.send(Event::Link { id, value });
///
/// // IMPOSSIBLE: no .get() method exists
/// // let state = input.get();  // COMPILE ERROR
/// ```
#[derive(Clone)]
pub struct Input<T> {
    sender: mpsc::UnboundedSender<T>,
}

impl<T> Input<T> {
    /// Create a new input from a sender channel.
    ///
    /// This should only be called from the DD worker when setting up input channels.
    pub(crate) fn new(sender: mpsc::UnboundedSender<T>) -> Self {
        Self { sender }
    }

    /// Inject an event into the DD dataflow.
    ///
    /// This is the ONLY way to interact with DD state - by injecting events.
    /// The DD worker will process the event and produce outputs via Output.
    ///
    /// Returns `Ok(())` if the event was sent, `Err(T)` if the channel is closed.
    pub fn send(&self, value: T) -> Result<(), T> {
        self.sender.unbounded_send(value).map_err(|e| e.into_inner())
    }

    /// Try to inject an event, ignoring if channel is closed.
    ///
    /// Use this for fire-and-forget event injection where eventual consistency
    /// is acceptable.
    pub fn send_or_drop(&self, value: T) {
        let _ = self.sender.unbounded_send(value);
    }

    // NO .get() method - intentionally omitted for anti-cheat
    // NO way to read current state - intentionally omitted for anti-cheat
}

/// Create a paired input/output channel for DD communication.
///
/// This is the standard way to create DD I/O channels that enforce
/// the anti-cheat constraints.
pub fn channel<T>() -> (Input<T>, Output<T>) {
    let (sender, receiver) = mpsc::unbounded();
    (Input::new(sender), Output::new(receiver))
}

/// A unique identifier for state cells.
///
/// Cells are the DD engine's storage mechanism - distinct from Boon's HOLD construct.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CellId {
    /// Static cell defined in source code (e.g., "count", "items")
    Static(std::sync::Arc<str>),
    /// Dynamic cell generated for list items
    Dynamic(u32),
}

impl CellId {
    /// Create a new static cell ID from a string name.
    pub fn new(name: impl Into<String>) -> Self {
        Self::Static(std::sync::Arc::from(name.into()))
    }

    /// Create a new dynamic cell ID from a counter.
    pub fn dynamic(counter: u32) -> Self {
        Self::Dynamic(counter)
    }

    /// Check if this is a dynamic cell ID.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Self::Dynamic(_))
    }

    /// Get the name/string representation of this cell ID.
    pub fn name(&self) -> String {
        match self {
            Self::Static(name) => name.to_string(),
            Self::Dynamic(counter) => format!("dynamic_cell_{}", counter),
        }
    }
}

impl std::fmt::Display for CellId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl From<&str> for CellId {
    fn from(s: &str) -> Self {
        // Check if it's a dynamic ID pattern
        if let Some(num_str) = s.strip_prefix("dynamic_cell_") {
            if let Ok(num) = num_str.parse::<u32>() {
                return CellId::Dynamic(num);
            }
        }
        CellId::Static(std::sync::Arc::from(s))
    }
}

impl From<String> for CellId {
    fn from(s: String) -> Self {
        CellId::from(s.as_str())
    }
}

impl From<std::sync::Arc<str>> for CellId {
    fn from(s: std::sync::Arc<str>) -> Self {
        CellId::from(s.as_ref())
    }
}

impl PartialEq<str> for CellId {
    fn eq(&self, other: &str) -> bool {
        self.name() == other
    }
}

impl PartialEq<String> for CellId {
    fn eq(&self, other: &String) -> bool {
        self.name() == *other
    }
}

/// Boolean tag - type-safe replacement for `tag == "True"/"False"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoolTag {
    True,
    False,
}

impl BoolTag {
    /// Convert from a string tag name.
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "True" => Some(Self::True),
            "False" => Some(Self::False),
            _ => None,
        }
    }

    /// Convert from a boolean value.
    pub fn from_bool(b: bool) -> Self {
        if b { Self::True } else { Self::False }
    }

    /// Convert to boolean value.
    pub fn to_bool(self) -> bool {
        match self {
            Self::True => true,
            Self::False => false,
        }
    }

    /// Check if a tag string represents True.
    pub fn is_true(tag: &str) -> bool {
        tag == "True"
    }

    /// Check if a tag string represents False.
    pub fn is_false(tag: &str) -> bool {
        tag == "False"
    }

    /// Check if a tag string is a boolean tag (True or False).
    pub fn is_bool_tag(tag: &str) -> bool {
        tag == "True" || tag == "False"
    }

    /// Check if a boolean value matches the given tag string.
    /// Returns true if: (b && tag=="True") || (!b && tag=="False")
    pub fn matches_bool(tag: &str, b: bool) -> bool {
        (b && tag == "True") || (!b && tag == "False")
    }

    /// Get the tag name as a static string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::True => "True",
            Self::False => "False",
        }
    }
}

/// Element tag - type-safe replacement for `tag == "Element"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ElementTag {
    Element,
    NoElement,
}

impl ElementTag {
    /// Check if a tag string represents an Element.
    pub fn is_element(tag: &str) -> bool {
        tag == "Element"
    }
}

/// Structured event payload - type-safe event data.
///
/// Phase 3.1: Replaces string-based event encoding (format! strings) with type-safe variants.
/// This enables compile-time checking of event payloads and eliminates string parsing overhead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventPayload {
    /// Text input submission: "Enter:text"
    Enter(String),
    /// Remove button click: "remove:link_id"
    Remove(LinkId),
    /// Toggle action: "toggle:cell_id"
    Toggle(CellId),
    /// Set cell to true: "set_true:cell_id"
    SetTrue(CellId),
    /// Set cell to false: "set_false:cell_id"
    SetFalse(CellId),
    /// Toggle boolean cell: "bool_toggle:cell_id"
    BoolToggle(CellId),
    /// Set hover state: "hover_set:cell_id:value"
    HoverSet { cell_id: CellId, value: bool },
    /// Toggle all items in list: "list_toggle_all:list_cell_id:field"
    ListToggleAll { list_cell_id: CellId, field: String },
    /// Set text value: "set_text:cell_id:value"
    SetText { cell_id: CellId, value: String },
    /// Simple trigger with no data
    Unit,
}

impl EventPayload {
    /// Parse an event payload from a text string.
    ///
    /// Recognizes:
    /// - `"Enter:text"` → `EventPayload::Enter("text")`
    /// - `"remove:link_id"` → `EventPayload::Remove(LinkId)`
    /// - `"toggle:cell_id"` → `EventPayload::Toggle(CellId)`
    /// - `"set_true:cell_id"` → `EventPayload::SetTrue(CellId)`
    /// - `"set_false:cell_id"` → `EventPayload::SetFalse(CellId)`
    /// - `"bool_toggle:cell_id"` → `EventPayload::BoolToggle(CellId)`
    /// - `"hover_set:cell_id:value"` → `EventPayload::HoverSet { cell_id, value }`
    /// - `"list_toggle_all:list_cell_id:field"` → `EventPayload::ListToggleAll { list_cell_id, field }`
    /// - `"set_text:cell_id:value"` → `EventPayload::SetText { cell_id, value }`
    /// - Other text → `EventPayload::Unit`
    pub fn parse(text: &str) -> Self {
        if let Some(enter_text) = text.strip_prefix("Enter:") {
            Self::Enter(enter_text.to_string())
        } else if let Some(link_id) = text.strip_prefix("remove:") {
            Self::Remove(LinkId::new(link_id))
        } else if let Some(cell_id) = text.strip_prefix("toggle:") {
            Self::Toggle(CellId::new(cell_id))
        } else if let Some(cell_id) = text.strip_prefix("set_true:") {
            Self::SetTrue(CellId::new(cell_id))
        } else if let Some(cell_id) = text.strip_prefix("set_false:") {
            Self::SetFalse(CellId::new(cell_id))
        } else if let Some(cell_id) = text.strip_prefix("bool_toggle:") {
            Self::BoolToggle(CellId::new(cell_id))
        } else if let Some(rest) = text.strip_prefix("hover_set:") {
            // Format: "hover_set:cell_id:value" where value is "true" or "false"
            if let Some((cell_id, value_str)) = rest.rsplit_once(':') {
                let value = value_str == "true";
                Self::HoverSet { cell_id: CellId::new(cell_id), value }
            } else {
                Self::Unit
            }
        } else if let Some(rest) = text.strip_prefix("list_toggle_all:") {
            // Format: "list_toggle_all:list_cell_id:field"
            if let Some((list_cell_id, field)) = rest.split_once(':') {
                Self::ListToggleAll {
                    list_cell_id: CellId::new(list_cell_id),
                    field: field.to_string(),
                }
            } else {
                Self::Unit
            }
        } else if let Some(rest) = text.strip_prefix("set_text:") {
            // Format: "set_text:cell_id:value" (value may contain colons, so use first colon only)
            if let Some((cell_id, value)) = rest.split_once(':') {
                Self::SetText {
                    cell_id: CellId::new(cell_id),
                    value: value.to_string(),
                }
            } else {
                Self::Unit
            }
        } else {
            Self::Unit
        }
    }

    /// Convert to format string for backwards compatibility with string-based APIs.
    /// Phase 3.1: This method enables gradual migration - existing code can call
    /// to_format_string() instead of format!() directly.
    pub fn to_format_string(&self) -> String {
        match self {
            Self::Enter(text) => format!("Enter:{}", text),
            Self::Remove(link_id) => format!("remove:{}", link_id.name()),
            Self::Toggle(cell_id) => format!("toggle:{}", cell_id.name()),
            Self::SetTrue(cell_id) => format!("set_true:{}", cell_id.name()),
            Self::SetFalse(cell_id) => format!("set_false:{}", cell_id.name()),
            Self::BoolToggle(cell_id) => format!("bool_toggle:{}", cell_id.name()),
            Self::HoverSet { cell_id, value } => format!("hover_set:{}:{}", cell_id.name(), value),
            Self::ListToggleAll { list_cell_id, field } => format!("list_toggle_all:{}:{}", list_cell_id.name(), field),
            Self::SetText { cell_id, value } => format!("set_text:{}:{}", cell_id.name(), value),
            Self::Unit => String::new(),
        }
    }

    /// Check if this payload is an Enter event.
    pub fn is_enter(&self) -> bool {
        matches!(self, Self::Enter(_))
    }

    /// Check if this payload is a Remove event.
    pub fn is_remove(&self) -> bool {
        matches!(self, Self::Remove(_))
    }

    /// Get the text from an Enter payload.
    pub fn enter_text(&self) -> Option<&str> {
        if let Self::Enter(text) = self {
            Some(text)
        } else {
            None
        }
    }

    /// Get the trimmed text from an Enter payload, or None if not Enter or empty.
    /// This is the common pattern used throughout the worker.
    pub fn enter_text_trimmed(&self) -> Option<&str> {
        if let Self::Enter(text) = self {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        } else {
            None
        }
    }

    /// Parse an event text and get the trimmed Enter text if it's an Enter event.
    /// Returns None for non-Enter events or empty text.
    /// This is a convenience method for the common pattern in worker.rs.
    pub fn parse_enter_text(text: &str) -> Option<&str> {
        if let Some(stripped) = text.strip_prefix("Enter:") {
            let trimmed = stripped.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        } else {
            None
        }
    }

    /// Get the link ID from a Remove payload.
    pub fn remove_link_id(&self) -> Option<&LinkId> {
        if let Self::Remove(link_id) = self {
            Some(link_id)
        } else {
            None
        }
    }

    /// Parse an event text and get the link ID string if it's a "remove:" event.
    /// Returns None for non-remove events.
    /// This is a convenience method for the common pattern in worker.rs.
    pub fn parse_remove_link<'a>(text: &'a str) -> Option<&'a str> {
        text.strip_prefix("remove:")
    }
}

/// Filter for WHEN pattern matching on event values.
///
/// Phase 3.3: Consolidated from duplicate EventFilter (worker.rs) and DdEventFilter (dataflow.rs).
/// This is now the single source of truth for event filtering.
#[derive(Clone, Debug)]
pub enum EventFilter {
    /// Accept any event value
    Any,
    /// Only accept events with this exact text value (e.g., "Enter" for key events)
    TextEquals(String),
    /// Accept events starting with this prefix (e.g., "Enter:" for Enter+text)
    TextStartsWith(String),
}

impl EventFilter {
    /// Check if an event value matches this filter.
    pub fn matches(&self, event_value: &EventValue) -> bool {
        match self {
            EventFilter::Any => true,
            EventFilter::TextStartsWith(prefix) => {
                matches!(event_value, EventValue::Text(t) if t.starts_with(prefix))
            }
            EventFilter::TextEquals(pattern) => {
                matches!(event_value, EventValue::Text(t) if t == pattern)
            }
        }
    }

    /// Check if a text value matches this filter.
    pub fn matches_text(&self, text: &str) -> bool {
        match self {
            EventFilter::Any => true,
            EventFilter::TextEquals(pattern) => text == pattern,
            EventFilter::TextStartsWith(prefix) => text.starts_with(prefix),
        }
    }
}

/// Prefix for dynamically generated LINK IDs.
///
/// NOTE: This is exposed for migration purposes. Prefer using `LinkId::dynamic()`
/// and `LinkId::is_dynamic()` methods instead of using this constant directly.
pub const DYNAMIC_LINK_PREFIX: &str = "dynamic_link_";

/// A unique identifier for LINK events.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkId(pub String);

impl LinkId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Create a dynamic link ID from a counter.
    pub fn dynamic(counter: u32) -> Self {
        Self(format!("{}{}", DYNAMIC_LINK_PREFIX, counter))
    }

    /// Get the name of this LINK ID.
    pub fn name(&self) -> &str {
        &self.0
    }

    /// Check if this is a dynamic link ID.
    pub fn is_dynamic(&self) -> bool {
        self.0.starts_with(DYNAMIC_LINK_PREFIX)
    }

    /// Get the dynamic counter if this is a dynamic link ID.
    pub fn dynamic_counter(&self) -> Option<u32> {
        if self.is_dynamic() {
            self.0.strip_prefix(DYNAMIC_LINK_PREFIX)
                .and_then(|s| s.parse().ok())
        } else {
            None
        }
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for LinkId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for LinkId {
    fn from(s: &str) -> Self {
        LinkId::new(s)
    }
}

impl From<String> for LinkId {
    fn from(s: String) -> Self {
        LinkId(s)
    }
}

impl From<std::sync::Arc<str>> for LinkId {
    fn from(s: std::sync::Arc<str>) -> Self {
        LinkId::new(s.to_string())
    }
}

impl PartialEq<str> for LinkId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<String> for LinkId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

/// A unique identifier for timers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(pub String);

impl TimerId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the name of this Timer ID.
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TimerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Event types that can be injected into DD.
#[derive(Clone, Debug)]
pub enum Event {
    /// A LINK was fired (e.g., button click)
    Link { id: LinkId, value: EventValue },
    /// A timer tick occurred
    Timer { id: TimerId, tick: u64 },
    /// An external event (for extensibility)
    External { name: String, value: EventValue },
}

/// Value types for DD events.
///
/// This is a simplified event value type. The full Value from dd_value.rs
/// is used for the actual dataflow, but events use this simpler type.
///
/// Note: Implements Ord/Hash using OrderedFloat for Number to satisfy DD trait bounds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventValue {
    /// No value (e.g., button press)
    Unit,
    /// Text value (e.g., text input change)
    Text(String),
    /// Boolean value (e.g., checkbox toggle)
    Bool(bool),
    /// Numeric value (using OrderedFloat for Ord impl)
    Number(ordered_float::OrderedFloat<f64>),
    /// Pre-instantiated item for list append (O(delta) optimization).
    /// The item has already had its CellRef/LinkRef IDs generated and registered
    /// BEFORE being injected into DD. This allows DD to do pure appending.
    PreparedItem(super::value::Value),
}

impl EventValue {
    pub fn unit() -> Self {
        Self::Unit
    }

    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn bool(b: bool) -> Self {
        Self::Bool(b)
    }

    pub fn number(n: f64) -> Self {
        Self::Number(ordered_float::OrderedFloat(n))
    }

    /// Create a prepared item event value for O(delta) list operations.
    /// The item should have already had its IDs generated and registered.
    pub fn prepared_item(item: super::value::Value) -> Self {
        Self::PreparedItem(item)
    }

    /// Check if this is a prepared item.
    pub fn is_prepared_item(&self) -> bool {
        matches!(self, Self::PreparedItem(_))
    }

    /// Get the prepared item if this is a PreparedItem variant.
    pub fn as_prepared_item(&self) -> Option<&super::value::Value> {
        if let Self::PreparedItem(item) = self {
            Some(item)
        } else {
            None
        }
    }
}

// ============================================================================
// PHASE 8: DD-Native LINK Handling
// ============================================================================

/// Action to perform when a link fires - DD-native version of DynamicLinkAction.
///
/// Unlike `DynamicLinkAction` in the IO layer (inputs.rs), these actions are
/// designed to be part of DD collections and used with DD joins. The IO layer
/// will be thinned to just convert browser events to DD events.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkAction {
    /// Toggle a boolean cell: `!state`
    BoolToggle,
    /// Set a cell to true (e.g., entering edit mode via double-click)
    SetTrue,
    /// Set a cell to false (e.g., exiting edit mode via Escape/blur)
    SetFalse,
    /// Update cell to match event boolean value (for hover state)
    HoverState,
    /// Remove a list item by identity (antijoin operation)
    RemoveListItem {
        /// The list cell containing items
        list_cell_id: CellId,
        /// Path to the identity field (e.g., ["remove", "link_ref"])
        identity_path: Vec<String>,
    },
    /// Toggle ALL items' completed field in a list
    ListToggleAllCompleted {
        /// The list cell to iterate
        list_cell_id: CellId,
        /// Field to toggle on each item (e.g., "completed")
        completed_field: String,
    },
    /// Set a text cell from the event text value
    SetText,
    /// Set a cell to a constant value
    SetValue(super::value::Value),
}

/// Mapping from a LINK to the cell it affects.
///
/// This replaces the `DYNAMIC_LINK_ACTIONS` HashMap in inputs.rs.
/// In DD, these mappings form a collection that can be joined with
/// events to determine what cell updates to produce.
///
/// # Example DD Usage
///
/// ```ignore
/// // Instead of HashMap lookup in IO layer:
/// let mappings: Collection<G, LinkCellMapping, isize> = ...;
/// let link_events: Collection<G, (LinkId, EventValue), isize> = ...;
///
/// // Join to get cell updates:
/// let cell_updates = link_events
///     .join(&mappings)  // O(delta) join
///     .map(|(link_id, (event, mapping))| {
///         let new_value = apply_action(mapping.action, mapping.cell_id, event);
///         (mapping.cell_id, new_value)
///     });
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkCellMapping {
    /// The link that triggers the action
    pub link_id: LinkId,
    /// The cell that gets updated
    pub cell_id: CellId,
    /// The action to perform
    pub action: LinkAction,
    /// Optional key filter (e.g., ["Escape", "Enter"] for SetFalseOnKeys)
    pub key_filter: Option<Vec<String>>,
}

impl LinkCellMapping {
    /// Create a new simple mapping (link → cell with action).
    pub fn new(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>, action: LinkAction) -> Self {
        Self {
            link_id: link_id.into(),
            cell_id: cell_id.into(),
            action,
            key_filter: None,
        }
    }

    /// Create a mapping with a key filter (for SetFalseOnKeys pattern).
    pub fn with_key_filter(
        link_id: impl Into<LinkId>,
        cell_id: impl Into<CellId>,
        action: LinkAction,
        keys: Vec<String>,
    ) -> Self {
        Self {
            link_id: link_id.into(),
            cell_id: cell_id.into(),
            action,
            key_filter: Some(keys),
        }
    }

    /// Create a BoolToggle mapping (most common for checkboxes).
    pub fn bool_toggle(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::BoolToggle)
    }

    /// Create a SetTrue mapping (e.g., double-click to edit).
    pub fn set_true(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::SetTrue)
    }

    /// Create a SetFalse mapping (e.g., blur to exit edit).
    pub fn set_false(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::SetFalse)
    }

    /// Create a HoverState mapping (mouseenter/mouseleave).
    pub fn hover_state(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::HoverState)
    }

    /// Create a RemoveListItem mapping.
    pub fn remove_list_item(
        link_id: impl Into<LinkId>,
        list_cell_id: impl Into<CellId>,
        identity_path: Vec<String>,
    ) -> Self {
        Self {
            link_id: link_id.into(),
            cell_id: list_cell_id.clone().into(), // Same as list_cell_id for convenience
            action: LinkAction::RemoveListItem {
                list_cell_id: list_cell_id.into(),
                identity_path,
            },
            key_filter: None,
        }
    }

    /// Check if this mapping matches the given event.
    /// Returns true if link_id matches and key_filter passes.
    pub fn matches(&self, event_link_id: &LinkId, event_value: &EventValue) -> bool {
        if self.link_id != *event_link_id {
            return false;
        }

        // Check key filter if present
        if let Some(ref keys) = self.key_filter {
            if let EventValue::Text(text) = event_value {
                // Check if text is one of the allowed keys
                return keys.iter().any(|k| text == k || text.starts_with(&format!("{}:", k)));
            }
            return false;
        }

        true
    }
}

/// Editing handler configuration - maps multiple links to a pair of cells.
///
/// This replaces `DynamicLinkAction::EditingHandler` with a declarative
/// structure that can be expanded into multiple `LinkCellMapping`s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditingHandlerConfig {
    /// The cell that tracks whether we're in edit mode
    pub editing_cell: CellId,
    /// The cell that stores the text being edited
    pub title_cell: CellId,
    /// Link for double-click to enter edit mode
    pub double_click_link: LinkId,
    /// Link for key events (Enter to save, Escape to cancel)
    pub key_down_link: LinkId,
    /// Link for blur event (exit edit mode)
    pub blur_link: LinkId,
}

impl EditingHandlerConfig {
    /// Expand this config into individual LinkCellMappings for DD.
    pub fn to_mappings(&self) -> Vec<LinkCellMapping> {
        vec![
            // Double-click → editing = true
            LinkCellMapping::set_true(self.double_click_link.clone(), self.editing_cell.clone()),
            // Blur → editing = false
            LinkCellMapping::set_false(self.blur_link.clone(), self.editing_cell.clone()),
            // Escape → editing = false
            LinkCellMapping::with_key_filter(
                self.key_down_link.clone(),
                self.editing_cell.clone(),
                LinkAction::SetFalse,
                vec!["Escape".to_string()],
            ),
            // Enter:text → title = text, editing = false
            // Note: This needs special handling for the two-step update
            LinkCellMapping::with_key_filter(
                self.key_down_link.clone(),
                self.title_cell.clone(),
                LinkAction::SetText,
                vec!["Enter".to_string()],
            ),
            LinkCellMapping::with_key_filter(
                self.key_down_link.clone(),
                self.editing_cell.clone(),
                LinkAction::SetFalse,
                vec!["Enter".to_string()],
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_send_and_close() {
        let (input, output) = channel::<i32>();

        // Send some values
        assert!(input.send(1).is_ok());
        assert!(input.send(2).is_ok());
        assert!(input.send(3).is_ok());

        // Drop sender to close channel
        drop(input);

        // Drop output - this is fine, the channel just closes
        drop(output);
    }

    #[test]
    fn test_send_or_drop_doesnt_panic_on_closed() {
        let (input, output) = channel::<i32>();
        drop(output); // Close the channel

        // Should not panic
        input.send_or_drop(42);
    }

    #[test]
    fn test_send_fails_when_receiver_dropped() {
        let (input, output) = channel::<i32>();
        drop(output);

        // Should return Err with the value
        let result = input.send(42);
        assert!(result.is_err());
        assert_eq!(result.err(), Some(42));
    }

    #[test]
    fn test_cell_id_static() {
        let id = CellId::new("counter");
        assert_eq!(id.name(), "counter");
        assert_eq!(format!("{}", id), "counter");
        assert!(!id.is_dynamic());
    }

    #[test]
    fn test_cell_id_dynamic() {
        let id = CellId::dynamic(42);
        assert_eq!(id.name(), "dynamic_cell_42");
        assert_eq!(format!("{}", id), "dynamic_cell_42");
        assert!(id.is_dynamic());
    }

    #[test]
    fn test_bool_tag() {
        assert_eq!(BoolTag::from_tag("True"), Some(BoolTag::True));
        assert_eq!(BoolTag::from_tag("False"), Some(BoolTag::False));
        assert_eq!(BoolTag::from_tag("Other"), None);
        assert!(BoolTag::True.to_bool());
        assert!(!BoolTag::False.to_bool());
    }

    #[test]
    fn test_element_tag() {
        assert!(ElementTag::is_element("Element"));
        assert!(!ElementTag::is_element("NotElement"));
    }

    #[test]
    fn test_link_id() {
        let id = LinkId::new("button.press");
        assert_eq!(id.name(), "button.press");
        assert_eq!(format!("{}", id), "button.press");
    }

    #[test]
    fn test_dd_event_value_constructors() {
        assert!(matches!(EventValue::unit(), EventValue::Unit));
        assert!(matches!(EventValue::text("hello"), EventValue::Text(s) if s == "hello"));
        assert!(matches!(EventValue::bool(true), EventValue::Bool(true)));
        assert!(matches!(EventValue::number(3.14), EventValue::Number(n) if (n.0 - 3.14).abs() < f64::EPSILON));
    }

    // Phase 8: LinkCellMapping tests

    #[test]
    fn test_link_action_variants() {
        // Basic actions
        assert_eq!(LinkAction::BoolToggle, LinkAction::BoolToggle);
        assert_eq!(LinkAction::SetTrue, LinkAction::SetTrue);
        assert_eq!(LinkAction::SetFalse, LinkAction::SetFalse);
        assert_eq!(LinkAction::HoverState, LinkAction::HoverState);
        assert_eq!(LinkAction::SetText, LinkAction::SetText);

        // RemoveListItem
        let remove = LinkAction::RemoveListItem {
            list_cell_id: CellId::new("todos"),
            identity_path: vec!["remove".to_string(), "link_ref".to_string()],
        };
        assert!(matches!(remove, LinkAction::RemoveListItem { .. }));
    }

    #[test]
    fn test_link_cell_mapping_constructors() {
        // Basic mapping
        let toggle = LinkCellMapping::bool_toggle("checkbox_click", "completed");
        assert_eq!(toggle.link_id, LinkId::new("checkbox_click"));
        assert_eq!(toggle.cell_id, CellId::new("completed"));
        assert_eq!(toggle.action, LinkAction::BoolToggle);
        assert!(toggle.key_filter.is_none());

        // SetTrue mapping
        let set_true = LinkCellMapping::set_true("double_click", "editing");
        assert_eq!(set_true.action, LinkAction::SetTrue);

        // SetFalse mapping
        let set_false = LinkCellMapping::set_false("blur", "editing");
        assert_eq!(set_false.action, LinkAction::SetFalse);

        // HoverState mapping
        let hover = LinkCellMapping::hover_state("mouseenter", "is_hovered");
        assert_eq!(hover.action, LinkAction::HoverState);
    }

    #[test]
    fn test_link_cell_mapping_with_key_filter() {
        let mapping = LinkCellMapping::with_key_filter(
            "key_down",
            "editing",
            LinkAction::SetFalse,
            vec!["Escape".to_string(), "Enter".to_string()],
        );

        assert_eq!(mapping.link_id, LinkId::new("key_down"));
        assert_eq!(mapping.cell_id, CellId::new("editing"));
        assert_eq!(mapping.action, LinkAction::SetFalse);
        assert!(mapping.key_filter.is_some());
        assert_eq!(mapping.key_filter.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_link_cell_mapping_matches() {
        // Simple mapping matches its link
        let toggle = LinkCellMapping::bool_toggle("checkbox", "completed");
        assert!(toggle.matches(&LinkId::new("checkbox"), &EventValue::Unit));
        assert!(!toggle.matches(&LinkId::new("other"), &EventValue::Unit));

        // Key-filtered mapping
        let escape_only = LinkCellMapping::with_key_filter(
            "key_down",
            "editing",
            LinkAction::SetFalse,
            vec!["Escape".to_string()],
        );
        assert!(escape_only.matches(&LinkId::new("key_down"), &EventValue::Text("Escape".to_string())));
        assert!(!escape_only.matches(&LinkId::new("key_down"), &EventValue::Text("Enter".to_string())));
        assert!(!escape_only.matches(&LinkId::new("key_down"), &EventValue::Unit));

        // Key filter with prefix matching (e.g., "Enter:text")
        let enter_filter = LinkCellMapping::with_key_filter(
            "key_down",
            "title",
            LinkAction::SetText,
            vec!["Enter".to_string()],
        );
        assert!(enter_filter.matches(&LinkId::new("key_down"), &EventValue::Text("Enter:hello".to_string())));
        assert!(!enter_filter.matches(&LinkId::new("key_down"), &EventValue::Text("Escape".to_string())));
    }

    #[test]
    fn test_editing_handler_config() {
        let config = EditingHandlerConfig {
            editing_cell: CellId::new("editing"),
            title_cell: CellId::new("title"),
            double_click_link: LinkId::new("dblclick"),
            key_down_link: LinkId::new("keydown"),
            blur_link: LinkId::new("blur"),
        };

        let mappings = config.to_mappings();

        // Should produce 5 mappings:
        // 1. dblclick → editing = true
        // 2. blur → editing = false
        // 3. Escape → editing = false
        // 4. Enter → title = text
        // 5. Enter → editing = false
        assert_eq!(mappings.len(), 5);

        // Check double-click mapping
        assert_eq!(mappings[0].link_id, LinkId::new("dblclick"));
        assert_eq!(mappings[0].cell_id, CellId::new("editing"));
        assert_eq!(mappings[0].action, LinkAction::SetTrue);

        // Check blur mapping
        assert_eq!(mappings[1].link_id, LinkId::new("blur"));
        assert_eq!(mappings[1].cell_id, CellId::new("editing"));
        assert_eq!(mappings[1].action, LinkAction::SetFalse);

        // Check Escape mapping has key filter
        assert!(mappings[2].key_filter.is_some());
        assert!(mappings[2].key_filter.as_ref().unwrap().contains(&"Escape".to_string()));
    }

    #[test]
    fn test_remove_list_item_mapping() {
        let mapping = LinkCellMapping::remove_list_item(
            "delete_btn",
            "todos",
            vec!["remove".to_string(), "link_ref".to_string()],
        );

        assert_eq!(mapping.link_id, LinkId::new("delete_btn"));
        assert!(matches!(mapping.action, LinkAction::RemoveListItem { .. }));

        if let LinkAction::RemoveListItem { list_cell_id, identity_path } = mapping.action {
            assert_eq!(list_cell_id, CellId::new("todos"));
            assert_eq!(identity_path, vec!["remove".to_string(), "link_ref".to_string()]);
        }
    }
}
