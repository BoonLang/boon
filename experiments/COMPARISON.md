# Boon Engine Architecture Comparison

## Executive Summary

| Architecture | Core Approach | Performance | Code Size | Maturity |
|-------------|---------------|-------------|-----------|----------|
| **Old Engine** | Async actors + Arc + channels | Baseline | ~8,600 lines | Production |
| **V2 (New Engine)** | Sync arena + message passing | Expected 10-100x | Design only | Design phase |
| **Experiment A** | Dirty propagation + explicit captures | O(n²) worst case | ~1,747 lines | Prototype |
| **Experiment B** | Full re-eval + caching | 4114x faster than A | ~1,622 lines | Prototype |

---

## Dimension-by-Dimension Comparison

### 1. Memory Model

| Aspect | Old Engine | V2 | Exp A | Exp B |
|--------|------------|-----|-------|-------|
| **Allocation** | Arc<T> heap-scattered | Arena contiguous | Vec slots | FxHashMap |
| **Reference counting** | Yes (atomic ops) | No (indices) | No (owned) | Arc for collections |
| **Cache locality** | Poor (fragmented) | Excellent (64B aligned) | Good (Vec) | Moderate (HashMap) |
| **Snapshot-able** | No (closures) | Yes (plain data) | Partial | Yes (cells + cache) |
| **Memory overhead** | High (Arc metadata) | Low (indices) | Medium | Medium |

**Analysis**: V2's arena design is most hardware-friendly and cache-efficient. Exp B's FxHashMap provides good practical performance. Old Engine's Arc overhead is significant.

### 2. Reactivity Model

| Aspect | Old Engine | V2 | Exp A | Exp B |
|--------|------------|-----|-------|-------|
| **Propagation** | Push (streams) | Push (dirty queue) | Pull (multi-pass) | Pull (re-eval) |
| **Determinism** | Executor-dependent | Explicit sort order | Topo order | Evaluation order |
| **Passes per tick** | N/A (continuous) | 1 (quiescence loop) | 3-20 | 1 |
| **Steady state** | Always polling | O(dirty nodes) | O(n) scans | O(1) cache hits |
| **Cloning** | ValueActor clones | Node instantiation | Subgraph cloning | No cloning |

**Analysis**: Exp B's "no cloning" model is conceptually cleanest. V2's quiescence loop is most deterministic. Old Engine's push model creates complex debugging.

### 3. Performance Benchmarks

```
Benchmark                    | Old   | V2    | Exp A      | Exp B
-----------------------------|-------|-------|------------|--------
counter (1000 clicks)        | ~1ms  | N/A   | 2.28ms     | 1.30ms
list_append (1000 items)     | ~100ms| N/A   | 175ms      | 1.4ms
toggle_all (100 items)       | ~5ms  | N/A   | 1.7ms      | 364µs
toggle_all (1000 items)      | ~500ms| N/A   | 14.4 sec   | 3.5ms
steady_state (1000 slots)    | ~10ms | N/A   | 20.3ms     | 3.2µs
```

**Key ratios**:
- Exp B vs Exp A toggle_all (1000): **4114x faster**
- Exp B vs Exp A list_append: **125x faster**
- Exp B vs Exp A steady_state: **6344x faster**

**Analysis**: Exp B dramatically outperforms Exp A. Old Engine falls between them. V2 targets 10-100x improvement over Old Engine.

### 4. Correctness & Bug Handling

| Bug Class | Old Engine | V2 | Exp A | Exp B |
|-----------|------------|-----|-------|-------|
| **"Receiver is gone"** | Common, hard to debug | Impossible (no channels) | N/A | N/A |
| **Missed wires (toggle-all)** | Requires explicit tracking | Explicit captures | CaptureSpec | Solved by design |
| **Race conditions** | Possible (executor) | Impossible (sync) | N/A (sync) | N/A (sync) |
| **Stale cache** | N/A | Wire transparency | Multi-pass | Dep tracking |
| **Memory leaks** | Arc cycles possible | Arena frees all | No cycles | Arc cycles impossible |

**Analysis**: Exp B solves toggle-all bug *by design* (no cloning means all instances see same external refs). V2 and Exp A require explicit capture tracking.

### 5. Hardware/HVM Portability

| Criterion | Old Engine | V2 | Exp A | Exp B |
|-----------|------------|-----|-------|-------|
| **Maps to FSMs** | Actors = FSMs | Nodes = registers | Unclear | Eval = combinational |
| **Maps to wires** | Streams = wires | Edges = wires | Slots = wires | Scopes = address space |
| **No dynamic alloc** | No (Arc, heap) | Design goal | No (Vec grows) | No (HashMap grows) |
| **Fixed routing** | No (channels) | Static after compile | Static | Static |
| **Synthesis-ready** | No | Not yet (uses Vec) | No | No |

**Analysis**: V2 is designed with hardware in mind but isn't synthesis-ready yet. All use dynamic allocation. True FPGA synthesis requires fixed sizes.

### 6. Developer Experience

| Aspect | Old Engine | V2 | Exp A | Exp B |
|--------|------------|-----|-------|-------|
| **Debugging** | "Receiver is gone" | Dump arena | Ledger | Diagnostics |
| **Hot reload** | Impossible | Full support | Partial | Full support |
| **Time-travel debug** | Impossible | Snapshots | Ledger | TickSeq tracking |
| **Code complexity** | High (async) | Medium (sync) | Medium | Low |
| **Learning curve** | Steep (actors) | Medium | Medium | Low |

**Analysis**: Exp B has best DX with diagnostics and simple mental model. V2 has best theoretical DX with full snapshots. Old Engine is hardest to debug.

