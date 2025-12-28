//! Hierarchical scope system for Path B engine.
//!
//! ScopeId is a path from the root that provides stable identity
//! for values across ticks.

use std::sync::Arc;

/// Hierarchical scope identifier
/// Uses Arc for O(1) clone - expensive operations (child, parent) still allocate
/// but most clones (for HashMap keys, etc.) are cheap reference counts
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeId {
    /// Path from root - each segment is a discriminator
    /// Wrapped in Arc for cheap cloning
    path: Arc<Vec<u64>>,
}

impl ScopeId {
    /// Create the root scope
    pub fn root() -> Self {
        Self {
            path: Arc::new(Vec::new()),
        }
    }

    /// Create a child scope with a discriminator
    /// Note: This allocates a new Vec (can't reuse Arc without Arc::make_mut complexity)
    pub fn child(&self, discriminator: u64) -> Self {
        let mut new_path = (*self.path).clone();
        new_path.push(discriminator);
        Self {
            path: Arc::new(new_path),
        }
    }

    /// Get the depth of this scope
    pub fn depth(&self) -> usize {
        self.path.len()
    }

    /// Check if this is the root scope
    pub fn is_root(&self) -> bool {
        self.path.is_empty()
    }

    /// Get parent scope (if not root)
    /// Note: This allocates a new Vec
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            None
        } else {
            let mut new_path = (*self.path).clone();
            new_path.pop();
            Some(Self {
                path: Arc::new(new_path),
            })
        }
    }

    /// Check if this scope is an ancestor of another
    pub fn is_ancestor_of(&self, other: &ScopeId) -> bool {
        if self.path.len() >= other.path.len() {
            return false;
        }
        self.path.iter().zip(other.path.iter()).all(|(a, b)| a == b)
    }

    /// Get the path as a slice
    pub fn path(&self) -> &[u64] {
        &self.path
    }
}

impl Default for ScopeId {
    fn default() -> Self {
        Self::root()
    }
}

/// Allocator for scope discriminators within a list
#[derive(Debug, Default)]
pub struct ScopeAllocator {
    next: u64,
}

impl ScopeAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new discriminator
    pub fn alloc(&mut self) -> u64 {
        let d = self.next;
        self.next += 1;
        d
    }
}
