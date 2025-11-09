# Theme System Architecture

## Vision: Ultra-Thin Control

The theme system provides **global control over the entire visual design** through a small set of high-level parameters. Instead of manually setting hundreds of style properties across your UI, you configure one theme and everything cascades down automatically.

## Emergent Design Patterns

The theme system extracts **6 major patterns** discovered in the TodoMVC Physical code:

### 1. Material Presets
**Problem**: Material properties scattered everywhere (gloss, transparency values)
**Solution**: Semantic material presets with physical properties

```boon
materials: [
    panel: [
        transparency: 0.0     -- Defaults to opaque (0.0)
        refraction: 1.0       -- Defaults to no refraction (1.0 = air)
        gloss: 0.12           -- Surface glossiness (low gloss = rough/matte)
        metal: 0.02           -- Metallic property
        shine: 0.6            -- Clearcoat shine
    ]
    button: [gloss: 0.3, metal: 0.03]
    input_interior: [gloss: 0.65]
    -- Glassmorphism example
    glass_panel: [
        transparency: 1.0     -- Fully transparent
        refraction: 1.5       -- Glass IOR
        gloss: 0.6            -- Lower gloss = frosted glass effect
    ]
]
```

**Material Properties:**
- `transparency: 0.0-1.0` - How much light passes through (0 = opaque, 1 = fully transparent)
- `refraction: 1.0+` - Index of refraction (1.0 = air, 1.5 = glass, 2.4 = diamond)
- `gloss: 0.0-1.0` - Surface glossiness (0 = matte/rough, 1 = mirror-smooth). When combined with transparency, lower gloss creates frosted glass effect.
- `metal: 0.0-1.0` - Metallic property
- `shine: 0.0-1.0` - Clearcoat shine

**Renderer behavior:**
- Physical renderers: Use transparency+refraction for accurate light transport. Gloss controls surface roughness (roughness = 1.0 - gloss).
- UI renderers: Approximate transparency+low gloss with backdrop blur for performance
- Simple renderers: Fall back to simple alpha blending

### 2. Elevation Hierarchy
**Problem**: Z-positions like 50, 24, 8, 4, -4 with unclear meaning
**Solution**: Semantic elevation scale

```boon
elevation: [
    card: 50      -- Major floating containers
    popup: 24     -- Modal overlays
    floating: 8   -- Emphasized elements
    raised: 4     -- Interactive resting state
    grounded: 0   -- Flush with surface
    recessed: -4  -- Inset elements
]
```

### 3. Depth Scale
**Problem**: Thickness values (8, 10, 6, 2, 4) with unclear purpose
**Solution**: Semantic depth scale

```boon
depth: [
    major: 8      -- Large structures
    standard: 6   -- Normal elements
    subtle: 2     -- Thin details
    emphasis: 10  -- Important/bold
]
```

### 4. Interaction Physics
**Problem**: Repeated hover/press patterns throughout code
**Solution**: Global interaction settings

```boon
interaction: [
    hover_lift: 2        -- How much buttons lift on hover
    press_depth: 4       -- How much they sink when pressed
    emphasis_lift: 4     -- Extra lift for important buttons
    transition_ms: 150   -- Animation speed
]
```

### 5. Corner Radius Scale
**Problem**: Various corner values (4, 2, 6, Fully)
**Solution**: Semantic corner scale

```boon
corners: [
    sharp: 0
    subtle: 2
    standard: 4
    round: 6
    full: Fully
]
```

### 6. Color Theme
**Problem**: Repeated color patterns throughout
**Solution**: Semantic color palette

```boon
colors: [
    surface: ...
    primary: ...
    focus: ...
    on_surface: ...
    -- etc
]
```

## Architecture Layers

