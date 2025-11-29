//! Source code management with zero-copy string slices.
//!
//! This module provides `SourceCode` and `StrSlice` types that allow
//! expressions to reference strings from source code without allocating.
//!
//! The key insight is that source code is wrapped in `Arc<String>`, and
//! all string references become `StrSlice` which stores the Arc plus
//! byte offsets. This makes all expressions inherently `'static` while
//! avoiding any string copying.
//!
//! Benefits:
//! - Single allocation for entire source code
//! - Zero string copies at runtime
//! - All expressions are `'static`, `Clone`, `Send`, `Sync`
//! - Trivially serializable (source + spans)
//! - Works with async handlers and WebWorkers

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// Wrapper around source code that can be cheaply cloned.
///
/// All string slices in expressions reference into this source.
#[derive(Clone)]
pub struct SourceCode(Arc<String>);

impl SourceCode {
    /// Create a new SourceCode from a String.
    pub fn new(code: String) -> Self {
        SourceCode(Arc::new(code))
    }

    /// Get the full source code as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Get the length of the source code in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check if the source code is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Create a StrSlice referencing a portion of this source.
    ///
    /// # Panics
    /// Panics if the range is out of bounds or not on UTF-8 boundaries.
    pub fn slice(&self, start: usize, end: usize) -> StrSlice {
        // Validate the slice is valid UTF-8 boundary
        assert!(self.0.is_char_boundary(start), "start not on char boundary");
        assert!(self.0.is_char_boundary(end), "end not on char boundary");
        assert!(start <= end, "start > end");
        assert!(end <= self.0.len(), "end out of bounds");

        StrSlice {
            source: self.clone(),
            start,
            end,
        }
    }

    /// Create a StrSlice from a string slice that is known to be within this source.
    ///
    /// # Safety
    /// The caller must ensure that `s` is a slice of `self.as_str()`.
    pub fn slice_from_str(&self, s: &str) -> StrSlice {
        let source_ptr = self.0.as_ptr() as usize;
        let slice_ptr = s.as_ptr() as usize;

        assert!(
            slice_ptr >= source_ptr && slice_ptr + s.len() <= source_ptr + self.0.len(),
            "string slice is not from this source"
        );

        let start = slice_ptr - source_ptr;
        let end = start + s.len();

        StrSlice {
            source: self.clone(),
            start,
            end,
        }
    }
}

impl fmt::Debug for SourceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SourceCode({} bytes)", self.0.len())
    }
}

/// A string slice that references into a `SourceCode`.
///
/// This type is:
/// - `'static` (no lifetime parameter)
/// - `Clone` (cheap - just Arc increment + copy offsets)
/// - `Send + Sync` (can be used across threads/workers)
/// - 24 bytes (Arc pointer + 2 usizes)
///
/// Compared to `&str` (16 bytes), this is 8 bytes larger but provides
/// ownership semantics and static lifetime.
#[derive(Clone)]
pub struct StrSlice {
    source: SourceCode,
    start: usize,
    end: usize,
}

impl StrSlice {
    /// Get the string content.
    #[inline]
    pub fn as_str(&self) -> &str {
        // SAFETY: We validated UTF-8 boundaries in SourceCode::slice
        unsafe { self.source.0.get_unchecked(self.start..self.end) }
    }

    /// Get the length in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Check if empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Get the start offset in the source.
    #[inline]
    pub fn start(&self) -> usize {
        self.start
    }

    /// Get the end offset in the source.
    #[inline]
    pub fn end(&self) -> usize {
        self.end
    }

    /// Get the underlying source code.
    pub fn source(&self) -> &SourceCode {
        &self.source
    }

    /// Create an empty StrSlice (for default values).
    pub fn empty(source: SourceCode) -> Self {
        StrSlice {
            source,
            start: 0,
            end: 0,
        }
    }
}

impl Deref for StrSlice {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for StrSlice {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for StrSlice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl fmt::Display for StrSlice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl PartialEq for StrSlice {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for StrSlice {}

impl PartialEq<str> for StrSlice {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for StrSlice {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl Hash for StrSlice {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

// For pattern matching in HashMap/BTreeMap
impl std::borrow::Borrow<str> for StrSlice {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_code_slice() {
        let source = SourceCode::new("hello world".to_string());
        let slice = source.slice(0, 5);
        assert_eq!(slice.as_str(), "hello");
        assert_eq!(slice.len(), 5);
    }

    #[test]
    fn test_slice_from_str() {
        let source = SourceCode::new("hello world".to_string());
        let s = &source.as_str()[6..11];
        let slice = source.slice_from_str(s);
        assert_eq!(slice.as_str(), "world");
    }

    #[test]
    fn test_str_slice_is_static() {
        fn takes_static<T: 'static>(_: T) {}
        let source = SourceCode::new("test".to_string());
        let slice = source.slice(0, 4);
        takes_static(slice);
    }

    #[test]
    fn test_str_slice_is_send_sync() {
        fn is_send<T: Send>() {}
        fn is_sync<T: Sync>() {}
        is_send::<StrSlice>();
        is_sync::<StrSlice>();
    }

    #[test]
    fn test_equality() {
        let source = SourceCode::new("hello hello".to_string());
        let slice1 = source.slice(0, 5);
        let slice2 = source.slice(6, 11);
        assert_eq!(slice1, slice2);
        assert_eq!(slice1, "hello");
    }
}
