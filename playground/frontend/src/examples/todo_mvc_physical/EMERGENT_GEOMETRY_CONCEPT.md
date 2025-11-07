# Emergent 3D Geometry Concept

## The Paradigm Shift

### From Explicit to Emergent

**Current Paradigm (Explicit):**
"I declare this button has outward bevels, this input has inward fillets"

**New Paradigm (Emergent/Interactive):**
"Objects are just positioned in 3D space. Bevels and fillets emerge naturally from their spatial relationships based on global rules"

---

## Core Concept: Physical Interaction Model

Think of it like **real physical objects**:

1. **Parent = base surface** (like a table or tray)
2. **Child moves closer** = object sits ON the surface → creates **outward bevel** at contact
3. **Child moves further** = object sinks INTO surface → parent cuts cavity, creates **inward fillet**
4. **Global settings** = material properties (how sharp/rounded are transitions)
5. **Explicit overrides** = manual control when needed (rare)

---

## Geometric Principles

### Scenario 1: Child Raised Above Parent (move_closer)

```
Side view:

Child moves UP (toward viewer):

        ╱───╲         ← Child's bottom edge
       │Child│
    ───┴─────┴───     ← Parent surface
    │  Parent  │

Result: OUTWARD BEVEL forms naturally where child meets parent
```

**Why bevel forms:**
- Child is "sitting on" parent
- Contact creates a raised edge
- Global setting determines bevel radius/angle

### Scenario 2: Child Recessed Into Parent (move_further)

```
Side view:

Child moves DOWN (away from viewer):

    ───┬─────┬───     ← Parent surface
       │Child│
        ╲───╱         ← Child's top edge

Result: INWARD FILLET forms naturally where parent cuts cavity
```

**Why fillet forms:**
- Child is "pressed into" parent
- Parent material "wraps" into cavity
- Global setting determines fillet radius

### Scenario 3: Child Flush with Parent

```
Side view:

Child at same Z as parent surface:

    ───┤Child├───     ← Both at same level
    │  Parent  │

Result: NO bevel/fillet, just edge meeting edge
```

---

## Boolean Operations Implied

This approach implies automatic **CSG (Constructive Solid Geometry)** operations:

### When child moves closer (above parent):
```
Operation: UNION (additive)
- Child sits on top
- Contact zone creates bevel
- No subtraction needed
```

### When child moves further (into parent):
```
Operation: SUBTRACTION (subtractive)
- Parent volume minus child volume
- Cut edge creates fillet
- Parent "wraps around" cavity
```

### Visual:
```
move_closer (UNION):
    Child
   ┌─────┐
───┴─────┴───  Parent (child added ON TOP)
└───────────┘

move_further (SUBTRACT):
───┬─────┬───  Parent (child carved OUT)
   │Child│
   └─────┘
```

---

## Key Insight: Current 2D Uses Fake Shadows

Looking at the 2D TodoMVC code:
- `shadows: [direction: Inwards]` - FAKE inset shadow
- `shadows: LIST { [y: 2, blur: 4] ... }` - FAKE drop shadows

**With emergent 3D, these become REAL shadows from REAL geometry!**

---

## Translation: 2D → 3D Physical

### Current (2D with fake shadows):
```boon
-- Main panel with fake drop shadow
Element/stripe(
    style: [
        shadows: LIST {
            [y: 2, blur: 4, color: Oklch[alpha: 0.2]]
            [y: 25, blur: 50, color: Oklch[alpha: 0.1]]
        }
        background: [color: Oklch[lightness: 1]]
    ]
)

-- Input with fake inset shadow
Element/text_input(
    style: [
        shadows: LIST {
            [direction: Inwards, y: -2, blur: 1, color: Oklch[alpha: 0.03]]
        }
    ]
)
```

