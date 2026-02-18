// @TODO remove
#![allow(dead_code, unused_variables)]

// Compile-time check: at least one engine must be enabled
#[cfg(not(any(feature = "engine-actors", feature = "engine-dd", feature = "engine-wasm")))]
compile_error!(
    "At least one Boon engine must be enabled. \
     Use --features engine-actors, --features engine-dd, or --features engine-wasm"
);

pub mod parser;
pub mod platform;

pub use zoon;
