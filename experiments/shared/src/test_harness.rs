//! Test harness for running both engine prototypes without a browser.
//! Provides event simulation and assertions.

use crate::ast::Program;
use std::collections::HashMap;
use std::sync::Arc;

/// Runtime value type shared between engines
/// Uses Arc for aggregate types to make Clone O(1) instead of O(n)
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    String(Arc<str>),  // Arc for O(1) clone
    Bool(bool),
    Unit,
    /// SKIP value - represents "no value" / unbound LINK
    Skip,
    /// Object with named fields - Arc-wrapped for O(1) clone
    Object(Arc<HashMap<String, Value>>),
    /// List of values - Arc-wrapped for O(1) clone
    List(Arc<Vec<Value>>),
}

impl Value {
    pub fn int(v: i64) -> Self {
        Value::Int(v)
    }

    pub fn bool(v: bool) -> Self {
        Value::Bool(v)
    }

    pub fn string(v: impl Into<String>) -> Self {
        Value::String(v.into().into())  // String -> Arc<str>
    }

    pub fn unit() -> Self {
        Value::Unit
    }

    pub fn skip() -> Self {
        Value::Skip
    }

    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Value)>) -> Self {
        Value::Object(Arc::new(fields.into_iter().map(|(k, v)| (k.into(), v)).collect()))
    }

    pub fn list(items: impl IntoIterator<Item = Value>) -> Self {
        Value::List(Arc::new(items.into_iter().collect()))
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&HashMap<String, Value>> {
        match self {
            Value::Object(v) => Some(v.as_ref()),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&Vec<Value>> {
        match self {
            Value::List(v) => Some(v.as_ref()),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    pub fn get_index(&self, index: usize) -> Option<&Value> {
        self.as_list()?.get(index)
    }

    pub fn is_skip(&self) -> bool {
        matches!(self, Value::Skip)
    }
}

/// Trait that both engine prototypes must implement
pub trait Engine: Sized {
    /// Create a new engine from an AST
    fn new(program: &Program) -> Self;

    /// Inject an event at the given path with a payload
    fn inject(&mut self, path: &str, payload: Value);

    /// Run one tick of the engine
    fn tick(&mut self);

    /// Read the current value at a path
    fn read(&self, path: &str) -> Value;
}

/// Test wrapper that provides convenient test methods
pub struct TestEngine<E: Engine> {
    engine: E,
}

impl<E: Engine> TestEngine<E> {
    pub fn new(program: &Program) -> Self {
        Self {
            engine: E::new(program),
        }
    }

    /// Inject an event and immediately tick
    pub fn inject_event(&mut self, path: &str, payload: Value) {
        self.engine.inject(path, payload);
        self.engine.tick();
    }

    /// Just tick without injecting
    pub fn tick(&mut self) {
        self.engine.tick();
    }

    /// Read a value
    pub fn read(&self, path: &str) -> Value {
        self.engine.read(path)
    }

    /// Assert that a path equals an expected value
    pub fn assert_eq(&self, path: &str, expected: Value) {
        let actual = self.engine.read(path);
        assert_eq!(
            actual, expected,
            "Path '{}' expected {:?} but got {:?}",
            path, expected, actual
        );
    }

    /// Assert that a path is SKIP
    pub fn assert_skip(&self, path: &str) {
        let actual = self.engine.read(path);
        assert!(
            actual.is_skip(),
            "Path '{}' expected SKIP but got {:?}",
            path,
            actual
        );
    }

    /// Get the underlying engine for direct access
    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// Get mutable access to the underlying engine
    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }
}

/// Helper to create a text value (for event payloads)
pub fn text(s: impl Into<String>) -> Value {
    Value::String(s.into().into())  // String -> Arc<str>
}

/// Helper to create a click event payload
pub fn click() -> Value {
    Value::Unit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_constructors() {
        assert_eq!(Value::int(42).as_int(), Some(42));
        assert_eq!(Value::bool(true).as_bool(), Some(true));
        assert_eq!(Value::string("hello").as_string(), Some("hello"));
        assert!(Value::skip().is_skip());
    }

    #[test]
    fn test_value_object() {
        let obj = Value::object([("a", Value::int(1)), ("b", Value::int(2))]);
        assert_eq!(obj.get("a"), Some(&Value::int(1)));
        assert_eq!(obj.get("b"), Some(&Value::int(2)));
        assert_eq!(obj.get("c"), None);
    }

    #[test]
    fn test_value_list() {
        let list = Value::list([Value::int(1), Value::int(2), Value::int(3)]);
        assert_eq!(list.get_index(0), Some(&Value::int(1)));
        assert_eq!(list.get_index(2), Some(&Value::int(3)));
        assert_eq!(list.get_index(5), None);
    }
}
