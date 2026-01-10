//! Subscription scope management for DD engine.
//!
//! Manages the lifetime of reactive subscriptions using RAII pattern.
//! When a DdScope is dropped, all subscriptions it owns are cancelled.

use super::dd_stream::DdSubscription;

/// A scope that manages the lifetime of reactive subscriptions.
///
/// Subscriptions added to this scope are automatically cancelled
/// when the scope is dropped. This prevents memory leaks and ensures
/// proper cleanup of reactive dependencies.
///
/// # Example
/// ```ignore
/// {
///     let mut scope = DdScope::new();
///     let signal = DdSignal::new(DdValue::int(0));
///
///     // Add subscription to scope
///     scope.add(subscribe(&signal, |v| println!("{:?}", v)));
///
///     signal.set(DdValue::int(1)); // Triggers callback
/// } // Scope dropped, subscription cancelled
///
/// signal.set(DdValue::int(2)); // No callback triggered
/// ```
pub struct DdScope {
    subscriptions: Vec<DdSubscription>,
}

impl DdScope {
    /// Create a new empty scope.
    pub fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
        }
    }

    /// Add a subscription to this scope.
    ///
    /// The subscription will be kept alive until the scope is dropped
    /// or `clear()` is called.
    pub fn add(&mut self, subscription: DdSubscription) {
        self.subscriptions.push(subscription);
    }

    /// Get the number of active subscriptions.
    pub fn len(&self) -> usize {
        self.subscriptions.len()
    }

    /// Check if the scope has no subscriptions.
    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    /// Clear all subscriptions, cancelling them.
    pub fn clear(&mut self) {
        self.subscriptions.clear();
    }
}

impl Default for DdScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DdScope {
    fn drop(&mut self) {
        // Subscriptions are cancelled when dropped
        self.subscriptions.clear();
    }
}

impl std::fmt::Debug for DdScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdScope")
            .field("subscriptions", &self.subscriptions.len())
            .finish()
    }
}

/// A guard that holds a DdScope and cancels its subscriptions when dropped.
///
/// Useful for creating scoped reactive regions in WHILE patterns
/// where switching to a new arm should cancel the previous arm's subscriptions.
pub struct DdScopeGuard {
    scope: DdScope,
}

impl DdScopeGuard {
    /// Create a new scope guard with an empty scope.
    pub fn new() -> Self {
        Self {
            scope: DdScope::new(),
        }
    }

    /// Get a mutable reference to the underlying scope.
    pub fn scope_mut(&mut self) -> &mut DdScope {
        &mut self.scope
    }

    /// Get an immutable reference to the underlying scope.
    pub fn scope(&self) -> &DdScope {
        &self.scope
    }
}

impl Default for DdScopeGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DdScopeGuard {
    fn drop(&mut self) {
        // Scope is cleared, cancelling all subscriptions
    }
}
