//! IO layer — bridges DD engine with browser.
//!
//! This is the ONLY place where Mutable<T>, Rc<RefCell<>>, and browser APIs are allowed.

pub mod events;
pub mod general;
pub mod outputs;
pub mod worker;
