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
        self.sender
            .unbounded_send(value)
            .map_err(|e| e.into_inner())
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
        if b {
            Self::True
        } else {
            Self::False
        }
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

/// Filter for WHEN pattern matching on event values.
///
/// Consolidated from duplicate EventFilter (worker.rs) and DdEventFilter (dataflow.rs).
/// This is now the single source of truth for event filtering.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EventFilter {
    /// Accept any event value
    Any,
    /// Only accept events with this exact text value
    TextEquals(String),
    /// Only accept key-down events matching this key
    KeyEquals(Key),
}

impl EventFilter {
    /// Check if an event value matches this filter.
    pub fn matches(&self, event_value: &EventValue) -> bool {
        match self {
            EventFilter::Any => true,
            EventFilter::TextEquals(pattern) => {
                matches!(event_value, EventValue::Text(t) if t == pattern)
                    || matches!(event_value, EventValue::PreparedItem { source_text: Some(t), .. } if t == pattern)
            }
            EventFilter::KeyEquals(key) => {
                matches!(event_value, EventValue::KeyDown { key: k, .. } if k == key)
                    || matches!(event_value, EventValue::PreparedItem { source_key: Some(k), .. } if k == key)
            }
        }
    }

    /// Check if a text value matches this filter.
    pub fn matches_text(&self, text: &str) -> bool {
        match self {
            EventFilter::Any => true,
            EventFilter::TextEquals(pattern) => text == pattern,
            EventFilter::KeyEquals(key) => key.as_str() == text,
        }
    }
}

/// Prefix for dynamically generated LINK IDs.
///
/// NOTE: This is exposed for migration purposes. Prefer using `LinkId::dynamic()`
/// and `LinkId::is_dynamic()` methods instead of using this constant directly.
pub const DYNAMIC_LINK_PREFIX: &str = "dynamic_link_";
/// Internal link ID used to propagate browser route changes into DD.
pub const ROUTE_CHANGE_LINK_ID: &str = "__route_change__";
/// Field name for per-item identity keys embedded in list items.
pub const ITEM_KEY_FIELD: &str = "__key";

/// A unique identifier for LINK events.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkId {
    /// Static link defined in source code
    Static(std::sync::Arc<str>),
    /// Dynamic link generated for list items
    Dynamic {
        counter: u32,
        name: std::sync::Arc<str>,
    },
}

impl LinkId {
    pub fn new(name: impl Into<String>) -> Self {
        Self::Static(std::sync::Arc::from(name.into()))
    }

    /// Create a dynamic link ID from a counter.
    pub fn dynamic(counter: u32) -> Self {
        let name = format!("{}{}", DYNAMIC_LINK_PREFIX, counter);
        Self::Dynamic {
            counter,
            name: std::sync::Arc::from(name),
        }
    }

    /// Get the name of this LINK ID.
    pub fn name(&self) -> &str {
        match self {
            Self::Static(name) => name.as_ref(),
            Self::Dynamic { name, .. } => name.as_ref(),
        }
    }

