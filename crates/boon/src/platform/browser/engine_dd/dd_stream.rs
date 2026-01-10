//! Reactive stream primitives for DD engine.
//!
//! This module provides lightweight reactive primitives that work with DdValue
//! and integrate naturally with the browser's event loop via Zoon.
//!
//! Unlike the actor engine's ValueActor (which uses async streams and channels),
//! these primitives use Zoon's Mutable/Signal system for simplicity.

use std::rc::Rc;

use zoon::{Mutable, MutableVec, Signal, SignalExtExt};

use super::dd_value::DdValue;

/// A reactive signal containing a DdValue.
///
/// This is a wrapper around Zoon's `Mutable<DdValue>` that provides
/// a consistent API for the DD engine.
///
/// # Example
/// ```ignore
/// let counter = DdSignal::new(DdValue::int(0));
/// counter.set(DdValue::int(1));
/// assert_eq!(counter.get(), DdValue::int(1));
/// ```
#[derive(Clone)]
pub struct DdSignal {
    inner: Mutable<DdValue>,
}

impl DdSignal {
    /// Create a new signal with an initial value.
    pub fn new(initial: DdValue) -> Self {
        Self {
            inner: Mutable::new(initial),
        }
    }

    /// Get the current value.
    pub fn get(&self) -> DdValue {
        self.inner.get_cloned()
    }

    /// Set a new value, notifying all subscribers.
    pub fn set(&self, value: DdValue) {
        self.inner.set(value);
    }

    /// Update the value using a closure.
    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&DdValue) -> DdValue,
    {
        let current = self.get();
        let new_value = f(&current);
        self.set(new_value);
    }

    /// Get a signal that emits when the value changes.
    ///
    /// This integrates with Zoon's reactive system for UI updates.
    pub fn signal(&self) -> impl Signal<Item = DdValue> + use<> {
        self.inner.signal_cloned()
    }

    /// Get the underlying Mutable for advanced usage.
    pub fn as_mutable(&self) -> &Mutable<DdValue> {
        &self.inner
    }
}

impl Default for DdSignal {
    fn default() -> Self {
        Self::new(DdValue::Unit)
    }
}

impl std::fmt::Debug for DdSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdSignal")
            .field("value", &self.get())
            .finish()
    }
}

/// A reactive list of DdValues.
///
/// This wraps Zoon's `MutableVec<DdValue>` for list operations
/// like append, remove, map, filter.
#[derive(Clone)]
pub struct DdList {
    inner: Rc<MutableVec<DdValue>>,
}

impl DdList {
    /// Create a new empty list.
    pub fn new() -> Self {
        Self {
            inner: Rc::new(MutableVec::new()),
        }
    }

    /// Create a list from initial values.
    pub fn from_values(values: impl IntoIterator<Item = DdValue>) -> Self {
        let list = Self::new();
        {
            let mut lock = list.inner.lock_mut();
            for value in values {
                lock.push_cloned(value);
            }
        }
        list
    }

    /// Get the number of items.
    pub fn len(&self) -> usize {
        self.inner.lock_ref().len()
    }

    /// Check if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.lock_ref().is_empty()
    }

    /// Append an item to the list.
    pub fn push(&self, value: DdValue) {
        self.inner.lock_mut().push_cloned(value);
    }

    /// Remove an item at index.
    pub fn remove(&self, index: usize) {
        self.inner.lock_mut().remove(index);
    }

    /// Clear all items.
    pub fn clear(&self) {
        self.inner.lock_mut().clear();
    }

    /// Get all items as a Vec.
    pub fn to_vec(&self) -> Vec<DdValue> {
        self.inner.lock_ref().iter().cloned().collect()
    }

    /// Get the underlying MutableVec for signal-based rendering.
    pub fn as_mutable_vec(&self) -> &Rc<MutableVec<DdValue>> {
        &self.inner
    }

    /// Convert to a DdValue::List.
    pub fn to_dd_value(&self) -> DdValue {
        DdValue::list(self.to_vec())
    }
}

