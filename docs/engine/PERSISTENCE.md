# State Persistence in Boon

## Goal

Apps can "unplug from electricity, start again and continue working" - state survives page reloads.

## Design Principles

1. **Store minimal state** - don't bloat localStorage
2. **General rules** - no special cases or hacks
3. **Redundancy is OK for Phase 1** - optimize later

## Current Implementation Status

| Construct | Status | What is Stored | When |
|-----------|--------|---------------|------|
| Variables (primitives) | **Working** | Text, Number, Tag values | Every emission |
| HOLD | **Working** | Accumulated state | Every state change |
| Math/sum | **Working** | Accumulated sum | Every increment |
| LATEST | **Working** | Idempotency keys | Every new input |
| List | **Not Yet** | (needs List-actor persistence) | - |

### Why Lists Don't Persist Yet

Restoring a List from JSON at the variable level creates a NEW List that's disconnected from the reactive chain. For example:

```boon
items: LIST {}
    |> List/append(item: text_to_add)
    |> List/clear(on: clear_button.event.press)
```

The `items` variable holds the result of `List/clear()`, which wraps `List/append()`, which wraps the base `LIST {}`. If we restore items from storage:
1. We create a new List with the stored items
2. But this List isn't connected to `List/append` or `List/clear`
3. New items would go to the original chain, not the restored List

**Solution (Future)**: Persist at the List actor level, not the variable level. The innermost `LIST {}` should save/restore its items directly.

## What Does NOT Persist (By Design)

| Construct | Why |
|-----------|-----|
| Timer/interval | Time-based, should restart fresh |
| Stream/debounce | Timing-based filter |
| Stream/skip/take | Counting-based filter |
| Stream/distinct | Ephemeral deduplication |
| Stream/pulses | Generates fixed sequence |
| WHEN/WHILE/THEN | Pattern matchers, stateless |
| Objects | Contain nested Variables that need code evaluation |

**Element state** is handled by variable persistence - elements are pure display widgets that show variable values.

## How It Works

### Variables (Primitives Only)
- Variables with PersistenceId store Text, Number, Tag outputs
- On reload: load stored value, emit it first, skip first source emission
- Expression still runs, can update if value changes
- Objects and Lists are NOT restored (see above)

### HOLD
- Stores accumulated internal state
- On reload: load stored state instead of piped initial
- Body continues from restored state

### LATEST
- Stores input idempotency keys (not output)
- Skips inputs with previously-seen keys
- Constants have stable keys (PersistenceId)
- Events have unique keys (new each emission)

### Events After Reload
- DOM events don't replay (user didn't click again)
- Variable has stored value from before reload
- If code unchanged, LATEST skips constants (same keys)
- If code changed, LATEST passes new constants (new PersistenceId)

## Storage Location

Uses `ConstructStorage` which wraps browser localStorage with:
- `save_state(persistence_id, &value)` - fire-and-forget
- `load_state(persistence_id).await` - returns Option

## Error Handling

- Try to deserialize stored value
- If fails (corruption, format change): continue as if no stored value exists (graceful degradation)

## Working Examples

| Example | Persistence | Notes |
|---------|-------------|-------|
| counter | Works | Uses Math/sum |
| counter_hold | Works | Uses HOLD |
| todo_mvc | Partial | Completed states persist (HOLD), but todo list itself doesn't |
| shopping_list | Not working | List items don't persist yet |

## Future: Schema Migrations

See `docs/language/storage/OVERVIEW.md` for the future triple store architecture where schema-less storage eliminates migrations entirely. Until then, graceful degradation handles format changes.

## Future Work

### Phase 2: List Persistence
- Implement persistence at the List actor level
- `LIST {}` should save/restore its items directly
- Wrapper operations (List/append, List/clear) forward to inner List

### Phase 3: Optimizations
- Only persist root variables (skip BLOCK internals)
- Only persist referenced variables
- Deduplicate redundant storage
- Compress large values
