//! TodoMVC test for Path B engine
//!
//! NOTE: Path B uses full re-evaluation approach. This test verifies
//! the engine compiles and initializes correctly.

use path_b::Engine;
use shared::examples::todo_mvc_program;
use shared::test_harness::{TestEngine, Value};

#[test]
fn todo_mvc_initializes() {
    let program = todo_mvc_program();
    let engine = TestEngine::<Engine>::new(&program);

    // Verify initial state
    engine.assert_eq("todos", Value::List(vec![]));
    engine.assert_eq("all_completed", Value::Bool(true)); // vacuously true for empty list
}
