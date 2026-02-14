//! Browser events → DD input injection.
//!
//! Handles routing browser events into DD input sessions:
//! - LINK events (click, press, blur, etc.) — transient, injected per epoch
//! - Router/route() — continuous input from browser URL
//! - element.hovered / element.focused — continuous hover/focus state

use super::super::core::types::LinkId;
use super::super::core::value::Value;

/// Router state: current browser route/path.
///
/// Injected as a continuous DD input. When the URL changes,
/// old route is retracted and new route is inserted.
pub struct RouterInput {
    pub current_route: String,
}

impl RouterInput {
    pub fn new() -> Self {
        RouterInput {
            current_route: "/".to_string(),
        }
    }

    /// Get the current route as a DD Value.
    pub fn as_value(&self) -> Value {
        Value::text(&*self.current_route)
    }
}

/// Element hover state: tracks which elements are hovered.
///
/// Injected as a continuous DD input per element.
/// `element.hovered |> WHILE { True => ..., False => ... }`
pub struct HoverInput {
    pub element_id: LinkId,
    pub is_hovered: bool,
}

impl HoverInput {
    pub fn new(element_id: LinkId) -> Self {
        HoverInput {
            element_id,
            is_hovered: false,
        }
    }

    pub fn as_value(&self) -> Value {
        Value::bool(self.is_hovered)
    }
}

/// Element focus state: tracks which elements are focused.
pub struct FocusInput {
    pub element_id: LinkId,
    pub is_focused: bool,
}

impl FocusInput {
    pub fn new(element_id: LinkId) -> Self {
        FocusInput {
            element_id,
            is_focused: false,
        }
    }

    pub fn as_value(&self) -> Value {
        Value::bool(self.is_focused)
    }
}
