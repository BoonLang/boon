# Boon 3D Physically-Based Rendering API

## Overview

This document describes the 3D API design for physically-based UIs in Boon. Elements are real 3D objects that can be raised, recessed, and have physical materials.

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

#### `rounded_corners` - Corner rounding
```boon
rounded_corners: 4     -- 4px radius on all corners
rounded_corners: Fully -- Maximum rounding (pill shape)
rounded_corners: None  -- Sharp 90° corners
```

#### `edges` - Edge treatment (defines raised vs recessed shape!)
```boon
-- Raised object (button, card) - beveled edges
edges: [
    side: Outside
    radius: 0.5
]

-- Recessed object (input, well) - filleted edges
edges: [
    side: Inside
    radius: 0.8
]

-- Advanced: different top and bottom
edges: [
    top: [side: Outside, radius: 0.5]
    bottom: [side: Inside, radius: 0.8]
]
```

**The `side` property is what makes an object raised or recessed!**

#### `rim` (formerly `borders`) - Raised/recessed rim around object
```boon
rim: [
    width: 2              -- Rim width
    elevation: 1          -- How much rim sticks up (or down if negative)
    radius: 0.3           -- Bevel/fillet on rim
    color: Oklch[...]     -- Rim color
    gloss: 0.4            -- Rim material
]
```

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

#### `metal` - Metallic vs non-metallic reflections

**Rarely needed for UI.** Changes how the material reflects light:
- `0.0` (default) = Non-metal: reflections are white/colorless (plastic, wood, glass)
- `1.0` = Metal: reflections are tinted with the object's color (gold, copper, steel)

```boon
-- Non-metal button (typical UI)
gloss: 0.3
metal: 0.0   -- White reflections

-- Metal button (unusual)
background: [color: Oklch[lightness: 0.6, chroma: 0.15, hue: 30]]  -- Gold color
gloss: 0.8
metal: 1.0   -- Gold-tinted reflections
```

**For UI elements, use 0.0-0.05** or omit entirely (defaults to 0).

#### `shine` - Additional glossy layer on top

**Optional clearcoat effect.** Adds a second glossy layer over the base material, like car paint or varnished wood.

```boon
gloss: 0.12   -- Base material (somewhat matte)
shine: 0.6    -- Glossy clearcoat on top = sophisticated look
```

**Use `shine` for premium/polished surfaces.** Otherwise omit it.

**When to use:**
- **`gloss` alone:** Simple matte-to-glossy materials (most UI elements)
- **`gloss` + `shine`:** Two-layer finish for premium cards/panels
- **`gloss` + `metal`:** Actual metal surfaces (rarely needed in UI)

#### `glow` - Emissive light
```boon
glow: [
    color: Oklch[lightness: 0.7, chroma: 0.08, hue: 220]
    intensity: 0.15
]
```

**Use for:** Focus indicators, active states, notifications.

---

## How Objects Define Their Shape

### The Key Insight: Edge Placement

**Outside edges** → Object is **raised/convex** (button, card, badge)
```
Side view:
  ╱─────╲  ← Top beveled (outside)
 │Button │
 └───────┘  ← Bottom sharp
```

**Inside edges** → Object is **recessed/concave** (input, well, tray)
```
Side view:
 ╲──────╱  ← Top concave (inside)
 │ Well │
 ╰──────╯  ← Bottom filleted (inside)
```

---

## Parent-Child Interaction

### Children Define ALL Their Geometry

When a child is positioned relative to parent:
1. Child defines its complete 3D volume (including bevels/fillets)
2. Parent automatically "makes room" by cutting holes where needed
3. No special "cavity" or "recess" logic in parent

```boon
-- Parent card
Element/stripe(
    style: [
        transform: [move_closer: 50]
        depth: 8
    ]
    items: LIST {
        -- Child input recessed into card
        Element/text_input(
            style: [
                transform: [move_further: 4]
                depth: 6
                edges: [side: Inside]  -- Well shape
            ]
        )
    }
)
```

**Renderer automatically:**
- Positions input 4px back from card surface
- Cuts hole in card where input's top edge intersects card surface
- Renders both objects normally

---

## Common Patterns

### 1. Raised Button
```boon
Element/button(
    style: [
        depth: 6
        rounded_corners: 4
        gloss: 0.3
        transform: [move_closer: 4]
        -- edges: [side: Outside] (automatic for buttons)
    ]
    label: 'Click me'
)
```

**Result:** Button with beveled edges, floats 4px above parent.

---

### 2. Recessed Input
```boon
Element/text_input(
    style: [
        depth: 6
        rounded_corners: 4
        gloss: 0.65
        transform: [move_further: 4]
        -- edges: [side: Inside] (automatic for inputs)
    ]
    text: '...'
)
```

**Result:** Input well recessed 4px into parent, with filleted edges.

---

### 3. Button Press Interaction
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
        gloss: 0.3
        -- edges: Outside (stays convex even when pressed!)
    ]
    label: 'Press me'
)
```

**Key:** Button keeps its convex shape, only position changes! Minimum 4px movements for visibility.

---

### 4. Input with Raised Border Frame
```boon
Element/text_input(
    style: [
        depth: 3
        rounded_corners: 2
        gloss: 0.65
        transform: [move_further: 1]
        rim: [
            width: 2
            elevation: 1         -- Rim raised 1 unit above parent
            radius: 0.3
            color: Oklch[lightness: 0.88]
        ]
        edges: [
            side: Inside         -- Well shape
            radius: 0.8          -- Smooth fillet
        ]
    ]
    text: '...'
)
```

**Result:**
```
Visual (side view):
        ┌─────────┐  ← Raised rim (elevation: 1)
        │╲       ╱│  ← Concave well opening
