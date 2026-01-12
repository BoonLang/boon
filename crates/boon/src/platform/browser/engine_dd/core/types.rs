//! Core DD types with anti-cheat enforcement.
//!
//! These types are designed to make synchronous state access IMPOSSIBLE:
//! - DdOutput<T>: Can only be observed via async stream, never read synchronously
//! - DdInput<T>: Can only inject events, never read state
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
/// let output: DdOutput<DdValue> = worker.output();
///
/// // CORRECT: async observation
/// let stream = output.stream();
/// stream.for_each(|value| { /* handle value */ }).await;
///
/// // IMPOSSIBLE: no .get() method exists
/// // let value = output.get();  // COMPILE ERROR
/// ```
pub struct DdOutput<T> {
    receiver: mpsc::UnboundedReceiver<T>,
}

impl<T> DdOutput<T> {
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
/// let input: DdInput<DdEvent> = worker.input();
///
/// // CORRECT: inject event
/// input.send(DdEvent::Link { id, value });
///
/// // IMPOSSIBLE: no .get() method exists
/// // let state = input.get();  // COMPILE ERROR
/// ```
#[derive(Clone)]
pub struct DdInput<T> {
    sender: mpsc::UnboundedSender<T>,
}

impl<T> DdInput<T> {
    /// Create a new input from a sender channel.
    ///
    /// This should only be called from the DD worker when setting up input channels.
    pub(crate) fn new(sender: mpsc::UnboundedSender<T>) -> Self {
        Self { sender }
    }

    /// Inject an event into the DD dataflow.
    ///
    /// This is the ONLY way to interact with DD state - by injecting events.
    /// The DD worker will process the event and produce outputs via DdOutput.
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
pub fn channel<T>() -> (DdInput<T>, DdOutput<T>) {
    let (sender, receiver) = mpsc::unbounded();
    (DdInput::new(sender), DdOutput::new(receiver))
}

/// A unique identifier for HOLD state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HoldId(pub String);

impl HoldId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the name of this HOLD ID.
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HoldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A unique identifier for LINK events.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(pub String);

impl LinkId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the name of this LINK ID.
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
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
pub enum DdEvent {
    /// A LINK was fired (e.g., button click)
    Link { id: LinkId, value: DdEventValue },
    /// A timer tick occurred
    Timer { id: TimerId, tick: u64 },
    /// An external event (for extensibility)
    External { name: String, value: DdEventValue },
}

/// Value types for DD events.
///
/// This is a simplified event value type. The full DdValue from dd_value.rs
/// is used for the actual dataflow, but events use this simpler type.
///
/// Note: Implements Ord/Hash using OrderedFloat for Number to satisfy DD trait bounds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DdEventValue {
    /// No value (e.g., button press)
    Unit,
    /// Text value (e.g., text input change)
    Text(String),
    /// Boolean value (e.g., checkbox toggle)
    Bool(bool),
    /// Numeric value (using OrderedFloat for Ord impl)
    Number(ordered_float::OrderedFloat<f64>),
}

impl DdEventValue {
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
    fn test_hold_id() {
        let id = HoldId::new("counter");
        assert_eq!(id.name(), "counter");
        assert_eq!(format!("{}", id), "counter");
    }

    #[test]
    fn test_link_id() {
        let id = LinkId::new("button.press");
        assert_eq!(id.name(), "button.press");
        assert_eq!(format!("{}", id), "button.press");
    }

    #[test]
    fn test_dd_event_value_constructors() {
        assert!(matches!(DdEventValue::unit(), DdEventValue::Unit));
        assert!(matches!(DdEventValue::text("hello"), DdEventValue::Text(s) if s == "hello"));
        assert!(matches!(DdEventValue::bool(true), DdEventValue::Bool(true)));
        assert!(matches!(DdEventValue::number(3.14), DdEventValue::Number(n) if (n.0 - 3.14).abs() < f64::EPSILON));
    }
}