### Emergent 3D (real geometry, real shadows):
```boon
-- Main panel floats above background → REAL drop shadow
Element/stripe(
    style: [
        depth: 8
        transform: [move_closer: 50]  -- Float 50px above background
        background: [color: Oklch[lightness: 1]]
        -- Shadow cast automatically by scene lighting!
    ]
)

-- Input recessed into panel → REAL inset shadow
Element/text_input(
    style: [
        depth: 6
        transform: [move_further: 4]  -- Recess 4px into panel
        -- Inset shadow appears naturally from recessed geometry!
    ]
)
```

**NO EXPLICIT SHADOWS NEEDED!** Lighting creates them automatically.

---

## Scene-Level Design Control

The KILLER feature: **change entire design aesthetic with ONE setting**

### Professional/Classic (soft, rounded):
```boon
scene: Scene/new(
    root: root_element(...)
    lights: Lights/basic()  -- Good default for most UIs
    -- Global defaults (can be omitted, these ARE the defaults)
    -- geometry: [
    --     edge_radius: 2              -- Rounded transitions
    --     bevel_angle: 45        -- Standard bevels
    -- ]
)
```

### Neobrutalism (sharp, hard):
```boon
scene: Scene/new(
    root: root_element(...)
    lights: LIST {
        Light/directional(
            azimuth: 90
            altitude: 60
            spread: 0
            intensity: 1.5
            color: Oklch[lightness: 1.0, chroma: 0.0, hue: 0]
        )
        Light/ambient(
            intensity: 0.3
            color: Oklch[lightness: 0.7, chroma: 0.0, hue: 0]
        )
    }
    geometry: [
        edge_radius: 0                -- CHAMFERED (sharp) transitions
        bevel_angle: 30          -- Aggressive sharp bevels
    ]
)
```

### Neumorphism (soft, subtle):
```boon
scene: Scene/new(
    root: root_element(...)
    lights: LIST {
        Light/directional(
            azimuth: 0
            altitude: 30
            spread: 2
            intensity: 1.0
            color: Oklch[lightness: 0.95, chroma: 0.01, hue: 50]
        )
        Light/ambient(
            intensity: 0.5
            color: Oklch[lightness: 0.85, chroma: 0.01, hue: 220]
        )
    }
    geometry: [
        edge_radius: 4           -- Very rounded
        bevel_angle: 60     -- Gentle bevels
    ]
)
```

### Glassmorphism:
```boon
scene: Scene/new(
    root: root_element(...)
    lights: LIST {
        Light/directional(
            azimuth: 345
            altitude: 40
            spread: 1.5
            intensity: 1.1
            color: Oklch[lightness: 0.97, chroma: 0.02, hue: 200]
        )
        Light/ambient(
            intensity: 0.45
            color: Oklch[lightness: 0.82, chroma: 0.01, hue: 220]
        )
    }
    geometry: [
        edge_radius: 2
        bevel_angle: 45
    ]
)

-- Elements use glass material
Element/stripe(
    style: [
        depth: 8
        background: [color: Oklch[lightness: 0.95, alpha: 0.3]]
        backdrop_blur: 10        -- Glass effect
        material: [
            gloss: 0.8           -- Very reflective
        ]
    ]
)
```

**Same element code, completely different look!** Just change scene settings.

---

## Complete TodoMVC 3D Translation

### Minimal Changes Needed:

