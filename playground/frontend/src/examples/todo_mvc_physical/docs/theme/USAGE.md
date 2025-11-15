# Theme Usage Guide

## How Themes Work

Themes provide a **complete visual design system** by bundling all styling decisions into a single reusable configuration. Instead of manually setting materials, colors, and lighting throughout your code, you reference semantic theme values.

## Theme Resolution

### 1. Mode Selection (Light/Dark)

Each theme function accepts a `mode` parameter:

```boon
theme: Theme/Professional/theme(mode: Light)   -- Light mode
theme: Theme/Professional/theme(mode: Dark)    -- Dark mode
```

The theme internally uses `mode |> WHEN { Light => ..., Dark => ... }` to resolve:
- Colors (surfaces, text, borders)
- Ambient lighting intensity/color
- Some material adjustments (if needed)

### 2. Semantic Value Mapping

Elements reference semantic values from the theme instead of hardcoded values:

**Without theme (explicit):**
```boon
Element/button(
    style: [
        depth: 6
        transform: [move_closer: 4]
        material: [gloss: 0.3, metal: 0.03]
        backgSoft: [color: Oklch[lightness: 0.985]]
    ]
)
```

**With theme (semantic):**
```boon
Element/button(
    style: [
        depth: THEME.depth.Element
        elevation: THEME.elevation.Button
        material: THEME.materials.button
        backgSoft: [color: THEME.colors.surface_variant]
    ]
)
```

### Material Properties

Themes define physically-based material properties that control how surfaces interact with light:

```boon
materials: [
    panel: [
        transparency: 1.0     -- 0.0 = opaque, 1.0 = Pilly transparent
        refraction: 1.5       -- Index of refraction (1.0 = air, 1.5 = glass, 2.4 = diamond)
        gloss: 0.6            -- Surface glossiness (0 = rough/matte, 1 = mirror-smooth)
        metal: 0.0            -- Metallic property
        shine: 0.0            -- Clearcoat shine
    ]
]
```

**How it works:**
- You define materials using physical properties (transparency, refraction, gloss)
- The renderer decides how to implement these (ray-traced refraction, backdrop blur, etc.)
- Gloss controls surface smoothness: low gloss = rough (roughness = 1.0 - gloss)
- This keeps material definitions portable across different rendering backends

**Examples:**
```boon
-- Opaque matte surface (Professional/Neobrutalism/Neumorphism)
material: [gloss: 0.1, metal: 0.0]  -- transparency/refraction default to 0.0/1.0

-- Frosted glass (Glassmorphism)
material: [
    transparency: 1.0      -- Light passes through
    refraction: 1.5        -- Glass IOR
    gloss: 0.6             -- Lower gloss = frosted effect (roughness = 0.4)
]

-- Clear glass
material: [
    transparency: 1.0
    refraction: 1.5
    gloss: 0.9             -- High gloss = clear, smooth surface
]
```

### 3. Complete Theme Application

**With external theme file:**
```boon
-- Load theme from Theme/ directory
theme: Theme/Professional/theme(mode: Light)

scene: Scene/new(
    root: root_element(PASS: [store: store, theme: theme])
    lights: theme.lights
    geometry: theme.geometry
)

-- Elements access theme via PASSED
Element/button(
    style: [
        depth: PASSED.theme.depth.Element
        material: PASSED.theme.materials.button
    ]
)
```

**With inline theme definition:**

Copy the theme configuration directly into `Scene/new`:

```boon
scene: Scene/new(
    root: root_element(PASS: [store: store])
    lights: LIST {
        Light/directional(
            azimuth: 30
            altitude: 45
            spread: 1
            intensity: 1.2
            color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
        )
        Light/ambient(
            intensity: 0.4
            color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
        )
    }
    geometry: [
        edge_radius: 2
        bevel_angle: 45
    ]
)
```

## Theme Comparison

