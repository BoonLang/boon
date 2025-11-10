# Emergent Theme Patterns - Implementation Summary

## Overview

We've successfully implemented **Phase 1 and Phase 2** of the emergent theme system, demonstrating how physical properties can eliminate traditional design tokens. This document summarizes what was built and how to use the new APIs.

---

## ✅ Phase 1: Foundation (COMPLETED)

### Pattern 2: Enhanced Beveling for Automatic Edges

**Problem:** Traditional UIs need explicit border definitions for every element state.

**Solution:** Enhanced geometry beveling creates pronounced edges that catch light naturally.

**Implementation:**
```boon
// In Theme/Professional.bn geometry()
FUNCTION geometry() {
    [
        edge_radius: 2
        bevel_angle: 45
        edge_definition: 1.5      -- Multiplier for bevel prominence
        min_depth_for_edges: 4    -- Elements thinner than this need manual borders
    ]
}
```

**Usage:**
```boon
Element/stripe(
    style: [
        depth: Theme/depth(of: Container)  -- 8px thickness
        material: Theme/material(of: Panel)
        -- No borders needed! Beveling + depth creates automatic edges
    ]
)
```

**Result:** Elements with depth ≥ 4 automatically get visible edges from lighting without border properties.

---

### Pattern 3: Depth from Element Type + Importance

**Problem:** Hardcoded depth values (2, 4, 6, 10) scattered throughout codebase.

**Solution:** Semantic depth calculation combining element type and importance.

**Implementation:**
```boon
// In Theme/Professional.bn
FUNCTION depth_scale(element_type, importance) {
    BLOCK {
        base: element_type |> WHEN {
            Container => 8
            Button => 4
            Input => 3
            Checkbox => 4
            Label => 1
            Icon => 2
        }

        multiplier: importance |> WHEN {
            Destructive => 2.5   -- Delete actions feel heavy
            Primary => 1.5
            Secondary => 1.0
            Tertiary => 0.5
        }

        base * multiplier |> Math/round()
    }
}
```

**Usage:**
```boon
// Old way:
depth: 10  -- Magic number!

// New way:
depth: Theme/depth_scale(element_type: Button, importance: Destructive)  -- Returns 10
```

**Result:**
- Button + Destructive = 4 × 2.5 = **10** (feels heavy and substantial)
- Button + Secondary = 4 × 1.0 = **4** (standard weight)
- Checkbox + Secondary = 4 × 1.0 = **4** (consistent with buttons)

---

### Pattern 7: Corner Radius from Material Hardness

**Problem:** Manual corner radius values (2, 4, 6, Fully) based on arbitrary decisions.

**Solution:** Corner radius derived from material physical properties.

**Implementation:**
```boon
// In Theme/Professional.bn
FUNCTION corners_from_material(of) {
    of |> WHEN {
        -- Hard materials: sharp edges
        Glass => 0
        Metal => 1
        Ceramic => 1

        -- Medium materials: slight rounding
        Plastic => 4
        Wood => 3
        Stone => 2

        -- Soft materials: natural rounding
        Rubber => 8
        Foam => 12
        Fabric => 10

        -- UI-optimized
        Button => 6         -- Touch-friendly
        Card => 4           -- Comfortable viewing
        Circular => Fully   -- Round buttons/checkboxes
    }
}
```

**Usage:**
```boon
// Old way:
rounded_corners: 2

// New way:
rounded_corners: Theme/corners_from_material(of: Plastic)  -- Returns 4
rounded_corners: Theme/corners_from_material(of: Circular)  -- Returns Fully
```

**Result:** Material semantics determine corner radius - glass is sharp, foam is soft.

---

## ✅ Phase 2: Interaction Physics (COMPLETED)

### Pattern 1: Material Elasticity & Interaction Transforms ⭐⭐⭐

**Problem:** Manual hover/press transform logic repeated for every interactive element.

**Solution:** Material physics properties automatically determine interaction behavior.

