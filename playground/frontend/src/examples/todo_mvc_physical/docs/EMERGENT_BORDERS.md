# Emergent Borders - Physical 3D UI Design

## Philosophy: "Don't paint borders. Light geometry."

In a physically-based 3D UI, traditional 2D borders are **unnecessary** and **non-emergent**. Instead, visual boundaries emerge naturally from the interaction of **light**, **geometry**, and **materials**.

---

## Why No Explicit Borders?

Traditional UI frameworks use borders for three purposes:
1. **Focus indication** - Visual feedback for interactive elements
2. **Section separation** - Dividing different content areas
3. **Visual hierarchy** - Emphasizing important elements

**All three emerge automatically in physical 3D space.**

---

## Emergent Solutions

### 1. Focus Indication → **Spotlight + Material Glow**

**Instead of focus borders:**
```boon
// ❌ Old 2D approach
edges: Theme/edge(of: Focus)  // Explicit border
```

**Physical 3D approach:**
```boon
// ✅ Emergent from light + material
material: Theme/material(of: InputInterior[focus: True])
// Material includes glow property when focused

// Plus: Spotlight automatically targets focused element
Light/spot(
    target: FocusedElement
    intensity: 0.5
    radius: 40
)
```

**Result:** Focused elements are **literally illuminated** by a spotlight and **glow from within**. Far more physically realistic than a painted line.

---

### 2. Section Separation → **Ambient Occlusion + Material Transitions**

**Instead of divider borders:**
```boon
// ❌ Old 2D approach
edges: Theme/edge(of: Standard)  // Painted line between sections
```

**Physical 3D approach:**
```boon
// ✅ Emergent from depth + materials
// Todos list
style: [
    material: Theme/material(of: SurfaceVariant)
]

// Panel footer
style: [
    material: Theme/material(of: PanelFooter)
]
```

**Result:** Where two surfaces with different materials meet, **ambient occlusion** creates natural shadow lines. The geometry itself creates the visual boundary.

---

### 3. Visual Hierarchy → **Elevation + Depth**

**Instead of outline borders:**
```boon
// ❌ Old 2D approach
edges: Theme/edge(of: Primary)  // Painted outline for emphasis
```

**Physical 3D approach:**
```boon
// ✅ Emergent from z-position
move: [closer: Theme/elevation(of: Selection)]
depth: Theme/depth(of: Element)
```

**Result:** Important elements **physically lift** toward the viewer. The shadow cast by elevation creates natural emphasis - no painting required.

---

## Technical Details

### Ambient Occlusion Shadows

When surfaces at different depths or with different materials meet:
- **Contact shadows** form at the intersection
- **Soft gradients** naturally occur at edges
- **Material properties** (roughness, metalness) affect shadow appearance

### Material Glow

Focus states use emissive glow:
```boon
glow: [
    color: Oklch[lightness: 0.7, chroma: 0.2, hue: 200]
    intensity: 0.15
]
```

This creates a **volumetric glow** around the element - far more sophisticated than a flat border.

### Spotlight System

```boon
Light/spot(
    target: FocusedElement
    color: Oklch[lightness: 0.9, chroma: 0.25, hue: 200]
    intensity: 0.5
    radius: 40
    softness: 0.3
)
```

The spotlight **tracks the focused element**, creating dynamic lighting that responds to user interaction.

---

## Migration Guide

### Removing Focus Borders

**Before:**
```boon
style: [
    material: Theme/material(of: InputInterior[focus: element.focused])
    edges: Theme/edge(of: Focus)  // Remove this
]
```

**After:**
```boon
style: [
    material: Theme/material(of: InputInterior[focus: element.focused])
    // Material already includes glow when focused
    // Spotlight automatically targets this element
]
```

### Removing Divider Borders

**Before:**
```boon
style: [
    edges: Theme/edge(of: Standard)  // Remove this
    material: Theme/material(of: SurfaceVariant)
]
```

**After:**
```boon
style: [
    material: Theme/material(of: SurfaceVariant)
    // Ambient occlusion creates natural divider line
]
```

### Removing Emphasis Borders

**Before:**
```boon
style: [
    edges: Theme/edge(of: Primary)  // Remove this
    material: Theme/material(of: Button)
]
```

**After:**
```boon
style: [
    move: [closer: Theme/elevation(of: Selection)]
    material: Theme/material(of: Button[selected: True])
    // Elevation creates natural emphasis through shadow
    // Material may include subtle glow
]
```

---

## Benefits

### 1. **Physically Accurate**
- Light behaves realistically
- Shadows emerge from geometry
- Materials interact naturally

### 2. **Less Code**
- No border property management
- No border color tokens
- No border width calculations

### 3. **Consistent Appearance**
- Automatic shadow quality
- Uniform glow behavior
- Predictable light interaction

### 4. **Performance**
- Fewer render passes (no separate border layer)
- GPU-optimized lighting calculations
- Hardware-accelerated shadows

### 5. **Accessible**
- High contrast from elevation
- Clear focus indicators (spotlight + glow)
- Material differentiation visible in all conditions

---

## Examples in TodoMVC

### Focus State
**Editing todo input** (RUN.bn:404-430):
```boon
Element/text_input(
    style: [
        move: [closer: 24]  // Lifts toward viewer
        material: Theme/material(of: InputInterior[focus: True])
        // Material includes glow: [intensity: 0.15]
    ]
    focus: True
)
// Spotlight automatically illuminates this element
```

### Section Divider
**Todos list container** (RUN.bn:295-309):
```boon
Element/stripe(
    style: [
        material: Theme/material(of: SurfaceVariant)
        // Ambient occlusion creates shadow line at top edge
    ]
)
```

### Visual Hierarchy
**Selected filter button** (RUN.bn:586-617):
```boon
move: [closer: selected |> WHEN {
    True => Theme/elevation(of: Selection)  // Lifts when selected
    False => 0
}]
material: Theme/material(of: ButtonFilter[
    selected: selected
    hovered: element.hovered
])
// Material includes glow when selected or hovered
```

---

## Conclusion

By eliminating explicit borders, we achieve:
- ✅ **Pure emergent design** - boundaries from physics, not painting
- ✅ **Simpler API** - fewer properties to manage
- ✅ **Better UX** - realistic lighting and depth perception
- ✅ **Less code** - let light and geometry do the work

**Remember:** In physical 3D space, you don't paint lines - you **light geometry**.