```
┌─────────────────────────────────────────────────────────┐
│ Theme Function (professional.bn, neobrutalism.bn, etc.) │
│  - Accepts: mode (Light/Dark)                           │
│  - Returns: Complete theme configuration                │
└───────────────────────┬─────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────┐
│ Scene/new (applies theme globally)                      │
│  - lights: theme.lights                                 │
│  - geometry: theme.geometry                             │
│  - materials: theme.materials (NEW)                     │
│  - elevation: theme.elevation (NEW)                     │
│  - depth: theme.depth (NEW)                             │
│  - interaction: theme.interaction (NEW)                 │
│  - corners: theme.corners (NEW)                         │
│  - colors: theme.colors (NEW)                           │
└───────────────────────┬─────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────┐
│ PASS Context (propagates theme down tree)              │
│  PASS: [store: store, theme: theme]                    │
└───────────────────────┬─────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────┐
│ Elements (reference theme values)                       │
│  depth: PASSED.theme.depth.standard                     │
│  material: PASSED.theme.materials.button                │
│  background: [color: PASSED.theme.colors.surface]       │
└─────────────────────────────────────────────────────────┘
```

## Light/Dark Mode Resolution

### Strategy: Resolve at Theme Level

Each theme function contains `mode |> WHEN { Light => ..., Dark => ... }` for properties that differ between modes.

**Properties that change:**
- Colors (surfaces, text, borders)
- Ambient lighting color/intensity
- Sometimes shadow intensity

**Properties that stay the same:**
- Geometry (edge_radius, bevel_angle)
- Material properties (transparency, refraction, gloss, metal, shine)
- Elevation scale
- Depth scale
- Interaction physics
- Corner radii

### Example Resolution:

```boon
FUNCTION Professional(mode) {
    [
        -- Colors resolve based on mode
        colors: mode |> WHEN {
            Light => [
                surface: Oklch[lightness: 1]
                on_surface: Oklch[lightness: 0.42]
            ]
            Dark => [
                surface: Oklch[lightness: 0.15]
                on_surface: Oklch[lightness: 0.9]
            ]
        }

        -- Geometry is always the same
        geometry: [
            edge_radius: 2
            bevel_angle: 45
        ]

        -- Ambient light adjusts for mode
        lights: LIST {
            Light/directional(...)  -- Same for both modes
            Light/ambient(
                intensity: 0.4
                color: mode |> WHEN {
                    Light => Oklch[lightness: 0.8, ...]
                    Dark => Oklch[lightness: 0.3, ...]
                }
            )
        }
    ]
}
```

## Complete Flow

### 1. Define Theme
```boon
-- themes/professional.bn
FUNCTION Professional(mode) { [...] }
```

### 2. Select Theme + Mode
```boon
-- Future: User preference
selected_theme: Professional
mode: Light

-- Resolve theme
theme: selected_theme(mode: mode)
```

### 3. Apply to Scene
```boon
scene: Scene/new(
    root: root_element(PASS: [store: store, theme: theme])
    lights: theme.lights
    geometry: theme.geometry
    materials: theme.materials
    elevation: theme.elevation
    depth: theme.depth
    interaction: theme.interaction
    corners: theme.corners
    colors: theme.colors
)
```

### 4. Elements Reference Theme
```boon
Element/button(
    style: [
        depth: PASSED.theme.depth.standard
        elevation: PASSED.theme.elevation.raised
        material: PASSED.theme.materials.button
        background: [color: PASSED.theme.colors.surface_variant]
        rounded_corners: PASSED.theme.corners.round
    ]
)
```

## Benefits

### Developer Experience
- ✅ **One-line theme switching** - Change `Professional` → `Neobrutalism`
- ✅ **Automatic dark mode** - Just flip mode parameter
- ✅ **Semantic clarity** - `surface_variant` vs `0.985`
- ✅ **No duplication** - Define once, use everywhere
- ✅ **Type safety** - Can't use non-existent theme values

### Design Consistency
- ✅ **Guaranteed consistency** - Can't have mismatched values
- ✅ **Easy refactoring** - Change theme, everything updates
- ✅ **Design tokens** - Industry standard pattern
- ✅ **Composable** - Mix theme + custom overrides

### Performance
- ✅ **Theme resolved once** - Not on every element
- ✅ **Passed via context** - No repeated lookups
- ✅ **Static values** - Can be optimized by compiler

## Comparison to Other Systems

