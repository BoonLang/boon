# Boon 3D Physically-Based Rendering API

## Overview

This document describes the 3D API design for physically-based UIs in Boon. Elements are real 3D objects with physical materials. Geometry (bevels, recesses, shadows) emerges automatically from element properties - no explicit geometric operations needed.

---

## Core Concepts

### 1. Scene vs Document

```boon
-- 3D scene with physical rendering and lighting
scene: Scene/new(
    root: root_element(...)
    lighting: LIST {
        [type: Directional, intensity: 2.8]
        [type: Ambient, intensity: 0.6]
    }
)

-- Traditional 2D document (for comparison)
document: Document/new(root: root_element(...))
```

**`Scene/new`** automatically enables physically-based rendering for all elements.

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
    text: 'Hello'
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
    label: 'Click'
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
    label: 'Press me'
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
    text: 'Type here...'
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
        Element/text(content: 'Header')

        -- Input (recessed into card)
        Element/text_input(
            style: [
                transform: [move_further: 4]
                depth: 6
                material: [
                    gloss: 0.65
                ]
            ]
            text: 'Username'
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
            label: 'Submit'
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
    text: '...'
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

```boon
scene: Scene/new(
    root: root_element(...)
    lighting: LIST {
        [
            type: Directional       -- Main light (sun/key light)
            direction: [x: -0.3, y: -0.7, z: -0.6]
            color: Oklch[lightness: 0.98, chroma: 0.015, hue: 65]
            intensity: 2.8
            -- Directional lights automatically cast shadows
        ]
        [
            type: Ambient           -- Ambient fill light
            color: Oklch[lightness: 0.8, chroma: 0.01, hue: 220]
            intensity: 0.6
            -- Ambient lights never cast shadows
        ]
    }
)
```

**Shadow casting is automatic:**
- Directional lights: Always cast shadows
- Ambient lights: Never cast shadows
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
    lighting: LIST {
        [type: Directional, intensity: 2.8]
        [type: Ambient, intensity: 0.6]
    }
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
        text: 'What needs to be done?'
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
        label: 'Remove'
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
