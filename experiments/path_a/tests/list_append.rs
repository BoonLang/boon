//! List append test for Path A engine

use path_a::Engine;
use shared::examples::list_append_program;
use shared::test_harness::{click, TestEngine, Value};

#[test]
fn list_appends_on_click() {
    let program = list_append_program();
    let mut engine = TestEngine::<Engine>::new(&program);

    // Initial list should be empty
    engine.assert_eq("items", Value::list([]));

    // Click button - appends 0 (current length)
    engine.inject_event("button.click", click());
    engine.assert_eq("items", Value::list([Value::Int(0)]));

    // Click again - appends 1
    engine.inject_event("button.click", click());
    engine.assert_eq("items", Value::list([Value::Int(0), Value::Int(1)]));

    // Click more
    engine.inject_event("button.click", click());
    engine.assert_eq(
        "items",
        Value::list([Value::Int(0), Value::Int(1), Value::Int(2)]),
    );
}
