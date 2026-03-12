use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum KernelValue {
    Number(f64),
    Text(String),
    Bool(bool),
    Tag(String),
    Object(BTreeMap<String, KernelValue>),
    List(Vec<KernelValue>),
    Skip,
}

impl KernelValue {
    #[must_use]
    pub const fn is_skip(&self) -> bool {
        matches!(self, Self::Skip)
    }
}

impl PartialEq for KernelValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(lhs), Self::Number(rhs)) => lhs.to_bits() == rhs.to_bits(),
            (Self::Text(lhs), Self::Text(rhs)) | (Self::Tag(lhs), Self::Tag(rhs)) => lhs == rhs,
            (Self::Bool(lhs), Self::Bool(rhs)) => lhs == rhs,
            (Self::Object(lhs), Self::Object(rhs)) => lhs == rhs,
            (Self::List(lhs), Self::List(rhs)) => lhs == rhs,
            (Self::Skip, Self::Skip) => true,
            _ => false,
        }
    }
}

impl Default for KernelValue {
    fn default() -> Self {
        Self::Skip
    }
}

impl From<f64> for KernelValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<bool> for KernelValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<String> for KernelValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for KernelValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}
