//! Core DD engine — pure computation, no browser dependencies.
//!
//! Anti-cheat boundary: this module must NOT use:
//! - zoon, web_sys, Mutable<T>, RefCell, thread_local, Mutex, RwLock

pub mod compile;
pub mod operators;
pub mod runtime;
pub mod types;
pub mod value;
