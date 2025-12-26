//! Virtual time clock for deterministic testing.
//!
//! TestClock provides controllable virtual time for CLI tests.
//! It allows advancing time instantly without real waiting,
//! making timer tests fast and deterministic.

use std::collections::BinaryHeap;
use std::cmp::Ordering;
use crate::engine_v2::arena::SlotId;

/// Entry for a pending timer.
#[derive(Debug, Clone)]
struct TimerEntry {
    /// When the timer should fire (virtual time in ms)
    fire_at_ms: u64,
    /// The timer node to fire
    node_id: SlotId,
    /// The interval for re-scheduling (if repeating)
    interval_ms: f64,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.fire_at_ms == other.fire_at_ms
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: smaller fire_at_ms comes first
        other.fire_at_ms.cmp(&self.fire_at_ms)
    }
}

/// Virtual time clock for testing.
///
/// Tracks virtual time and manages timer scheduling.
/// Time only advances when explicitly requested via `advance_by()`.
pub struct TestClock {
    /// Current virtual time in milliseconds
    current_time_ms: u64,
    /// Pending timers ordered by fire time
    pending_timers: BinaryHeap<TimerEntry>,
}

impl TestClock {
    /// Create a new TestClock starting at time 0.
    pub fn new() -> Self {
        Self {
            current_time_ms: 0,
            pending_timers: BinaryHeap::new(),
        }
    }

    /// Get the current virtual time in milliseconds.
    pub fn now_ms(&self) -> u64 {
        self.current_time_ms
    }

    /// Register a timer to fire after a delay.
    ///
    /// # Arguments
    /// * `node_id` - The timer node slot ID
    /// * `interval_ms` - The interval in milliseconds
    pub fn register_timer(&mut self, node_id: SlotId, interval_ms: f64) {
        let fire_at_ms = self.current_time_ms + (interval_ms as u64);
        self.pending_timers.push(TimerEntry {
            fire_at_ms,
            node_id,
            interval_ms,
        });
    }

    /// Advance virtual time by the specified milliseconds.
    ///
    /// Returns a list of timer node IDs that should fire.
    /// For repeating timers, they are automatically re-scheduled.
    pub fn advance_by(&mut self, ms: u64) -> Vec<SlotId> {
        let target_time = self.current_time_ms + ms;
        let mut fired = Vec::new();

        // Fire all timers that should have fired by target_time
        // Re-schedule immediately so cascading fires work in single advance
        while let Some(entry) = self.pending_timers.peek() {
            if entry.fire_at_ms <= target_time {
                let entry = self.pending_timers.pop().unwrap();
                fired.push(entry.node_id);

                // Re-schedule repeating timers IMMEDIATELY
                // so they can fire again within the same advance_by call
                self.pending_timers.push(TimerEntry {
                    fire_at_ms: entry.fire_at_ms + (entry.interval_ms as u64),
                    node_id: entry.node_id,
                    interval_ms: entry.interval_ms,
                });
            } else {
                break;
            }
        }

        self.current_time_ms = target_time;
        fired
    }

    /// Check if there are any pending timers.
    pub fn has_pending_timers(&self) -> bool {
        !self.pending_timers.is_empty()
    }

    /// Get the time until the next timer fires (if any).
    pub fn time_to_next_timer(&self) -> Option<u64> {
        self.pending_timers.peek().map(|entry| {
            entry.fire_at_ms.saturating_sub(self.current_time_ms)
        })
    }

    /// Clear all pending timers.
    pub fn clear_timers(&mut self) {
        self.pending_timers.clear();
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_starts_at_zero() {
        let clock = TestClock::new();
        assert_eq!(clock.now_ms(), 0);
    }

    #[test]
    fn advance_increases_time() {
        let mut clock = TestClock::new();
        clock.advance_by(1000);
        assert_eq!(clock.now_ms(), 1000);

        clock.advance_by(500);
        assert_eq!(clock.now_ms(), 1500);
    }

    #[test]
    fn timer_fires_at_deadline() {
        let mut clock = TestClock::new();
        let node_id = SlotId { index: 0, generation: 0 };

        clock.register_timer(node_id, 1000.0);
        assert!(clock.has_pending_timers());

        // Advance less than interval - no fire
        let fired = clock.advance_by(500);
        assert!(fired.is_empty());

        // Advance to exactly the deadline
        let fired = clock.advance_by(500);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0], node_id);

        // Timer re-scheduled, fires again at 2000ms
        let fired = clock.advance_by(1000);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0], node_id);
    }

    #[test]
    fn multiple_fires_in_single_advance() {
        let mut clock = TestClock::new();
        let node_id = SlotId { index: 0, generation: 0 };

        clock.register_timer(node_id, 100.0);

        // Advance 350ms - should fire at 100, 200, 300
        let fired = clock.advance_by(350);
        assert_eq!(fired.len(), 3);
    }

    #[test]
    fn time_to_next_timer() {
        let mut clock = TestClock::new();
        assert!(clock.time_to_next_timer().is_none());

        let node_id = SlotId { index: 0, generation: 0 };
        clock.register_timer(node_id, 1000.0);

        assert_eq!(clock.time_to_next_timer(), Some(1000));

        clock.advance_by(300);
        assert_eq!(clock.time_to_next_timer(), Some(700));
    }
}
