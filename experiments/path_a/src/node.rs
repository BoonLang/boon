//! Node types for Path A engine.
//!
//! Each node represents a reactive computation unit.

use crate::arena::SlotId;
use crate::template::TemplateId;
use shared::test_harness::Value;

/// A node in the reactive graph
#[derive(Debug, Clone)]
pub struct Node {
    /// The kind of node
    pub kind: NodeKind,
    /// Human-readable name for debugging
    pub name: Option<String>,
}

impl Node {
    pub fn new(kind: NodeKind) -> Self {
        Self { kind, name: None }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// The different kinds of nodes
#[derive(Debug, Clone)]
pub enum NodeKind {
    /// Constant value
    Constant(Value),

    /// Mutable cell - stores state that can be updated (used by HOLD)
    Cell,

    /// Wire that reads from another slot
    Wire(SlotId),

    /// LATEST - merges multiple inputs, takes most recent non-SKIP
    Latest(Vec<SlotId>),

    /// HOLD - stateful accumulator
    Hold {
        /// Current state slot
        state: SlotId,
        /// Body expression slot (lazy)
        body: SlotId,
        /// Initial value
        initial: Value,
    },

    /// THEN - copies body when input fires (non-SKIP)
    Then {
        /// Input slot
        input: SlotId,
        /// Body slot
        body: SlotId,
    },

    /// WHEN - pattern match and copy
    When {
        /// Input slot
        input: SlotId,
        /// Arms: (pattern check function name, body slot)
        arms: Vec<(String, SlotId)>,
    },

    /// WHILE - continuous data while pattern matches
    While {
        /// Input slot
        input: SlotId,
        /// Pattern check function name
        pattern: String,
        /// Body slot
        body: SlotId,
    },

    /// LINK - event binding point
    Link {
        /// Bound slot (if any)
        bound: Option<SlotId>,
    },

    /// Object construction
    Object(Vec<(String, SlotId)>),

    /// List construction
    List(Vec<SlotId>),

    /// Path access: base.field
    Path {
        base: SlotId,
        field: String,
    },

    /// Function call
    Call {
        name: String,
        args: Vec<SlotId>,
    },

    /// List/map - maps a template over a list
    ListMap {
        /// Source list slot
        list: SlotId,
        /// Template to instantiate per item
        template: TemplateId,
        /// Instantiated item slots
        instances: Vec<SlotId>,
    },

    /// List/append - appends to a list
    ListAppend {
        list: SlotId,
        item: SlotId,
    },

    /// Block with local bindings
    Block {
        /// Local binding slots
        bindings: Vec<(String, SlotId)>,
        /// Output slot
        output: SlotId,
    },
}
