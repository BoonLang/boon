//! LINK registry for DD engine.
//!
//! Manages event bindings between UI elements and Boon variables.
//! This is the DD equivalent of the actor engine's LinkConnector.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use zoon::Mutable;

use super::dd_value::DdValue;

/// Unique identifier for a LINK binding.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(pub String);

impl LinkId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn from_variable(var_name: &str) -> Self {
        Self(var_name.to_string())
    }

    pub fn from_path(path: &[&str]) -> Self {
        Self(path.join("."))
    }
}

/// A registered LINK that can receive events.
#[derive(Clone)]
pub struct Link {
    /// The signal that emits when the link is triggered
    pub signal: Mutable<Option<DdValue>>,
    /// Counter for how many times the link has been triggered
    trigger_count: Rc<RefCell<u64>>,
}

impl Link {
    pub fn new() -> Self {
        Self {
            signal: Mutable::new(None),
            trigger_count: Rc::new(RefCell::new(0)),
        }
    }

    /// Fire the link with a value.
    pub fn fire(&self, value: DdValue) {
        *self.trigger_count.borrow_mut() += 1;
        self.signal.set(Some(value));
    }

    /// Fire the link with Unit (for button presses).
    pub fn fire_unit(&self) {
        self.fire(DdValue::Unit);
    }

    /// Get the current value (if any).
    pub fn get(&self) -> Option<DdValue> {
        self.signal.get_cloned()
    }

    /// Get how many times this link has been triggered.
    pub fn trigger_count(&self) -> u64 {
        *self.trigger_count.borrow()
    }
}

impl Default for Link {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry for all LINK bindings in a program.
#[derive(Clone, Default)]
pub struct LinkRegistry {
    links: Rc<RefCell<HashMap<LinkId, Link>>>,
}

impl LinkRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new LINK or get existing one.
    pub fn register(&self, id: LinkId) -> Link {
        let mut links = self.links.borrow_mut();
        links.entry(id).or_insert_with(Link::new).clone()
    }

    /// Get an existing LINK by ID.
    pub fn get(&self, id: &LinkId) -> Option<Link> {
        self.links.borrow().get(id).cloned()
    }

    /// Fire a LINK by ID.
    pub fn fire(&self, id: &LinkId, value: DdValue) {
        if let Some(link) = self.get(id) {
            link.fire(value);
        }
    }

    /// Fire a LINK with Unit.
    pub fn fire_unit(&self, id: &LinkId) {
        self.fire(id, DdValue::Unit);
    }

    /// Get all registered link IDs.
    pub fn all_ids(&self) -> Vec<LinkId> {
        self.links.borrow().keys().cloned().collect()
    }

    /// Clear all links (for re-evaluation).
    pub fn clear(&self) {
        self.links.borrow_mut().clear();
    }
}

impl std::fmt::Debug for LinkRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkRegistry")
            .field("count", &self.links.borrow().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_registry() {
        let registry = LinkRegistry::new();

        let id = LinkId::new("button_click");
        let link = registry.register(id.clone());

        assert_eq!(link.trigger_count(), 0);
        assert!(link.get().is_none());

        registry.fire_unit(&id);

        assert_eq!(link.trigger_count(), 1);
        assert!(link.get().is_some());
    }

    #[test]
    fn test_link_fire_with_value() {
        let registry = LinkRegistry::new();

        let id = LinkId::new("text_input");
        let link = registry.register(id.clone());

        registry.fire(&id, DdValue::text("hello"));

        assert_eq!(link.get(), Some(DdValue::text("hello")));
    }
}
