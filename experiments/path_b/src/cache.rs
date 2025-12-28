//! Cache system for Path B engine.
//!
//! CacheEntry tracks computed values and their dependencies
//! for efficient re-evaluation and "why did X change?" queries.

use crate::slot::SlotKey;
use crate::tick::TickSeq;
use shared::test_harness::Value;
use smallvec::SmallVec;

/// A cached computation result
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// The computed value
    pub value: Value,
    /// Tick when this was computed
    pub computed_at: u64,
    /// When the value actually changed
    pub last_changed: TickSeq,
    /// Dependencies that were read during computation
    pub deps: SmallVec<[SlotKey; 8]>,
}

impl CacheEntry {
    pub fn new(value: Value, computed_at: u64, last_changed: TickSeq) -> Self {
        Self {
            value,
            computed_at,
            last_changed,
            deps: SmallVec::new(),
        }
    }

    /// Add a dependency
    pub fn add_dep(&mut self, dep: SlotKey) {
        if !self.deps.contains(&dep) {
            self.deps.push(dep);
        }
    }

    /// Check if this entry is from the current tick
    pub fn is_current(&self, tick: u64) -> bool {
        self.computed_at == tick
    }

    /// Check if this entry is stale
    pub fn is_stale(&self, tick: u64) -> bool {
        self.computed_at < tick
    }
}

/// Cache for computed values
#[derive(Debug, Default)]
pub struct Cache {
    entries: std::collections::HashMap<SlotKey, CacheEntry>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a cache entry
    pub fn get(&self, key: &SlotKey) -> Option<&CacheEntry> {
        self.entries.get(key)
    }

    /// Get a mutable cache entry
    pub fn get_mut(&mut self, key: &SlotKey) -> Option<&mut CacheEntry> {
        self.entries.get_mut(key)
    }

    /// Insert or update a cache entry
    pub fn insert(&mut self, key: SlotKey, entry: CacheEntry) {
        self.entries.insert(key, entry);
    }

    /// Check if a key is cached and current
    pub fn is_cached(&self, key: &SlotKey, tick: u64) -> bool {
        self.entries.get(key).map(|e| e.is_current(tick)).unwrap_or(false)
    }

    /// Get cached value if current
    pub fn get_if_current(&self, key: &SlotKey, tick: u64) -> Option<&Value> {
        self.entries.get(key).and_then(|e| {
            if e.is_current(tick) {
                Some(&e.value)
            } else {
                None
            }
        })
    }

    /// Clear all entries (for debugging/testing)
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get all entries (for diagnostics)
    pub fn entries(&self) -> impl Iterator<Item = (&SlotKey, &CacheEntry)> {
        self.entries.iter()
    }
}
