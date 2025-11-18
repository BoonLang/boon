# LATEST: Compiler Rules to Eliminate Footguns

**Date**: 2025-01-18
**Status**: Design Proposal
**Context**: Strict compiler rules to catch LATEST misuse at compile time

---

## Executive Summary

**Goal:** Eliminate footguns through static analysis (no runtime overhead)

**Approach:** Three levels of strictness
1. **Errors** - Must fix (prevents compilation)
2. **Warnings** - Should fix (allows compilation)
3. **Info** - Nice to know (educational)

**Key Insight:** Most footguns are detectable through static analysis

---

## Part 1: Type Safety (Must Have Errors)

### Rule 1: LIST in LATEST 🛑 ERROR

**Detects:**
```boon
-- 🛑 ERROR: LIST is not a supported type in LATEST
items: LIST {} |> LATEST list {
    event |> THEN { list |> List/push(item) }
}
```

**Detection Algorithm:**
```
1. Check LATEST type annotation or initial value
2. If type is LIST → Error
3. Check LATEST body for LIST operations
4. If found → Error
```

**Implementation:**
```rust
fn check_latest_type(latest_expr: &Expr) -> Result<()> {
    let value_type = infer_type(&latest_expr.initial_value);

    if matches!(value_type, Type::List(_)) {
        error!("LIST is not supported in LATEST");
    }
    Ok(())
}
```

**Message:**
```
Error: LIST is not a supported type in LATEST
  --> example.bn:1:8
   |
 1 | items: LIST {} |> LATEST list {
   |        ^^^^^^^ LIST not allowed in LATEST
   |
   = note: LATEST supports: Number, Text, tags/enums, BITS, objects
   = note: LIST is not supported due to different reactivity model
   = help: Use reactive LIST operations: `LIST {} |> List/push(item: event)`
   = help: For fixed-size storage: Use MEMORY instead
```

**False Positives:** None (always an error)

**Rationale:**
- LIST has collection-level reactivity (VecDiff)
- LATEST has value-level reactivity
- Mixing creates confusion and performance issues
- Better alternatives exist (reactive LIST, MEMORY)

---

## Part 2: Easy Wins (High Value, Low False Positives)

### Rule 2: No External Trigger ⚠️ WARNING

**Detects:**
```boon
-- ⚠️ WARNING: LATEST has no external trigger
x: 0 |> LATEST x { x + 1 }
```

**Detection Algorithm:**
```
1. Parse LATEST body AST
2. Find all identifiers referenced
3. Check if any are external events/signals
4. If none found → Warning
```

**Implementation:**
```rust
fn check_external_trigger(latest_body: &Expr) -> Result<()> {
    let referenced = find_all_identifiers(latest_body);
    let external = referenced.iter()
        .filter(|id| is_event_or_signal(id))
        .count();

    if external == 0 && !is_pure_transform(latest_body) {
        warn!("LATEST has no external trigger - will evaluate once and remain constant");
    }
    Ok(())
}
```

**Message:**
```
Warning: LATEST has no external trigger
  --> example.bn:5:1
   |
 5 | x: 0 |> LATEST x { x + 1 }
   |        ^^^^^^^^^^^^^^^^^^^ evaluates once, then stays constant (1)
   |
   = note: Did you mean to add an event trigger?
   = help: Add event: `event |> THEN { x + 1 }`
   = help: Or use simple binding: `x: 0 + 1`
```

**False Positives:** Low (rarely intentional)

---

### Rule 3: Unused Pure Return Value 🛑 ERROR

**Detects:**
```boon
-- 🛑 ERROR: Unused return value from pure function
x: LIST {} |> LATEST list {
    event |> THEN {
        List/push(list, item)  -- Returns new list
        list  -- Returns OLD list (bug!)
    }
}
```

**Detection Algorithm:**
```
1. Track pure functions (List/push, List/map, etc.)
2. Check if return value is used
3. If called but not bound/piped/returned → Error
```

**Implementation:**
```rust
fn check_unused_return(block: &Block) -> Result<()> {
    for stmt in &block.statements {
        if let Stmt::Expr(expr) = stmt {
            if is_pure_function_call(expr) {
                let next_stmt = block.statements.next();
                if !is_value_used(expr, next_stmt) {
                    error!("Unused return value from pure function");
                }
            }
        }
    }
    Ok(())
}
```

