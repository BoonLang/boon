# Theme Usage Guide

## How Themes Work

Themes provide a **complete visual design system** by bundling all styling decisions into a single reusable configuration. Instead of manually setting materials, colors, and lighting throughout your code, you reference semantic theme values.

## Theme Resolution

### 1. Mode Selection (Light/Dark)

Each theme function accepts a `mode` parameter:

```boon
theme: Themes/Professional/theme(mode: Light)   -- Light mode
theme: Themes/Professional/theme(mode: Dark)    -- Dark mode
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
        background: [color: Oklch[lightness: 0.985]]
    ]
)
```

**With theme (semantic):**
```boon
Element/button(
    style: [
        depth: THEME.depth.standard
        elevation: THEME.elevation.raised
        material: THEME.materials.button
        background: [color: THEME.colors.surface_variant]
    ]
)
```

### 3. Complete Theme Application

**With external theme file:**
```boon
-- Load theme from Themes/ directory
theme: Themes/Professional/theme(mode: Light)

scene: Scene/new(
    root: root_element(PASS: [store: store, theme: theme])
    lights: theme.lights
    geometry: theme.geometry
)

-- Elements access theme via PASSED
Element/button(
    style: [
        depth: PASSED.theme.depth.standard
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
| **Edge Radius** | 2 | 0 (sharp) | 2 | 4 (soft) |
| **Bevel Angle** | 45° | 30° (sharp) | 45° | 60° (gentle) |
| **Shadow Spread** | 1 (soft) | 0 (hard) | 1.5 (very soft) | 2 (very soft) |
| **Gloss Range** | 0.12-0.65 | 0.05-0.15 (matte) | 0.7-0.85 (glossy) | 0.2-0.3 (low) |
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
theme: Themes/Neobrutalism/theme(mode: Dark)  -- Was: Themes/Professional/theme(mode: Light)

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
    Professional => Themes/Professional/theme(mode: mode)
    Neobrutalism => Themes/Neobrutalism/theme(mode: mode)
    Glassmorphism => Themes/Glassmorphism/theme(mode: mode)
    Neumorphism => Themes/Neumorphism/theme(mode: mode)
}
```

## Creating Element Variants

You can create element wrappers that automatically use theme values:

```boon
FUNCTION themed_button(label, variant) {
    Element/button(
        style: [
            depth: PASSED.theme.depth.standard
            elevation: PASSED.theme.elevation.raised
            material: variant |> WHEN {
                Primary => PASSED.theme.materials.button
                Emphasis => PASSED.theme.materials.button_emphasis
            }
            background: [color: variant |> WHEN {
                Primary => PASSED.theme.colors.surface_variant
                Emphasis => PASSED.theme.colors.primary
            }]
            rounded_corners: PASSED.theme.corners.round
        ]
        label: label
    )
}
```

## Best Practices

### 1. Always use semantic values
❌ **Bad:**
```boon
background: [color: Oklch[lightness: 0.92]]
```

✅ **Good:**
```boon
background: [color: THEME.colors.surface_dim]
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
elevation: THEME.elevation.popup  -- Semantic meaning
```

### 4. Define custom values in theme, not inline
❌ **Bad:**
```boon
-- Special button with custom color in code
background: [color: Oklch[lightness: 0.65, chroma: 0.15, hue: 120]]
```

✅ **Good:**
```boon
-- Add to theme colors
colors: [
    ...
    success: Oklch[lightness: 0.65, chroma: 0.15, hue: 120]
]

-- Use in code
background: [color: THEME.colors.success]
```

## Theme Architecture Benefits

1. **🎨 One-line design changes** - Switch entire aesthetic instantly
2. **🌓 Automatic dark mode** - Just change mode parameter
3. **♻️ No duplication** - Define once, use everywhere
4. **🎯 Semantic clarity** - `surface_variant` is clearer than `0.985`
5. **🔧 Easy customization** - Override individual properties
6. **📏 Guaranteed consistency** - Impossible to have mismatched values
7. **🚀 Composable** - Mix theme values with custom overrides

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
