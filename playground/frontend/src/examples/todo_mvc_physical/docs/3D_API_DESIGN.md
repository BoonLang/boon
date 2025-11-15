# Boon 3D Physically-Based Rendering API

## Overview

This document describes the 3D API design for physically-based UIs in Boon. Elements are real 3D objects with physical materials. Geometry (bevels, recesses, shadows) emerges automatically from element properties - no explicit geometric operations needed.

---

## Core Concepts

### 1. Scene vs Document

```boon
-- 3D scene with physical rendering and lighting (simple)
scene: Scene/new(
    root: root_element(...)
    lights: Lights/basic()  -- Good default lighting
)

-- 3D scene with custom lighting
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

-- Traditional 2D document (for comparison)
document: Document/new(root: root_element(...))
```

**`Scene/new`** automatically enables physically-based rendering for all elements.

**Recommended:** Use `Lights/basic()` to start, customize only if needed.

---

## Properties

### Position in 3D Space

All positioning values are in **pixels**.

#### `transform: [move_closer: N]` - Move toward viewer
```boon
transform: [move_closer: 50]  -- Moves 50px toward viewer (relative to parent)
```

#### `transform: [move_further: N]` - Move away from viewer
```boon
transform: [move_further: 4]  -- Moves 4px away from viewer (relative to parent)
```

**Key insights:**
- `move_closer` and `move_further` are **pure positioning** - they don't change object geometry!
- All positioning is **relative to parent** - no absolute positioning needed
- Use minimum **4px** for noticeable movement effects

---

### Object Geometry

All geometry values are in **pixels**.

#### `depth` - How tall/thick the object is
```boon
depth: 8  -- Object is 8px thick
```

**For built-in elements:**
- `Element/button` - Creates raised convex shape (automatic beveled edges)
- `Element/text_input` - Creates recessed well (automatic cavity with walls)
- `Element/checkbox` - Creates small recessed well

**Geometry emerges from element type + depth value.** No manual configuration needed!

#### `rounded_corners` - Corner rounding
```boon
rounded_corners: 4     -- 4px radius on all corners
rounded_corners: Fully -- Maximum rounding (pill shape)
rounded_corners: None  -- Sharp 90° corners
```

#### `borders` - Flat decorative outlines

```boon
borders: [
    width: 2                     -- Border width
    color: Oklch[...]            -- Border color
    material: [
        glow: [                  -- Optional glow effect
            color: Oklch[...]
            intensity: 0.2
        ]
    ]
]

-- Specific sides
borders: [top: [color: Oklch[...]]]
borders: [bottom: [width: 1, color: Oklch[...]]]
```

**Use for:** Focus rings, divider lines, decorative outlines, visual feedback.

**Note:** `borders` creates flat 2D lines, not 3D frames. Physical depth comes from automatic geometry generation.

---

### Materials

#### `gloss` - Surface finish (0 = matte, 1 = mirror)

**The primary material property.** Controls how rough or smooth the surface is.

```boon
gloss: 0.0   -- Matte (chalk, flat paint)
gloss: 0.3   -- Low gloss (matte plastic) - good for UI buttons
gloss: 0.5   -- Satin (brushed metal)
gloss: 0.8   -- High gloss (glossy plastic, polished wood)
gloss: 1.0   -- Mirror (chrome, glass)
```

**For most UI elements, use 0.15-0.4** (low gloss plastic look).

**Built-in elements automatically:**
- Make button exteriors slightly matte
- Make input interiors glossier than exteriors
- Create natural material contrast

#### `metal` - Metallic vs non-metallic reflections

**Rarely needed for UI.** Changes how the material reflects light:
- `0.0` (default) = Non-metal: reflections are white/colorless (plastic, wood, glass)
- `1.0` = Metal: reflections are tinted with the object's color (gold, copper, steel)

