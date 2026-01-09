// @TODO remove
#![allow(dead_code, unused_variables)]

// Compile-time check: at least one engine must be enabled
#[cfg(not(any(feature = "engine-actors", feature = "engine-dd")))]
compile_error!(
    "At least one Boon engine must be enabled. \
     Use --features engine-actors (default) or --features engine-dd"
);

pub mod parser;
pub mod platform;

pub use zoon;
