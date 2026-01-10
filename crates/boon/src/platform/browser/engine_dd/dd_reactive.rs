//! Reactive evaluation layer for DD engine.
//!
//! This module bridges the static DD evaluator with reactive UI updates.
//! It uses DdSignal to create reactive state that can update the UI when
//! events occur.
//!
//! # Architecture
//!
//! ```text
//! Boon Source → Parser → Static Evaluation → Initial DdValue document
//!                              ↓
//!                   DdReactiveContext
//!                   ├── holds: HashMap<HoldId, DdSignal>
//!                   ├── links: HashMap<LinkId, DdSignal>
//!                   └── scope: DdScope (subscription lifetime)
//!                              ↓
//!                   Bridge renders with signal values
//!                              ↓
//!                   Events → Update signals → Re-render
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::dd_scope::DdScope;
use super::dd_stream::{DdSignal, DdSubscription, subscribe};
use super::dd_value::DdValue;

/// A unique identifier for a HOLD expression.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HoldId(pub String);

/// A unique identifier for a LINK expression.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(pub String);

/// Reactive context for DD evaluation.
///
/// Manages reactive state (signals) that can be updated by events
/// and trigger UI re-renders.
#[derive(Default)]
pub struct DdReactiveContext {
    /// HOLD state signals, keyed by variable name or persistence ID
    holds: Rc<RefCell<HashMap<HoldId, DdSignal>>>,

    /// LINK event signals, keyed by variable name or span
    links: Rc<RefCell<HashMap<LinkId, DdSignal>>>,

    /// Subscription scope - subscriptions are cancelled when context is dropped
    scope: RefCell<DdScope>,
}

impl DdReactiveContext {
    /// Create a new reactive context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a HOLD expression and get its signal.
    ///
    /// If the HOLD was already registered, returns the existing signal.
    /// Otherwise creates a new signal with the initial value.
    pub fn register_hold(&self, id: HoldId, initial: DdValue) -> DdSignal {
        let mut holds = self.holds.borrow_mut();
        if let Some(signal) = holds.get(&id) {
            return signal.clone();
        }

        let signal = DdSignal::new(initial);
        holds.insert(id, signal.clone());
        signal
    }

    /// Get an existing HOLD signal.
    pub fn get_hold(&self, id: &HoldId) -> Option<DdSignal> {
        self.holds.borrow().get(id).cloned()
    }

    /// Update a HOLD signal's value.
    pub fn update_hold(&self, id: &HoldId, value: DdValue) {
        if let Some(signal) = self.holds.borrow().get(id) {
            signal.set(value);
        }
    }

    /// Register a LINK expression and get its signal.
    ///
    /// LINK signals emit Unit when the associated event fires.
    pub fn register_link(&self, id: LinkId) -> DdSignal {
        let mut links = self.links.borrow_mut();
        if let Some(signal) = links.get(&id) {
            return signal.clone();
        }

        let signal = DdSignal::new(DdValue::Unit);
        links.insert(id, signal.clone());
        signal
    }

    /// Get an existing LINK signal.
    pub fn get_link(&self, id: &LinkId) -> Option<DdSignal> {
        self.links.borrow().get(id).cloned()
    }

    /// Fire a LINK event (e.g., from button click).
    pub fn fire_link(&self, id: &LinkId) {
        if let Some(signal) = self.links.borrow().get(id) {
            signal.set(DdValue::Unit);
        }
    }

    /// Fire a LINK event with a value (e.g., from input change).
    pub fn fire_link_with_value(&self, id: &LinkId, value: DdValue) {
        if let Some(signal) = self.links.borrow().get(id) {
            signal.set(value);
        }
    }

    /// Add a subscription to this context's scope.
    ///
    /// The subscription will be kept alive until the context is dropped
    /// or `clear_subscriptions()` is called.
    pub fn add_subscription(&self, subscription: DdSubscription) {
        self.scope.borrow_mut().add(subscription);
    }

    /// Clear all subscriptions.
    pub fn clear_subscriptions(&self) {
        self.scope.borrow_mut().clear();
    }