```boon
-- Change Document to Scene
scene: Scene/new(
    root: root_element(PASS: [store: store])
    lights: Lights/basic()  -- Simple default lighting
)

-- Main panel
FUNCTION main_panel() {
    Element/stripe(
        element: [tag: Section]
        direction: Column
        gap: 0
        style: [
            width: Fill
            depth: 8                      -- NEW: Object thickness
            transform: [move_closer: 50]  -- NEW: Float above background
            background: [color: Oklch[lightness: 1]]
            -- REMOVED: shadows (now automatic!)
        ]
        items: LIST {
            new_todo_title_text_input()
                |> LINK { PASSED.store.elements.new_todo_title_text_input }
            -- ... rest same
        }
    )
}

-- Input
FUNCTION new_todo_title_text_input() {
    Element/text_input(
        element: [
            event: [change: LINK, key_down: LINK]
        ]
        style: [
            padding: [column: 19, left: 60, right: 16]
            font: [size: 24, color: Oklch[lightness: 0.42]]
            depth: 6                       -- NEW: Well depth
            transform: [move_further: 4]   -- NEW: Recess into panel
            background: [color: Oklch[lightness: 0.99]]
            -- REMOVED: fake inset shadow
            -- REMOVED: background alpha trick
        ]
        -- ... rest same
    )
}

-- Todo items
FUNCTION todo_element(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 5
        style: [
            width: Fill
            depth: 2                      -- NEW: Subtle thickness
            transform: [move_closer: 2]   -- NEW: Slightly raised
            background: [color: Oklch[lightness: 1]]
            font: [size: 24]
        ]
        items: -- ... same
    )
}

-- Filter buttons
FUNCTION filter_button(filter) {
    Element/button(
        element: [event: [press: LINK], hovered: LINK, pressed: LINK]
        style: [
            padding: [row: 8, column: 4]
            rounded_corners: 6
            depth: 5                              -- NEW: Button thickness
            transform: LIST { selected, element.hovered, element.pressed } |> WHEN {
                LIST { __, __, True } => []                  -- Pressed flat
                LIST { True, __, False } => [move_closer: 6] -- Selected raised
                LIST { False, True, False } => [move_closer: 4] -- Hovered
                LIST { False, False, False } => []           -- Resting flat
            }
            -- REMOVED: outline (replaced by real 3D depth change)
        ]
        label: filter |> WHEN { ... }
    )
}
```

---

## What Gets Removed/Replaced

| 2D Property | 3D Replacement | Benefit |
|-------------|----------------|---------|
| `shadows: [direction: Inwards]` | `transform: [move_further: N]` | Real geometry, real shadow |
| `shadows: LIST { [y, blur] }` | `transform: [move_closer: N]` | Scene lighting handles it |
| `outline: [side: Inner]` | `depth` + `transform` | Real 3D edge, not fake outline |
| `background: [alpha: 0.003]` | Actual surface color + depth | Physically accurate |
| Complex shadow stacks | Single `depth` + position | Massively simpler |

---

## Code Reduction Analysis

### Before (2D fake shadows):
```boon
shadows: LIST {
    [y: 2, blur: 4, color: Oklch[alpha: 0.2]]
    [y: 25, blur: 50, color: Oklch[alpha: 0.1]]
}
shadows: LIST {
    [direction: Inwards, y: -2, blur: 1, color: Oklch[alpha: 0.03]]
}
shadows: [
    [y: 1, blur: 1, color: Oklch[alpha: 0.2]]
    [y: 8, spread: -3, color: Oklch[lightness: 0.973]]
    [y: 9, blur: 1, spread: -3, color: Oklch[alpha: 0.2]]
    [y: 16, spread: -6, color: Oklch[lightness: 0.973]]
    [y: 17, blur: 2, spread: -6, color: Oklch[alpha: 0.2]]
]
```

### After (3D physical):
```boon
depth: 8
transform: [move_closer: 50]
-- That's it! Shadows are automatic.
```

**~30 lines of shadow config → 2 lines of geometry**

---

## What Users Specify

### Per-element (common):
```boon
depth: 6                      -- How thick is it?
transform: [move_closer: 4]   -- Where is it?
rounded_corners: 4            -- XY plane rounding
```

### Per-element (rare overrides):
```boon
edge_radius: 8                -- Override global rounding
bevel_angle: 30          -- Override global bevel
```