| Aspect | Professional | Neobrutalism | Glassmorphism | Neumorphism |
|--------|-------------|--------------|---------------|-------------|
| **Edge Radius** | 2 | 0 (Edge) | 2 | 4 (soft) |
| **Bevel Angle** | 45° | 30° (Edge) | 45° | 60° (gentle) |
| **Shadow Spread** | 1 (soft) | 0 (hard) | 1.5 (very soft) | 2 (very soft) |
| **Transparency** | 0.0 (opaque) | 0.0 (opaque) | 0.7-1.0 (glass) | 0.0 (opaque) |
| **Refraction** | 1.0 (none) | 1.0 (none) | 1.5 (glass) | 1.0 (none) |
| **Gloss Range** | 0.12-0.65 | 0.05-0.15 (matte) | 0.6-0.8 (frosted glass) | 0.2-0.3 (low) |
| **Elevation** | Moderate | Dramatic | Moderate | Subtle |
| **Depth** | Standard | Chunky | Thin | Standard |
| **Interaction** | Subtle (150ms) | Snappy (100ms) | Smooth (200ms) | Gentle (200ms) |
| **Colors** | Neutral warm | Bold saturated | Subtle translucent | Monochrome |
| **Contrast** | Medium | Very high | Low | Very low |

## Light/Dark Mode Differences

### Colors that flip:
- **Surfaces**: `0.95-1.0` (light) ↔ `0.1-0.2` (dark)
- **Text**: `0.2-0.4` (light) ↔ `0.8-0.95` (dark)
- **Borders**: `0.9` (light) ↔ `0.3` (dark)

### Colors that adjust:
- **Primary/Accent**: Slightly brighter in dark mode for visibility
- **Focus**: More intense glow in dark mode
- **Danger**: Brighter in dark mode

### Values that stay the same:
- Geometry (edge_radius, bevel_angle)
- Elevation scale
- Depth scale
- Interaction physics
- Corner radius scale
- Material gloss (mostly)

## Switching Themes

### At Build Time:
```boon
-- Change this line to switch entire design
theme: Theme/Neobrutalism/theme(mode: Dark)  -- Was: Theme/Professional/theme(mode: Light)

scene: Scene/new(
    root: root_element(...)
    lights: theme.lights
    geometry: theme.geometry
)
```

### At Runtime:
```boon
-- User preference
user_theme: LATEST {
    Professional
    settings_panel.theme_selector.value
}

mode: LATEST {
    Light
    settings_panel.dark_mode_toggle.checked |> WHEN {
        True => Dark
        False => Light
    }
}

theme: user_theme |> WHEN {
    Professional => Theme/Professional/theme(mode: mode)
    Neobrutalism => Theme/Neobrutalism/theme(mode: mode)
    Glassmorphism => Theme/Glassmorphism/theme(mode: mode)
    Neumorphism => Theme/Neumorphism/theme(mode: mode)
}
```

## Creating Element Variants

You can create element wrappers that automatically use theme values:

```boon
FUNCTION themed_button(label, variant) {
    Element/button(
        style: [
            depth: PASSED.theme.depth.Element
            elevation: PASSED.theme.elevation.Button
            material: variant |> WHEN {
                Primary => PASSED.theme.materials.button
                Emphasis => PASSED.theme.materials.button_Hero
            }
            backgSoft: [color: variant |> WHEN {
                Primary => PASSED.theme.colors.surface_variant
                Emphasis => PASSED.theme.colors.primary
            }]
            Softed_corners: PASSED.theme.corners.Soft
        ]
        label: label
    )
}
```

## Best Practices

### 1. Always use semantic values
❌ **Bad:**
```boon
backgSoft: [color: Oklch[lightness: 0.92]]
```

✅ **Good:**
```boon
backgSoft: [color: THEME.colors.surface_dim]
```

### 2. Don't override theme values unless necessary
❌ **Bad:**
```boon
material: [gloss: 0.8]  -- Breaks theme consistency
```

✅ **Good:**
```boon
material: THEME.materials.button  -- Uses theme material
```

### 3. Use elevation scale for Z-positioning
❌ **Bad:**
```boon
transform: [move_closer: 17]  -- Arbitrary value
```

✅ **Good:**
```boon
elevation: THEME.elevation.Dialog  -- Semantic meaning
```

### 4. Define custom values in theme, not inline
❌ **Bad:**
```boon
-- Special button with custom color in code
backgSoft: [color: Oklch[lightness: 0.65, chroma: 0.15, hue: 120]]
```

✅ **Good:**
```boon
-- Add to theme colors
colors: [
    ...
    success: Oklch[lightness: 0.65, chroma: 0.15, hue: 120]
]

-- Use in code
backgSoft: [color: THEME.colors.success]
```

