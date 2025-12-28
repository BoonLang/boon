//! Benchmarks comparing Path A and Path B engines.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use shared::examples::{counter_program, list_append_program, todo_mvc_program};
use shared::test_harness::{click, text, Engine, Value};

/// Benchmark toggle-all operation with varying item counts
fn bench_toggle_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("toggle_all");

    for n in [10, 100, 1000] {
        // Path A
        group.bench_with_input(BenchmarkId::new("path_a", n), &n, |b, &n| {
            b.iter(|| {
                let program = todo_mvc_program();
                let mut engine = path_a::Engine::new(&program);

                // Add n items
                for i in 0..n {
                    engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
                    engine.tick();
                }

                // Toggle all
                engine.inject("toggle_all.click", click());
                engine.tick();
            });
        });

        // Path B
        group.bench_with_input(BenchmarkId::new("path_b", n), &n, |b, &n| {
            b.iter(|| {
                let program = todo_mvc_program();
                let mut engine = path_b::Engine::new(&program);

                // Add n items
                for i in 0..n {
                    engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
                    engine.tick();
                }

                // Toggle all
                engine.inject("toggle_all.click", click());
                engine.tick();
            });
        });
    }

    group.finish();
}

/// Benchmark steady state (tick with no changes)
fn bench_steady_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("steady_state");

    for n in [10, 100, 1000] {
        // Path A - setup once, then benchmark just ticks
        group.bench_with_input(BenchmarkId::new("path_a", n), &n, |b, &n| {
            let program = todo_mvc_program();
            let mut engine = path_a::Engine::new(&program);

            // Add n items
            for i in 0..n {
                engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
                engine.tick();
            }

            // Benchmark empty ticks
            b.iter(|| {
                engine.tick();
            });
        });

        // Path B
        group.bench_with_input(BenchmarkId::new("path_b", n), &n, |b, &n| {
            let program = todo_mvc_program();
            let mut engine = path_b::Engine::new(&program);

            // Add n items
            for i in 0..n {
                engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
                engine.tick();
            }

            // Benchmark empty ticks
            b.iter(|| {
                engine.tick();
            });
        });
    }

    group.finish();
}

/// Benchmark adding items
fn bench_add_item(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_item");

    // Path A
    group.bench_function("path_a", |b| {
        b.iter(|| {
            let program = todo_mvc_program();
            let mut engine = path_a::Engine::new(&program);

            // Add 100 items
            for i in 0..100 {
                engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
                engine.tick();
            }
        });
    });

    // Path B
    group.bench_function("path_b", |b| {
        b.iter(|| {
            let program = todo_mvc_program();
            let mut engine = path_b::Engine::new(&program);

            // Add 100 items
            for i in 0..100 {
                engine.inject("new_todo_input.submit", text(format!("Item {}", i)));
                engine.tick();
            }
        });
    });

    group.finish();
}

/// Benchmark counter increments
fn bench_counter(c: &mut Criterion) {
    let mut group = c.benchmark_group("counter");

    // Path A
    group.bench_function("path_a_1000_clicks", |b| {
        b.iter(|| {
            let program = counter_program();
            let mut engine = path_a::Engine::new(&program);

            for _ in 0..1000 {
                engine.inject("button.click", click());
                engine.tick();
            }
        });
    });

    // Path B
    group.bench_function("path_b_1000_clicks", |b| {
        b.iter(|| {
            let program = counter_program();
            let mut engine = path_b::Engine::new(&program);

            for _ in 0..1000 {
                engine.inject("button.click", click());
                engine.tick();
            }
        });
    });

    group.finish();
}

/// Benchmark list append
fn bench_list_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_append");

    // Path A
    group.bench_function("path_a_1000_appends", |b| {
        b.iter(|| {
            let program = list_append_program();
            let mut engine = path_a::Engine::new(&program);

            for _ in 0..1000 {
                engine.inject("button.click", click());
                engine.tick();
            }
        });
    });

    // Path B
    group.bench_function("path_b_1000_appends", |b| {
        b.iter(|| {
            let program = list_append_program();
            let mut engine = path_b::Engine::new(&program);

            for _ in 0..1000 {
                engine.inject("button.click", click());
                engine.tick();
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_toggle_all,
    bench_steady_state,
    bench_add_item,
    bench_counter,
    bench_list_append,
);
criterion_main!(benches);
