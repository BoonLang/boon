//! Counter test for Path B engine
//!
//! NOTE: Path B uses full re-evaluation approach. This test verifies
//! the engine compiles and initializes correctly.

use path_b::Engine;
use shared::examples::counter_program;
use shared::test_harness::{TestEngine, Value};

#[test]
fn counter_initializes() {
    let program = counter_program();
    let engine = TestEngine::<Engine>::new(&program);

    // Verify initial count is 0
    engine.assert_eq("count", Value::Int(0));
}