### Material Design 3 (Google)
- **Similarities**: Semantic tokens (surface, primary, etc.)
- **Differences**: Boon includes 3D-specific properties (elevation, depth, materials)

### Tailwind CSS
- **Similarities**: Semantic scales (colors, spacing)
- **Differences**: Boon is holistic (includes lighting, materials, physics)

### CSS Variables
- **Similarities**: Cascade, override, compose
- **Differences**: Boon themes are typed and validated

### Design Tokens (Style Dictionary)
- **Similarities**: Semantic naming, light/dark modes
- **Differences**: Boon themes are executable code, not just data

## Future Enhancements

### 1. Runtime Theme Switching
```boon
-- Theme selector in UI
theme_selector: Element/dropdown(
    options: LIST { Professional, Neobrutalism, Glassmorphism }
)

active_theme: LATEST {
    Professional
    theme_selector.selected
} |> WHEN {
    Professional => Professional(mode: mode)
    Neobrutalism => Neobrutalism(mode: mode)
    Glassmorphism => Glassmorphism(mode: mode)
}
```

### 2. Theme Composition
```boon
-- Merge multiple themes
FUNCTION CustomTheme(mode) {
    base: Professional(mode: mode)

    base |> merge([
        colors: CustomColors(mode: mode)
        interaction: SmoothInteraction
    ])
}
```

### 3. Per-Component Overrides
```boon
Element/button(
    style: [
        -- Use theme defaults
        ...PASSED.theme.materials.button

        -- Override specific property
        gloss: 0.5
    ]
)
```

### 4. Responsive Themes
```boon
-- Different themes for different screen sizes
theme: viewport.width |> WHEN {
    width if width < 600 => MobileTheme(mode: mode)
    width if width < 1200 => TabletTheme(mode: mode)
    __ => DesktopTheme(mode: mode)
}
```

### 5. Animation Curves
```boon
animation: [
    spring_stiffness: 200
    spring_damping: 20
    easing: EaseOutCubic
]
```

### 6. Typography Scale
```boon
typography: [
    heading: [size: 24, weight: Bold, line_height: 1.2]
    body: [size: 14, weight: Regular, line_height: 1.5]
    caption: [size: 12, weight: Light, line_height: 1.3]
]
```

## Implementation Roadmap

### Phase 1: Foundation (Current)
- ✅ Define theme structure
- ✅ Create 4 complete themes
- ✅ Document usage patterns
- ⏳ Add `geometry: []` to Scene/new

### Phase 2: Core Implementation
- ⏳ Implement Scene/new theme parameters
- ⏳ Add PASS context for theme propagation
- ⏳ Create theme resolver/validator
- ⏳ Implement light/dark mode switching

### Phase 3: Element Integration
- ⏳ Update elements to accept semantic values
- ⏳ Provide defaults from theme
- ⏳ Allow per-element overrides
- ⏳ Update TodoMVC to use theme system

### Phase 4: Advanced Features
- ⏳ Runtime theme switching
- ⏳ Theme composition/merging
- ⏳ Animation curve system
- ⏳ Typography scale
- ⏳ Custom theme properties

## Open Questions

1. **Import syntax**: How should themes be imported?
2. **Theme validation**: How to ensure themes are complete?
3. **Default fallbacks**: What happens if theme is missing a property?
4. **Override precedence**: Element override > theme > global default?
5. **Performance**: Cache resolved theme values?
6. **Type system**: Can we type-check theme structure?

## Conclusion

The theme system transforms UI development from **manual styling** to **semantic design**. By extracting patterns from real code and bundling them into reusable configurations, we enable:

- 🎨 **Instant design changes** (one line)
- 🌓 **Automatic dark mode** (flip parameter)
- ♻️ **Zero duplication** (DRY principle)
- 🎯 **Semantic clarity** (self-documenting)
- 📏 **Guaranteed consistency** (no mismatches)
- 🚀 **Emergent complexity** (simple API, powerful results)

This is the "ultrathin control" you envisioned - a handful of settings that cascade through the entire scene, creating complete, coherent design systems.