```boon
-- Non-metal button (typical UI)
material: [
    gloss: 0.3
    metal: 0.0   -- White reflections
]

-- Metal button (unusual)
background: [color: Oklch[lightness: 0.6, chroma: 0.15, hue: 30]]  -- Gold color
material: [
    gloss: 0.8
    metal: 1.0   -- Gold-tinted reflections
]
```

**For UI elements, use 0.0-0.05** or omit entirely (defaults to 0).

#### `shine` - Additional glossy layer on top

**Optional clearcoat effect.** Adds a second glossy layer over the base material, like car paint or varnished wood.

```boon
material: [
    gloss: 0.12   -- Base material (somewhat matte)
    shine: 0.6    -- Glossy clearcoat on top = sophisticated look
]
```

**Use `shine` for premium/polished surfaces.** Otherwise omit it.

**When to use:**
- **`gloss` alone:** Simple matte-to-glossy materials (most UI elements)
- **`gloss` + `shine`:** Two-layer finish for premium cards/panels
- **`gloss` + `metal`:** Actual metal surfaces (rarely needed in UI)

#### `glow` - Emissive light
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

## Automatic Geometry Generation

### Built-in Elements Create Their Own 3D Geometry

**No manual configuration needed!** Elements automatically generate appropriate geometry based on their type and properties.

#### Text Input - Recessed Well

```boon
Element/text_input(
    style: [
        depth: 6              -- Creates ~4px deep cavity automatically
        padding: [all: 10]    -- Controls wall thickness
        material: [
            gloss: 0.65       -- Shiny interior
        ]
    ]
    text: TEXT { Hello }
)
```

**Renderer automatically creates:**
- Outer block (6px deep, matte exterior)
- Inner cavity (~4px deep, glossy interior)
- Walls (thickness from padding)
- Text on cavity floor
- Natural inset shadow from lighting

---

#### Button - Raised Surface

```boon
Element/button(
    style: [
        depth: 6              -- Creates solid convex shape
        transform: [move_closer: 4]  -- Floats 4px above surface
        material: [
            gloss: 0.3
        ]
    ]
    label: TEXT { Click }
)
```

**Renderer automatically creates:**
- Solid raised block (6px deep)
- Beveled edges (convex)
- Drop shadow below (from lighting)

---

#### Checkbox - Small Recessed Box

```boon
Element/checkbox(
    style: [
        depth: 5              -- Creates small shallow well
        material: [
            gloss: 0.25
        ]
    ]
    checked: True
)
```

**Renderer automatically creates:**
- Outer box (5px deep)
- Inner cavity (~3px deep)
- 2px walls
- Checkmark on cavity floor or raised inside well

---

## Common Patterns

### 1. Raised Button with Interaction

```boon
Element/button(
    element: [
        event: [press: LINK]
        hovered: LINK
        pressed: LINK
    ]
    style: [
        depth: 6
        rounded_corners: 4
        transform: LIST { element.hovered, element.pressed } |> WHEN {
            LIST { __, True } => []                  -- Pressed flush
            LIST { True, False } => [move_closer: 6] -- Lifted on hover
            LIST { False, False } => [move_closer: 4] -- Resting raised
        }
        material: [
            gloss: 0.3
        ]
    ]
    label: TEXT { Press me }
)
```

**Key:** Button geometry stays constant, only position changes! Minimum 4px movements for visibility.

---

### 2. Recessed Input

```boon
Element/text_input(
    style: [
        depth: 6
        rounded_corners: 4
        material: [
            gloss: 0.65
        ]
        transform: [move_further: 4]
        padding: [all: 10]
    ]
    text: TEXT { Type here... }
)
```

**Result:** Input well recessed 4px into parent, walls from padding, automatic inset shadow.

---

### 3. Floating Card with Multiple Elements

