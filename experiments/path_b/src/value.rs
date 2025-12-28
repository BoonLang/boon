//! Value utilities for Path B engine.

use shared::test_harness::Value;

/// Check if a value is SKIP
pub fn is_skip(value: &Value) -> bool {
    matches!(value, Value::Skip)
}

/// Built-in operations on values
pub mod ops {
    use super::*;

    pub fn add(a: &Value, b: &Value) -> Value {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
            (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
            (Value::Float(a), Value::Int(b)) => Value::Float(a + *b as f64),
            (Value::Int(a), Value::Float(b)) => Value::Float(*a as f64 + b),
            _ => Value::Skip,
        }
    }

    pub fn bool_not(v: &Value) -> Value {
        match v {
            Value::Bool(b) => Value::Bool(!b),
            _ => Value::Skip,
        }
    }

    pub fn list_len(v: &Value) -> Value {
        match v {
            Value::List(items) => Value::Int(items.len() as i64),
            _ => Value::Skip,
        }
    }

    pub fn list_append(list: &Value, item: Value) -> Value {
        use std::sync::Arc;
        if is_skip(&item) {
            return list.clone();
        }
        match list {
            Value::List(items) => {
                // Clone the underlying Vec (not just the Arc) and append
                let mut new_items = (**items).clone();
                new_items.push(item);
                Value::List(Arc::new(new_items))
            }
            _ => Value::Skip,
        }
    }

    pub fn list_every<F>(list: &Value, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        match list {
            Value::List(items) => Value::Bool(items.iter().all(predicate)),
            _ => Value::Skip,
        }
    }

    pub fn list_clear(_list: &Value) -> Value {
        use std::sync::Arc;
        Value::List(Arc::new(Vec::new()))
    }

    pub fn list_remove(list: &Value, index: i64) -> Value {
        use std::sync::Arc;
        match list {
            Value::List(items) => {
                let idx = index as usize;
                if idx < items.len() {
                    let mut new_items = (**items).clone();
                    new_items.remove(idx);
                    Value::List(Arc::new(new_items))
                } else {
                    list.clone()
                }
            }
            _ => Value::Skip,
        }
    }

    pub fn get_field(obj: &Value, field: &str) -> Value {
        match obj {
            Value::Object(fields) => fields.get(field).cloned().unwrap_or(Value::Skip),
            _ => Value::Skip,
        }
    }
}
