//! Pure Value type for DD v2 engine.
//!
//! This is the data type stored in DD collections. It must implement
//! Clone, Debug, Ord, Hash for DD operators (arrange, join, etc.).
//! No DD, Zoon, or browser dependencies.

use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A Boon value that lives inside DD collections.
///
/// Every Boon value is represented as a DD collection element.
/// Scalars are single-element collections. Lists are multi-element collections.
///
/// Derives Serialize/Deserialize for DD's ExchangeData trait (required by
/// join, arrange, count, reduce operators).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Value {
    /// No value / unit
    Unit,
    /// Boolean: True or False
    Bool(bool),
    /// Number (f64 with total ordering)
    Number(OrderedFloat<f64>),
    /// Text string
    Text(Arc<str>),
    /// Tag (enum-like label): True, False, All, Active, Completed, NoElement, etc.
    Tag(Arc<str>),
    /// Object with named fields
    Object(Arc<BTreeMap<Arc<str>, Value>>),
    /// Tagged object (e.g., Element/button, TodoId[id:])
    Tagged {
        tag: Arc<str>,
        fields: Arc<BTreeMap<Arc<str>, Value>>,
    },
}

impl Value {
    pub fn number(n: f64) -> Self {
        Value::Number(OrderedFloat(n))
    }

    pub fn text(s: impl Into<Arc<str>>) -> Self {
        Value::Text(s.into())
    }

    pub fn tag(s: impl Into<Arc<str>>) -> Self {
        Value::Tag(s.into())
    }

    pub fn bool(b: bool) -> Self {
        Value::Bool(b)
    }

    pub fn object(fields: impl IntoIterator<Item = (impl Into<Arc<str>>, Value)>) -> Self {
        Value::Object(Arc::new(
            fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        ))
    }

    pub fn tagged(
        tag: impl Into<Arc<str>>,
        fields: impl IntoIterator<Item = (impl Into<Arc<str>>, Value)>,
    ) -> Self {
        Value::Tagged {
            tag: tag.into(),
            fields: Arc::new(fields.into_iter().map(|(k, v)| (k.into(), v)).collect()),
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(n.0),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_tag(&self) -> Option<&str> {
        match self {
            Value::Tag(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Tag(t) if t.as_ref() == "True" => Some(true),
            Value::Tag(t) if t.as_ref() == "False" => Some(false),
            _ => None,
        }
    }

    pub fn get_field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.get(name),
            Value::Tagged { fields, .. } => fields.get(name),
            _ => None,
        }
    }

    pub fn get_tag(&self) -> Option<&str> {
        match self {
            Value::Tag(t) => Some(t),
            Value::Tagged { tag, .. } => Some(tag),
            _ => None,
        }
    }

    /// Convert to user-visible display string.
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Unit => String::new(),
            Value::Bool(b) => {
                if *b {
                    "True".to_string()
                } else {
                    "False".to_string()
                }
            }
            Value::Number(n) => {
                if n.0 == n.0.floor() && n.0.is_finite() {
                    format!("{}", n.0 as i64)
                } else {
                    format!("{}", n.0)
                }
            }
            Value::Text(s) => s.to_string(),
            Value::Tag(s) => s.to_string(),
            Value::Object(fields) => {
                let entries: Vec<_> = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.to_display_string()))
                    .collect();
                format!("[{}]", entries.join(", "))
            }
            Value::Tagged { tag, fields } => {
                if fields.is_empty() {
                    tag.to_string()
                } else {
                    let entries: Vec<_> = fields
                        .iter()
                        .map(|(k, v)| format!("{}: {}", k, v.to_display_string()))
                        .collect();
                    format!("{}[{}]", tag, entries.join(", "))
                }
            }
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}
