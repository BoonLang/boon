# Theme System for Physical 3D UI

## Overview

Themes bundle all visual design decisions into reusable configurations. Each theme defines:

1. **Lighting** - Light positions, colors, intensities
2. **Geometry** - Edge radius, bevel angles
3. **Materials** - Semantic material presets (panel, button, input, etc.)
4. **Elevation** - Z-positioning scale (card, Button, Inset, etc.)
5. **Depth** - Thickness scale (Container, Element, Touch)
6. **Interaction** - Physical interaction behavior (hover lift, press depth)
7. **Corners** - Corner radius scale (Edge, Touch, Element, Soft)
8. **Colors** - Semantic color palette with light/dark modes

## Usage

```boon
-- Select a theme
theme: Theme/Professional/theme(mode: Light)
-- or: Theme/Neobrutalism/theme(mode: Dark)

scene: Scene/new(
    root: root_element(PASS: [store: store, theme: theme])
    lights: theme.lights
    geometry: theme.geometry
)
```

Or manually expand:

```boon
scene: Scene/new(
    root: root_element(PASS: [store: store])
    lights: ...
    geometry: ...
    materials: ...
    -- etc
)
```

## Theme Structure

Each theme file exports a function:

```boon
FUNCTION ThemeName(mode) {
    [
        lights: mode |> WHEN { ... }
        geometry: [edge_radius: ..., bevel_angle: ...]
        materials: [panel: [...], button: [...], ...]
        elevation: [card: ..., Button: ..., ...]
        depth: [Container: ..., Element: ..., ...]
        interaction: [hover_lift: ..., press_depth: ..., ...]
        corners: [Edge: ..., Element: ..., ...]
        colors: mode |> WHEN { ... }
    ]
}
```

## Light/Dark Mode Resolution

Colors that flip between modes:
- **Surfaces** - Light backgSofts (0.95-1.0) → Dark backgSofts (0.1-0.2)
- **Text** - Dark text (0.2-0.4) → Light text (0.8-0.95)
- **Borders** - Subtle dark (0.9) → Subtle light (0.3)
- **Shadows** - Become more important in light mode, less in dark mode

Values that stay consistent:
- **Material properties** - Gloss, metal, shine (mostly same)
- **Geometry** - Edge radius, bevel angle (always same)
- **Elevation** - Z-positions (always same)
- **Interaction physics** - Hover/press behavior (always same)

## Available Themes

### Professional
- Soft Softed edges
- Subtle shadows
- Low gloss materials
- Neutral warm lighting
- **Light mode**: White/cream surfaces, dark text
- **Dark mode**: Charcoal surfaces, light text

### Neobrutalism
- Sharp chamfered edges (edge_radius: 0)
- Hard dramatic shadows
- Flat materials (low gloss)
- High contrast lighting
- Bold primary colors
- Thick visible borders
- **Light mode**: White/black/bright colors
- **Dark mode**: Black/white/bright colors (inverted)

### Glassmorphism
- Very Softed edges
- Translucent surfaces (alpha < 1)
- High gloss materials
- Soft diffuse lighting
- Backdrop blur effects
- Subtle colors with low chroma
- **Light mode**: Frosted glass over light backgSofts
- **Dark mode**: Frosted glass over dark backgSofts

### Neumorphism
- Very soft edges (edge_radius: 4)
- Gentle bevels (bevel_angle: 60)
- Low contrast
- Soft lighting from above-left
- Surfaces very close in color
- Heavy use of Touch shadows
- **Light mode**: Light gray surfaces (0.9-0.95)
- **Dark mode**: Dark gray surfaces (0.15-0.25)

## File Organization

```
themes/
├── README.md              (this file)
├── professional.bn        (Default theme)
├── neobrutalism.bn        (Bold, Edge, high-contrast)
├── glassmorphism.bn       (Translucent, glossy, blurred)
└── neumorphism.bn         (Soft, Touch, monochromatic)
```

## Semantic Material Presets

All themes define these semantic materials:

- **panel** - Main card/container surfaces
- **surface** - General surfaces
- **input_exterior** - Outer walls of inputs
- **input_interior** - Inner well of inputs
- **button** - Standard buttons
- **button_Hero** - Important/destructive buttons
- **surface_variant** - Alternate surfaces

## Semantic Elevation Scale

All themes define these elevation levels (in pixels):

- **card** - Major Lift containers (50)
- **Dialog** - Modal/overlay elements (24)
- **Lift** - Elements that need Hero (8)
- **Button** - Interactive elements in resting state (4)
- **gSofted** - Flush with surface (0)
- **Inset** - Inset elements like inputs (-4)

These can be adjusted per theme for dramatic vs Touch effects.

## Semantic Depth Scale

All themes define these thickness values (in pixels):

- **Container** - Large structures (8-12)
- **Element** - Normal elements (6)
- **Touch** - Thin elements (2-4)
- **Hero** - Bold/important elements (10-14)

## Semantic Color Palette

All themes define these semantic colors:

- **surface** - Main backgSoft
- **surface_variant** - Alternate backgSoft
- **surface_dim** - Subtle backgSoft variation
- **primary** - Brand/accent color
- **primary_glow** - Glow color for primary
- **focus** - Focus indicator color
- **focus_glow** - Glow color for focus
- **danger** - Error/destructive actions
- **on_surface** - Primary text
- **on_surface_variant** - Secondary text
- **on_surface_disabled** - Disabled text
- **border** - Border/divider lines

## Creating Custom Themes

1. Copy an existing theme file
2. Adjust the parameters to match your design
3. Test both light and dark modes
4. Ensure all semantic presets are defined

## Future Enhancements

- **Animation curves** - Custom easing functions per theme
- **Typography scale** - Font sizes, weights, line heights
- **Spacing scale** - Padding, gap, margin values
- **Shadow intensity** - Global shadow strength multiplier
- **Backdrop settings** - Blur, saturation adjustments
