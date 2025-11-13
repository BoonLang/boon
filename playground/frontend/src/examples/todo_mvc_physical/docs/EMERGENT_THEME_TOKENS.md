# Emergent Theme Tokens: Eliminating Design System Complexity Through Physics

## Core Concept

Traditional design systems require extensive token hierarchies (shadow scales, border colors, text colors, hover states, etc.). In a physically-based 3D UI system, many of these tokens can be **eliminated entirely** - their visual effects emerge naturally from physical properties like depth, lighting, and material characteristics.

This document explores opportunities to reduce theme complexity by deriving visual properties from physics rather than manually specifying them.

---

## Current State Analysis

### Already Using Emergent Patterns ✓

**Shadows → Depth + Lighting**
- No shadow tokens needed (no `shadow-sm`, `shadow-lg`, etc.)
- Drop shadows emerge from elements with `depth` positioned in 3D space
- Light configuration determines shadow appearance
- **Eliminated:** Entire shadow token scale

### Mixed State (Some Themed, Some Hardcoded)

**Depth Values:**
- ✓ Themed: `Theme/depth(of: Container)`, `Theme/depth(of: Element)`
- ✗ Hardcoded: `depth: 2` (todo items), `depth: 6` (checkboxes), `depth: 10` (delete button)

**Elevation/Z-Position:**
- ✓ Themed: `Theme/elevation(of: Card)`, `Theme/elevation(of: Inset)`
- ✗ Hardcoded: All interaction states (hover/press transforms: `move_closer: 4`, etc.)

**Corner Radius:**
- ✓ Themed: `Theme/corners(of: Comfort)`, `Theme/corners(of: Touch)`, `Theme/corners(of: Soft)`
- ✗ Hardcoded: `rounded_corners: 2`, `rounded_corners: Fully`

---

## Pattern 1: Interaction Physics from Material Properties

### Traditional Approach
```boon
transform: LIST { element.hovered, element.pressed } |> WHEN {
    LIST { __, True } => [move_further: 4]      // Pressed
    LIST { True, False } => [move_closer: 6]    // Hover
    LIST { False, False } => [move_closer: 4]   // Rest
}
```

**Problem:** Every interactive element needs manual transform states (3 states × multiple elements = lots of repetition)

### Emergent Approach
```boon
material: Theme/material(of: Button[weight: Standard, elasticity: Springy])

// Theme internally derives interaction physics:
// Springy → rest: +4, hover: +6 (+2 lift), press: -2 (-6 depression)
// Rigid   → rest: +2, hover: +3 (+1 lift), press: 0 (-2 depression)
// Heavy   → rest: 0, hover: +1 (+1 lift), press: 0 (no depression)
```

**Tokens Eliminated:**
- All manual hover/press elevation deltas
- Interaction state position values

**Physical Justification:**
- Soft rubber bounces when touched
- Dense metal barely moves
- Material elasticity naturally determines interaction behavior

**Implementation:**
- Add `elasticity` parameter to materials: `Springy`, `Rigid`, `Heavy`
- Add `weight` parameter: `Light`, `Standard`, `Heavy`
- Theme calculates rest/hover/press positions from these properties

---

## Pattern 2: Borders from Edge Lighting (Beveled Geometry)

### Traditional Approach
```boon
borders: Theme/border(of: Standard)  // gray border
borders: Theme/border(of: Focus)     // blue border with glow
outline: Theme/border(of: Primary)   // accent color outline
```

**Problem:** Need border tokens for every semantic meaning and visual style

### Emergent Approach
```boon
// No borders property needed!
depth: 6  // Thick enough to create beveled edges
geometry: Theme/geometry()  // Includes edge beveling algorithm

// Beveling + lighting automatically creates edges:
// - Top edge catches light → appears lighter (natural highlight)
// - Bottom edge in shadow → appears darker (natural "border")
// - Side edges create subtle definition
// - Focus adds spotlight → edges glow naturally
```

**Tokens Eliminated:**
- Border color scale (default, hover, focus, error, success)
- Border width values
- Border style variants (solid, dashed, glowing, etc.)
- Outline colors

