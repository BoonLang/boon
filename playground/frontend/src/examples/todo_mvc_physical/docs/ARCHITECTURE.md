# TodoMVC Physical 3D - Architecture Notes

## Design System Architecture

### Semantic Design Tokens (Theme Layer)

**Problem Solved:** Original themes were coupled to TodoMVC components (`ButtonDelete`, `TodoCheckbox`, etc.), preventing reuse across different applications.

**Solution:** Migrated to pure semantic scales that work with any application:

**Material Scales (9):**
- `Background` - Page background
- `Surface` - Generic surfaces
- `SurfaceVariant` - Subtle surface variant
- `SurfaceElevated` - Elevated/floating surfaces
- `Interactive[hovered]` - Interactive elements
- `InteractiveRecessed[focus]` - Recessed inputs with focus states
- `Primary` - Brand color
- `PrimarySubtle` - Subtle brand background
- `Danger` - Destructive/error color

**Font Scales (9):**
- `Hero`, `Body`, `BodySecondary`, `BodyDisabled`, `BodyDanger`
- `Input`, `Placeholder`, `ButtonIcon[checked]`
- `Small`, `SmallLink[hovered]`

**Text Scales (5):** Font + 3D properties
- `Hero`, `BodySecondary`, `ButtonIcon[checked]`, `Small`, `SmallLink[hovered]`

**Other Functions:** `depth()`, `elevation()`, `corners()`, `lights()`, `geometry()`, `sizing()`, `spacing()`, `spring_range()`

### App-Specific Composition (RUN.bn Layer)

Complex component styles are composed from semantic primitives:

```bn
-- RUN.bn - COMPONENT STYLES section
FUNCTION delete_button_material(hovered) {
    Theme/material(of: SurfaceElevated) |> MERGE [
        glow: hovered |> WHEN {
            True => [
                color: Theme/material(of: Danger).color
                intensity: 0.08
            ]
            False => None
        }
    ]
}
```

**Benefits:**
- ✅ Themes are reusable across any app
- ✅ App-specific logic stays in the app
- ✅ Clear separation of concerns
- ✅ Easy theme switching (just change import)

### Migration Impact

**File Size Reduction:**
- Professional.bn: 546 → 318 lines (-42%)
- Neobrutalism.bn: 488 → 321 lines (-34%)
- Glassmorphism.bn: 527 → 331 lines (-37%)
- Neumorphism.bn: 497 → 321 lines (-35%)
- Theme.bn: 290 → 20 lines (-93%)

**Total: 2,348 → 1,311 lines (-44%)**

---

## Theme Routing Pattern

### The `get()` Function Pattern

**Theme.bn (Router):**
```bn
FUNCTION material(of) { get(from: Material, of: of) }
FUNCTION font(of) { get(from: Font, of: of) }
// ... 9 more thin wrappers

FUNCTION get(from, of) {
    PASSED.theme_options.name |> WHEN {
        Professional => Professional/get(from: from, of: of)
        Glassmorphism => Glassmorphism/get(from: from, of: of)
        Neobrutalism => Neobrutalism/get(from: from, of: of)
        Neumorphism => Neumorphism/get(from: from, of: of)
    }
}
```

**Each Theme (Interface Declaration):**
```bn
FUNCTION get(from, of) {
    from |> WHEN {
        Material => of |> material()
        Font => of |> font()
        Text => of |> text()
        Depth => of |> depth()
        Elevation => of |> elevation()
        Corners => of |> corners()
        Lights => lights()
        Geometry => geometry()
        Sizing => of |> sizing()
        Spacing => of |> spacing()
        SpringRange => of |> spring_range()
    }
}
```

### Why This Pattern?

**The `get()` function serves THREE purposes:**

1. **Routing** - Dispatches to the correct implementation function
2. **Type Declaration** - Via pattern matching, declares which categories this theme supports
3. **Interface Contract** - Visible documentation of required functions

**Type Safety via Tag Inference:**

Boon uses Roc-style tag inference. When a theme's `get()` function pattern-matches on:
```bn
from |> WHEN {
    Material => ...
    Font => ...
    // ... 11 cases
}
```

The compiler infers: `from : [Material, Font, Text, Depth, ...]`

**If a theme forgets a case:**
```bn
-- Oops, forgot SpringRange!
FUNCTION get(from, of) {
    from |> WHEN {
        Material => ...
        Font => ...
        // Missing SpringRange
    }
}
```

