//! DD-compatible value types for Boon.
//!
//! These are simple data types that can flow through DD dataflows.
//! Unlike the actor-based `Value` in engine.rs, these are pure data.

use std::collections::BTreeMap;
use std::sync::Arc;

use ordered_float::OrderedFloat;

/// A simple value type for DD dataflows.
///
/// These values are pure data - no actors, no channels, no async.
/// They can be cloned, compared, and serialized.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DdValue {
    /// Null/unit value
    Unit,
    /// Boolean
    Bool(bool),
    /// Number (using OrderedFloat for Ord/Hash impl)
    Number(OrderedFloat<f64>),
    /// Text string
    Text(Arc<str>),
    /// Object (key-value pairs, ordered for Ord impl)
    Object(Arc<BTreeMap<Arc<str>, DdValue>>),
    /// List of values
    List(Arc<Vec<DdValue>>),
    /// Tagged object (like Object but with a tag name)
    Tagged {
        tag: Arc<str>,
        fields: Arc<BTreeMap<Arc<str>, DdValue>>,
    },
}

impl DdValue {
    /// Create a unit value.
    pub fn unit() -> Self {
        Self::Unit
    }

    /// Create an integer value.
    pub fn int(n: i64) -> Self {
        Self::Number(OrderedFloat(n as f64))
    }

    /// Create a float value.
    pub fn float(n: f64) -> Self {
        Self::Number(OrderedFloat(n))
    }

    /// Create a text value.
    pub fn text(s: impl Into<Arc<str>>) -> Self {
        Self::Text(s.into())
    }

    /// Create an object from key-value pairs.
    pub fn object(pairs: impl IntoIterator<Item = (impl Into<Arc<str>>, DdValue)>) -> Self {
        let map: BTreeMap<Arc<str>, DdValue> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        Self::Object(Arc::new(map))
    }

    /// Create a list from values.
    pub fn list(items: impl IntoIterator<Item = DdValue>) -> Self {
        Self::List(Arc::new(items.into_iter().collect()))
    }

    /// Create a tagged object.
    pub fn tagged(
        tag: impl Into<Arc<str>>,
        fields: impl IntoIterator<Item = (impl Into<Arc<str>>, DdValue)>,
    ) -> Self {
        let map: BTreeMap<Arc<str>, DdValue> = fields
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
        Self::Tagged {
            tag: tag.into(),
            fields: Arc::new(map),
        }
    }

    /// Get a field from an object or tagged object.
    pub fn get(&self, key: &str) -> Option<&DdValue> {
        match self {
            Self::Object(map) => map.get(key),
            Self::Tagged { fields, .. } => fields.get(key),
            _ => None,
        }
    }

    /// Check if this is a truthy value.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Unit => false,
            Self::Bool(b) => *b,
            Self::Number(n) => n.0 != 0.0,
            Self::Text(s) => !s.is_empty(),
            Self::Object(map) => !map.is_empty(),
            Self::List(items) => !items.is_empty(),
            // False tag is falsy, True and all other tags are truthy
            Self::Tagged { tag, .. } => tag.as_ref() != "False",
        }
    }

    /// Convert to display string for rendering.
    pub fn to_display_string(&self) -> String {
        match self {
            Self::Unit => String::new(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => {
                // Display integers without decimal point
                if n.0.fract() == 0.0 && n.0.abs() < i64::MAX as f64 {
                    format!("{}", n.0 as i64)
                } else {
                    n.0.to_string()
                }
            }
            Self::Text(s) => s.to_string(),
            Self::Object(_) => "[object]".to_string(),
            Self::List(items) => format!("[list of {}]", items.len()),
            Self::Tagged { tag, .. } => format!("[{tag}]"),
        }
    }

    /// Try to get as integer.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Number(n) => Some(n.0 as i64),
            _ => None,
        }
    }

    /// Try to get as float.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(n.0),
            _ => None,
        }
    }

    /// Try to get as text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as list.
    pub fn as_list(&self) -> Option<&[DdValue]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }
}

impl Default for DdValue {
    fn default() -> Self {
        Self::Unit
    }
}

impl From<i64> for DdValue {
    fn from(n: i64) -> Self {
        Self::int(n)
    }
}

impl From<i32> for DdValue {
    fn from(n: i32) -> Self {
        Self::int(n as i64)
    }
}

impl From<f64> for DdValue {
    fn from(n: f64) -> Self {
        Self::float(n)
    }
}

impl From<bool> for DdValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<&str> for DdValue {
    fn from(s: &str) -> Self {
        Self::Text(Arc::from(s))
    }
}

impl From<String> for DdValue {
    fn from(s: String) -> Self {
        Self::Text(Arc::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_creation() {
        let unit = DdValue::unit();
        let num = DdValue::int(42);
        let float_num = DdValue::float(3.14);
        let text = DdValue::text("hello");
        let obj = DdValue::object([("x", DdValue::int(1)), ("y", DdValue::int(2))]);
        let list = DdValue::list([DdValue::int(1), DdValue::int(2), DdValue::int(3)]);

        assert_eq!(unit, DdValue::Unit);
        assert_eq!(num.as_int(), Some(42));
        assert_eq!(float_num.as_float(), Some(3.14));
        assert_eq!(text.as_text(), Some("hello"));
        assert_eq!(obj.get("x"), Some(&DdValue::int(1)));
        assert_eq!(list.as_list().map(|l| l.len()), Some(3));
    }

    #[test]
    fn test_value_ordering() {
        // Values must be Ord for DD
        let a = DdValue::int(1);
        let b = DdValue::int(2);
        assert!(a < b);

        let x = DdValue::text("a");
        let y = DdValue::text("b");
        assert!(x < y);
    }

    #[test]
    fn test_value_display() {
        assert_eq!(DdValue::int(42).to_display_string(), "42");
        assert_eq!(DdValue::float(3.14).to_display_string(), "3.14");
        assert_eq!(DdValue::text("hello").to_display_string(), "hello");
        assert_eq!(DdValue::Bool(true).to_display_string(), "true");
    }
}