**Physical Justification:**
- Real objects don't have "borders" painted on them
- Edges appear where surfaces meet at angles and catch/block light differently
- iOS calculator buttons have no borders - edges are visible purely from lighting

**Implementation:**
- Enhance geometry beveling algorithm to create more pronounced edges
- Ensure elements with sufficient depth get automatic edge definition
- Focus spotlight creates natural edge glow (see Pattern 5)

**Note:** For perfectly flat elements (depth: 0), may still need manual borders as fallback

---

## Pattern 3: Depth as Function of Type + Importance

### Traditional Approach
```boon
depth: 2   // todo item
depth: 6   // checkbox
depth: 10  // delete button
depth: 4   // clear button
```

**Problem:** Magic numbers scattered throughout code, no semantic relationship

### Emergent Approach
```boon
depth: Theme/depth(element_type, importance)

// Formula: depth = base_depth[element_type] × importance_multiplier

base_depth = {
    Container: 8,
    Button: 4,
    Input: 3,
    Label: 1,
    Checkbox: 4
}

importance = {
    Destructive: 2.5,   // delete button: 4 × 2.5 = 10
    Primary: 1.5,       // main action buttons
    Secondary: 1.0,     // checkboxes: 4 × 1.0 = 4
    Tertiary: 0.5       // minor elements
}
```

**Tokens Eliminated:**
- Individual depth constants throughout codebase

**Gained:**
- Semantic meaning in depth values
- Consistent depth hierarchy
- Easy to adjust depth scale globally

**Physical Justification:**
- Important objects are made from thicker/sturdier materials
- Cheap plastic vs quality metal difference
- Destructive actions feel "heavier" and more substantial

**Implementation:**
- Create `Theme/depth(element_type, importance)` function
- Define base depth scale for element types
- Define importance multipliers

---

## Pattern 4: Text Color from Z-Position (Carved Text)

### Traditional Approach
```boon
font: Theme/font(of: Primary)    // dark text
font: Theme/font(of: Secondary)  // gray text
font: Theme/font(of: Tertiary)   // light gray text
```

**Problem:** Separate color tokens for text hierarchy

### Emergent Approach
```boon
// Primary text at surface level (z: 0)
Element/text(
    style: [font: Theme/font(of: Text)]
    transform: []
)

// Secondary text recessed into surface (z: -4)
Element/text(
    style: [font: Theme/font(of: Text)]
    transform: [move_further: 4]
)

// Light is above surface, recessed text receives less light → appears grayer
```

**Tokens Eliminated:**
- Secondary text color
- Tertiary text color
- Disabled text color (very deeply recessed)

**Physical Justification:**
- Text carved/engraved into surface appears dimmer (in shadow)
- Text embossed (raised) appears brighter (catches light)
- Physical metaphor: engraved metal plaque

**Limitations:**
- Works best for monochrome themes or with carefully tuned ambient light color
- May need manual color override for colored text (links, warnings, etc.)
- Surface material color affects text appearance

**Implementation:**
- Define `move_further` values for text hierarchy: 0 (primary), 2 (secondary), 4 (tertiary)
- Ensure lighting system properly affects text rendering
- Might require text to be actual 3D geometry, not flat texture

---

## Pattern 5: Focus States from Spotlight

### Traditional Approach
```boon
borders: Theme/border(of: Focus)
material: Theme/material(of: InputInterior[focus: element.focused])
outline: focused |> WHEN {
    True => Theme/border(of: Primary)
    False => NoOutline
}
```

**Problem:** Manual focus indicators on every interactive element

### Emergent Approach
```boon
Scene/new(
    root: root_element()
    lights: Theme/lights()
        |> List/append(
            PASSED.store.focused_element |> WHEN {
                Some[el] => Light/spot(
                    target: el.position,
                    color: theme.accent_color,
                    intensity: 0.3,
                    radius: 40,
                    falloff: Gaussian
                )
                None => SKIP
            }
        )
)

// Focused element is literally spotlit!
// No focus border, no focus material variant needed
```

**Tokens Eliminated:**
- Focus border color
- Focus glow effect
- Focus background color
- Focus outline style

