# Boon Physically-Based 3D UI Rendering

**Last updated:** 2025-11-07

This document describes Boon's physically-based 3D rendering system for UI elements. Geometry, shadows, and depth effects emerge naturally from physical properties - no fake shadows or explicit geometric operations needed in user code.

---

## Table of Contents

1. [Core Philosophy](#core-philosophy)
2. [User API - Semantic Elements](#user-api---semantic-elements)
3. [Automatic Geometry](#automatic-geometry)
4. [Material Properties](#material-properties)
5. [Scene Lighting](#scene-lighting)
6. [Internal Implementation](#internal-implementation)

---

## Core Philosophy

### Design Principle: Describe Intent, Not Geometry

**Users write semantic UI elements:**
```boon
Element/text_input(
    style: [depth: 6, material: [gloss: 0.65]]
    text: 'Hello'
)
```

**Renderer creates physical 3D geometry:**
- Calculates cavity dimensions from padding
- Constructs recessed well using internal geometric operations
- Places text on cavity floor
- Applies lighting for natural shadows

**Users never specify:**
- ❌ Boolean operations
- ❌ Cavity shapes
- ❌ Cutter elements
- ❌ Shadow properties

**They just describe what they want visually!**

---

## User API - Semantic Elements

### Element/text_input - Recessed Input

```boon
Element/text_input(
    style: [
        depth: 6              -- Input thickness (creates automatic recess)
        width: 200
        padding: [all: 10]    -- Controls wall thickness
        rounded_corners: 4
        material: [gloss: 0.65]  -- Shiny interior
        background: [color: Oklch[lightness: 0.99]]
    ]
    text: 'Type here...'
    placeholder: 'Enter text'
)
```

**Result:** Input with recessed well, walls emerge from padding, shiny interior, natural inset shadow from lighting.

**How it works:**
- `depth: 6` → Renderer creates cavity ~4px deep
- `padding: [all: 10]` → Walls are 10px thick
- `gloss: 0.65` → Cavity interior is glossy
- Lighting creates natural inset shadow

---

### Element/checkbox - Small Recessed Box

```boon
Element/checkbox(
    style: [
        depth: 5              -- Creates shallow well
        width: 20
        height: 20
        rounded_corners: 3
        material: [gloss: 0.25]
    ]
    checked: True
)
```

**Result:** Checkbox with small recessed well, checkmark sits in cavity, automatic inset shadow.

---

### Element/button - Raised Surface

```boon
Element/button(
    style: [
        depth: 6              -- Button thickness (convex, not recessed)
        transform: [move_closer: 4]  -- Floats above surface
        rounded_corners: 4
        material: [gloss: 0.3]
    ]
    label: 'Click me'
)
```

**Result:** Raised button with beveled edges, floats above surface, casts shadow below.

**Note:** Buttons are convex (no recess) - they sit above the surface.

---

### Element/block - Generic 3D Container

```boon
Element/block(
    style: [
        depth: 8              -- Block thickness
        width: 200
        transform: [move_closer: 50]  -- Distance from background
        rounded_corners: 4
        material: [gloss: 0.12]
    ]
    child: Element/text(text: 'Card content')
)
```

**Result:** Solid 3D block (card) floating above background, casts shadow, beveled edges.

---

## Automatic Geometry

### How Recessed Elements Work

Built-in elements automatically generate recessed geometry based on their properties:

#### Text Input Geometry
```
User specifies:
  depth: 6
  padding: [all: 10]
  gloss: 0.65

Renderer automatically creates:
  - Outer block: 6px deep, matte exterior
  - Inner cavity: ~4px deep (2/3 of depth)
  - Wall thickness: 10px (from padding)
  - Cavity interior: glossier than exterior
  - Text positioned on cavity floor
```

#### Checkbox Geometry
```
User specifies:
  depth: 5
  width: 20, height: 20

Renderer automatically creates:
  - Outer box: 5px deep
  - Inner cavity: ~3px deep
  - Wall thickness: 2px (fixed for checkboxes)
  - Checkmark on cavity floor or raised inside well
```

### Minimum Values

For visible depth effects:
- **Minimum depth:** 4px (anything less is imperceptible)
- **Minimum wall thickness:** 2px
- **Minimum movement:** 4px on Z-axis

---

## Material Properties

### gloss - Surface Finish (Primary)

Controls how rough or smooth the surface appears:

```boon
material: [gloss: 0.0]   -- Matte (chalk, flat paint)
material: [gloss: 0.3]   -- Low gloss (matte plastic) - good for buttons
material: [gloss: 0.5]   -- Satin (brushed metal)
material: [gloss: 0.65]  -- Medium gloss - good for input interiors
material: [gloss: 0.8]   -- High gloss (glossy plastic, polished wood)
material: [gloss: 1.0]   -- Mirror (chrome, glass)
```

**For most UI elements, use 0.15-0.4 for exteriors, 0.6-0.8 for input interiors.**

---

### metal - Metallic Reflections (Rarely Used)

Controls whether reflections are colored or white:

```boon
material: [metal: 0.0]   -- Non-metal: white reflections (plastic, wood, glass) - DEFAULT
material: [metal: 1.0]   -- Metal: colored reflections (gold, copper, steel)
```

**For UI elements, use 0.0-0.05 or omit entirely.**

---

### shine - Clearcoat Layer (Optional)

Adds glossy coating over base material:

```boon
material: [
    gloss: 0.12   -- Base material (matte)
    shine: 0.6    -- Glossy clearcoat on top = sophisticated look
]
```

**Use for premium cards/panels. Otherwise omit.**

---

### glow - Emissive Light

```boon
material: [
    glow: [
        color: Oklch[lightness: 0.7, chroma: 0.08, hue: 220]
        intensity: 0.15
    ]
]
```

**Use for:** Focus indicators, active states, notifications.

---

## Scene Lighting

### Scene/new Enables Physical Rendering

```boon
scene: Scene/new(
    root: root_element(...)
    lights: Lights/basic()  -- Simple default lighting
)
```

Or with custom lights:

```boon
scene: Scene/new(
    root: root_element(...)
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
)
```

### Lights/basic() - Recommended Starting Point

**Use this for most UIs.** Provides good default lighting without configuration:

```boon
FUNCTION Lights/basic() {
    LIST {
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
}
```

### Light Types

#### Directional Light

Simulates a distant light source like the sun. Casts parallel shadows.

```boon
Light/directional(
    azimuth: 30           -- Degrees (0-360°) - rotation clockwise from top
    altitude: 45          -- Degrees (0-180°) - angle from screen toward viewer
    spread: 1             -- Multiplier (0 = sharp, 1 = sun-like, 2+ = softer)
    intensity: 1.2        -- Multiplier (1 = normal)
    color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
)
```

**Parameters:**
- **`azimuth`**: 0-360° clockwise from top
  - 0° = top, 90° = right, 180° = bottom, 270° = left
- **`altitude`**: 0-180° from screen toward viewer
  - 0° = parallel to screen, 45° = typical overhead
  - 90° = perpendicular, 135-180° = backlight
- **`spread`**: Shadow softness (normalized multiplier)
  - 0 = sharp point source
  - 1 = realistic sun-like shadows
  - 2-10 = progressively softer
- **`intensity`**: Brightness multiplier (1 = normal)
- **`color`**: Light color (warm whites for key lights)

**Always casts shadows.**

#### Ambient Light

Provides uniform fill light from all directions. Softens shadows.

```boon
Light/ambient(
    intensity: 0.4        -- Multiplier
    color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
)
```

**Parameters:**
- **`intensity`**: Brightness multiplier (typically 0.3-0.5)
- **`color`**: Fill light color (often cool/blue tinted)

**Never casts shadows.**

#### Point Light

Simulates a localized light source like a bulb or softbox.

```boon
Light/point(
    at: [x: 200, y: 150, z: 300]  -- Pixels - position in viewport space
    radius: 30                     -- Pixels - sphere radius
    intensity: 3.0                 -- Multiplier
    color: Oklch[lightness: 0.95, chroma: 0.2, hue: 30]
    range: 150                     -- Pixels - falloff distance
)
```

**Parameters:**
- **`at`**: Position [x, y, z] in pixels (x=left, y=top, z=depth from screen)
- **`radius`**: Light sphere radius in pixels (larger = softer shadows)
- **`intensity`**: Brightness multiplier
- **`color`**: Light color
- **`range`**: Falloff distance in pixels

**Casts shadows.**

### Common Examples

**Sharp dramatic shadows:**
```boon
Light/directional(
    azimuth: 90
    altitude: 30
    spread: 0
    intensity: 1.5
    color: Oklch[lightness: 0.98, chroma: 0.01, hue: 50]
)
```

**Soft studio lighting:**
```boon
Light/directional(
    azimuth: 0
    altitude: 60
    spread: 3
    intensity: 1.0
    color: Oklch[lightness: 0.95, chroma: 0.015, hue: 65]
)
```

**Point light (small bulb):**
```boon
Light/point(
    at: [x: 200, y: 150, z: 300]
    radius: 10
    intensity: 3.0
    color: Oklch[lightness: 0.95, chroma: 0.2, hue: 30]
    range: 150
)
```

**Point light (large softbox):**
```boon
Light/point(
    at: [x: 200, y: 150, z: 300]
    radius: 80
    intensity: 2.5
    color: Oklch[lightness: 0.98, chroma: 0.05, hue: 65]
    range: 200
)
```

### Units Reference

| Parameter | Light Type | Units | Typical Range | Meaning |
|-----------|-----------|-------|---------------|---------|
| `azimuth` | Directional | Degrees | 0-360° | Rotation around screen (clockwise from top) |
| `altitude` | Directional | Degrees | 0-180° | Angle from screen (0=parallel, 90=perpendicular) |
| `spread` | Directional | Multiplier | 0-10 | Shadow softness (0=sharp, 1=sun, 2+=softer) |
| `at` | Point | Pixels | - | Position [x, y, z] in viewport |
| `radius` | Point | Pixels | 5-100 | Light sphere radius |
| `range` | Point | Pixels | 100-500 | Distance falloff |
| `intensity` | All | Multiplier | 0.5-2.0 | Brightness (1=normal) |

### Shadow Casting

- **Directional lights:** Always cast shadows
- **Point lights:** Always cast shadows
- **Ambient lights:** Never cast shadows
- Shadows emerge from real geometry + lighting
- No fake `shadow` properties needed!

---

## Internal Implementation

**Note:** This section describes renderer internals. Users never interact with these APIs directly.

### Model/cut() Function (Internal)

The renderer uses `Model/cut(from, remove)` to construct recessed geometry:

```boon
-- Internal renderer function (NOT user API)
FUNCTION render_text_input(props) {
    -- Create outer block
    outer: Element/block(
        style: [
            depth: props.depth
            width: props.width
            height: props.height
            material: [gloss: 0.2]  -- Matte exterior
        ]
    )

    -- Calculate cavity dimensions
    cavity: Element/block(
        style: [
            depth: props.depth * 0.66              -- Shallower
            width: props.width - (2 * props.padding.horizontal)
            height: props.height - (2 * props.padding.vertical)
            material: [gloss: props.gloss]         -- User-specified gloss
            transform: [move_further: 1]
        ]
    )

    -- Use Model/cut to construct geometry
    geometry: Model/cut(from: outer, remove: cavity)

    -- Place text content on cavity floor
    return geometry_with_content(geometry, props.text)
}
```

---

### SDF Rendering (Internal)

**Signed Distance Fields** enable fast boolean operations:

```glsl
// Union: Take minimum distance
float sdf_union(float sdf1, float sdf2) {
    return min(sdf1, sdf2);
}

// Subtract: Invert and take maximum
float sdf_subtract(float sdf1, float sdf2) {
    return max(sdf1, -sdf2);
}
```

**Performance:** O(1) per operation, fully GPU-parallel, perfect for UI shapes.

**UI shapes have exact SDF formulas:**
```glsl
float roundedBox(vec3 p, vec3 size, float radius) {
    vec3 q = abs(p) - size + radius;
    return length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0) - radius;
}
```

**Complete scene evaluation:**
```glsl
float scene(vec3 p) {
    float card = roundedBox(p - cardPos, cardSize, 4.0);
    float cavity = roundedBox(p - cavityPos, cavitySize, 2.0);
    float button = roundedBox(p - buttonPos, buttonSize, 6.0);

    float result = max(card, -cavity);  // Subtract cavity
    result = min(result, button);        // Add button
    return result;
}
```

---

## Key Principles Summary

1. **Users write semantic elements** - describe intent, not geometry
2. **Renderer generates 3D geometry** - automatic cavity calculation
3. **`Model/cut()` is internal** - not exposed to users
4. **Physical lighting creates shadows** - no fake shadow properties
5. **Minimum 4px for visibility** - depth, movement, walls
6. **`gloss` is primary material** - 0=matte to 1=mirror
7. **Built-in elements are smart** - text_input, checkbox auto-generate recesses
8. **Keep it simple** - add complexity only when proven necessary

---

## TodoMVC Example

### Text Input (Automatic Recess)

```boon
Element/text_input(
    style: [
        padding: [column: 19, left: 60, right: 16]
        font: [size: 24, color: Oklch[lightness: 0.42]]
        depth: 6                    -- Creates automatic recess
        transform: [move_further: 4]  -- Position relative to parent
        rounded_corners: 2
        background: [color: Oklch[lightness: 0.99]]
        material: [gloss: 0.65]     -- Shiny interior
    ]
    text: 'What needs to be done?'
    placeholder: [text: 'What needs to be done?']
)
```

**Result:** Recessed input with:
- 6px deep outer block
- ~4px deep cavity (automatic)
- Walls from padding (60px left, 16px right, 19px top/bottom)
- Glossy interior (0.65)
- Natural inset shadow from lighting

---

### Main Card (Floating Panel)

```boon
Element/stripe(
    style: [
        width: Fill
        depth: 8                      -- Panel thickness
        transform: [move_closer: 50]  -- Floats 50px above background
        rounded_corners: 4
        background: [color: Oklch[lightness: 1]]
        material: [                   -- Material properties
            gloss: 0.12               -- Very glossy
            metal: 0.02
            shine: 0.6                -- Clearcoat finish
        ]
    ]
    items: LIST {
        new_todo_input()
        todo_list()
        footer()
    }
)
```

**Result:** Floating card with:
- Drop shadow below (from lighting)
- Beveled edges (automatic from geometry)
- Glossy clearcoat finish
- All children recessed or raised relative to card surface

---

## Future Extensibility

**If needed in the future, we can add:**
- `cavity` style property for manual override
- `cutters` style property for multiple cuts
- `Model/cut()` as user-facing API
- Custom geometry operations

**For now: Keep it simple!** Automatic geometry covers 99%+ of UI cases.

---

**End of Document**