```boon
Element/stripe(
    style: [
        width: Fill
        depth: 8
        transform: [move_closer: 50]  -- Card floats 50px above background
        rounded_corners: 4
        material: [
            gloss: 0.12    -- Very glossy
            metal: 0.02
            shine: 0.6     -- Clearcoat finish
        ]
    ]
    items: LIST {
        -- Header (flush with card surface)
        Element/text(content: TEXT { Header })

        -- Input (recessed into card)
        Element/text_input(
            style: [
                transform: [move_further: 4]
                depth: 6
                material: [
                    gloss: 0.65
                ]
            ]
            text: TEXT { Username }
        )

        -- Button (raised from card)
        Element/button(
            style: [
                transform: [move_closer: 4]
                depth: 6
                material: [
                    gloss: 0.3
                ]
            ]
            label: TEXT { Submit }
        )
    }
)
```

**Result:** Card with automatic drop shadow, input with automatic inset shadow, button with automatic elevation shadow.

---

### 4. Focus State with Glowing Border

```boon
Element/text_input(
    element: [focused: LINK]
    style: [
        depth: 6
        borders: element.focused |> WHEN {
            True => [
                width: 2
                color: Oklch[lightness: 0.68, chroma: 0.08, hue: 220]
                material: [
                    glow: [
                        color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220]
                        intensity: 0.2
                    ]
                ]
            ]
            False => []
        }
    ]
    text: TEXT { ... }
)
```

**Result:** Input with glowing flat border when focused. Physical geometry unchanged.

---

## Material Properties Reference

### `gloss` (Surface roughness)
- **0.0 - 0.2:** Matte (chalk, flat paint, unfinished wood)
- **0.2 - 0.4:** Low gloss (matte plastic, concrete)
- **0.4 - 0.6:** Satin (brushed metal, semi-gloss paint)
- **0.6 - 0.8:** High gloss (glossy plastic, polished wood)
- **0.8 - 1.0:** Mirror (chrome, glass, polished metal)

### `metal`
- **0.0:** Non-metal (plastic, wood, fabric, paper)
- **0.5:** Semi-metallic (metallic paint)
- **1.0:** Full metal (steel, aluminum, copper)

### `shine` (Clearcoat layer)
- **0.0:** No clearcoat
- **0.5:** Moderate coating (satin varnish)
- **1.0:** Full clearcoat (car paint, lacquer)

---

## Scene Lighting

### Simple Setup (Recommended)

**For most UIs, use the built-in helper:**

```boon
scene: Scene/new(
    root: root_element(...)
    lights: Lights/basic()
)
```

This provides good default lighting (directional + ambient) without configuration.

### Custom Lighting

**For full control, specify custom lights:**

```boon
scene: Scene/new(
    root: root_element(...)
    lights: LIST {
        Light/directional(
            azimuth: 30           -- Degrees (0-360°) - rotation clockwise from top
            altitude: 45          -- Degrees (0-180°) - angle from screen toward viewer
            spread: 1             -- Multiplier (0 = sharp, 1 = sun-like, 2+ = softer)
            intensity: 1.2        -- Multiplier (1 = normal)
            color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
        )
        Light/point(
            at: [x: 200, y: 150, z: 300]  -- Pixels - position in viewport space
            radius: 30                     -- Pixels - sphere radius
            intensity: 3.0                 -- Multiplier
            color: Oklch[lightness: 0.95, chroma: 0.2, hue: 30]
            range: 150                     -- Pixels - falloff distance
        )
        Light/ambient(
            intensity: 0.4        -- Multiplier
            color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
        )
    }
)
```

### Lights/basic() Implementation

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

#### Light/directional - Distant Light Source

Simulates a distant light like the sun. Casts parallel shadows.

```boon
Light/directional(
    azimuth: 30           -- Degrees (0-360°)
    altitude: 45          -- Degrees (0-180°)
    spread: 1             -- Multiplier
    intensity: 1.2        -- Multiplier
    color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
)
```