**Physical Justification:**
- Stage spotlight metaphor
- Focused element has attention → literally illuminated
- Natural fade at edges (Gaussian falloff) creates free glow effect

**Implementation:**
- Track currently focused element in store
- Add dynamic spotlight to scene lights
- Spotlight color = theme accent color
- Adjust spotlight intensity/radius for different element sizes

**Bonus Effects:**
- Spotlight naturally highlights beveled edges (combines with Pattern 2)
- Can animate spotlight transition (fade in/out, move)
- Multiple focus levels possible (nested focus = multiple spotlights)

---

## Pattern 6: Hover Effects from Cursor Gravity Field

### Traditional Approach
```boon
transform: element.hovered |> WHEN {
    True => [move_closer: 4]
    False => []
}
```

**Problem:** Every element needs manual hover elevation

### Emergent Approach (Radical)
```boon
// Global cursor system
cursor_position: Mouse/position()
cursor_field: [
    type: Magnetic,
    strength: 50,
    radius: 100
]

// Each interactive element automatically:
distance_to_cursor = distance(element.center, cursor_position)
if distance_to_cursor < cursor_field.radius {
    attraction = cursor_field.strength / (distance_to_cursor ^ 2)
    auto_transform: [move_closer: min(attraction, 6)]
}
```

**Tokens Eliminated:**
- All hover elevation values
- Hover state management per element

**Physical Justification:**
- Magnetic/gravitational attraction
- Inverse square law (closer = stronger effect)
- Multiple nearby elements affected simultaneously

**Bonus Effects:**
- Smooth gradient as cursor approaches (not binary on/off)
- Multiple nearby elements lift together (natural grouping)
- Cursor-following tilt effect possible (element "looks at" cursor)
- Dragging feels physically connected

**Implementation Challenges:**
- High performance cost (calculate for every element)
- May need spatial partitioning / quadtree
- Needs careful tuning to avoid overwhelming users
- Accessibility concerns (reduced motion preference)

**Verdict:** Experimental - try as opt-in theme variant

---

## Pattern 7: Corner Radius from Material Hardness

### Traditional Approach
```boon
rounded_corners: 2                      // sharp
rounded_corners: Theme/corners(of: Soft)   // medium
rounded_corners: Fully                  // circular
```

**Problem:** Manual corner radius decisions for each element

### Emergent Approach
```boon
material: Theme/material(of: Glass)     // → auto corners: 0-1px (sharp)
material: Theme/material(of: Metal)     // → auto corners: 2-4px (slight)
material: Theme/material(of: Plastic)   // → auto corners: 6-8px (medium)
material: Theme/material(of: Foam)      // → auto corners: 12px+ (soft)
material: Theme/material(of: Button)    // → auto corners: 8px (touch-optimized)
```

**Tokens Eliminated:**
- Corner radius scale (sharp, soft, comfort, touch)
- Manual corner radius values

**Physical Justification:**
- Hard materials (glass, metal) have sharp edges - difficult to round
- Soft materials (rubber, foam) naturally have rounded edges
- Buttons optimized for ergonomic touch have specific curve radii
- Wear patterns (soft materials round at edges over time)

**Implementation:**
- Add `hardness` or `edge_character` to material definitions
- Auto-calculate corner radius from material type
- Allow override for special cases (circular buttons)

**Material-to-Corner Mapping:**
```
Glass/Metal:     0-2px   (sharp, modern, precise)
Plastic:         4-8px   (friendly, approachable)
Wood/Stone:      3-6px   (natural, slightly worn)
Foam/Fabric:     10-16px (soft, comfortable)
Touch UI:        8-12px  (ergonomic, finger-friendly)
Pill/Circular:   Fully   (semantic override)
```

---

## Pattern 8: Disabled States from Ghost Material

### Traditional Approach
```boon
material: Theme/material(of: Button[disabled: True])  // → gray color, low opacity
font: Theme/font(of: Text[disabled: True])            // → gray text
```

**Problem:** Disabled variants for every material/font combination

