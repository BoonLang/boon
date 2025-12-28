//! Quick timing comparison for Path A and Path B engines.

use std::time::Instant;
use shared::examples::{todo_mvc_program, list_clear_program, list_remove_program};
use shared::test_harness::{click, text, Engine};

fn main() {
    println!("=== Quick Performance Comparison (TodoMVC) ===\n");

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

    // Detailed comparison
    println!("=== Per-Tick Cost Comparison (100 items) ===\n");

    // Path A breakdown
    {
        let program = todo_mvc_program();
        let mut engine = path_a::Engine::new(&program);

        let setup_start = Instant::now();
        for i in 0..100 {
            engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
            engine.tick();
        }
        let setup_time = setup_start.elapsed();
        println!("Path A Setup: {:?} ({:.1}µs per add)", setup_time, setup_time.as_micros() as f64 / 100.0);

        engine.inject("toggle_all.click", click());
        let toggle_start = Instant::now();
        engine.tick();
        let toggle_time = toggle_start.elapsed();
        println!("Path A Toggle: {:?}", toggle_time);
    }

    // Path B breakdown
    {
        let program = todo_mvc_program();
        let mut engine = path_b::Engine::new(&program);

        let setup_start = Instant::now();
        for i in 0..100 {
            engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
            engine.tick();
        }
        let setup_time = setup_start.elapsed();
        println!("Path B Setup: {:?} ({:.1}µs per add)", setup_time, setup_time.as_micros() as f64 / 100.0);

        engine.inject("toggle_all.click", click());
        let toggle_start = Instant::now();
        engine.tick();
        let toggle_time = toggle_start.elapsed();
        println!("Path B Toggle: {:?}", toggle_time);
    }

    // List Operations Benchmarks
    println!("\n=== List Operations Benchmark ===\n");

    // List/clear benchmark
    println!("--- List/clear (100 items) ---");
    {
        // Path A
        let program = list_clear_program();
        let mut engine_a = path_a::Engine::new(&program);
        for _ in 0..100 {
            engine_a.inject("add.click", click());
            engine_a.tick();
        }
        let start = Instant::now();
        engine_a.inject("clear.click", click());
        engine_a.tick();
        let path_a_clear = start.elapsed();

        // Path B
        let program = list_clear_program();
        let mut engine_b = path_b::Engine::new(&program);
        for _ in 0..100 {
            engine_b.inject("add.click", click());
            engine_b.tick();
        }
        let start = Instant::now();
        engine_b.inject("clear.click", click());
        engine_b.tick();
        let path_b_clear = start.elapsed();

        println!("Path A: {:?}", path_a_clear);
        println!("Path B: {:?}", path_b_clear);
        println!("Ratio (A/B): {:.2}x\n", path_a_clear.as_secs_f64() / path_b_clear.as_secs_f64());
    }

    // List/remove benchmark
    println!("--- List/remove (remove 50 from 100 items) ---");
    {
        // Path A
        let program = list_remove_program();
        let mut engine_a = path_a::Engine::new(&program);
        for _ in 0..100 {
            engine_a.inject("add.click", click());
            engine_a.tick();
        }
        let start = Instant::now();
        for _ in 0..50 {
            engine_a.inject("remove.click", click());
            engine_a.tick();
        }
        let path_a_remove = start.elapsed();

        // Path B
        let program = list_remove_program();
        let mut engine_b = path_b::Engine::new(&program);
        for _ in 0..100 {
            engine_b.inject("add.click", click());
            engine_b.tick();
        }
        let start = Instant::now();
        for _ in 0..50 {
            engine_b.inject("remove.click", click());
            engine_b.tick();
        }
        let path_b_remove = start.elapsed();

        println!("Path A: {:?} ({:.1}µs per remove)", path_a_remove, path_a_remove.as_micros() as f64 / 50.0);
        println!("Path B: {:?} ({:.1}µs per remove)", path_b_remove, path_b_remove.as_micros() as f64 / 50.0);
        println!("Ratio (A/B): {:.2}x\n", path_a_remove.as_secs_f64() / path_b_remove.as_secs_f64());
    }

    // Summary table
    println!("=== Summary ===\n");
    println!("| Operation | Path A | Path B | Winner |");
    println!("|-----------|--------|--------|--------|");
}