impl Default for DdList {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DdList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdList")
            .field("len", &self.len())
            .finish()
    }
}

/// A subscription handle that can be used to cancel a subscription.
///
/// When dropped, the subscription is automatically cancelled.
pub struct DdSubscription {
    _handle: zoon::TaskHandle,
}

impl DdSubscription {
    /// Create a new subscription from a task handle.
    pub fn new(handle: zoon::TaskHandle) -> Self {
        Self { _handle: handle }
    }
}

/// Subscribe to a DdSignal and call a callback on each change.
///
/// Returns a subscription handle that keeps the subscription alive.
/// When the handle is dropped, the subscription is cancelled.
///
/// # Example
/// ```ignore
/// let signal = DdSignal::new(DdValue::int(0));
/// let sub = subscribe(&signal, |value| {
///     zoon::println!("Value changed: {:?}", value);
/// });
/// signal.set(DdValue::int(1)); // Triggers callback
/// drop(sub); // Cancels subscription
/// ```
pub fn subscribe<F>(signal: &DdSignal, callback: F) -> DdSubscription
where
    F: Fn(DdValue) + 'static,
{
    let task = zoon::Task::start_droppable(
        signal.signal().for_each_sync(move |value| {
            callback(value);
        }),
    );
    DdSubscription::new(task)
}

/// Combine two signals, emitting when either changes.
///
/// The callback receives both current values.
pub fn combine<F>(a: &DdSignal, b: &DdSignal, callback: F) -> DdSubscription
where
    F: Fn(DdValue, DdValue) + 'static,
{
    let combined = zoon::map_ref! {
        let a_val = a.signal(),
        let b_val = b.signal() =>
        (a_val.clone(), b_val.clone())
    };

    let task = zoon::Task::start_droppable(combined.for_each_sync(move |(a_val, b_val)| {
        callback(a_val, b_val);
    }));
    DdSubscription::new(task)
}

/// Create a derived signal that maps values through a function.
///
/// Returns a new DdSignal that updates when the source changes.
pub fn map<F>(source: &DdSignal, f: F) -> DdSignal
where
    F: Fn(&DdValue) -> DdValue + 'static,
{
    let result = DdSignal::new(f(&source.get()));
    let result_clone = result.clone();

    // Subscribe to source and update result
    let _sub = subscribe(source, move |value| {
        result_clone.set(f(&value));
    });

    // Note: The subscription is leaked here intentionally.
    // In a full implementation, we'd need to manage subscription lifetimes
    // through DdScope.

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_get_set() {
        let signal = DdSignal::new(DdValue::int(42));
        assert_eq!(signal.get(), DdValue::int(42));

        signal.set(DdValue::int(100));
        assert_eq!(signal.get(), DdValue::int(100));
    }

    #[test]
    fn test_signal_update() {
        let signal = DdSignal::new(DdValue::int(10));
        signal.update(|v| {
            if let DdValue::Number(n) = v {
                DdValue::int((n.0 as i64) + 5)
            } else {
                v.clone()
            }
        });
        assert_eq!(signal.get(), DdValue::int(15));
    }

    #[test]
    fn test_list_operations() {
        let list = DdList::new();
        assert!(list.is_empty());

        list.push(DdValue::text("a"));
        list.push(DdValue::text("b"));
        assert_eq!(list.len(), 2);

        list.remove(0);
        assert_eq!(list.len(), 1);

        list.clear();
        assert!(list.is_empty());
    }

    #[test]
    fn test_list_from_values() {
        let list = DdList::from_values([DdValue::int(1), DdValue::int(2), DdValue::int(3)]);
        assert_eq!(list.len(), 3);
        assert_eq!(
            list.to_dd_value(),
            DdValue::list([DdValue::int(1), DdValue::int(2), DdValue::int(3)])
        );
    }
}