### Emergent Approach
```boon
disabled |> WHEN {
    True => [
        material: current_material |> Material/with(opacity: 0.3)
        depth: 1,  // Very thin, almost insubstantial
        transform: [move_further: 2]  // Pushed back into surface
    ]
}

// Visual result:
// - Low opacity → light passes through (ghostly)
// - Pushed back → receives less light (dimmer)
// - Thin depth → barely exists physically
// Combined effect = clearly disabled without color change
```

**Tokens Eliminated:**
- Disabled color variants for all materials
- Disabled text colors
- Disabled opacity values (or reduced to single global constant)

**Physical Justification:**
- Disabled = insubstantial, not fully present
- Ghost metaphor: translucent, recessed, fading
- Objects pushed away receive less attention (literally less light)

**Implementation:**
- Create `Material/with(opacity: Number)` helper
- Define disabled transform as global constant: `Theme/disabled_transform()`
- Apply automatically to all interactive elements when `disabled: True`

---

## Pattern 9: Loading States from Shimmer Light

### Traditional Approach
```boon
material: Theme/material(of: Skeleton)  // → animated gray shimmer background
background: loading |> WHEN {
    True => animated_gradient(colors: [gray100, gray200, gray100])
}
```

**Problem:** Skeleton loading colors and animations

### Emergent Approach
```boon
loading |> WHEN {
    True => Scene/add_light(
        Light/sweep(
            direction: LeftToRight,
            speed: 2,
            color: White,
            intensity: 0.2,
            width: 100
        )
    )
}

// Sweeping light naturally creates shimmer on all surfaces
// No skeleton colors needed - regular materials just receive moving light
```

**Tokens Eliminated:**
- Loading background colors
- Skeleton shimmer animation
- Placeholder gradient colors

**Physical Justification:**
- Sweeping searchlight effect
- Like a lighthouse beam passing over objects
- Or scanning laser in sci-fi interfaces

**Implementation:**
- Create `Light/sweep()` animated light type
- Automatically cycles across scene at defined speed
- Works on any material without special loading variants

**Bonus:**
- Loading state naturally highlights structure (depth, edges)
- Can use colored light for branded loading (blue sweep for primary color)
- Multiple sweeps possible (crosshatch pattern)

---

## Pattern 10: Error/Success States from Material Glow

### Traditional Approach
```boon
borders: error |> WHEN {
    True => Theme/border(of: Error)   // red border
    False => Theme/border(of: Default)
}
material: Theme/material(of: Input[error: True])  // red tinted background
```

**Problem:** State variants for every element

### Emergent Approach
```boon
error |> WHEN {
    True => [
        material: current_material |> Material/with(
            emissive_color: theme.danger_color,
            emissive_intensity: 0.2
        )
    ]
}

// Material emits red light from within
// Combines with surface lighting to create red glow
// Especially visible at beveled edges
```

**Tokens Eliminated:**
- Error border colors
- Error background tints
- Success highlight colors

**Physical Justification:**
- Error = warning light (like brake lights, fire, danger)
- Success = indicator light (like green LED)
- Material emits its own light (self-illuminated)

**Implementation:**
- Add `emissive_color` and `emissive_intensity` to material system
- Error: emit danger_color (red/orange)
- Success: emit success_color (green)
- Warning: emit warning_color (yellow/amber)

**Bonus:**
- Emissive edges glow naturally without separate glow effect
- Can pulse intensity for animation
- Combines with depth for dramatic effect (thick elements glow more)

---

## Token Elimination Summary

### Traditional Design System Needs:
- ❌ Shadow scale (sm, md, lg, xl) → **Eliminated by depth + lighting**
- ❌ Border colors (default, focus, error, success) → **Eliminated by beveled edges + lighting**
- ❌ Text colors (primary, secondary, tertiary, disabled) → **Partially eliminated by z-position**
- ❌ Hover/press/focus background colors → **Eliminated by material properties**
- ❌ Interaction transforms (hover lift, press depth) → **Eliminated by material elasticity**
- ❌ Corner radius scale → **Reduced to material hardness property**
- ❌ Disabled state colors → **Eliminated by opacity + z-position**
- ❌ Loading skeleton colors → **Eliminated by sweeping light**
- ❌ State highlight colors → **Eliminated by emissive materials**

