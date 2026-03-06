// @TODO remove
#![allow(dead_code, unused_variables)]

// Compile-time check: at least one engine must be enabled
#[cfg(not(any(
    feature = "engine-actors",
    feature = "engine-dd",
    feature = "engine-wasm"
)))]
compile_error!(
    "At least one Boon engine must be enabled. \
     Use --features engine-actors, --features engine-dd, or --features engine-wasm"
);

pub mod parser;
pub mod platform;

pub use boon_monitor_protocol as monitor_protocol;
pub use boon_renderer_zoon as renderer_zoon;
pub use boon_renderer_zoon::zoon;
pub use boon_scene as scene;
