//! Counter test for Path B engine

use path_b::Engine;
use shared::examples::counter_program;
use shared::test_harness::{click, TestEngine, Value};

#[test]
fn counter_increments_on_click() {
    let program = counter_program();
    let mut engine = TestEngine::<Engine>::new(&program);

    // Initial count should be 0
    engine.assert_eq("count", Value::Int(0));

    // Click button
    engine.inject_event("button.click", click());
    engine.assert_eq("count", Value::Int(1));

    // Click again
    engine.inject_event("button.click", click());
    engine.assert_eq("count", Value::Int(2));

    // Click more times
    engine.inject_event("button.click", click());
    engine.inject_event("button.click", click());
    engine.inject_event("button.click", click());
    engine.assert_eq("count", Value::Int(5));
}
