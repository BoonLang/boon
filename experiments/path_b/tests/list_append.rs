//! List append test for Path B engine
//!
//! NOTE: Path B uses full re-evaluation approach. This test verifies
//! the engine compiles and initializes correctly.

use path_b::Engine;
use shared::examples::list_append_program;
use shared::test_harness::{TestEngine, Value};

#[test]
fn list_initializes() {
    let program = list_append_program();
    let engine = TestEngine::<Engine>::new(&program);

    // Verify initial list is empty
    engine.assert_eq("items", Value::List(vec![]));
}
