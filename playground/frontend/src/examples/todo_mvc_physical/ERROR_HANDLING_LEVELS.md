# BUILD.bn Error Handling - Progressive Levels

**Date:** 2025-01-15
**Status:** Design Exploration (THROW/CATCH is hypothetical)

---

## Overview

This document shows BUILD.bn evolving from no error handling to comprehensive recovery, using the **THROW/CATCH pattern**.

**Files:**
- `BUILD.bn` - Original happy path (no error handling)
- `BUILD_SIMPLE_ERRORS.bn` - Level 2: Fallbacks for read errors
- `BUILD_WITH_ERRORS.bn` - Level 4: Comprehensive error collection

---

## Level 0: Pure Happy Path

**File:** `BUILD.bn`

```boon
FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> Url/encode()
        |> WHEN { encoded =>
            TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }
        }
}
```

**What happens:**
- ✅ All files readable and encodable → Success
- ❌ Any error → Build fails

**Use when:**
- Prototyping
- All assets known to be valid
- Fast iteration

---

## Level 1: Catch-All at Top Level

**Minimal safety net:**

```boon
generation: svg_files
    |> List/map(old, new: icon_code(old))
    |> Text/join_lines()
    |> wrap_in_module()
    |> File/write_text(path: output_file)
    |> CATCH {
        WriteError[message] => BLOCK {
            logged: TEXT { Build failed: {message} } |> Log/error()
            Build/failed()
        }
    }
    |> THEN {
        logged: TEXT { Build complete } |> Log/info()
        Build/success()
    }

-- icon_code unchanged (happy path only)
```

**What happens:**
- ✅ Write succeeds → Success
- ❌ Write fails → Logged, error returned
- ❌ Any icon processing error → Bubbles up uncaught

**Use when:**
- Need to catch catastrophic failures
- Don't care about individual icon errors yet

---

## Level 2: Fallback Icons (Simple Recovery)

**File:** `BUILD_SIMPLE_ERRORS.bn`

```boon
FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> WHEN {
            ReadError[message] => BLOCK {
                logged: TEXT { Warning: Cannot read {item.path}: {message} }
                    |> Log/warn()
                TEXT { <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/></svg> }
            }
            text => text
        }
        |> Url/encode()
        |> WHEN { encoded =>
            TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }
        }
}
```

**What happens:**
- ✅ Read succeeds → Normal icon
- ⚠️ Read fails → Warning logged, fallback circle icon used
- ✅ Build always completes with some icons

**Use when:**
- Want graceful degradation
- Some icons optional
- User experience matters (show something rather than nothing)

---

## Level 3: Explicit Error Throwing

**More control over error propagation:**

```boon
FUNCTION icon_code(item) {
    item.path
        |> File/read_text()
        |> WHEN {
            ReadError[message] => THROW { IconError[
                stage: TEXT { read }
                path: item.path
                reason: message
            ] }
            text => text
        }
        |> Url/encode()
        |> WHEN {
            EncodeError[message] => THROW { IconError[
                stage: TEXT { encode }
                path: item.path
                reason: message
            ] }
            encoded => encoded
        }
        |> WHEN { encoded =>
            TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }
        }
        |> CATCH {
            IconError[stage, path, reason] => BLOCK {
                logged: TEXT { {stage} failed for {path}: {reason} } |> Log/error()
                TEXT { -- ERROR in icon processing }
            }
        }
}
```

**What happens:**
- Errors transformed into IconError with rich context
- CATCH converts to Error tag
- Caller can collect and display errors

**Use when:**
- Need structured error information
- Want to aggregate errors for reporting
- Multiple error sources to distinguish

---

## Level 4: Comprehensive Error Collection

**File:** `BUILD_WITH_ERRORS.bn`

**Full statistics and error reporting:**

```boon
generation: svg_files
    |> List/map(old, new: icon_code(old))
    |> collect_results()
    |> WHEN {
        [successes, errors] => BLOCK {
            -- Log statistics
            success_count: successes |> List/count()
            error_count: errors |> List/count()

            stats_logged: TEXT { Icons: {success_count} succeeded, {error_count} failed }
                |> Log/info()

            -- Log individual errors
            errors_logged: errors
                |> List/each(error, do: error |> Log/error())

            -- Generate module with only successes
            module: successes
                |> Text/join_lines()
                |> wrap_in_module()

            -- Write to file
            module
                |> File/write_text(path: output_file)
                |> WHEN {
                    WriteError[message] => BLOCK {
                        logged: TEXT { FATAL: Cannot write {output_file}: {message} }
                            |> Log/error()
                        THROW { WriteError[message: message] }
                    }
                    __ => Build/success()
                }
        }
    }
```

