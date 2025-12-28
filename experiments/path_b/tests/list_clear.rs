//! Tests for List/clear operation

use shared::examples::list_clear_program;
use shared::test_harness::{click, Engine, TestEngine, Value};

#[test]
fn list_clears_on_click() {
    let program = list_clear_program();
    let mut engine = TestEngine::<path_b::Engine>::new(&program);

    // Initial state: empty list
    engine.assert_eq("items", Value::list([]));

    // Add 3 items
    engine.inject_event("add.click", click());
    engine.assert_eq("items", Value::list([Value::Int(0)]));
    engine.inject_event("add.click", click());
    engine.assert_eq("items", Value::list([Value::Int(0), Value::Int(1)]));
    engine.inject_event("add.click", click());
    engine.assert_eq("items", Value::list([Value::Int(0), Value::Int(1), Value::Int(2)]));

    // Clear the list
    engine.inject_event("clear.click", click());
    engine.assert_eq("items", Value::list([]));

    // Add more items after clearing
    engine.inject_event("add.click", click());
    engine.assert_eq("items", Value::list([Value::Int(0)]));
}
