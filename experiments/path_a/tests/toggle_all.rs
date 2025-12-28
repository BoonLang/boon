//! Toggle-all test for Path A engine
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

use path_a::Engine;
use shared::examples::todo_mvc_program;
use shared::test_harness::{text, TestEngine, Value};

#[test]
fn toggle_all_requires_template_instantiation() {
    // This test documents what the toggle-all fix needs:
    // - Each todo item needs its own HOLD instance
    // - External dependencies (toggle_all, all_completed) must be captured
    // - Template instantiation (List/map) must create new reactive graphs per item

    let program = todo_mvc_program();
    let mut engine = TestEngine::<Engine>::new(&program);

    // Add items
    engine.inject_event("new_todo_input.submit", text("Buy milk"));
    engine.inject_event("new_todo_input.submit", text("Walk dog"));

    // Verify items added (this works)
    let todos = engine.read("todos");
    match todos {
        Value::List(items) => {
            assert_eq!(items.len(), 2);
        }
        _ => panic!("Expected list"),
    }

    // Note: Individual item toggles would require per-item HOLD instances
    // which needs List/map template instantiation, not just List/append
}