**Parameters:**
- **`azimuth`**: 0-360° clockwise from top (0° = top, 90° = right, 180° = bottom, 270° = left)
- **`altitude`**: 0-180° from screen toward viewer (0° = parallel to screen, 45° = typical overhead, 90° = perpendicular, 135-180° = backlight)
- **`spread`**: Normalized multiplier where 1 = sun-like shadows (0 = sharp point source, 1 = realistic sun, 2-10 = progressively softer)
- **`intensity`**: Brightness multiplier (1 = normal)
- **`color`**: Light color (warm whites for key lights)

**Always casts shadows.**

#### Light/point - Localized Light Source

Simulates a localized light like a bulb or softbox.

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
- **`at`**: Position in pixels [x, y, z] where x=left, y=top, z=depth from screen
- **`radius`**: Sphere radius in pixels (larger = softer shadows)
- **`intensity`**: Brightness multiplier
- **`color`**: Light color
- **`range`**: Falloff distance in pixels

**Casts shadows.**

#### Light/ambient - Omnidirectional Fill

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

### Units Reference Table

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
- No fake shadow properties needed!

---

## Summary of Key Principles

1. **`Scene/new`** enables physically-based rendering automatically
2. **All values in pixels** - depth, movement, corner radii use pixel units
3. **Minimum 4px movements** for noticeable effects (otherwise remove them)
4. **`move_closer`/`move_further`** are pure positioning (don't change geometry)
5. **All positioning is relative to parent** - no absolute positioning needed
6. **Geometry emerges automatically** from element type + properties
7. **Built-in elements know their shape** - buttons raised, inputs recessed
8. **`gloss`** is the main material property (0 = matte to 1 = mirror)
9. **`metal`** controls metallic vs non-metallic reflections (rarely used)
10. **`shine`** adds clearcoat layer (optional, for premium surfaces)
11. **`borders`** creates flat decorative outlines (not 3D frames)
12. **Physical lighting creates real shadows** - no fake shadow properties

---

## TodoMVC Example

```boon
scene: Scene/new(
    root: root_element(PASS: [store: store])
    lights: Lights/basic()  -- Simple default lighting
)

FUNCTION main_panel() {
    Element/stripe(
        element: [tag: Section]
        style: [
            width: Fill
            depth: 8
            transform: [move_closer: 50]  -- Floats 50px above background
            rounded_corners: 4
            material: [
                gloss: 0.12        -- Very glossy card
                metal: 0.02
                shine: 0.6
            ]
        ]
        items: LIST {
            new_todo_input()
            todo_list()
            footer()
        }
    )
}

FUNCTION new_todo_input() {
    Element/text_input(
        style: [
            transform: [move_further: 4]  -- Recessed 4px into card
            depth: 6
            rounded_corners: 2
            material: [
                gloss: 0.65
            ]
            -- Cavity geometry automatic!
        ]
        text: TEXT { What needs to be done? }
    )
}

FUNCTION todo_button() {
    Element/button(
        style: [
            depth: 6
            rounded_corners: Fully
            transform: LIST { element.hovered, element.pressed } |> WHEN {
                LIST { __, True } => []                  -- Pressed flush
                LIST { True, False } => [move_closer: 6] -- Lifted 6px
                LIST { False, False } => [move_closer: 4] -- Resting 4px up
            }
            material: [
                gloss: 0.25
                metal: 0.03
            ]
            -- Raised geometry automatic!
        ]
        label: TEXT { Remove }
    )
}
```

---

## Implementation Notes (For Renderer Developers)

**Internal geometry generation:**
- `Element/text_input` uses `Model/cut(from: outer_block, remove: cavity_block)` internally
- Cavity dimensions calculated from `depth`, `padding`, `rounded_corners`
- Wall thickness emerges from size difference between outer and cavity
- Cavity interior automatically made glossier than exterior
- Text positioned on cavity floor automatically

**Users never see these details!** They just write semantic elements with visual properties.

---

**End of 3D API Design Document**
