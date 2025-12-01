# SKIP: Stay at Current Value

**Date**: 2025-12-01
**Status**: Implementation Document

---

## Overview

`SKIP` means "don't update, stay at current value". It's used in WHEN/WHILE arms to explicitly indicate no change should occur.

---

## Syntax

```boon
value |> WHEN {
    pattern1 => result1
    pattern2 => SKIP      -- stay at current value
    pattern3 => result3
}
```

---

## Exhaustive WHEN

WHEN must handle all cases. Use SKIP for cases where no update is needed:

```boon
-- Error: Missing False case
flag: signal |> WHEN { True => 1 }

-- OK: All cases handled
flag: signal |> WHEN {
    True => 1
    False => SKIP
}
```

---

## Examples

### In LATEST

```boon
mode: LATEST {
    rst |> WHEN { True => Idle, False => SKIP }
    start |> WHEN { True => Running, False => SKIP }
    stop |> WHEN { True => Stopped, False => SKIP }
}
-- If all evaluate to SKIP → mode stays at current value
```

### In HOLD

```boon
counter: 0 |> HOLD counter {
    control |> WHEN {
        [inc: True] => counter + 1
        [dec: True] => counter - 1
        __ => SKIP  -- or use wildcard with value: __ => counter
    }
}
```

### Hardware Signal Gating

```boon
output: input |> WHEN {
    enable => input
    __ => SKIP  -- hold previous value when disabled
}
```

---

## SKIP vs Wildcard

Two ways to handle "stay at current":

```boon
-- Using SKIP
value |> WHEN {
    pattern => result
    __ => SKIP
}

-- Using wildcard with explicit value
value |> WHEN {
    pattern => result
    __ => value  -- explicitly return current
}
```

**Difference:**
- `SKIP` - semantic: "don't update"
- `__ => value` - explicit: "set to this value" (happens to be current)

---

## See Also

- **WHEN_VS_WHILE.md** - Pattern matching contexts
- **LATEST.md** - Event merging
- **HOLD.md** - Stateful accumulation
