# TodoMVC Physical 3D Example

This directory contains the TodoMVC implementation with physically-based 3D rendering.

## Files

### Code
- **`todo_mvc_physical.bn`** - Main TodoMVC implementation using physically-based 3D UI
- **`Theme/`** - Complete design system presets (Professional, Neobrutalism, Glassmorphism, Neumorphism)

### Documentation

- **`PHYSICALLY_BASED_RENDERING.md`** - **START HERE** - Complete guide to Boon's 3D UI system
  - User API (semantic elements)
  - Automatic cavity generation
  - Material properties
  - Scene lighting (`Lights/basic()` helper or custom light lists)
  - Internal implementation details

- **`3D_API_DESIGN.md`** - Detailed API reference for 3D properties
  - `transform: [move_closer/move_further]` positioning
  - `depth` property for 3D thickness
  - `gloss`, `metal`, `shine` material properties
  - `edges`, `rim` properties
  - Complete TodoMVC examples

- **`EMERGENT_GEOMETRY_CONCEPT.md`** - Philosophy document
  - How geometry emerges from spatial relationships
  - Design system switching (Professional, Neobrutalism, etc.)
  - Paradigm shift from explicit to emergent

## Key Concepts

### User Perspective (Simple)

Users write semantic elements with visual properties:

```boon
Element/text_input(
    style: [
        depth: 6                   -- Creates automatic recess
        material: [gloss: 0.65]    -- Shiny interior
        padding: [all: 10]         -- Controls wall thickness
    ]
    text: 'Hello'
)
```

**No geometric operations needed!** The element automatically:
- Creates recessed well based on `depth`
- Calculates wall thickness from `padding`
- Makes interior glossier
- Places text on cavity floor

### Renderer Perspective (Internal)

The renderer uses internal geometric operations to construct 3D geometry:

- `Model/cut(from, remove)` - Boolean subtraction (internal only)
- SDF-based rendering for fast GPU evaluation
- Automatic cavity generation based on element properties
- Physical lighting creates real shadows

**These are implementation details, not user-facing API.**

## Theme System

**Ultra-thin control over the entire visual design.** Change the complete look and feel with one line:

```boon
-- Select a theme
theme: Professional/theme(mode: Light)
-- or: Neobrutalism/theme(mode: Dark)

scene: Scene/new(
    root: root_element(...)
    lights: theme.lights
    geometry: theme.geometry
    materials: theme.materials
    colors: theme.colors
)
```

### Available Themes

- **Professional** - Soft rounded edges, subtle shadows, neutral colors (default)
- **Neobrutalism** - Sharp chamfered edges, hard shadows, bold saturated colors
- **Glassmorphism** - Translucent surfaces, high gloss, backdrop blur
- **Neumorphism** - Very soft edges, monochrome, low contrast

### What Themes Control

Each theme bundles **8 properties** that cascade through the entire scene:

1. **Lights** - Directional/ambient lighting setup
2. **Geometry** - Edge radius, bevel angles (emergent edge shapes)
3. **Materials** - Semantic presets (panel, button, input gloss/metal/shine)
4. **Elevation** - Z-position scale (card, popup, raised, recessed)
5. **Depth** - Thickness scale (major, standard, subtle)
6. **Interaction** - Physical behavior (hover lift, press depth, animation speed)
7. **Corners** - Radius scale (sharp, subtle, standard, round)
8. **Colors** - Semantic palette with automatic light/dark mode

**See `Theme/` directory for complete theme definitions and documentation.**

## Design Philosophy

**Keep it Simple:**
- Users describe visual intent, not geometry
- Built-in elements handle complexity automatically
- No `Element/cavity`, `Model/cut()`, or `cavity` properties exposed
- Can add advanced features later if proven necessary

**Start simple, add complexity only when needed.**

## Running the Example

```bash
# Start development server
cargo run

# Open browser to localhost:8080
# Navigate to TodoMVC Physical example
```

## Current Status

✅ **User API:** Clean and simple - semantic elements only
✅ **Documentation:** Complete guides for users and implementers
✅ **Code:** TodoMVC working with automatic 3D geometry
✅ **Theme System:** 4 complete design presets, fully integrated with demo elements
⏳ **Renderer:** Internal `Model/cut()` implementation pending

## Future Possibilities

If needed, we can add:
- `cavity` style property for manual control
- `cutters` style property for multiple cuts
- `Model/cut()` as user-facing API
- Custom geometry operations

But for now: **keep it simple!**
