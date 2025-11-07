# TodoMVC Physical 3D Example

This directory contains the TodoMVC implementation with physically-based 3D rendering.

## Files

### Code
- **`todo_mvc_physical.bn`** - Main TodoMVC implementation using physically-based 3D UI

### Documentation

- **`PHYSICALLY_BASED_RENDERING.md`** - **START HERE** - Complete guide to Boon's 3D UI system
  - User API (semantic elements)
  - Automatic cavity generation
  - Material properties
  - Scene lighting
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
        depth: 6              -- Creates automatic recess
        gloss: 0.65           -- Shiny interior
        padding: [all: 10]    -- Controls wall thickness
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
⏳ **Renderer:** Internal `Model/cut()` implementation pending

## Future Possibilities

If needed, we can add:
- `cavity` style property for manual control
- `cutters` style property for multiple cuts
- `Model/cut()` as user-facing API
- Custom geometry operations

But for now: **keep it simple!**