### Scene-level (design system):
```boon
-- Simple (recommended for most UIs)
lights: Lights/basic()

-- Or custom:
lights: LIST {
    Light/directional(
        azimuth: 30
        altitude: 45
        spread: 1
        intensity: 1.2
        color: Oklch[...]
    )
    Light/ambient(
        intensity: 0.4
        color: Oklch[...]
    )
}
geometry: [                   -- Optional global overrides
    edge_radius: 0            -- For neobrutalism, etc.
    bevel_angle: 30      -- For aggressive bevels
]
```

---

## What Users DON'T Specify (Hidden Complexity)

These are **hardcoded defaults or internal renderer settings**:

```javascript
// Internal renderer defaults (not exposed in API):
const GEOMETRY_DEFAULTS = {
  edge_radius: 2,              // How rounded (0 = chamfer)
  bevel_angle: 45,        // Bevel slope
  min_intersection: 1,         // Overlap threshold (px)
  contact_tolerance: 0.5,      // How close = "touching"
}

// Automatic behaviors (not configurable):
- Child.z > Parent.surface.z → outward bevel (raised)
- Child.z < Parent.surface.z → inward fillet (recessed)
- Child.z == Parent.surface.z → no bevel (flush)
- Overlap < min_intersection → no interaction
- Scene lighting → automatic shadow casting
```

**Users never see these.** They just position elements and it works.

---

## Advanced: Multi-Level Interactions

### Three-level hierarchy:
```boon
-- Background (Z=0)
  └─ Card (Z=50, depth=8)
      ├─ Input (Z=46, depth=6)      -- 4px into card
      ├─ Button (Z=54, depth=6)     -- 4px above card
      └─ Label (Z=50, depth=0)      -- flush with card
```

**Automatic geometry:**
- Card/Background: Outward bevel at card bottom
- Input/Card: Inward fillet where input recesses into card
- Button/Card: Outward bevel where button sits on card
- Label/Card: No bevel (flush, coplanar)

**All determined by Z-positions alone!**

---

## Edge Cases & Interaction Rules

### Rule 1: Overlap Detection
```javascript
overlap_amount = abs(child.z - parent.surface.z)

if (overlap_amount >= min_intersection) {
    if (child.z > parent.surface.z) {
        create_outward_bevel(overlap_amount)
    } else {
        create_inward_fillet(overlap_amount)
    }
}
```

### Rule 2: Partial Overlap
```
Child depth: 6px
Parent surface: Z=50
Child bottom: Z=48
Child top: Z=54

Result:
- Z=48-50: Inward fillet (2px below surface)
- Z=50-54: Outward bevel (4px above surface)
- Creates "shelf" geometry
```

### Rule 3: No Contact = No Interaction
```
Child floating far above parent (Z difference > depth):
→ No bevel/fillet, objects are independent
```

---

## Complex Example: Input with Raised Rim

```boon
Element/text_input(
    style: [
        depth: 6
        transform: [move_further: 4]
        rim: [
            width: 2
            depth: 3
            transform: [move_closer: 2]  -- Rim raised above parent
        ]
    ]
)
```

**What happens automatically:**
1. Input body sinks 4px into parent → creates cavity with fillet
2. Rim rises 2px above parent → creates outward bevel
3. Rim surrounds cavity → creates complex transition geometry
4. **All geometry emerges from spatial relationships!**

---

## Rendering Pipeline (Conceptual)

```
1. Build scene hierarchy with Z-positions
2. For each parent-child pair:
   a. Calculate intersection/overlap
   b. Determine contact zones
   c. Apply global edge settings
   d. Apply local overrides
   e. Generate bevel/fillet geometry
3. Perform boolean operations (union/subtract)
4. Render final mesh with lighting
5. Cast shadows automatically
```

---

## Comparison: Explicit vs Emergent