    /// Check if this is a dynamic link ID.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Self::Dynamic { .. })
    }

    /// Get the dynamic counter if this is a dynamic link ID.
    pub fn dynamic_counter(&self) -> Option<u32> {
        match self {
            Self::Dynamic { counter, .. } => Some(*counter),
            _ => None,
        }
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl AsRef<str> for LinkId {
    fn as_ref(&self) -> &str {
        self.name()
    }
}

impl From<&str> for LinkId {
    fn from(s: &str) -> Self {
        LinkId::new(s)
    }
}

impl From<String> for LinkId {
    fn from(s: String) -> Self {
        LinkId::new(s)
    }
}

impl From<std::sync::Arc<str>> for LinkId {
    fn from(s: std::sync::Arc<str>) -> Self {
        LinkId::Static(s)
    }
}

impl PartialEq<str> for LinkId {
    fn eq(&self, other: &str) -> bool {
        self.name() == other
    }
}

impl PartialEq<String> for LinkId {
    fn eq(&self, other: &String) -> bool {
        self.name() == other.as_str()
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
    /// An item-scoped LINK fired from a list row interaction.
    ///
    /// `item_key` identifies the concrete row item. `action_id` identifies which
    /// per-item interaction fired (for example, title double-click vs checkbox toggle).
    /// `list_cell_id` is optional during migration and may be populated when the
    /// bridge knows the owning list cell at dispatch time.
    ItemLink {
        list_cell_id: Option<CellId>,
        item_key: std::sync::Arc<str>,
        action_id: LinkId,
        value: EventValue,
    },
    /// A timer tick occurred
    Timer { id: TimerId, tick: u64 },
    /// An external event (for extensibility)
    External { name: String, value: EventValue },
}

/// Normalized key representation for DD events.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Enter,
    Escape,
    Other(std::sync::Arc<str>),
}

impl Key {
    pub fn from_str(key: &str) -> Self {
        match key {
            "Enter" => Self::Enter,
            "Escape" => Self::Escape,
            other => Self::Other(std::sync::Arc::from(other)),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::Other(s) => s.as_ref(),
        }
    }
}

impl From<&str> for Key {
    fn from(value: &str) -> Self {
        Self::from_str(value)
    }
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
    /// Key-down event with optional text (e.g., Enter with input contents)
    KeyDown { key: Key, text: Option<String> },
    /// Boolean value (e.g., checkbox toggle)
    Bool(bool),
    /// Numeric value (using OrderedFloat for Ord impl)
    Number(ordered_float::OrderedFloat<f64>),
    /// Pre-instantiated item for list append (O(delta) optimization).
    /// The item has fresh CellRef/LinkRef IDs generated BEFORE being injected into DD.
    /// Initial cell values are carried alongside to keep DD transforms pure.
    PreparedItem {
        /// Fully instantiated item with fresh CellRef/LinkRef IDs.
        item: super::value::Value,
        /// Initial values for any new cells created during instantiation.
        /// Each entry is (cell_id, initial_value).
        initializations: Vec<(String, super::value::Value)>,
        /// Original event text (if any) for filter matching and downstream transforms.
        source_text: Option<String>,
        /// Original event key (if any) for filter matching and downstream transforms.
        source_key: Option<Key>,
    },
}

impl EventValue {
    pub fn unit() -> Self {
        Self::Unit
    }

    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn key_down(key: Key, text: Option<String>) -> Self {
        Self::KeyDown { key, text }
    }

    pub fn bool(b: bool) -> Self {
        Self::Bool(b)
    }

    pub fn number(n: f64) -> Self {
        Self::Number(ordered_float::OrderedFloat(n))
    }

    /// Create a prepared item event value for O(delta) list operations.
    /// The item should have already had its IDs generated and initializations collected.
    pub fn prepared_item(
        item: super::value::Value,
        initializations: Vec<(String, super::value::Value)>,
    ) -> Self {
        Self::PreparedItem {
            item,
            initializations,
            source_text: None,
            source_key: None,
        }
    }

    /// Create a prepared item with original event metadata.
    pub fn prepared_item_with_source(
        item: super::value::Value,
        initializations: Vec<(String, super::value::Value)>,
        source_key: Option<Key>,
        source_text: Option<String>,
    ) -> Self {
        Self::PreparedItem {
            item,
            initializations,
            source_text,
            source_key,
        }
    }

    /// Check if this is a prepared item.
    pub fn is_prepared_item(&self) -> bool {
        matches!(self, Self::PreparedItem { .. })
    }

    /// Get the prepared item if this is a PreparedItem variant.
    pub fn as_prepared_item(
        &self,
    ) -> Option<(&super::value::Value, &Vec<(String, super::value::Value)>)> {
        if let Self::PreparedItem {
            item,
            initializations,
            ..
        } = self
        {
            Some((item, initializations))
        } else {
            None
        }
    }

    pub fn key(&self) -> Option<&Key> {
        match self {
            Self::KeyDown { key, .. } => Some(key),
            Self::PreparedItem {
                source_key: Some(key),
                ..
            } => Some(key),
            _ => None,
        }
    }

    pub fn enter_text(&self) -> Option<&str> {
        match self {
            Self::KeyDown {
                key: Key::Enter,
                text: Some(text),
            } => Some(text.as_str()),
            Self::KeyDown {
                key: Key::Enter,
                text: None,
            } => {
                panic!("[DD EventValue] Enter key event missing text payload");
            }
            Self::PreparedItem {
                source_key: Some(Key::Enter),
                source_text: Some(text),
                ..
            } => Some(text.as_str()),
            Self::PreparedItem {
                source_key: Some(Key::Enter),
                source_text: None,
                ..
            } => {
                panic!("[DD EventValue] PreparedItem missing source_text for Enter key");
            }
            _ => None,
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
    /// Set a cell to true
    SetTrue,
    /// Set a cell to false
    SetFalse,
    /// Add a constant numeric value to a numeric cell
    AddValue(super::value::Value),
    /// Update cell to match event boolean value (for hover state)
    HoverState,
    /// Remove a list item by identity (antijoin operation)
    RemoveListItem {
        /// The list cell containing items
        list_cell_id: CellId,
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
    /// Optional key filter (e.g., [Key::Escape, Key::Enter] for SetFalseOnKeys)
    pub key_filter: Option<Vec<Key>>,
}

impl LinkCellMapping {
    /// Create a new simple mapping (link → cell with action).
    pub fn new(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>, action: LinkAction) -> Self {
        let mapping = Self {
            link_id: link_id.into(),
            cell_id: cell_id.into(),
            action,
            key_filter: None,
        };
        if let LinkAction::RemoveListItem { list_cell_id } = &mapping.action {
            if *list_cell_id != mapping.cell_id {
                panic!(
                    "[DD LinkCellMapping] RemoveListItem list_cell_id '{}' must match mapping cell_id '{}'",
                    list_cell_id.name(),
                    mapping.cell_id.name()
                );
            }
        }
        mapping
    }

    /// Create a mapping with a key filter (for SetFalseOnKeys pattern).
    pub fn with_key_filter(
        link_id: impl Into<LinkId>,
        cell_id: impl Into<CellId>,
        action: LinkAction,
        keys: Vec<Key>,
    ) -> Self {
        if keys.is_empty() {
            panic!("[DD LinkCellMapping] key_filter must not be empty");
        }
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

    /// Create a SetTrue mapping.
    pub fn set_true(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::SetTrue)
    }

    /// Create a SetFalse mapping.
    pub fn set_false(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::SetFalse)
    }

    /// Create a HoverState mapping (mouseenter/mouseleave).
    pub fn hover_state(link_id: impl Into<LinkId>, cell_id: impl Into<CellId>) -> Self {
        Self::new(link_id, cell_id, LinkAction::HoverState)
    }

    /// Create a RemoveListItem mapping.
    pub fn remove_list_item(link_id: impl Into<LinkId>, list_cell_id: impl Into<CellId>) -> Self {
        let list_cell_id: CellId = list_cell_id.into();
        Self {
            link_id: link_id.into(),
            cell_id: list_cell_id.clone(), // Same as list_cell_id for convenience
            action: LinkAction::RemoveListItem { list_cell_id },
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
            return matches!(event_value, EventValue::KeyDown { key, .. } if keys.contains(key))
                || matches!(event_value, EventValue::PreparedItem { source_key: Some(key), .. } if keys.contains(key));
        }

        true
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
        assert!(matches!(
            EventValue::key_down(Key::Enter, None),
            EventValue::KeyDown {
                key: Key::Enter,
                text: None
            }
        ));
        assert!(matches!(EventValue::bool(true), EventValue::Bool(true)));
        assert!(
            matches!(EventValue::number(3.14), EventValue::Number(n) if (n.0 - 3.14).abs() < f64::EPSILON)
        );
    }

    // LinkCellMapping tests

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
            vec![Key::Escape, Key::Enter],
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
            vec![Key::Escape],
        );
        assert!(escape_only.matches(
            &LinkId::new("key_down"),
            &EventValue::key_down(Key::Escape, None)
        ));
        assert!(!escape_only.matches(
            &LinkId::new("key_down"),
            &EventValue::key_down(Key::Enter, None)
        ));
        assert!(!escape_only.matches(&LinkId::new("key_down"), &EventValue::Unit));

        // Key filter with Enter and text payload
        let enter_filter = LinkCellMapping::with_key_filter(
            "key_down",
            "title",
            LinkAction::SetText,
            vec![Key::Enter],
        );
        assert!(enter_filter.matches(
            &LinkId::new("key_down"),
            &EventValue::key_down(Key::Enter, Some("hello".to_string()))
        ));
        assert!(!enter_filter.matches(
            &LinkId::new("key_down"),
            &EventValue::key_down(Key::Escape, None)
        ));
    }

    #[test]
    fn test_remove_list_item_mapping() {
        let mapping = LinkCellMapping::remove_list_item("delete_btn", "todos");

        assert_eq!(mapping.link_id, LinkId::new("delete_btn"));
        assert!(matches!(mapping.action, LinkAction::RemoveListItem { .. }));

        if let LinkAction::RemoveListItem { list_cell_id } = mapping.action {
            assert_eq!(list_cell_id, CellId::new("todos"));
        }
    }
}