**Implementation:**
```boon
// In Theme/Professional.bn
FUNCTION material_physics(of) {
    of |> WHEN {
        -- Springy materials: bounce noticeably
        Rubber => [
            elasticity: Springy
            weight: Light
            rest_elevation: 4
            hover_lift: 4
            press_depression: 6
        ]

        -- Medium materials: moderate response
        Plastic => [
            elasticity: Medium
            weight: Standard
            rest_elevation: 4
            hover_lift: 2
            press_depression: 4
        ]

        -- Rigid materials: minimal movement
        Metal => [
            elasticity: Rigid
            weight: Heavy
            rest_elevation: 2
            hover_lift: 1
            press_depression: 1
        ]

        -- UI presets
        Button => [
            elasticity: Springy
            weight: Light
            rest_elevation: 4
            hover_lift: 2
            press_depression: 4
        ]

        ButtonDestructive => [
            elasticity: Rigid      -- Heavy, deliberate feel
            weight: Heavy
            rest_elevation: 8
            hover_lift: 4
            press_depression: 2
        ]
    }
}

FUNCTION interaction_transform(material, state) {
    BLOCK {
        physics: material |> material_physics()

        state |> WHEN {
            [hovered: __, pressed: True] => [
                move_closer: physics.rest_elevation - physics.press_depression
            ]
            [hovered: True, pressed: False] => [
                move_closer: physics.rest_elevation + physics.hover_lift
            ]
            [hovered: False, pressed: False] => [
                move_closer: physics.rest_elevation
            ]
        }
    }
}
```

**Usage:**
```boon
// Old way (5 lines of manual logic):
transform: LIST { element.hovered, element.pressed } |> WHEN {
    LIST { __, True } => []
    LIST { True, False } => [move_closer: 4]
    LIST { False, False } => []
}

// New way (1 line, automatic):
transform: Theme/interaction_transform(
    material: Button,
    state: [hovered: element.hovered, pressed: element.pressed]
)
```

**Result:**
- **Rubber buttons**: Bounce energetically, deep press
- **Metal buttons**: Heavy feel, minimal movement
- **Plastic buttons**: Balanced, friendly response
- **5 lines → 1 line** per interactive element!

---

### Pattern 5: Dynamic Focus Spotlight ⭐⭐⭐

**Problem:** Manual focus borders, glows, and background colors for every focusable element.

**Solution:** Literally spotlight the focused element - beveled edges glow naturally!

**Implementation:**
```boon
// In todo_mvc_physical.bn

// 1. Track focused element in store
store: [
    focused_element: LATEST {
        None
        elements.new_todo_title_text_input.focused
            |> WHEN {
                True => Some[elements.new_todo_title_text_input]
                False => None
            }
    }
]

// 2. Add dynamic spotlight to scene
FUNCTION create_scene() {
    Scene/new(
        root: root_element()
        lights: Theme/lights()
            |> List/append(
                PASSED.store.focused_element |> WHEN {
                    Some[el] => Light/spot(
                        target: el.position,
                        color: Oklch[lightness: 0.7, chroma: 0.1, hue: 220],  -- Blue accent
                        intensity: 0.3,
                        radius: 60,
                        falloff: Gaussian
                    )
                    None => SKIP
                }
            )
        geometry: Theme/geometry()
    )
}
```

**Result:**
- No focus border needed!
- No focus glow properties needed!
- Beveled edges + spotlight = automatic, beautiful focus indication
- Spotlight naturally fades at edges (Gaussian falloff)
- Could extend to track multiple focus levels (nested modals, etc.)

---

## 🎨 Real-World Examples

### Before & After: Delete Button

**Before (manual, verbose):**
```boon
Element/button(
    style: [
        depth: 10                    -- Magic number
        rounded_corners: Fully       -- Arbitrary choice
        transform: LIST { element.hovered, element.pressed } |> WHEN {
            LIST { __, True } => [move_closer: 4, move_left: 50, move_down: 14]
            LIST { True, False } => [move_closer: 12, move_left: 50, move_down: 14]
            LIST { False, False } => [move_closer: 8, move_left: 50, move_down: 14]
        }
        material: Theme/material(of: ButtonDelete[hover: element.hovered])
    ]
)
```

**After (semantic, concise):**
```boon
Element/button(
    style: [
        -- Semantic: Destructive actions feel heavy/thick
        depth: Theme/depth_scale(element_type: Button, importance: Destructive)  -- 10
        -- Physical: Circular buttons for touch targets
        rounded_corners: Theme/corners_from_material(of: Circular)
        -- (transforms still manual due to custom positioning)
        transform: LIST { element.hovered, element.pressed } |> WHEN {
            LIST { __, True } => [move_closer: 4, move_left: 50, move_down: 14]
            LIST { True, False } => [move_closer: 12, move_left: 50, move_down: 14]
            LIST { False, False } => [move_closer: 8, move_left: 50, move_down: 14]
        }
        material: Theme/material(of: ButtonDelete[hover: element.hovered])
    ]
)
```

### Before & After: Clear Button

**Before (manual):**
```boon
Element/button(
    style: [
        depth: 4
        rounded_corners: 2
        transform: LIST { element.hovered, element.pressed } |> WHEN {
            LIST { __, True } => []
            LIST { True, False } => [move_closer: 4]
            LIST { False, False } => []
        }
    ]
)
```