### Physical Design System Needs:
1. **Material Properties**
   - Base colors (still needed!)
   - Elasticity (Springy, Rigid, Heavy)
   - Hardness (determines corner radius)
   - Weight (affects interaction physics)
   - Emissive capabilities (for states)

2. **Light Configuration**
   - Ambient light (base illumination)
   - Directional lights (shadows, highlights)
   - Spot lights (focus indicators)
   - Animated lights (loading, effects)

3. **Geometry Rules**
   - Beveling algorithm (edge definition)
   - Depth-to-bevel ratio
   - Surface normal calculation

4. **Physics Constants**
   - Gravity/attraction strength (optional: hover effects)
   - Interaction response curves
   - Animation easing from physics

5. **Semantic Colors** (Unavoidable)
   - Accent color (brand)
   - Danger color (destructive actions)
   - Success color (confirmations)
   - Warning color (alerts)

**Result:** ~70-80% reduction in theme tokens, with stronger semantic meaning and automatic consistency.

---

## Implementation Priority

### Phase 1: High Value, Low Risk
1. ✅ **Borders from beveling** (Pattern 2)
   - Enhance existing geometry beveling
   - Most visible improvement
   - No breaking changes

2. ✅ **Depth from type + importance** (Pattern 3)
   - Create `Theme/depth(type, importance)` function
   - Systematic, easy to understand
   - Improves consistency

3. ✅ **Corner radius from material** (Pattern 7)
   - Add hardness property to materials
   - Natural, intuitive mapping
   - Reduces manual decisions

### Phase 2: High Value, Moderate Risk
4. ✅ **Interaction physics from material** (Pattern 1)
   - Add elasticity/weight to materials
   - Auto-calculate hover/press transforms
   - Requires careful tuning

5. ✅ **Focus from spotlight** (Pattern 5)
   - Track focused element
   - Add dynamic spotlight
   - Beautiful effect, moderate complexity

6. ✅ **Loading from sweeping light** (Pattern 9)
   - Implement animated lights
   - Apply globally during loading
   - Nice bonus feature

### Phase 3: Experimental, High Risk
7. 🧪 **Disabled states from ghost material** (Pattern 8)
   - Requires opacity in material system
   - May need accessibility overrides
   - Test with users

8. 🧪 **Text color from z-position** (Pattern 4)
   - Requires 3D text rendering
   - May conflict with readability needs
   - Consider as optional mode

9. 🧪 **Hover from cursor gravity** (Pattern 6)
   - Performance intensive
   - May be overwhelming
   - Offer as theme option, not default

10. 🧪 **States from emissive materials** (Pattern 10)
    - Requires emissive material support
    - May be too subtle or too garish
    - Needs careful intensity tuning

---

## Philosophical Notes

### The Power of Constraints
By committing to physical realism, we gain:
- **Automatic consistency** - physics is consistent
- **Reduced decision fatigue** - fewer arbitrary choices
- **Natural beauty** - realistic lighting just looks good
- **Emergent complexity** - rich effects from simple rules

### The Limitation of Physics
Not everything can emerge:
- **Semantic colors** - red for danger is cultural, not physical
- **Content positioning** - layout is geometric, not physical (mostly)
- **Specific dimensions** - sizing is ergonomic/functional, not emergent
- **Accessibility overrides** - users may need high contrast, motion reduction

### The Balance
The goal isn't to eliminate ALL tokens, but to eliminate UNNECESSARY ones. The remaining tokens should be:
1. **Semantic** (danger = red because of meaning)
2. **Functional** (button width fits text)
3. **Accessible** (contrast meets WCAG)
4. **Cultural** (directional layouts for RTL languages)

Everything else should emerge from physics.

---

## Related Documents
- `EMERGENT_GEOMETRY_CONCEPT.md` - Physical beveling and depth
- `PHYSICALLY_BASED_RENDERING.md` - Material and lighting system
- `3D_API_DESIGN.md` - 3D positioning and transforms
- `todo_mvc_physical.bn` - Reference implementation
