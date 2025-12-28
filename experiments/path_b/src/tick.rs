//! Tick sequencing for Path B engine.
//!
//! TickSeq provides ordering within a single tick to track
//! when values were computed.

/// Ordering within and across ticks
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TickSeq {
    /// The tick number
    pub tick: u64,
    /// Sequence within the tick
    pub seq: u32,
}

impl TickSeq {
    pub fn new(tick: u64, seq: u32) -> Self {
        Self { tick, seq }
    }

    /// Create a zero tick (initial state)
    pub fn zero() -> Self {
        Self { tick: 0, seq: 0 }
    }

    /// Check if this is from the current tick
    pub fn is_current(&self, current_tick: u64) -> bool {
        self.tick == current_tick
    }

    /// Check if this is stale (from a previous tick)
    pub fn is_stale(&self, current_tick: u64) -> bool {
        self.tick < current_tick
    }

    /// Increment sequence within the same tick
    pub fn next_seq(&self) -> Self {
        Self {
            tick: self.tick,
            seq: self.seq + 1,
        }
    }
}

/// Counter for generating TickSeq values within a tick
#[derive(Debug, Default)]
pub struct TickCounter {
    current_tick: u64,
    current_seq: u32,
}

impl TickCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new tick
    pub fn next_tick(&mut self) {
        self.current_tick += 1;
        self.current_seq = 0;
    }

    /// Get current tick number
    pub fn current_tick(&self) -> u64 {
        self.current_tick
    }

    /// Get next TickSeq and increment counter
    pub fn next(&mut self) -> TickSeq {
        let ts = TickSeq::new(self.current_tick, self.current_seq);
        self.current_seq += 1;
        ts
    }

    /// Get current TickSeq without incrementing
    pub fn current(&self) -> TickSeq {
        TickSeq::new(self.current_tick, self.current_seq)
    }
}