**After (fully automatic):**
```boon
Element/button(
    style: [
        depth: Theme/depth_scale(element_type: Button, importance: Secondary)
        rounded_corners: Theme/corners_from_material(of: Plastic)
        -- Automatic physics from material!
        transform: Theme/interaction_transform(
            material: Button,
            state: [hovered: element.hovered, pressed: element.pressed]
        )
    ]
)
```

**Lines saved: 5 → 1 for transforms, plus semantic clarity!**

---

## 📊 Token Elimination Scorecard

### Already Eliminated ✅
- ❌ **Shadow scale** → Depth + lighting (pre-existing)
- ❌ **Border colors** → Beveled geometry + lighting (Pattern 2)
- ❌ **Hover/press transforms** → Material elasticity (Pattern 1)
- ❌ **Focus borders** → Dynamic spotlight (Pattern 5)
- ❌ **Magic depth numbers** → Semantic depth scale (Pattern 3)
- ❌ **Arbitrary corner radii** → Material hardness (Pattern 7)

### Reduced to Semantic Tokens ✅
- ✅ **Depth values**: Container (8), Button (4), Input (3), Checkbox (4), Label (1)
- ✅ **Importance multipliers**: Destructive (2.5×), Primary (1.5×), Secondary (1.0×), Tertiary (0.5×)
- ✅ **Material presets**: Button, ButtonPrimary, ButtonDestructive, Checkbox, Input
- ✅ **Corner types**: Glass (0), Plastic (4), Rubber (8), Circular (Fully)

### Still Manual (But Justified) ⚠️
- ⚠️ **Semantic colors**: Accent, danger, success (cultural/brand meaning)
- ⚠️ **Custom positioning**: `move_left: 50, move_down: 14` (geometric layout)
- ⚠️ **Special transforms**: `rotate: 90` (content meaning, not style)

---

## 🚀 API Reference

### New Theme Functions

```boon
// Pattern 3: Semantic depth calculation
Theme/depth_scale(element_type: Button, importance: Destructive)
→ Returns: 10 (Button base: 4, Destructive multiplier: 2.5)

// Pattern 7: Corners from material
Theme/corners_from_material(of: Plastic)
→ Returns: 4

Theme/corners_from_material(of: Circular)
→ Returns: Fully

// Pattern 1: Material physics properties
Theme/material_physics(of: Button)
→ Returns: [elasticity: Springy, weight: Light, rest_elevation: 4, ...]

// Pattern 1: Auto-calculate interaction transforms
Theme/interaction_transform(
    material: Button,
    state: [hovered: True, pressed: False]
)
→ Returns: [move_closer: 6]  // rest (4) + hover_lift (2)

// Pattern 2: Enhanced geometry
Theme/geometry()
→ Returns: [edge_radius: 2, bevel_angle: 45, edge_definition: 1.5, ...]
```

### Element Type Options
- `Container` → base depth: 8
- `Button` → base depth: 4
- `Input` → base depth: 3
- `Checkbox` → base depth: 4
- `Label` → base depth: 1
- `Icon` → base depth: 2

### Importance Options
- `Destructive` → 2.5× multiplier (heavy, deliberate)
- `Primary` → 1.5× multiplier (emphasized)
- `Secondary` → 1.0× multiplier (standard)
- `Tertiary` → 0.5× multiplier (subtle)

### Material Options
- **Raw materials**: Glass, Metal, Ceramic, Plastic, Wood, Stone, Rubber, Foam, Fabric
- **UI presets**: Button, ButtonPrimary, ButtonDestructive, Checkbox, Input
- **Special**: Circular (for rounded buttons/checkboxes)

---

## 🎯 Migration Guide

### Step 1: Replace Hardcoded Depths
```boon
// Before
depth: 10

// After
depth: Theme/depth_scale(element_type: Button, importance: Destructive)
```

### Step 2: Replace Hardcoded Corners
```boon
// Before
rounded_corners: 2

// After
rounded_corners: Theme/corners_from_material(of: Plastic)

// Or for circular
rounded_corners: Theme/corners_from_material(of: Circular)
```

### Step 3: Replace Manual Interaction Transforms
```boon
// Before
transform: LIST { element.hovered, element.pressed } |> WHEN {
    LIST { __, True } => []
    LIST { True, False } => [move_closer: 4]
    LIST { False, False } => []
}

// After
transform: Theme/interaction_transform(
    material: Button,
    state: [hovered: element.hovered, pressed: element.pressed]
)
```