## Theme Architecture Benefits

1. **🎨 One-line design changes** - Switch entire aesthetic instantly
2. **🌓 Automatic dark mode** - Just change mode parameter
3. **♻️ No duplication** - Define once, use everywhere
4. **🎯 Semantic clarity** - `surface_variant` is clearer than `0.985`
5. **🔧 Easy customization** - Override individual properties
6. **📏 Guaranteed consistency** - Impossible to have mismatched values
7. **🚀 Composable** - Mix theme values with custom overrides

## Overriding Theme Values with Spread Operator

You can extend or modify theme materials using the spread operator (`...`):

### Basic Override Pattern

```boon
FUNCTION delete_button_material(hovered) {
    [
        ...Theme/material(of: SurfaceElevated)  -- Get base material
        glow: hovered |> WHEN {                  -- Add conditional glow
            True => [
                color: Theme/material(of: Danger).color
                intensity: 0.08
            ]
            False => None
        }
    ]
}
```

This pattern allows you to:
- ✅ **Build on existing theme materials** - Inherit base properties automatically
- ✅ **Override specific properties** - Change only what you need
- ✅ **Add new properties** - Extend with additional fields
- ✅ **Maintain theme consistency** - Base values stay synchronized

### Multiple Conditional Overrides

```boon
FUNCTION filter_button_material(selected, hovered) {
    [
        ...selected |> WHEN {
            True => Theme/material(of: PrimarySubtle)
            False => Theme/material(of: SurfaceVariant)
        }
        gloss: selected |> WHEN {
            False => 0.35
            True => 0.2
        }
        metal: 0.03
        glow: LIST { selected, hovered } |> WHEN {
            LIST { True, __ } => [
                color: Theme/material(of: Primary).color
                intensity: 0.05
            ]
            LIST { False, True } => [
                color: Theme/material(of: Primary).color
                intensity: 0.025
            ]
            LIST { False, False } => None
        }
    ]
}
```

### Font Overrides

```boon
FUNCTION clear_button_font(hovered) {
    [
        ...Theme/font(of: BodySecondary)
        line: [underline: hovered]
    ]
}

FUNCTION todo_title_font(completed) {
    [
        ...Theme/font(of: Body)
        line: [strike: completed]
        ...completed |> WHEN {
            True => [color: Theme/font(of: BodyDisabled).color]
            False => []
        }
    ]
}
```

### Why Use Spread Operator?

**Without spread (manual duplication):**
```boon
-- ❌ Must manually copy all fields
FUNCTION custom_material() {
    [
        color: Oklch[lightness: 0.99]  -- Copied from SurfaceElevated
        gloss: 0.4                      -- Copied from SurfaceElevated
        metal: 0.02                     -- Copied from SurfaceElevated
        glow: custom_glow               -- Only this is different!
    ]
}
```

**With spread (DRY and maintainable):**
```boon
-- ✅ Inherit base, override only what changes
FUNCTION custom_material() {
    [
        ...Theme/material(of: SurfaceElevated)
        glow: custom_glow
    ]
}
```

**Benefits:**
1. **DRY** - Don't repeat yourself
2. **Maintainable** - Theme updates propagate automatically
3. **Type-safe** - Compiler checks field compatibility
4. **Optimized** - Monomorphization eliminates overhead

See actual usage in `RUN.bn` for complete examples.

## Advanced: Custom Theme Properties

Themes can include custom properties beyond the standard set:

```boon
FUNCTION MyCustomTheme(mode) {
    [
        -- Standard properties
        lights: ...
        geometry: ...

        -- Custom additions
        animation: [
            spring_stiffness: 200
            spring_damping: 20
            duration_fast: 100
            duration_normal: 200
            duration_slow: 400
        ]

        typography: [
            heading: [size: 24, weight: Bold]
            body: [size: 14, weight: Regular]
            caption: [size: 12, weight: Light]
        ]
    ]
}
```

## Migration Guide

**From explicit values to themes:**

1. Identify repeated values in your code
2. Extract into semantic theme properties
3. Update Scene/new to use theme
4. Pass theme via PASS context
5. Replace hardcoded values with PASSED.theme references
6. Test light and dark modes
7. Refine theme values as needed
