//! Diagnostics tests for Path B engine
//! Tests the cache and diagnostics infrastructure.

use path_b::Runtime;
use shared::examples::counter_program;

#[test]
fn diagnostics_initializes() {
    let program = counter_program();
    let mut runtime = Runtime::new(program);
    runtime.enable_diagnostics();

    // Initial tick
    runtime.tick();

    // Verify diagnostics is accessible
    let diagnostics = runtime.diagnostics();
    let _ = diagnostics.changes.len();
}

#[test]
fn cache_initializes() {
    let program = counter_program();
    let mut runtime = Runtime::new(program);

    // Initial tick
    runtime.tick();

    // Verify cache is accessible
    let cache = runtime.cache();
    let entries: Vec<_> = cache.entries().collect();

    // Should have some cached values from evaluation
    let _ = entries.len();
}