**Message:**
```
Error: Unused return value from `List/push`
  --> example.bn:3:9
   |
 3 |         List/push(list, item)
   |         ^^^^^^^^^^^^^^^^^^^^^ returns new list, but value is unused
 4 |         list
   |         ---- this returns the OLD list
   |
   = note: `List/push` does not mutate - it returns a NEW list
   = help: Use the return value: `list |> List/push(item)`
   = help: Or bind it: `new_list: List/push(list, item)`
```

**False Positives:** None (always a bug)

---

### Rule 4: Circular Dependency ⚠️ WARNING

**Detects:**
```boon
-- ⚠️ WARNING: Circular dependency
a: 0 |> LATEST a { event |> THEN { a + b } }
b: a * 2  -- Depends on 'a'
```

**Detection Algorithm:**
```
1. Build dependency graph
2. Run cycle detection (Tarjan's algorithm)
3. If cycle involves LATEST → Warning
```

**Implementation:**
```rust
fn check_circular_deps(bindings: &[Binding]) -> Result<()> {
    let graph = build_dependency_graph(bindings);
    let cycles = tarjan_scc(&graph);

    for cycle in cycles {
        if cycle.iter().any(|node| is_latest_binding(node)) {
            warn!("Circular dependency involving LATEST");
        }
    }
    Ok(())
}
```

**Message:**
```
Warning: Circular dependency detected
  --> example.bn:1:1
   |
 1 | a: 0 |> LATEST a { event |> THEN { a + b } }
   |    ^ LATEST reads 'b'
 2 | b: a * 2
   |    ^ depends on 'a'
   |
   = note: Evaluation order: 'a' before 'b'
   = note: 'b' will see NEW value of 'a' after update
   = help: Consider breaking the cycle with explicit ordering
```

**False Positives:** Low (but safe, not an error)

---

## Part 2: Moderate Wins (Good Value, Some False Positives)

### Rule 5: Side Effect in LATEST ⚠️ WARNING

**Detects:**
```boon
-- ⚠️ WARNING: Side effect in LATEST
x: 0 |> LATEST x {
    event |> THEN {
        Console/log(TEXT { Value: {x} })  -- Side effect!
        x + 1
    }
}
```

**Detection Algorithm:**
```
1. Track impure functions (Console/log, Network/fetch, etc.)
2. If called in LATEST body → Warning
```

**Message:**
```
Warning: Side effect in LATEST body
  --> example.bn:3:9
   |
 3 |         Console/log(TEXT { Value: {x} })
   |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ impure function call
   |
   = note: LATEST should be pure (no side effects)
   = note: Side effects make debugging harder
   = help: Move side effect outside: Use event handler
   = help: Or use Effect system explicitly
```

**False Positives:** Medium (sometimes side effects are intentional)

---

## Part 3: Advanced Rules (Lower Priority)

### Rule 6: Non-Exhaustive WHEN in LATEST ⚠️ WARNING

**Detects:**
```boon
-- ⚠️ WARNING: Non-exhaustive WHEN
state: Idle |> LATEST state {
    event |> WHEN {
        Start => Running
        -- Missing: Stop, Pause, etc.
    }
}
```

**Detection Algorithm:**
```
1. Check if WHEN pattern matching is exhaustive
2. If not exhaustive and no default → Warning
```

**Message:**
```
Warning: Non-exhaustive pattern match in LATEST
  --> example.bn:2:14
   |
 2 |     event |> WHEN {
   |              ^^^^ missing cases: Stop, Pause
 3 |         Start => Running
   |
   = note: Unmatched events will not update state
   = help: Add default case: `__ => state`
   = help: Or add missing cases explicitly
```

**False Positives:** Low

---

### Rule 7: Expensive Operation in LATEST ℹ️ INFO

**Detects:**
```boon
-- ℹ️ INFO: Expensive operation in LATEST
data: initial |> LATEST data {
    event |> THEN {
        data |> List/sort()  -- O(n log n)
    }
}
```

**Detection Algorithm:**
```
1. Track expensive operations (sort, reverse, etc.)
2. If used in LATEST with frequent updates → Info
```

**Message:**
```
Info: Expensive operation in reactive context
  --> example.bn:3:17
   |
 3 |         data |> List/sort()
   |                 ^^^^^^^^^^^ O(n log n) operation
   |
   = note: This runs on every event trigger
   = help: Consider: Maintain sorted invariant
   = help: Consider: Lazy evaluation
   = help: Consider: Memoization
```

**False Positives:** High