**Compile Error:** When Theme.bn tries `Professional/get(from: SpringRange, ...)`, compiler knows Professional doesn't accept SpringRange tag!

**This means:**
- ✅ Missing implementations caught at **compile-time**, not runtime
- ✅ No need for runtime error cases or catch-alls
- ✅ Cannot call theme with unsupported category
- ✅ The 64 duplicate lines across themes aren't waste - they're **type declarations**

### Why Not Extract to Shared Module?

**Attempted optimization:**
```bn
-- Interface.bn
FUNCTION get(from, of) { ... }

-- Professional.bn
get: Interface/get  // Try to reuse
```

**Won't work because:**
1. The pattern matching in `get()` creates the **type constraint** for each theme
2. If shared, all themes would have identical type signatures even if some don't implement all functions
3. The compiler needs each theme's `get()` to be **in that theme** to type-check calls to theme-specific functions

**Conclusion:** The duplication is intentional and valuable - it's how Boon's type system ensures completeness.

---

## Boon Language Insights

### Tag Inference (Roc-style)

Tags aren't pre-declared - they're inferred from usage:

```bn
material |> WHEN {
    Surface => [...]
    Interactive[hovered] => [...]
}
```

Compiler infers: `material : [Surface, Interactive { hovered : Bool }]`

**Exhaustiveness checking:** Compiler ensures all possible tags are handled.

### PASSED Context

Implicit context passing without explicit parameters:

```bn
-- Theme.bn
PASSED.theme_options.name |> WHEN { ... }

-- In themes
color: PASSED.mode |> WHEN {
    Light => ...
    Dark => ...
}
```

**Benefits:**
- Cleaner function signatures
- Automatic context propagation
- No manual threading of parameters

### Pattern Matching as Routing

Pattern matching is first-class and used for:
- Type declarations
- Value extraction
- Routing/dispatch
- Exhaustiveness checking

### Empty Values

`[]` represents empty list/no value - cleaner than `None` or `null`:

```bn
FUNCTION lights() { get(from: Lights, of: []) }
FUNCTION geometry() { get(from: Geometry, of: []) }
```

---

## Design Decisions

### Parameter Naming: `from` vs `group`

Original: `get(group: Material, of: Surface)`
Problem: Sounds like "getting a group" not "getting FROM a group"

Solution: `get(from: Material, of: Surface)`
Reads as: "Get from Material, of Surface"

### Function Naming: `token` → `get`

Evolution:
- `token()` - Confusing (implies auth token or design token value)
- `route()` - Accurate but implies HTTP
- `dispatch()` - Standard but event-oriented
- **`get()`** - Simplest, clearest, most intuitive ✅

### Empty Value: `None` → `[]`

For functions with no `of` parameter:
- `None` - Requires importing/defining
- `Unit` - FP term but uncommon
- **`[]`** - Built-in, clear, concise ✅

---

## Key Learnings

### Architecture
1. **Semantic tokens > Component-specific** - Enables theme reusability
2. **Composition > Inheritance** - App composes primitives into components
3. **Type safety through pattern matching** - The structure IS the interface
4. **Duplication with purpose** - Repeated code can be type declarations

### Boon-Specific
1. **Tags are inferred, not declared** - Pattern matching defines types
2. **PASSED context propagation** - Cleaner than explicit parameters
3. **Exhaustiveness checking** - Compiler ensures complete pattern matches
4. **No inheritance needed** - Composition and routing patterns suffice

### Trade-offs
1. **64 duplicate lines across themes** - Necessary for type safety
2. **Theme.bn verbosity** - Could be reduced but current form is clearest
3. **Two-layer architecture** - Small overhead but massive flexibility gain

---

## Future Considerations

### Adding a New Theme
1. Add 1 line to Theme.bn's `get()` routing
2. Implement 11 functions (compiler will ensure completeness via `get()`)
3. Done!

### Adding a New Token Category
1. Add to all thin wrappers in Theme.bn
2. Add to all theme `get()` functions (compiler enforces)
3. Implement in all themes

### Migration from Component-Specific Themes
If other codebases have old component-specific themes:
1. Add semantic scales alongside existing names (Phase 1)
2. Create composition layer in app (Phase 2)
3. Migrate components one-by-one (Phase 3)
4. Remove old component names (Phase 4)

Non-breaking, gradual migration path.
