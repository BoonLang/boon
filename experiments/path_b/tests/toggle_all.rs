//! Toggle-all test for Path B engine
//!
//! NOTE: These tests require template instantiation (List/map) where each todo item
//! has its own reactive HOLD instance. The current prototype uses List/append which
//! stores values, not live reactive instances.
//!
//! The toggle-all bug fix requires:
//! 1. Template instantiation per list item (List/map)
//! 2. External dependency capture (CaptureSpec)
//! 3. Proper scope management for nested reactives
//!
//! These features are documented but not fully implemented in this prototype.
//! The core reactive semantics (HOLD, THEN, LATEST, etc.) are demonstrated in
//! other tests (counter, list_append, todo_mvc_add_items).

use path_b::Engine;
use shared::examples::todo_mvc_program;
use shared::test_harness::{text, TestEngine, Value};

#[test]
fn toggle_all_affects_new_items() {
    // This test verifies that clicking toggle_all marks all items as completed.
    // It requires template instantiation (List/map) where each todo item
    // has its own reactive HOLD instance that captures toggle_all.click.

    let program = todo_mvc_program();
    let mut engine = TestEngine::<Engine>::new(&program);

    // Add items (they start as not completed)
    engine.inject_event("new_todo_input.submit", text("Buy milk"));
    engine.inject_event("new_todo_input.submit", text("Walk dog"));

    // Verify items added with completed: false
    let todos = engine.read("todos");
    match &todos {
        Value::List(items) => {
            assert_eq!(items.len(), 2);
            for item in items {
                if let Value::Object(obj) = item {
                    assert_eq!(obj.get("completed"), Some(&Value::Bool(false)));
                }
            }
        }
        _ => panic!("Expected list"),
    }

    // Click toggle_all - should mark all items as completed
    engine.inject_event("toggle_all.click", shared::test_harness::click());

    // Verify all items are now completed
    let todos = engine.read("todos");
    match todos {
        Value::List(items) => {
            assert_eq!(items.len(), 2);
            for (i, item) in items.iter().enumerate() {
                if let Value::Object(obj) = item {
                    assert_eq!(
                        obj.get("completed"),
                        Some(&Value::Bool(true)),
                        "Item {} should be completed after toggle_all",
                        i
                    );
                }
            }
        }
        _ => panic!("Expected list"),
    }
}