Parent══│ │Input│ │══
        │ ╰─────╯ │  ← Filleted bottom
        └─────────┘
```

Light from above creates natural inset shadow effect!

---

### 5. Floating Input (Above Parent)
```boon
Element/text_input(
    style: [
        depth: 6
        rounded_corners: 4
        edges: [side: Inside]    -- Still a well shape!
        transform: [move_closer: 24]  -- Floats 24px above parent
    ]
)
```

**Result:** Input floats above parent but still looks like a well (concave), not a button.

---

### 6. Glossy Card with Multiple Depths
```boon
Element/stripe(
    element: [tag: Section]
    direction: Column
    gap: 0
    style: [
        width: Fill
        depth: 8
        transform: [move_closer: 50]  -- Card floats 50px above background
        rounded_corners: 4
        gloss: 0.12        -- Very glossy
        metal: 0.02
        shine: 0.6         -- Clearcoat finish
    ]
    items: LIST {
        -- Header (flush with card surface)
        Element/container(
            style: [
                transform: []  -- No movement = at surface
                padding: [all: 20]
            ]
            child: 'Header'
        )

        -- Input (recessed into card)
        Element/text_input(
            style: [
                transform: [move_further: 4]
                depth: 6
                gloss: 0.65
            ]
        )

        -- Button (raised from card)
        Element/button(
            style: [
                transform: [move_closer: 4]
                depth: 6
                gloss: 0.3
            ]
            label: 'Submit'
        )
    }
)
```

---

## Automatic Defaults

### Element types have smart defaults:

```boon
Element/button(...)
-- Automatic: edges: [side: Outside]

Element/text_input(...)
-- Automatic: edges: [side: Inside]

Element/checkbox(...)
-- Automatic: edges: [side: Outside]

Element/label(...)
-- Automatic: no depth, flat on surface
```

### Can override:
```boon
Element/text_input(
    style: [
        edges: [side: Outside]  -- Weird raised input!
    ]
)
```

---

## Edge Configurations

### Outside (Raised/Convex)
```boon
edges: [side: Outside, radius: 0.5]
```
```
  ╱─────╲  ← Beveled top
 │Object │
 └───────┘  ← Sharp bottom
```

### Inside (Recessed/Concave)
```boon
edges: [side: Inside, radius: 0.8]
```
```
 ╲──────╱  ← Concave top
 │Object │
 ╰──────╯  ← Filleted bottom
```

### Both Outside (Floating Panel)
```boon
edges: [
    top: [side: Outside, radius: 0.5]
    bottom: [side: Outside, radius: 0.5]
]
```
```
  ╱─────╲  ← Top beveled
 │ Panel │
 ╰───────╯  ← Bottom beveled
```

### Both Inside (Tube/Pipe)
```boon
edges: [
    top: [side: Inside, radius: 0.8]
    bottom: [side: Inside, radius: 0.8]
]
```
```
 ╲──────╱  ← Top concave
 │ Tube │
 ╱──────╲  ← Bottom concave
```

---

## Material Properties Reference

### `gloss` (Combined roughness + shine)
- **0.0 - 0.2:** Matte (chalk, flat paint, unfinished wood)
- **0.2 - 0.4:** Low gloss (matte plastic, concrete)
- **0.4 - 0.6:** Satin (brushed metal, semi-gloss paint)
- **0.6 - 0.8:** High gloss (glossy plastic, polished wood)
- **0.8 - 1.0:** Mirror (chrome, glass, polished metal)

### `metal`
- **0.0:** Non-metal (plastic, wood, fabric, paper)
- **0.5:** Semi-metallic (metallic paint)
- **1.0:** Full metal (steel, aluminum, copper)

### `shine` (Clearcoat/additional gloss layer)
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

**Note:** Shadow casting is automatic based on light type:
- Directional lights: Always cast shadows
- Ambient lights: Never cast shadows

---

## Summary of Key Principles

1. **`Scene/new`** enables physically-based rendering automatically
2. **All values in pixels** - depth, movement, corner radii use pixel units
3. **Minimum 4px movements** for noticeable effects (otherwise remove them)
4. **`move_closer`/`move_further`** are pure positioning (don't change geometry)
5. **All positioning is relative to parent** - no absolute positioning needed
6. **`edges: [side: Inside/Outside]`** defines object shape (raised vs recessed)
7. **Children own all their geometry** including bevels/fillets
8. **Parent just "makes room"** by cutting holes where children intersect
9. **`gloss`** is the main material property (0 = matte to 1 = mirror)
10. **`metal`** controls metallic vs non-metallic reflections
11. **`rim`** creates raised/recessed borders around objects
12. **Automatic defaults** for common element types (button, input, checkbox)
13. **Light naturally creates shadows** - no fake "inset shadow" properties

---

## TodoMVC Example (Simplified)

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
            gloss: 0.12            -- Very glossy card
            metal: 0.02
            shine: 0.6
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
            gloss: 0.65
            -- edges: Inside (automatic for inputs)
        ]
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
            gloss: 0.25
            metal: 0.03
            -- edges: Outside (automatic for buttons)
        ]
    )
}
```