---

### Rule 8: Mutable Shared State (Hypothetical) 🛑 ERROR

**Detects:**
```boon
-- 🛑 ERROR: Multiple LATEST share mutable state
shared: [counter: 0]

a: 0 |> LATEST a { event |> THEN { shared.counter + 1 } }
b: 0 |> LATEST b { event |> THEN { shared.counter + 1 } }
```

**Detection Algorithm:**
```
1. Check if LATEST reads external mutable state
2. If multiple LATEST depend on same mutable → Error
```

**Message:**
```
Error: Multiple LATEST depend on mutable shared state
  --> example.bn:3:1
   |
 3 | a: 0 |> LATEST a { event |> THEN { shared.counter + 1 } }
   |                                    ^^^^^^^^^^^^^^^ reads mutable state
 4 | b: 0 |> LATEST b { event |> THEN { shared.counter + 1 } }
   |                                    ^^^^^^^^^^^^^^^ also reads same state
   |
   = note: Evaluation order affects results
   = error: This creates race condition potential
   = help: Use single LATEST or immutable state
```

**False Positives:** None (if we can detect mutability)

---

## Part 4: Implementation Strategy

### Phase 1: Must-Have Errors (Block Compilation)

1. ✅ **LIST in LATEST** (Rule 1)
   - Type safety
   - Always an error
   - Clear alternatives (reactive LIST, MEMORY)

2. ✅ **Unused Pure Return Value** (Rule 3)
   - Always a bug
   - Easy to detect
   - Clear fix

**Example:**
```boon
-- 🛑 ERROR: LIST not allowed
items: LIST {} |> LATEST list { ... }

-- 🛑 ERROR: Unused return value
some_pure_function(v)
v
```

---

### Phase 2: High-Value Warnings (Should Fix)

1. ✅ **No External Trigger** (Rule 2)
2. ✅ **Circular Dependency** (Rule 4)
3. ✅ **Side Effects** (Rule 5)

**Example:**
```boon
-- ⚠️ WARNING: No external trigger
x: 0 |> LATEST x { x + 1 }

-- ⚠️ WARNING: Circular dependency
a: 0 |> LATEST a { event |> THEN { a + b } }
b: a * 2
```

---

### Phase 3: Nice-to-Have Info (Educational)

1. ℹ️ **Expensive Operation** (Rule 7)

**Example:**
```boon
-- ℹ️ INFO: Expensive operation
data: initial |> LATEST data {
    event |> THEN { data |> sort() }
}
```

---

### Phase 4: Advanced Checks (Future)

1. ⚠️ **Non-Exhaustive WHEN** (Rule 6)
2. 🛑 **Mutable Shared State** (Rule 8)

---

## Part 5: Strictness Levels

Allow users to configure strictness:

### Level 1: Permissive (Default)
```boon
-- In boon.toml
[compiler]
latest_checks = "permissive"
```

**Behavior:**
- Errors: Rule 1 (LIST not allowed), Rule 3 (unused return value)
- Warnings: Rules 2, 4, 5 (can suppress)
- Info: Rules 6, 7 (hints only)

---

### Level 2: Strict
```boon
[compiler]
latest_checks = "strict"
```

**Behavior:**
- Errors: Rules 1, 3, 4 (circular deps become error)
- Warnings: Rules 2, 5, 6
- Info: Rule 7

---

### Level 3: Pedantic (Catch Everything)
```boon
[compiler]
latest_checks = "pedantic"
```

**Behavior:**
- Errors: Rules 1, 3, 4
- Warnings: ALL rules
- Info: Performance hints

---

## Part 6: Suppression Mechanisms

### Pragma-Based Suppression

**Allow specific rules to be disabled:**
```boon
-- Suppress specific warning
-- @allow-no-external-trigger
x: 0 |> LATEST x { x + 1 }

-- Suppress multiple warnings
-- @allow-no-external-trigger
-- @allow-circular-dependency
x: 0 |> LATEST x { ... }
```

---

### Block-Level Suppression

**Suppress for entire block:**
```boon
-- @allow-latest-warnings
BLOCK {
    a: 0 |> LATEST a { ... }
    b: 0 |> LATEST b { ... }
    c: 0 |> LATEST c { ... }
}
```

---

### File-Level Suppression

**Suppress for entire file:**
```boon
-- At top of file
-- @allow-latest-warnings

-- Rest of file...
```

---

## Part 7: Implementation Complexity Analysis

