//! TodoMVC integration test for Path A engine

use path_a::Engine;
use shared::examples::todo_mvc_program;
use shared::test_harness::{text, TestEngine, Value};

#[test]
fn todo_mvc_add_items() {
    let program = todo_mvc_program();
    let mut engine = TestEngine::<Engine>::new(&program);

    // Initially empty
    let todos = engine.read("todos");
    assert!(matches!(todos, Value::List(ref v) if v.is_empty()));

    // Add first item
    engine.inject_event("new_todo_input.submit", text("Buy groceries"));

    let todos = engine.read("todos");
    match todos {
        Value::List(items) => {
            assert_eq!(items.len(), 1);
            // Check text field
            if let Value::Object(obj) = &items[0] {
                assert_eq!(obj.get("text"), Some(&Value::String("Buy groceries".to_string())));
            }
        }
        _ => panic!("Expected list"),
    }

    // Add second item
    engine.inject_event("new_todo_input.submit", text("Clean room"));

    let todos = engine.read("todos");
    match todos {
        Value::List(items) => {
            assert_eq!(items.len(), 2);
        }
        _ => panic!("Expected list"),
    }
}

#[test]
fn all_completed_tracks_completion() {
    // NOTE: Full toggle-all bug fix requires template instantiation (List/map)
    // so each todo item has its own reactive HOLD instance.
    // This prototype uses List/append which stores values, not live instances.
    // This test verifies the basic all_completed computation works.

    let program = todo_mvc_program();
    let mut engine = TestEngine::<Engine>::new(&program);

    // No items - should be true (vacuously true for empty list)
    engine.assert_eq("all_completed", Value::Bool(true));

    // Add items (they start as not completed)
    engine.inject_event("new_todo_input.submit", text("Item 1"));
    engine.inject_event("new_todo_input.submit", text("Item 2"));

    // Not all completed - items have completed: false
    engine.assert_eq("all_completed", Value::Bool(false));

    // Note: toggle_all would require template instantiation to work properly
    // Each item would need its own HOLD instance that reacts to toggle_all.click
}
