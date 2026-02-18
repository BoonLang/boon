# Boon Performance Landscape

*Single source of truth for performance expectations across all engines.*

**Date:** 2026-02-18

---

## 1. Where Time Goes in a UI Frame

Before comparing engines, understand where time is actually spent in a Boon UI update:

```
User event (click/type/hover)
  │
  ├─ [5-15%]  Event routing & dispatch
  ├─ [5-20%]  Reactive computation (the part engines control)
  ├─ [10-30%] DOM diffing & patching (Zoon signal → DOM mutation)
  └─ [40-70%] Browser layout + paint + composite
```

**Key insight:** Even a 1000x improvement in reactive computation only affects 5-20%
of total frame time. A 10,000x faster engine turns a 16ms frame into a 15ms frame —
imperceptible. The wins that users actually *feel* come from:
- Reducing unnecessary DOM patches (algorithmic, not speed)
- Batching updates to avoid layout thrashing (architectural)
- Eliminating GC pauses (memory management)

This context is essential for evaluating the performance claims below.

---

## 2. Current State (Interpreted Engines)

### Per-Operation Computation Cost [ESTIMATED — not profiled]

| Operation           | DD Engine    | Actors Engine | Native Rust | Overhead |
|---------------------|-------------|---------------|-------------|----------|
| Counter increment   | ~1-10 us    | ~2-20 us      | ~1 ns       | 1K-20Kx  |
| List item toggle    | ~5-50 us    | ~10-100 us    | ~5 ns       | 1K-20Kx  |
| Object field update | ~0.5-5 us   | ~1-5 us       | ~1 ns       | 500-5Kx  |
| List append (1 item)| ~10-50 us   | ~20-100 us    | ~10 ns      | 1K-10Kx  |
| Text interpolation  | ~5-20 us    | ~5-20 us      | ~50 ns      | 100-400x |

> These are order-of-magnitude estimates from code analysis. The overhead comes from:
> `Value` enum dispatch, `Arc` cloning, `BTreeMap`/`HashMap` lookups, channel message
> passing, and DD framework coordination.

### End-to-End Frame Impact

For a typical TodoMVC interaction (toggle one item):

| Phase                  | DD Engine  | Actors Engine | Notes                    |
|------------------------|-----------|---------------|--------------------------|
| Event dispatch         | ~50 us    | ~100 us       | Channel routing          |
| Reactive computation   | ~100 us   | ~200 us       | Toggle + retain + count  |
| Signal → DOM patches   | ~200 us   | ~200 us       | Zoon framework           |
| Browser layout + paint | ~2-8 ms   | ~2-8 ms       | Same for both engines    |
| **Total frame time**   | **~3-9 ms** | **~3-9 ms** | Well under 16ms budget   |

**Conclusion:** Current engines are already fast enough for most UI interactions.
The bottleneck is browser rendering, not Boon computation. Performance problems
appear with **large lists** (100+ items) or **rapid events** (fast typing, hover).

---

## 3. After Actor Engine Optimizations (Track A-D)

Reference: `actor_engine_performance_plan.md`

### Expected Improvements

| Track | Focus | Expected Gain | Confidence |
|-------|-------|---------------|------------|
| A (P0) | Event dedup, bounded buffers, SmallVec | ~10-20% less allocation | HIGH — mechanical changes |
| B (P1) | List path: shared Replace, incremental retain | ~30-50% less list overhead | MEDIUM — algorithmic |
| C (P1/P2) | Value/persistence allocation | ~10-20% less GC pressure | MEDIUM |
| D (P2) | Arc reduction, ownership simplification | ~20-40% less refcount overhead | MEDIUM — structural |

### What Users Would Feel

- **Typing:** Smoother in long text inputs (A1 event dedup removes redundant updates)
- **Large lists (100+ items):** Noticeably faster filter/toggle-all (B1-B3 reduce O(n) to O(delta))
- **Hover:** Less CPU waste (A1 latest-wins coalescing)
- **Memory:** More stable under sustained interaction (C3 write coalescing, D3 cleanup)

### What Users Would NOT Feel

- Counter increment: already fast enough, optimizations are invisible
- Small list operations (< 20 items): already well under frame budget
- Text rendering: dominated by browser, not computation

---

## 4. After WASM Compilation (Theoretical)

Reference: `boon_as_systems_language.md` §5-6

### Per-Operation Compiled Cost [PROJECTED]