    /// Connect a HOLD to a LINK: when the LINK fires, update the HOLD.
    ///
    /// This is the reactive equivalent of:
    /// ```boon
    /// 0 |> HOLD count {
    ///     increment |> THEN { count + 1 }
    /// }
    /// ```
    ///
    /// The `compute` closure receives the current state and event value,
    /// and returns the new state.
    pub fn connect_hold_to_link<F>(&self, hold_id: &HoldId, link_id: &LinkId, compute: F)
    where
        F: Fn(&DdValue, &DdValue) -> DdValue + 'static,
    {
        let hold_signal = match self.get_hold(hold_id) {
            Some(s) => s,
            None => return,
        };

        let link_signal = match self.get_link(link_id) {
            Some(s) => s,
            None => return,
        };

        let hold_signal_clone = hold_signal.clone();
        let subscription = subscribe(&link_signal, move |event_value| {
            let current_state = hold_signal_clone.get();
            let new_state = compute(&current_state, &event_value);
            hold_signal_clone.set(new_state);
        });

        self.add_subscription(subscription);
    }

    /// Get a snapshot of all HOLD values for rendering.
    pub fn get_hold_values(&self) -> HashMap<String, DdValue> {
        self.holds
            .borrow()
            .iter()
            .map(|(id, signal)| (id.0.clone(), signal.get()))
            .collect()
    }
}

impl std::fmt::Debug for DdReactiveContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdReactiveContext")
            .field("holds", &self.holds.borrow().len())
            .field("links", &self.links.borrow().len())
            .finish()
    }
}

/// Helper to create a simple counter pattern.
///
/// This is the reactive equivalent of:
/// ```boon
/// 0 |> HOLD count {
///     increment |> THEN { count + 1 }
/// }
/// ```
pub fn create_counter(ctx: &DdReactiveContext, name: &str) -> DdSignal {
    let hold_id = HoldId(name.to_string());
    let link_id = LinkId(format!("{}_increment", name));

    // Register the HOLD with initial value 0
    let counter = ctx.register_hold(hold_id.clone(), DdValue::int(0));

    // Register the LINK for increment events
    ctx.register_link(link_id.clone());

    // Connect: when link fires, increment counter
    ctx.connect_hold_to_link(&hold_id, &link_id, |current, _event| {
        if let DdValue::Number(n) = current {
            DdValue::int((n.0 as i64) + 1)
        } else {
            DdValue::int(1)
        }
    });

    counter
}

/// Helper to create a bidirectional counter pattern.
///
/// Reactive equivalent of counter with increment and decrement buttons.
pub fn create_bidirectional_counter(ctx: &DdReactiveContext, name: &str) -> DdSignal {
    let hold_id = HoldId(name.to_string());
    let inc_link_id = LinkId(format!("{}_increment", name));
    let dec_link_id = LinkId(format!("{}_decrement", name));

    // Register the HOLD with initial value 0
    let counter = ctx.register_hold(hold_id.clone(), DdValue::int(0));

    // Register both LINKs
    ctx.register_link(inc_link_id.clone());
    ctx.register_link(dec_link_id.clone());

    // Connect increment
    ctx.connect_hold_to_link(&hold_id, &inc_link_id, |current, _| {
        if let DdValue::Number(n) = current {
            DdValue::int((n.0 as i64) + 1)
        } else {
            DdValue::int(1)
        }
    });

    // Connect decrement
    ctx.connect_hold_to_link(&hold_id, &dec_link_id, |current, _| {
        if let DdValue::Number(n) = current {
            DdValue::int((n.0 as i64) - 1)
        } else {
            DdValue::int(-1)
        }
    });

    counter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reactive_context_creation() {
        let ctx = DdReactiveContext::new();
        assert!(ctx.get_hold(&HoldId("test".to_string())).is_none());
    }

    #[test]
    fn test_hold_registration() {
        let ctx = DdReactiveContext::new();
        let id = HoldId("counter".to_string());

        let signal = ctx.register_hold(id.clone(), DdValue::int(42));
        assert_eq!(signal.get(), DdValue::int(42));

        // Second registration returns same signal
        let signal2 = ctx.register_hold(id.clone(), DdValue::int(0));
        assert_eq!(signal2.get(), DdValue::int(42));
    }

    #[test]
    fn test_link_registration() {
        let ctx = DdReactiveContext::new();
        let id = LinkId("button_click".to_string());

        let signal = ctx.register_link(id.clone());
        assert_eq!(signal.get(), DdValue::Unit);

        ctx.fire_link(&id);
        // Signal was set, but value is still Unit (fire sets Unit)
        assert_eq!(signal.get(), DdValue::Unit);
    }

    #[test]
    fn test_hold_update() {
        let ctx = DdReactiveContext::new();
        let id = HoldId("counter".to_string());

        let signal = ctx.register_hold(id.clone(), DdValue::int(0));
        assert_eq!(signal.get(), DdValue::int(0));

        ctx.update_hold(&id, DdValue::int(5));
        assert_eq!(signal.get(), DdValue::int(5));
    }
}
