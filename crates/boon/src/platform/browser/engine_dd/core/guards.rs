//! Runtime assertion guards for anti-cheat enforcement.
//!
//! These guards panic in debug builds if forbidden patterns are detected,
//! providing an additional layer of protection beyond compile-time type safety.

use std::cell::Cell;

#[cfg(debug_assertions)]
thread_local! {
    /// Flag indicating if we're currently inside a DD computation context.
    /// Used to detect nested DD contexts or sync operations during DD processing.
    static IN_DD_CONTEXT: Cell<bool> = const { Cell::new(false) };

    /// Counter for tracking active DD operations.
    /// Useful for debugging and detecting leaked operations.
    static DD_OPERATION_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Guard that marks entry into DD computation context.
///
/// # Anti-Cheat Usage
///
/// Wrap DD computation code with this guard to detect:
/// - Nested DD contexts (possible sync access attempt)
/// - Sync operations called during DD computation
///
/// # Example
///
/// ```ignore
/// fn process_dd_event(event: Event) {
///     let _guard = DdContextGuard::enter();
///     // Any sync operation called here will panic in debug builds
///     worker.inject_event(event);
/// }
/// ```
pub struct DdContextGuard {
    #[cfg(debug_assertions)]
    _marker: (),
}

impl DdContextGuard {
    /// Enter DD computation context.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if already inside a DD context (nested context detection).
    pub fn enter() -> Self {
        #[cfg(debug_assertions)]
        {
            IN_DD_CONTEXT.with(|c| {
                if c.get() {
                    panic!(
                        "CHEAT DETECTED: Nested DD context - possible synchronous state access. \
                         DD operations must not be nested. This often indicates an attempt to \
                         read DD state synchronously during DD computation."
                    );
                }
                c.set(true);
            });
            DD_OPERATION_COUNT.with(|c| c.set(c.get() + 1));
        }

        Self {
            #[cfg(debug_assertions)]
            _marker: (),
        }
    }

    /// Check if currently inside a DD context.
    ///
    /// Useful for debugging and assertions.
    #[cfg(debug_assertions)]
    pub fn is_active() -> bool {
        IN_DD_CONTEXT.with(|c| c.get())
    }

    #[cfg(not(debug_assertions))]
    pub fn is_active() -> bool {
        false
    }
}

impl Drop for DdContextGuard {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        IN_DD_CONTEXT.with(|c| c.set(false));
    }
}

/// Assert that we are NOT inside a DD context.
///
/// Use this at the start of operations that should never be called during DD computation,
/// such as synchronous state reads (which shouldn't exist, but this catches attempts).
///
/// # Panics
///
/// In debug builds, panics if called while inside a DD context.
///
/// # Example
///
/// ```ignore
/// fn sync_operation_that_should_never_happen() {
///     assert_not_in_dd_context("sync_state_read");
///     // This code should never run during DD computation
/// }
/// ```
pub fn assert_not_in_dd_context(operation: &str) {
    #[cfg(debug_assertions)]
    IN_DD_CONTEXT.with(|c| {
        if c.get() {
            panic!(
                "CHEAT DETECTED: '{}' called during DD computation. \
                 This operation requires synchronous state access which is forbidden. \
                 All state observation must go through async streams (Output).",
                operation
            );
        }
    });

    #[cfg(not(debug_assertions))]
    let _ = operation;
}

/// Assert that we ARE inside a DD context.
///
/// Use this at the start of operations that should only be called during DD computation.
///
/// # Panics
///
/// In debug builds, panics if called while NOT inside a DD context.
pub fn assert_in_dd_context(operation: &str) {
    #[cfg(debug_assertions)]
    IN_DD_CONTEXT.with(|c| {
        if !c.get() {
            panic!(
                "DD INVARIANT VIOLATION: '{}' called outside DD computation context. \
                 This operation should only be called from within DD processing.",
                operation
            );
        }
    });

    #[cfg(not(debug_assertions))]
    let _ = operation;
}

/// Get the total count of DD operations (debug builds only).
///
/// Useful for debugging and detecting resource leaks.
#[cfg(debug_assertions)]
pub fn dd_operation_count() -> u64 {
    DD_OPERATION_COUNT.with(|c| c.get())
}

#[cfg(not(debug_assertions))]
pub fn dd_operation_count() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_guard_basic() {
        assert!(!DdContextGuard::is_active());

        {
            let _guard = DdContextGuard::enter();
            assert!(DdContextGuard::is_active());
        }

        assert!(!DdContextGuard::is_active());
    }

    #[test]
    #[should_panic(expected = "CHEAT DETECTED")]
    fn test_nested_context_panics() {
        let _guard1 = DdContextGuard::enter();
        let _guard2 = DdContextGuard::enter(); // Should panic
    }

    #[test]
    fn test_assert_not_in_dd_context_outside() {
        // Should not panic when outside DD context
        assert_not_in_dd_context("test_operation");
    }

    #[test]
    #[should_panic(expected = "CHEAT DETECTED")]
    fn test_assert_not_in_dd_context_inside() {
        let _guard = DdContextGuard::enter();
        assert_not_in_dd_context("forbidden_sync_read"); // Should panic
    }

    #[test]
    fn test_assert_in_dd_context_inside() {
        let _guard = DdContextGuard::enter();
        // Should not panic when inside DD context
        assert_in_dd_context("dd_operation");
    }

    #[test]
    #[should_panic(expected = "DD INVARIANT VIOLATION")]
    fn test_assert_in_dd_context_outside() {
        assert_in_dd_context("dd_only_operation"); // Should panic
    }

    #[test]
    fn test_operation_count() {
        let initial = dd_operation_count();

        {
            let _guard = DdContextGuard::enter();
        }

        assert_eq!(dd_operation_count(), initial + 1);
    }
}