| Aspect | Explicit (Current) | Emergent (Proposed) |
|--------|-------------------|---------------------|
| **Mental Model** | "What shape is it?" | "Where is it positioned?" |
| **Code** | `edges: [side: Outside]` | `transform: [move_closer: 4]` |
| **Shadows** | Manual fake shadows | Automatic real shadows |
| **Consistency** | Must remember rules | Physics handles it |
| **Control** | Direct | Through spatial relationships |
| **Complexity** | Medium | High (but hidden) |
| **Surprise** | Low | Medium (emergent behavior) |
| **Power** | Good | Very high (composition) |
| **Design switching** | Manual per-element | Global scene settings |

---

## Benefits

1. **Massively simpler**: No fake shadows, just position + depth
2. **Design flexibility**: Change entire aesthetic with scene settings
3. **Physically accurate**: Real geometry = real shadows
4. **Intuitive**: "Float above" = raised, "press into" = recessed
5. **Boon philosophy**: Minimal, discoverable, playful
6. **Composable**: Multi-level hierarchies work automatically
7. **Consistent**: Same rules everywhere

---

## Challenges

1. **Computationally expensive**: Real-time CSG operations are hard
2. **Non-obvious behavior**: "Why did a fillet appear here?"
3. **Hard to predict**: Emergent systems can surprise users
4. **Debugging difficulty**: Must visualize 3D spatial relationships
5. **Edge cases**: Floating elements? Overlapping siblings?
6. **Performance**: What about 100s of elements?

---

## Potential Solutions

### Performance:
- Use approximate/fake geometry for UI (not true CSG)
- Pre-compute common patterns
- GPU shaders for geometry generation
- Cache geometry when positions don't change
- Use simple heuristics before expensive operations

### Predictability:
- Good defaults (most cases work automatically)
- Visual debugging tools (show Z-positions, contact zones)
- Clear documentation with examples
- Escape hatch: explicit overrides when needed

---

## Hybrid Approach

Combine both paradigms:

```boon
-- Default: Emergent (automatic)
Element/button(
    style: [
        depth: 6
        transform: [move_closer: 4]
        -- Automatic bevel based on position
    ]
)

-- Override: Explicit (manual control)
Element/button(
    style: [
        depth: 6
        transform: [move_closer: 4]
        edge_treatment: [          -- Explicit override
            type: Manual
            bottom: [curve: Outward, radius: 8]
            top: [curve: Outward, radius: 2]
        ]
    ]
)

-- Disable: None (sharp edges)
Element/block(
    style: [
        depth: 4
        edge_treatment: None       -- No interaction, sharp 90° edges
    ]
)
```

---

## Why This Is Revolutionary

**No other UI framework does emergent 3D geometry based on spatial relationships.**

This could be a **defining feature of Boon**:
- Position objects in 3D space
- Physics determines the geometry
- Lighting creates real shadows
- Change design aesthetic globally
- Intuitive for beginners ("just move things around")
- Powerful for experts (multi-level composition)

**This shifts UI development from "painting shadows" to "arranging physical objects in space."**

---

## Next Steps

1. **Prototype the concept**: Start with simple parent-child cases
2. **Define hardcoded defaults**: `edge_radius`, `bevel_angle`, etc.
3. **Implement basic CSG**: Union (raised) and subtract (recessed)
4. **Add global geometry settings**: Scene-level design control
5. **Create escape hatches**: Manual overrides when needed
6. **Optimize performance**: Cache, approximate, GPU shaders
7. **Document thoroughly**: Examples, edge cases, mental models

---

## Conclusion

**This is a RADICAL and BEAUTIFUL concept.** It shifts from "declaring shapes" to "positioning objects and letting physics create the geometry."

The emergent model:
- ✅ Massively simplifies code (~30 lines → 2 lines for shadows)
- ✅ Enables instant design system switching
- ✅ Matches physical intuition
- ✅ Removes fake shadows/outlines
- ✅ Makes Boon unique in the UI framework landscape

**Recommendation: Try it!** This could be genuinely innovative and a defining feature of Boon.
