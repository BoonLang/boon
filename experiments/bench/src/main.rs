//! Quick timing comparison for Path A and Path B engines.

use std::time::Instant;
use shared::examples::todo_mvc_program;
use shared::test_harness::{click, text, Engine};

fn main() {
    println!("=== Quick Performance Comparison ===\n");

    // Test with 10, 50, 100 items
    for n in [10, 50, 100] {
        println!("--- {} items ---", n);

        // Path A
        let program = todo_mvc_program();
        let start = Instant::now();
        let mut engine_a = path_a::Engine::new(&program);
        for i in 0..n {
            engine_a.inject("new_todo_input.submit", text(format!("Item {}", i)));
            engine_a.tick();
        }
        engine_a.inject("toggle_all.click", click());
        engine_a.tick();
        let path_a_time = start.elapsed();

        // Path B
        let program = todo_mvc_program();
        let start = Instant::now();
        let mut engine_b = path_b::Engine::new(&program);
        for i in 0..n {
            engine_b.inject("new_todo_input.submit", text(format!("Item {}", i)));
            engine_b.tick();
        }
        engine_b.inject("toggle_all.click", click());
        engine_b.tick();
        let path_b_time = start.elapsed();

        println!("Path A: {:?}", path_a_time);
        println!("Path B: {:?}", path_b_time);
        println!("Ratio (A/B): {:.2}x\n", path_a_time.as_secs_f64() / path_b_time.as_secs_f64());
    }
}