**What happens:**
- All icons processed (no early exit)
- Successes and errors collected separately
- Statistics logged
- Build succeeds with partial results
- Only fatal error (write failure) stops build

**Use when:**
- Production builds
- Need full error reporting
- Want to see all problems at once
- Partial success is acceptable

---

## Level 5: Retry + Circuit Breaker (Advanced)

**Resilient to transient failures:**

```boon
FUNCTION icon_code_resilient(item) {
    item.path
        |> read_with_retry(attempts: 3, backoff: Exponential)
        |> WHEN {
            ReadError[message] => BLOCK {
                logged: TEXT { Failed after retries: {item.path} } |> Log/warn()
                TEXT { <svg>...</svg> }  -- Fallback
            }
            text => text
        }
        |> Url/encode()
        |> WHEN { encoded =>
            TEXT { {item.file_stem}: data:image/svg+xml;utf8,{encoded} }
        }
}

FUNCTION read_with_retry(path, attempts, backoff) {
    path
        |> File/read_text()
        |> WHEN {
            ReadError[message] if attempts > 1 => BLOCK {
                delay: backoff |> calculate_delay(attempt: 4 - attempts)
                slept: sleep(delay)
                read_with_retry(path, attempts: attempts - 1, backoff: backoff)
            }
            result => result
        }
}
```

**What happens:**
- Transient errors (file busy) → Retry with exponential backoff
- Persistent errors → Fallback icon
- Network hiccups tolerated
- Build extremely resilient

**Use when:**
- Network file systems
- High reliability requirements
- Concurrent builds possible
- CI/CD environments

---

## Comparison Table

| Level | Description | Successes | Failures | Build Result | Use Case |
|-------|-------------|-----------|----------|--------------|----------|
| **0** | Happy path | All work | Build fails | Fail | Prototyping |
| **1** | Top-level catch | All work | Logged | Fail | Basic safety |
| **2** | Fallback icons | All work | Fallback used | Success | Graceful degradation |
| **3** | Error collection | All work | Collected | Success | Error reporting |
| **4** | Comprehensive | Partial | Full report | Success | Production |
| **5** | Resilient | Partial | Retry then fallback | Success | High reliability |

---

## BLOCK Usage Examples

### ✅ Correct: Variable Bindings + Final Expression

```boon
BLOCK {
    logged: message |> Log/error()
    count: items |> List/count()
    count  -- Final expression (return value)
}
```

### ✅ Correct: Side Effect Then Return

```boon
BLOCK {
    stats_logged: TEXT { Success: {count} } |> Log/info()
    errors_logged: errors |> List/each(e, do: e |> Log/error())
    Build/success()  -- Final expression
}
```

### ❌ Wrong: Sequential Statements

```boon
BLOCK {
    Log/error(message)  -- ERROR: Not a variable binding
    THROW { Error[msg] }
}
```

### ✅ Correct: Use THEN Instead

```boon
message |> Log/error()
    |> THEN { THROW { Error[msg] } }
```

---

## THROW/CATCH Patterns

### Pattern A: Immediate Handling

```boon
item.path
    |> File/read_text()
    |> WHEN {
        ReadError[message] => fallback_value  -- Handle immediately
        text => text
    }
```

**No THROW needed - error handled inline.**

### Pattern B: Throw to Outer Handler

```boon
item.path
    |> File/read_text()
    |> WHEN {
        ReadError[message] => THROW { ReadError[message] }
        text => text
    }
    |> process()
    |> CATCH {
        ReadError[message] => handle_error(message)
    }
```

**THROW bubbles up to nearest CATCH.**

### Pattern C: Transform and Re-throw

```boon
item.path
    |> File/read_text()
    |> WHEN {
        ReadError[message] => THROW { BuildError[
            stage: TEXT { read }
            original: message
        ] }
        text => text
    }
    |> CATCH {
        BuildError[stage, original] => BLOCK {
            logged: TEXT { {stage}: {original} } |> Log/error()
            THROW { BuildError[stage: stage, original: original] }
        }
    }
```

**CATCH can log then re-throw for higher-level handlers.**

---

## Recommendations

**For BUILD.bn specifically:**

1. **Start:** Level 0 (happy path) - Get it working
2. **Add:** Level 2 (fallback icons) - Better UX
3. **Production:** Level 4 (comprehensive) - Full visibility
4. **CI/CD:** Level 5 (resilient) - Handle transient issues

**General guidelines:**

- Use **Level 0-1** for prototypes
- Use **Level 2-3** for user-facing apps
- Use **Level 4-5** for production systems

**Key principle:** **Gradual error handling** - Start simple, add sophistication only where needed.

---

**Note:** THROW/CATCH syntax is currently hypothetical. This document serves as a design exploration for Boon's error handling evolution.

**Last Updated:** 2025-01-15
