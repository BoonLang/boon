# Code Optimizations Analysis

**Date:** 2025-11-12
**Purpose:** Identify opportunities to simplify and improve todo_mvc_physical codebase

This document analyzes the current codebase to find optimization opportunities. We focus on simplification and essential patterns, only using new language features where they genuinely add value.

---

## Optimization 1: Focus Spotlight - Use Theme System (Simplified)

**Current Code (RUN.bn:161-167):**
```boon
lights: Theme/lights()
    |> List/append(
        Light/spot(
            target: FocusedElement,
            color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220],
            intensity: 0.3,
            radius: 60,
            falloff: Gaussian
        )
    )
```

**Problem:**
- Hardcoded light properties in app code
- Not theme-aware (Professional vs Neobrutalism want different softness)
- Manual parameter management

**Optimized Code:**
```boon
lights: Theme/lights()
    |> List/append(
        Theme/light(of: FocusSpotlight)
    )
```

**That's it!** No overrides, no optional parameters, just semantic types.

**Implementation (Theme/Theme.bn - add new function):**
```boon
FUNCTION light(of) {
    PASSED.theme_options.name |> WHEN {
        Professional => Professional/light(of: of)
        Glassmorphism => Glassmorphism/light(of: of)
        Neobrutalism => Neobrutalism/light(of: of)
        Neumorphism => Neumorphism/light(of: of)
    }
}
```

**Implementation (Theme/Professional.bn - add new function):**
```boon
FUNCTION light(of) {
    of |> WHEN {
        FocusSpotlight => Light/spot(
            target: FocusedElement,
            color: PASSED.mode |> WHEN {
                Light => Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
                Dark => Oklch[lightness: 0.8, chroma: 0.12, hue: 220]
            },
            intensity: 0.3,
            radius: 60,
            softness: 0.85
        )
    }
}
```

**Implementation (Theme/Neobrutalism.bn - different defaults):**
```boon
FUNCTION light(of) {
    of |> WHEN {
        FocusSpotlight => Light/spot(
            target: FocusedElement,
            color: PASSED.mode |> WHEN {
                Light => Oklch[lightness: 0.9, chroma: 0.15, hue: 220]
                Dark => Oklch[lightness: 0.85, chroma: 0.18, hue: 220]
            },
            intensity: 0.5,
            radius: 40,
            softness: 0.1  -- Much sharper!
        )
    }
}
```

**Benefits:**
- ✅ Simple: Just semantic types, no configuration
- ✅ Theme-aware: Each theme defines its own interpretation
- ✅ Clean separation: Theme API for common cases, raw `Light/spot()` for custom
- ✅ Consistent: Follows semantic type pattern
- ✅ One line instead of eight

**Note:** If users need custom light properties, they use `Light/spot()` directly. Theme API is for semantic, themed lights only.

---

## Optimization 2: Material System

**Current Pattern (Theme/Professional.bn:80-99):**
```boon
InputInterior[focus] => [
    color: PASSED.mode |> WHEN {
        Light => Oklch[lightness: 1]
        Dark => Oklch[lightness: 0.15]
    }
    gloss: focus |> WHEN {
        False => 0.65
        True => 0.15
    }
    glow: focus |> WHEN {
        True => [...]
        False => None
    }
]
```

**Analysis:** ✅ **Already optimal**

The material system correctly uses:
- Tagged object fields for reactive state (`focus`, `hover`, `press`)
- Pattern matching to handle different states
- Theme provides semantic materials

**If users need custom materials:**
- Create new material tag: `CustomButton`, not override existing
- Or define custom record directly (bypass theme)

**Status:** ✅ No changes needed - current design is correct

---

## Optimization 3: Pattern Matching - Use Partial Matching

**Current Pattern (Theme/Professional.bn:5-6):**
```boon
FUNCTION material(material) {
    material |> WHEN {
        Background => [...]
        Panel => [...]
        InputInterior[focus] => [...]
        Button[hover, press] => [...]
        ButtonEmphasis[hover, press] => [...]
        ButtonDelete[hover] => [...]
        ...
    }
}
```

**Already Optimal!** ✅

The current code already uses the pattern we want:
- Bare tags for simple materials: `Background`, `Panel`
- Tagged objects with explicit fields: `InputInterior[focus]`, `Button[hover, press]`

With our new **partial pattern matching**, if someone calls:
```boon
Theme/material(of: Button[hover: True])
```

It will match `Button[hover, press]` and bind `hover: True`, `press: UNPLUGGED`.

Then inside the pattern body, we could access:
```boon
Button[hover, press] => BLOCK {
    -- Future: Could access other fields if needed
    custom_glow: material.glow? |> WHEN {
        UNPLUGGED => None
        g => g
    }

    [
        color: ...,
        gloss: ...,
        glow: custom_glow
    ]
}
```

**Status:** ✅ Already using best pattern - no changes needed

---

## Optimization 4: Router Pattern - Already Using LATEST Correctly

**Current Code (RUN.bn:43-49):**
```boon
go_to_result:
    LATEST {
        filter_buttons.all.event.press |> THEN { '/' }
        filter_buttons.active.event.press |> THEN { '/active' }
        filter_buttons.completed.event.press |> THEN { '/completed' }
    }
    |> Router/go_to()
```

**Analysis:** ✅ **Correct usage of LATEST**

This is temporal reactive routing:
- Waiting for button press events (temporal)
- Each event arrives at different times
- Latest event wins

**Not** a case for UNPLUGGED because:
- Not accessing optional fields
- Not structural alternatives
- This is genuinely "most recent event"

**Status:** ✅ Perfect - no changes needed

---