| Operation           | Compiled WASM | Native Rust | WASM Overhead | vs Interpreted |
|---------------------|--------------|-------------|---------------|----------------|
| Counter increment   | ~2-10 ns     | ~1 ns       | 2-10x         | 1,000-5,000x faster |
| List item toggle    | ~10-50 ns    | ~5 ns       | 2-10x         | 1,000-5,000x faster |
| Object field update | ~2-10 ns     | ~1 ns       | 2-10x         | 500-2,000x faster |
| List append (1 item)| ~20-100 ns   | ~10 ns      | 2-10x         | 500-2,000x faster |

> The 2-10x WASM overhead vs native comes from: WASM sandbox checks, indirect calls,
> bounds checking, and V8/SpiderMonkey compilation overhead. This matches published
> USENIX ATC 2019 data (1.45-1.55x average, with outliers up to 3-5x).

### End-to-End Frame Impact with Compilation

For the same TodoMVC toggle:

| Phase                  | Compiled WASM | Current DD | Improvement |
|------------------------|--------------|------------|-------------|
| Event dispatch         | ~1 us        | ~50 us     | 50x         |
| Reactive computation   | ~2 us        | ~100 us    | 50x         |
| Signal → DOM patches   | ~200 us      | ~200 us    | 1x (same)   |
| Browser layout + paint | ~2-8 ms      | ~2-8 ms    | 1x (same)   |
| **Total frame time**   | **~2-8 ms**  | **~3-9 ms**| **~10-15%** |

**Honest assessment:** For typical single-item interactions, compilation provides
~10-15% end-to-end improvement. Users would barely notice. The real wins appear for:

### Where Compilation Actually Matters

| Scenario | Current (DD) | Compiled WASM | User Impact |
|----------|-------------|---------------|-------------|
| Toggle all (200 items) | ~20-100 ms (jank!) | ~0.5-2 ms | **Eliminates frame drops** |
| Rapid typing (10 chars/sec) | ~1-5 ms/event | ~0.01-0.05 ms/event | **Smoother input** |
| Filter switch (200 items) | ~10-50 ms | ~0.2-1 ms | **Instant response** |
| Initial render (100 items) | ~50-200 ms | ~1-5 ms | **Faster startup** |

**Conclusion:** Compilation matters for **large lists** and **rapid events** — the
exact scenarios where current engines struggle. For simple interactions (counter,
small forms), it's invisible.

---

## 5. Theoretical Limits

What's the absolute best compiled Boon could achieve?

| System | Approach | Overhead vs Native C/Rust | Applicability to Boon |
|--------|----------|---------------------------|----------------------|
| Lustre/SCADE | Static schedule, typed structs | **~0%** | Static subgraphs only (no dynamic lists) |
| StreamIt | Dataflow fusion, SDF scheduling | **0.5-2x faster** | SDF subgraphs only (fixed-rate pipes) |
| Pony | Actor model, zero-copy | **~1-2x** | Full dynamic programs |
| Futhark | Uniqueness types, data-parallel | **~1x GPU** | Parallel array operations only |

**Boon's realistic ceiling:** ~1.5-3x of Rust for the computation phase, with WASM
adding another 1.5-2x on top. Net: **~2-6x of native Rust** for compiled Boon in WASM.

For static subgraphs with fusion: potentially **matching or beating** hand-written
Rust, but this applies to a minority of real-world Boon programs (perhaps 30-60% of
a typical UI's dataflow graph is "static" in this sense).

---

## 6. Performance Roadmap Summary

```
TODAY                    NEAR-TERM               MEDIUM-TERM             LONG-TERM
─────                    ──────────              ────────────            ──────────
DD: 1K-20Kx overhead     Actor opts (A-D):       WASM compilation:       StreamIt-style fusion:
Actors: 1K-20Kx          10-50% less overhead    1,000-5,000x faster     matching native for
                         (for list operations)    per-operation           static subgraphs
                                                                         [SPECULATIVE]

End-to-end UI:           End-to-end UI:          End-to-end UI:
3-9ms per event          2-8ms per event         2-8ms per event (small)
(fine for most cases)    (smoother large lists)  0.5-5ms (large lists)   [PROJECTED]
```

**Key takeaway:** Performance is NOT the primary reason to build the WASM compiler.
The primary reasons are: self-hosting (language maturity), eliminating the Rust
toolchain dependency (developer experience), and enabling new deployment targets
(server-side WASM, embedded). Performance is a nice bonus, not the motivation.