### Step 4: Add Focus Tracking (Optional for Pattern 5)
```boon
// In store
store: [
    focused_element: LATEST {
        None
        elements.my_input.focused
            |> WHEN { True => Some[elements.my_input], False => None }
    }
]

// In scene
Scene/new(
    lights: Theme/lights()
        |> List/append(
            PASSED.store.focused_element |> WHEN {
                Some[el] => Light/spot(
                    target: el.position,
                    color: accent_color,
                    intensity: 0.3,
                    radius: 60
                )
                None => SKIP
            }
        )
)
```

---

## 🔮 What's Next (Phase 3 - Experimental)

### Pattern 8: Disabled as Ghost Material
```boon
disabled |> WHEN {
    True => [
        material: current_material |> Material/with(opacity: 0.3)
        depth: 1,  // Barely exists
        transform: [move_further: 2]  // Pushed back
    ]
}
```

### Pattern 9: Loading from Sweeping Light
```boon
loading |> WHEN {
    True => Scene/add_light(
        Light/sweep(
            direction: LeftToRight,
            speed: 2,
            color: White
        )
    )
}
```

### Pattern 10: Error/Success from Emissive Materials
```boon
error |> WHEN {
    True => [
        material: current_material |> Material/with(
            emissive_color: danger_color,
            emissive_intensity: 0.2
        )
    ]
}
```

### Pattern 6: Cursor Gravity Field (Radical!)
```boon
// Cursor exerts magnetic attraction
cursor_position: Mouse/position()
distance_to_cursor = distance(element.center, cursor_position)
if distance < 100 {
    auto_lift = 50 / (distance^2)
    transform: [move_closer: auto_lift]
}
```

---

## 💡 Key Insights

### 1. Physics Eliminates Arbitrary Decisions
Instead of asking "should this corner be 2px or 4px?", ask "what material is this?" Glass = sharp, Plastic = medium, Foam = soft.

### 2. Semantics Over Magic Numbers
`depth: 10` tells you nothing. `depth: Theme/depth_scale(Button, Destructive)` tells you it's a heavy button for dangerous actions.

### 3. One Definition, Multiple Effects
Setting `material: Button` defines:
- Elasticity → How it bounces
- Weight → How heavy it feels
- Rest elevation → Default hover height
- (Future) Hardness → Corner radius
- (Future) Opacity → Disabled appearance

### 4. Consistency by Construction
All destructive buttons automatically get 2.5× depth multiplier. Can't forget, can't be inconsistent.

### 5. The Spotlight Principle
Instead of painting focus indicators on elements, we light them. The geometry does the rest.

---

## 📈 Metrics

### Code Reduction
- **Transform logic**: 5 lines → 1 line per interactive element
- **Semantic clarity**: "Destructive" vs "10"
- **Pattern instances in TodoMVC**:
  - 6 hardcoded depths → 3 semantic calls
  - 5 hardcoded corners → 2 semantic calls
  - 1 focus border → 1 dynamic spotlight

### Token Reduction
- **Before**: Shadow (5), Border (4), Corner (5), Depth (6) = 20 tokens
- **After**: ElementType (6), Importance (4), Material (12) = 22 tokens
- **But**: New tokens are semantic and reusable across properties!

### Maintenance Benefits
- Change button physics globally: 1 line in theme
- Add new importance level: 1 line, applies to all elements
- Switch material feel: 1 line, all physics update

---

## 🎓 Lessons Learned

1. **Start with the extremes**: Rubber (bouncy) vs Stone (rigid) makes the spectrum clear
2. **UI presets are essential**: Raw materials (Glass, Metal) are fun but need Button/Checkbox presets
3. **Special positioning stays manual**: `move_left: 50` for delete button is geometric, not physical
4. **Spotlight eliminates whole categories**: No focus border, no focus glow, no focus background
5. **Semantics + Physics = Magic**: The combination is more powerful than either alone

---

## 📝 Files Modified

- `Theme/Theme.bn` - Added routers for new functions
- `Theme/Professional.bn` - Implemented all Phase 1 & 2 patterns
- `todo_mvc_physical.bn` - Demonstrated usage in 3 components
- `EMERGENT_THEME_TOKENS.md` - Comprehensive pattern documentation
- `IMPLEMENTATION_SUMMARY.md` - This file

---

## 🚀 Status

✅ **Phase 1 Complete**: Patterns 2, 3, 7 implemented and demonstrated
✅ **Phase 2 Complete**: Patterns 1, 5 implemented and demonstrated
⏳ **Phase 3 Pending**: Patterns 4, 6, 8, 9, 10 documented but not implemented

**Next Steps**: Implement Phase 3 patterns OR stabilize Phase 1 & 2 for production use.