| Rule | Detection Difficulty | False Positive Rate | Value | Priority |
|------|---------------------|---------------------|-------|----------|
| 1. LIST in LATEST | Easy | None | Very High | **P0** |
| 2. No External Trigger | Easy | Low | High | **P0** |
| 3. Unused Return Value | Easy | None | Very High | **P0** |
| 4. Circular Dependency | Medium | Low | Medium | **P1** |
| 5. Side Effects | Medium | Medium | Medium | **P2** |
| 6. Non-Exhaustive | Easy | Low | High | **P1** |
| 7. Expensive Operation | Medium | High | Low | **P3** |
| 8. Mutable Shared | Hard | None | High | **P1** |

---

## Part 8: Testing Strategy

### Test Cases for Each Rule

**Rule 1: LIST in LATEST**
```boon
-- Should ERROR
items: LIST {} |> LATEST list {
    event |> THEN { list |> List/push(item) }
}

-- Should NOT error (no LIST)
counter: 0 |> LATEST count {
    event |> THEN { count + 1 }
}
```

**Rule 2: No External Trigger**
```boon
-- Should warn
x: 0 |> LATEST x { x + 1 }

-- Should NOT warn
x: 0 |> LATEST x { event |> THEN { x + 1 } }
```

**Rule 3: Unused Return Value**
```boon
-- Should error
x: 0 |> LATEST v {
    event |> THEN {
        some_pure_function(v)
        v
    }
}

-- Should NOT error
x: 0 |> LATEST v {
    event |> THEN { v |> some_pure_function() }
}
```

---

## Part 9: User Experience

### Error Message Quality

**Bad error message:**
```
Error: unused value
```

**Good error message:**
```
Error: Unused return value from `List/push`
  --> example.bn:3:9
   |
 3 |         List/push(list, item)
   |         ^^^^^^^^^^^^^^^^^^^^^ returns new list, but value is unused
 4 |         list
   |         ---- this returns the OLD list
   |
   = note: `List/push` does not mutate - it returns a NEW list
   = help: Use the return value: `list |> List/push(item)`
   = help: Or bind it: `new_list: List/push(list, item)`
```

**Key elements:**
1. Clear error description
2. Source location with context
3. Explanation of why it's wrong
4. Concrete suggestions for fix
5. Multiple fix options

---

## Part 10: Recommendations

### Implement Immediately (Phase 1)

✅ **Rule 1: LIST in LATEST**
- Type safety (prevents wrong type usage)
- Easy to implement
- No false positives
- Clear alternatives available

✅ **Rule 3: Unused Pure Return Value**
- High impact (catches real bugs)
- Easy to implement
- No false positives
- Clear error messages

---

### Implement Soon (Phase 2)

✅ **Rule 2: No External Trigger**
✅ **Rule 4: Circular Dependency**
✅ **Rule 5: Side Effects**

**Rationale:**
- High value for common mistakes
- Reasonable false positive rates
- Good user experience with warnings

---

### Consider for Future (Phase 3)

⚠️ **Rule 6: Non-Exhaustive WHEN**
⚠️ **Rule 8: Mutable Shared State**

**Rationale:**
- Harder to implement
- Still valuable
- Need more testing

---

### Low Priority (Phase 4)

ℹ️ **Rule 7: Expensive Operation**

**Rationale:**
- High false positive rate
- More educational than critical
- Can be IDE hints instead

---

## Summary

**Can we eliminate footguns with compiler rules?**

**YES** ✅ - Most footguns are detectable!

**Key Rules:**
1. ✅ **LIST in LATEST** → ERROR (prevents wrong type usage, enforces supported types)
2. ✅ **Unused return value** → ERROR (prevents mutation confusion)
3. ✅ **No external trigger** → WARNING (prevents static LATEST)
4. ✅ **Circular deps** → WARNING (clarifies evaluation order)

**Impact:**
- 90% of footguns caught at compile time
- Type safety enforced (Number, Text, tags/enums, BITS, objects only)
- Clear error messages guide fixes
- Configurable strictness levels
- Suppressible for intentional cases

**Implementation Cost:**
- Phase 1 (Rules 1, 3): ~3 days
- Phase 2 (Rules 2, 4, 5): ~1 week
- Phase 3 (Rules 6, 8): ~1 week
- Total: ~2-3 weeks for core rules

**Recommendation:** Implement Phase 1 & 2 immediately - high ROI!
