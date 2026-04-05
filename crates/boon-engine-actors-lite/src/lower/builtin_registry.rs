//! Builtin function registry for ActorsLite.
//!
//! Builtins are selected by function path (e.g., `["Math", "sum"]`),
//! not by example name or structural validation.

use std::collections::HashMap;

/// Identifier for a builtin function implementation in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinId(pub u32);

/// A builtin function descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinFn {
    /// Display name for diagnostics.
    pub name: String,
    /// The full path segments (e.g., `["Math", "sum"]`).
    pub path: Vec<String>,
    /// Number of expected positional arguments (not counting the piped receiver).
    pub arg_count: usize,
    /// Whether this builtin takes a piped receiver value.
    pub takes_pipe: bool,
}

/// Registry of builtin functions, indexed by a canonical path key.
#[derive(Debug, Default)]
pub struct BuiltinRegistry {
    /// Map from canonical path key (e.g., "Math.sum") to builtin descriptor.
    by_path: HashMap<String, BuiltinId>,
    /// Storage for builtin descriptors.
    builtins: Vec<BuiltinFn>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        let mut registry = Self::default();
        register_all_builtins(&mut registry);
        registry
    }

    /// Look up a builtin by its full path (e.g., `&["Math", "sum"]`).
    pub fn lookup(&self, path: &[impl AsRef<str>]) -> Option<BuiltinId> {
        let key = path.iter().map(|s| s.as_ref()).collect::<Vec<_>>().join(".");
        self.by_path.get(&key).copied()
    }

    /// Get a builtin descriptor by id.
    pub fn get(&self, id: BuiltinId) -> &BuiltinFn {
        &self.builtins[id.0 as usize]
    }
}

fn register(registry: &mut BuiltinRegistry, path: &[&str], arg_count: usize, takes_pipe: bool) {
    let id = BuiltinId(registry.builtins.len() as u32);
    let key = path.iter().copied().collect::<Vec<_>>().join(".");
    let name = path.last().copied().unwrap_or("").to_string();
    registry.builtins.push(BuiltinFn {
        name,
        path: path.iter().map(|s| s.to_string()).collect(),
        arg_count,
        takes_pipe,
    });
    registry.by_path.insert(key, id);
}

fn register_all_builtins(registry: &mut BuiltinRegistry) {
    // Arithmetic
    register(registry, &["Math", "sum"], 0, true);
    register(registry, &["Math", "min"], 1, true);
    register(registry, &["Math", "max"], 1, true);
    register(registry, &["Math", "round"], 0, true);
    register(registry, &["Math", "modulo"], 1, true);

    // Bool
    register(registry, &["Bool", "not"], 0, true);
    register(registry, &["Bool", "or"], 1, true);
    register(registry, &["Bool", "and"], 1, true);

    // Text
    register(registry, &["Text", "trim"], 0, true);
    register(registry, &["Text", "is_not_empty"], 0, true);
    register(registry, &["Text", "is_empty"], 0, true);
    register(registry, &["Text", "to_number"], 0, true);
    register(registry, &["Text", "starts_with"], 1, true);
    register(registry, &["Text", "length"], 0, true);
    register(registry, &["Text", "char_at"], 1, true);
    register(registry, &["Text", "char_code"], 0, true);
    register(registry, &["Text", "from_char_code"], 0, true);
    register(registry, &["Text", "find"], 1, true);
    register(registry, &["Text", "find_closing"], 2, true);
    register(registry, &["Text", "substring"], 2, true);
    register(registry, &["Text", "to_uppercase"], 0, true);
    register(registry, &["Text", "empty"], 0, false);
    register(registry, &["Text", "space"], 0, false);

    // List
    register(registry, &["List", "count"], 0, true);
    register(registry, &["List", "is_empty"], 0, true);
    register(registry, &["List", "sum"], 0, true);
    register(registry, &["List", "product"], 0, true);
    register(registry, &["List", "last"], 0, true);
    register(registry, &["List", "get"], 1, true);
    register(registry, &["List", "append"], 1, false);
    register(registry, &["List", "remove"], 2, true);
    register(registry, &["List", "retain"], 1, true);
    register(registry, &["List", "range"], 0, false);
    register(registry, &["List", "map"], 1, true);

    // Router
    register(registry, &["Router", "go_to"], 0, true);
    register(registry, &["Router", "route"], 0, false);

    // Document / Element (host-bound, handled specially in view lowering)
    register(registry, &["Document", "new"], 1, false);
    register(registry, &["Element", "button"], 1, false);
    register(registry, &["Element", "checkbox"], 1, false);
    register(registry, &["Element", "container"], 1, false);
    register(registry, &["Element", "label"], 1, false);
    register(registry, &["Element", "link"], 1, false);
    register(registry, &["Element", "paragraph"], 1, false);
    register(registry, &["Element", "stripe"], 1, false);
    register(registry, &["Element", "text_input"], 1, false);

    // Log
    register(registry, &["Log", "info"], 0, true);
}
