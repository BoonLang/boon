//! Template system for Path A engine.
//!
//! Templates store the structure of code that gets instantiated
//! multiple times (e.g., in List/map). The key innovation is
//! explicit capture tracking for external dependencies.

use crate::arena::SlotId;
use shared::ast::Expr;

/// Unique identifier for a template
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TemplateId(pub u32);

/// Specification for an external dependency capture
#[derive(Debug, Clone)]
pub struct CaptureSpec {
    /// The external slot being captured
    pub external_slot: SlotId,
    /// The path to access (e.g., "event.click")
    pub path: Vec<String>,
    /// Placeholder slot in the template to rebind
    pub placeholder_slot: SlotId,
}

/// A template that can be instantiated multiple times
#[derive(Debug, Clone)]
pub struct Template {
    /// Template ID
    pub id: TemplateId,
    /// Input slots (e.g., `item` parameter)
    pub input_slots: Vec<SlotId>,
    /// Input parameter names
    pub input_names: Vec<String>,
    /// Output slot of the template
    pub output_slot: SlotId,
    /// External dependencies that need rebinding
    pub captures: Vec<CaptureSpec>,
    /// Internal node slots
    pub internal_slots: Vec<SlotId>,
    /// The AST for this template (for re-evaluation)
    pub ast: Option<Expr>,
}

impl Template {
    pub fn new(id: TemplateId) -> Self {
        Self {
            id,
            input_slots: Vec::new(),
            input_names: Vec::new(),
            output_slot: SlotId(0),
            captures: Vec::new(),
            internal_slots: Vec::new(),
            ast: None,
        }
    }

    /// Add an input parameter
    pub fn add_input(&mut self, name: impl Into<String>, slot: SlotId) {
        self.input_names.push(name.into());
        self.input_slots.push(slot);
    }

    /// Add a capture for an external dependency
    pub fn add_capture(&mut self, external: SlotId, path: Vec<String>, placeholder: SlotId) {
        self.captures.push(CaptureSpec {
            external_slot: external,
            path,
            placeholder_slot: placeholder,
        });
    }
}

/// Template storage
#[derive(Default)]
pub struct TemplateRegistry {
    templates: Vec<Template>,
    next_id: u32,
}

impl TemplateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new template
    pub fn create(&mut self) -> TemplateId {
        let id = TemplateId(self.next_id);
        self.next_id += 1;
        self.templates.push(Template::new(id));
        id
    }

    /// Get a template by ID
    pub fn get(&self, id: TemplateId) -> Option<&Template> {
        self.templates.get(id.0 as usize)
    }

    /// Get mutable template by ID
    pub fn get_mut(&mut self, id: TemplateId) -> Option<&mut Template> {
        self.templates.get_mut(id.0 as usize)
    }
}
