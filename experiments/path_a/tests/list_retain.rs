//! Tests for List/retain operation

use shared::examples::list_retain_program;
use shared::test_harness::{click, Engine, TestEngine, Value};

#[test]
fn list_retains_uncompleted_items() {
    let program = list_retain_program();
    let mut engine = TestEngine::<path_a::Engine>::new(&program);

    // Initial state: empty list
    engine.assert_eq("items", Value::list([]));

    // Add 3 items (all with completed: false)
    engine.inject_event("add.click", click());
    engine.inject_event("add.click", click());
    engine.inject_event("add.click", click());

    let items = engine.read("items");
    if let Value::List(items) = items {
        assert_eq!(items.len(), 3);
        // All items should have completed: false
        for item in items.iter() {
            if let Value::Object(obj) = item {
                assert_eq!(obj.get("completed"), Some(&Value::Bool(false)));
            }
        }
    }

    // Clear completed - should keep all items since none are completed
    engine.inject_event("clear_completed.click", click());

    let items = engine.read("items");
    if let Value::List(items) = items {
        // All items should still be there since none were completed
        assert_eq!(items.len(), 3);
    }
}