### 7. Code Complexity

```
                 | Lines  | Key Abstractions
-----------------|--------|------------------
Old Engine       | ~8,600 | ValueActor, ActorLoop, LazyValueActor, TypedStream, NamedChannel
V2 (docs only)   | N/A    | ReactiveNode, Arena, SlotId, NodeAddress, SourceId, ScopeId
Exp A            | 1,747  | Arena, NodeKind (16), Template, CaptureSpec, 4-phase tick
Exp B            | 1,622  | Runtime, Cache, Cells (3), ScopeId, SlotKey
```

**Analysis**: Exp B is simplest (125 fewer lines than Exp A, 80% less than Old Engine). V2 introduces many new concepts but has clear documentation.

---

## Deep Analysis: Why Exp B Wins on Performance

### The Fundamental Insight

**Exp A (Dirty Propagation)**:
```
tick():
  Phase 1: Process dirty queue (may need 20 passes to stabilize)
  Phase 2: Fire pulse nodes
  Phase 3: Propagate results (more passes)
  Phase 4: Reset fired pulses

  Complexity: O(n × passes) where passes can be 20+
```

**Exp B (Full Re-evaluation)**:
```
tick():
  For each top-level binding:
    eval(expr, scope, cache)
      → Cache hit? Return immediately
      → Cache miss? Compute, store, return

  Complexity: O(expressions × cache_check)
  In steady state: O(1) cache hits
```

### Why Multi-Pass Hurts

In Exp A's toggle_all scenario (1000 items):
1. Click toggle_all -> marks subscribers dirty
2. Phase 1 pass 1: Process ~100 nodes
3. Pass 2: Propagate to nested nodes
4. ... 20 passes to reach inner HOLD states
5. **Result**: 20 x 1000 = 20,000 node visits

In Exp B's toggle_all scenario (1000 items):
1. Click toggle_all -> set pending event
2. Re-eval top bindings -> cache hits
3. Re-eval nested HOLDs -> each checks toggle_all once
4. **Result**: 1000 cache checks + 1000 HOLD evals = 2000 operations

### The "No Cloning" Advantage

**Exp A must clone subgraphs** when creating list items:
- Each item gets copy of template nodes
- External references (toggle_all) must be explicitly captured
- If capture is missed -> bug (toggle_all doesn't affect new items)

**Exp B evaluates same AST** in different scopes:
- No copying needed
- External references naturally visible (same code, different scope)
- Cannot miss external dependencies by design

---

## Recommendation: Experiment B with V2 Concepts

### Primary Recommendation

**Use Experiment B as the foundation** for Boon's production engine because:

1. **Proven Performance**: 4114x faster than Exp A on toggle_all (1000 items)
2. **Conceptual Simplicity**: "Re-evaluate everything, cache aggressively" is easy to understand
3. **Correctness by Design**: No cloning eliminates toggle-all bug class entirely
4. **Smallest Codebase**: 1,622 lines (easiest to maintain)
5. **Working Implementation**: All tests pass, including edge cases

### Incorporate V2 Concepts

While building on Exp B, adopt these V2 ideas:

| V2 Concept | Why Adopt | How |
|------------|-----------|-----|
| **SourceId (structural hash)** | Hot reload without state loss | Add to ExprId computation |
| **NodeAddress (domain routing)** | WebWorker support | Extend ScopeId with domain |
| **Arena allocation** | Better cache locality | Replace FxHashMap with Arena |
| **Delta streams** | Efficient list sync | Add ListDelta to Value |
| **Effect queue (FIFO)** | Consistent side-effect ordering | Add effect queue to Runtime |

### Migration Path

```
Phase 1: Production-ify Exp B
  - Add SourceId for stable identity
  - Add effect queue for side effects
  - Integrate with browser bridge

Phase 2: Optimize Memory
  - Replace FxHashMap<SlotKey, _> with Arena
  - Use SmallVec more aggressively
  - Align nodes to cache lines

Phase 3: Add V2 Features
  - Delta streams for lists
  - Cross-domain routing
  - Full snapshot/restore for hot reload

Phase 4: Hardware Exploration
  - Generate Verilog from subset
  - Identify synthesis constraints
  - Iterate on synthesizable subset
```

### Why Not the Others?

| Architecture | Why Not Primary |
|--------------|-----------------|
| **Old Engine** | Performance issues, debugging nightmares, no hot reload |
| **V2** | Design only, no implementation, unproven |
| **Exp A** | O(n²) performance, multi-pass complexity, cloning overhead |

### Risk Mitigation

**Risk**: Exp B's full re-evaluation might not scale to massive apps.
**Mitigation**: V2's dirty queue can be added as optimization layer on top of Exp B's cache. Start simple, add complexity only when benchmarks demand.

**Risk**: Exp B lacks V2's hardware mental model.
**Mitigation**: Exp B's ScopeId + SlotKey map cleanly to V2's NodeAddress. The fundamental data flow is compatible.

---

## Final Verdict

```
┌─────────────────────────────────────────────────────────────┐
│                                                             │
│   RECOMMENDED: Experiment B + V2 concepts                   │
│                                                             │
│   - Start with Exp B's re-eval + cache model                │
│   - Add V2's SourceId for stable identity                   │
│   - Add V2's delta streams for efficient sync               │
│   - Keep V2's arena design as future optimization           │
│                                                             │
│   Performance: 4114x faster than Exp A on real workloads    │
│   Correctness: Toggle-all bug impossible by design          │
│   Simplicity: 1,622 lines, easy to understand and maintain  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```
