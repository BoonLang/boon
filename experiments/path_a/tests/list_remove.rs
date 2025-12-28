//! Tests for List/remove operation

use shared::examples::list_remove_program;
use shared::test_harness::{click, Engine, TestEngine, Value};

#[test]
fn list_removes_first_item_on_click() {
    let program = list_remove_program();
    let mut engine = TestEngine::<path_a::Engine>::new(&program);

    // Initial state: empty list
    engine.assert_eq("items", Value::list([]));

    // Add 3 items
    engine.inject_event("add.click", click());
    engine.inject_event("add.click", click());
    engine.inject_event("add.click", click());

    let items = engine.read("items");
    assert!(matches!(items, Value::List(_)));
    if let Value::List(items) = items {
        assert_eq!(items.len(), 3);
    }

    // Remove first item
    engine.inject_event("remove.click", click());

    let items = engine.read("items");
    if let Value::List(items) = items {
        assert_eq!(items.len(), 2);
    }

    // Remove again
    engine.inject_event("remove.click", click());

    let items = engine.read("items");
    if let Value::List(items) = items {
        assert_eq!(items.len(), 1);
    }
}
