//! Node types for Path A engine.
//!
//! Each node represents a reactive computation unit.

use crate::arena::SlotId;
use crate::template::TemplateId;
use shared::ast::Expr;
use shared::test_harness::Value;
use std::collections::HashMap;

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

    /// List/append - appends to a list with template instantiation
    ListAppend {
        /// Source list slot (reads current list for HOLD state)
        list: SlotId,
        /// Trigger slot - when non-Skip, instantiate a new item
        trigger: Option<SlotId>,
        /// Item template AST (compiled fresh for each append)
        item_template: Box<Expr>,
        /// Captured bindings from outer scope (name -> slot)
        captures: HashMap<String, SlotId>,
        /// Instantiated item slots (one per appended item)
        instances: Vec<SlotId>,
        /// Number of items instantiated (to track when to create new ones)
        instantiated_count: usize,
    },

    /// List/clear - clears all items from a list
    ListClear {
        /// Source list slot
        list: SlotId,
        /// Trigger slot - when non-Skip, clear the list
        trigger: Option<SlotId>,
    },

    /// List/remove - removes item at index from a list
    ListRemove {
        /// Source list slot
        list: SlotId,
        /// Index to remove
        index: SlotId,
        /// Trigger slot - when non-Skip, remove the item
        trigger: Option<SlotId>,
    },

    /// List/retain - keeps items matching predicate
    ListRetain {
        /// Source list slot
        list: SlotId,
        /// Trigger slot - when non-Skip, filter the list
        trigger: Option<SlotId>,
        /// Predicate template AST (compiled fresh for each item)
        predicate_template: Box<Expr>,
        /// Item name for predicate binding
        item_name: String,
        /// Captured bindings from outer scope
        captures: HashMap<String, SlotId>,
    },

    /// Block with local bindings
    Block {
        /// Local binding slots
        bindings: Vec<(String, SlotId)>,
        /// Output slot
        output: SlotId,
    },
}