## Optimization 5: Theme Router - Could Use Helper Function

**Current Code (Theme/Theme.bn:11-18):**
```boon
FUNCTION material(of) {
    PASSED.theme_options.name |> WHEN {
        Professional => of |> Professional/material()
        Glassmorphism => of |> Glassmorphism/material()
        Neobrutalism => of |> Neobrutalism/material()
        Neumorphism => of |> Neumorphism/material()
    }
}

-- Repeated for every function: font, border, depth, elevation, corners, etc.
```

**Opportunity: DRY with Helper**

**Problem:** Repetitive routing pattern for 20+ functions

**Potential Solution (if Boon had higher-order functions):**
```boon
-- CANNOT DO THIS - Functions not first-class in Boon!
FUNCTION route(fn) {
    PASSED.theme_options.name |> WHEN {
        Professional => Professional/fn
        Glassmorphism => Glassmorphism/fn
        Neobrutalism => Neobrutalism/fn
        Neumorphism => Neumorphism/fn
    }
}
```

**Analysis:**
- Cannot reduce repetition without first-class functions
- Could use code generation/macros (outside Boon)
- Repetition is explicit and clear (not necessarily bad)

**Status:** ⚠️ Low priority - explicit is fine, consider codegen if it grows

---

## Optimization 6: User Configuration (Future Consideration)

**Potential Use Case:** User preferences with graceful fallbacks

When this becomes needed, UNPLUGGED would be appropriate:

```boon
-- User provides optional configuration
user_prefs: load_user_preferences()

-- Graceful fallback using UNPLUGGED
font_size: user_prefs.font_size? |> WHEN {
    UNPLUGGED => 14  -- Default
    size => size
}

theme_name: user_prefs.theme? |> WHEN {
    UNPLUGGED => Professional  -- Default theme
    name => name
}
```

**When to implement:**
- Only if user customization is actually needed
- Keep it simple: flat preferences, not nested
- Use UNPLUGGED for truly optional config fields

**Status:** 💡 Not needed yet - wait for real use case

---

## Summary of Optimizations

| # | Optimization | Priority | Status | Impact |
|---|--------------|----------|--------|--------|
| 1 | **Theme-aware light system** | 🔥 HIGH | TODO | High - cleaner API, theme consistency |
| 2 | Material system | ✅ DONE | Already optimal | N/A - current design is correct |
| 3 | Pattern matching | ✅ DONE | Already optimal | N/A - already using best pattern |
| 4 | Router LATEST usage | ✅ DONE | Already optimal | N/A - correct usage |
| 5 | Theme router DRY | ⚠️ LOW | Consider codegen | Low - explicit is clear |
| 6 | User config | 💡 FUTURE | Wait for real need | Medium - when customization needed |

---

## Recommended Actions

### Immediate (High Priority)

1. **Add `Theme/light()` function** to theme system
   - Simple semantic types: `Theme/light(of: FocusSpotlight)`
   - No overrides, no optional parameters
   - Implement in `Theme/Theme.bn`
   - Implement in all 4 themes with their specific defaults
   - Update `RUN.bn` to use new API

2. **Rename `falloff` → `softness`** in Light API (optional)
   - More user-friendly name
   - Numeric 0.0-1.0 range

### Future (Only If Needed)

3. **User preferences** - Add when customization is actually requested
4. **Theme router codegen** - Only if manual repetition becomes painful

---

## Code Changes Required

### File: `RUN.bn` (lines 159-168)

**Before (8 lines, hardcoded):**
```boon
lights: Theme/lights()
    |> List/append(
        Light/spot(
            target: FocusedElement,
            color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220],
            intensity: 0.3,
            radius: 60,
            falloff: Gaussian
        )
    )
```

**After (3 lines, themed):**
```boon
lights: Theme/lights()
    |> List/append(Theme/light(of: FocusSpotlight))
```

### File: `Theme/Theme.bn` (add new function)

```boon
FUNCTION light(of) {
    PASSED.theme_options.name |> WHEN {
        Professional => Professional/light(of: of)
        Glassmorphism => Glassmorphism/light(of: of)
        Neobrutalism => Neobrutalism/light(of: of)
        Neumorphism => Neumorphism/light(of: of)
    }
}
```

### Files: `Theme/Professional.bn`, `Theme/Glassmorphism.bn`, `Theme/Neobrutalism.bn`, `Theme/Neumorphism.bn`

Add to each theme file (example from Professional):

```boon
FUNCTION light(of) {
    of |> WHEN {
        FocusSpotlight => Light/spot(
            target: FocusedElement,
            color: PASSED.mode |> WHEN {
                Light => Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
                Dark => Oklch[lightness: 0.8, chroma: 0.12, hue: 220]
            },
            intensity: 0.3,
            radius: 60,
            softness: 0.85
        )
    }
}
```

Each theme defines its own interpretation (Neobrutalism uses `softness: 0.1`, etc.)

---

## Design Philosophy

**Key Principle: Simplicity First**

- Only introduce complexity when it solves a real problem
- New language features (UNPLUGGED, partial matching) are tools, not goals
- Theme API should use semantic types, not configuration objects
- If users need customization, they can use low-level APIs directly
- Explicit is better than clever

**For this codebase:**
- ✅ Theme-aware semantic lights: Solves real problem (hardcoded values)
- ❌ Optional parameters for lights: Over-engineering, no clear need
- ❌ Material overrides: Edge case, better solved with new material types
- ✅ Current patterns: Already optimal, no changes needed

**Remember:** The best code is code you don't write. 🎯

---

**Last Updated:** 2025-11-12
**Next Review:** After implementing `Theme/light()` system
