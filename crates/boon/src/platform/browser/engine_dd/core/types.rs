//! Core types for the DD v2 engine.
//! No DD, Zoon, or browser dependencies.

use std::sync::Arc;

/// Identifies a variable / collection in the DD dataflow graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VarId(pub Arc<str>);

impl VarId {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        VarId(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for VarId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifies an external input source (LINK events, timers, browser state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputId(pub usize);

/// Identifies a LINK event binding path (e.g., "increment_button.event.press").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(pub Arc<str>);

impl LinkId {
    pub fn new(path: impl Into<Arc<str>>) -> Self {
        LinkId(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Specification for an external input source.
#[derive(Clone, Debug)]
pub struct InputSpec {
    pub id: InputId,
    pub link_id: LinkId,
}

/// Key for list elements in DD collections.
///
/// Lists are DD collections of `(ListKey, Value)` pairs.
/// ListKey provides stable identity for incremental updates.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct ListKey(pub Arc<str>);

impl ListKey {
    pub fn new(key: impl Into<Arc<str>>) -> Self {
        ListKey(key.into())
    }

    pub fn from_index(index: usize) -> Self {
        ListKey(Arc::from(format!("{index}")))
    }
}

impl std::fmt::Display for ListKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
