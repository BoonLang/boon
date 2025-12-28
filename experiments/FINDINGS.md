# Engine Prototype Findings

## Summary

Two prototype engines were built to evaluate different reactive engine architectures:

- **Path A**: Dirty propagation + explicit captures (arena-based slots)
- **Path B**: Full re-evaluation + no cloning (Instance VM style)

Both prototypes implement the core reactive constructs (HOLD, THEN, WHEN, WHILE, LATEST, LINK) and **pass all tests including the critical toggle_all_affects_new_items test**.

### Lines of Code

| Crate | Lines |
|-------|-------|
| Path A | 1,747 |
| Path B | 1,622 |
| Shared | 791 |
| **Total** | **4,160** |

## Benchmark Results

| Benchmark | Path A | Path B | Winner |
|-----------|--------|--------|--------|
| counter (1000 clicks) | 2.28ms | 1.30ms | **Path B** (1.8x) |
| list_append (1000 items) | 177ms | 1.4ms | **Path B** (126x) |
| add_item (todo) | 151ms | 360µs | **Path B** (420x) |
| toggle_all (100 items) | 1.7ms | 364µs | **Path B** (4.7x) |
| toggle_all (1000 items) | 14.4s | 3.5ms | **Path B** (4114x) |
| steady_state (10 slots) | 59µs | 3.2µs | **Path B** (18x) |
| steady_state (100 slots) | 772µs | 3.2µs | **Path B** (241x) |
| steady_state (1000 slots) | 20.3ms | 3.2µs | **Path B** (6340x) |

## Key Findings

### Path A: Dirty Propagation Issues

1. **O(n²) Complexity**: The 3-phase tick algorithm iterates all slots multiple times:
   - Phase 1: Stabilize non-pulse nodes (up to 20 passes)
   - Phase 2: Fire pulse nodes (THEN/WHEN/WHILE)
   - Phase 3: Propagate pulse results (up to 10 passes)

2. **No Topological Ordering**: Slots are processed in allocation order, not dependency order. This requires multiple passes for changes to propagate.

3. **Pulse Semantics Complexity**: The `fired_this_tick` tracking adds overhead and complexity.

4. **State Management**: The Cell node type for HOLD state works but adds indirection.

### Path B: Re-evaluation Advantages

1. **O(n) Complexity**: Each tick evaluates each slot exactly once per pass.

2. **Simpler Model**: No dirty tracking, no subscriber lists, no pulse firing logic.

3. **Constant Steady-State**: When nothing changes, evaluation is O(1) with caching.

4. **Cache Coherence**: Slot values are computed on-demand and cached.

## Toggle-All Bug Analysis

The "toggle-all bug" requires template instantiation where each todo item has its own reactive HOLD instance that responds to external events like `toggle_all.click`.

**✅ FIXED**: Both engines now pass the `toggle_all_affects_new_items` test.

### Path A Solution
- Two-pass compilation for forward references (pre-allocate slots before compilation)
- Two-pass Object compilation for field forward references
- HOLD reads from ListAppend slot when THEN body is Skip
- Snapshotting captured values as Constants for non-Object items

### Path B Solution
- Re-evaluates nested HOLDs (non-root scope HOLDs) on every tick
- Finds HOLD expressions in AST by recursive search
- Updates HOLD cells if body produces non-Skip value
- Refreshes nested HOLD values when reading parent HOLD lists

Both solutions ensure that items created via ListAppend respond to external events (like `toggle_all.click`) on every tick.

## Recommendations

### For the New Engine

1. **Use Path B's approach** as the baseline:
   - Full re-evaluation per tick
   - Slot caching with dependency tracking
   - No explicit dirty propagation
   - Simpler code (125 fewer lines than Path A)

2. **Add dirty propagation as optimization**:
   - Mark slots dirty when inputs change
   - Skip re-evaluation for clean slots
   - This is an optimization, not the core algorithm

3. **Template instantiation is working**:
   - ✅ List/append creates per-item reactive subgraphs
   - ✅ External dependency capture via scope hierarchy
   - ✅ Nested HOLDs re-evaluated each tick

### Architecture Principles

1. **Slots are addressed by (ScopeId, ExprId)** - confirmed as correct
2. **SKIP is a first-class value** - works well for pulse semantics
3. **LINKs hold event values for entire tick** - required for path access
4. **Pulse nodes fire once per tick** - THEN/WHEN/WHILE semantics

## Files Structure

```
experiments/
├── PLAN.md              # Implementation plan
├── FINDINGS.md          # This document
├── test_all.sh          # Completeness verification
├── shared/              # Common infrastructure
│   ├── ast.rs           # Simplified AST
│   ├── test_harness.rs  # Engine testing trait
│   └── examples.rs      # Test programs
├── path_a/              # Dirty propagation prototype
│   ├── engine.rs        # 3-phase tick implementation
│   ├── arena.rs         # Slot storage
│   ├── evaluator.rs     # AST to slots compiler
│   └── tests/           # Integration tests
├── path_b/              # Re-evaluation prototype
│   ├── runtime.rs       # Full re-eval implementation
│   ├── cache.rs         # Slot caching
│   ├── diagnostics.rs   # "Why did X change?"
│   └── tests/           # Integration tests
└── bench/               # Criterion benchmarks
```

## Conclusion

**Path B's full re-evaluation approach is simpler and faster** for the prototype scale. The dirty propagation in Path A added complexity without improving performance due to the multi-pass requirement.

### Final Status

| Criterion | Path A | Path B |
|-----------|--------|--------|
| All tests pass | ✅ | ✅ |
| toggle_all_affects_new_items | ✅ | ✅ |
| Lines of code | 1,747 | 1,622 |
| Performance (toggle_all 1000 items) | 14.4s | 3.5ms |
| Steady-state overhead | O(n) passes | O(1) cache check |

### Recommendation

**Winner: Path B**

For the production engine:
1. ✅ Start with Path B's re-evaluation model
2. Add incremental dirty tracking as optimization
3. ✅ Template instantiation working (toggle_all test passes)
4. Keep the diagnostics infrastructure from Path B

Both prototypes successfully fix the toggle-all bug. Path B is recommended due to simpler code and significantly better performance.
