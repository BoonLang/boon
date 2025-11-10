# Pattern 6: Cursor Gravity Field - Deep Analysis & Questions

## Core Concept

**Cursor exerts magnetic/gravitational attraction on nearby interactive elements**

```
    Cursor
      ↓
     ⚫ ← Gravity field (radius: 120px)
    / | \
   /  |  \
  🔲 🔲 🔲  ← Elements lift toward cursor
              Force = strength / distance²
```

### Current Theme Implementation

```boon
FUNCTION cursor_gravity_field() {
    [
        enabled: True
        strength: 50            // Force strength constant
        radius: 120             // Effective radius in pixels
        max_lift: 8             // Cap lift amount
        min_distance: 20        // Deadzone before force applies
        falloff: InverseSquare  // Physics: F = strength / d²
        enable_tilt: True
        max_tilt: 3             // Maximum tilt angle (degrees)
        update_rate: 60         // Hz (60fps)
        spatial_partition: True // Use quadtree for performance
    ]
}

FUNCTION cursor_lift(element_center, cursor_position) {
    distance: Math/distance(element_center, cursor_position)

    distance |> WHEN {
        d if d < min_distance => 0      // Too close: deadzone
        d if d > radius => 0            // Too far: no effect
        d => strength / (d * d) |> Math/min(max_lift)  // Inverse square law
    }
}

FUNCTION cursor_tilt(element_center, cursor_position) {
    // Element rotates to "look at" cursor
    // Returns: [rotate_x: ..., rotate_y: ...]
}
```

---

## Critical Design Questions

### 🔴 **1. INTERACTION WITH PATTERN 1 (Material Physics)**

**Problem:** Elements already have hover transforms from material physics:

```boon
// Pattern 1: Material physics
Button => [
    rest_elevation: 4
    hover_lift: 2      // ← Explicit hover behavior
    press_depression: 4
]

// Pattern 6: Cursor gravity
cursor_lift: 3         // ← Physics-based lift from proximity
```

**Question: How do these combine when cursor is near (but not hovering)?**

**Option A: Additive (before hover boundary)**
```
Cursor 30px away: gravity_lift = 3
Element total lift: rest_elevation (4) + gravity (3) = 7
```

**Option B: Replace hover_lift when in gravity range**
```
Cursor 30px away: gravity_lift = 3 (replaces hover_lift: 2)
Element total lift: rest_elevation (4) + max(gravity, 0) = 7
```

**Option C: Disable gravity when hovered**
```
Cursor over element (hovered: True): gravity = 0, use Pattern 1 only
Element total lift: rest_elevation (4) + hover_lift (2) = 6
```

**Option D: Maximum of the two**
```
Element total lift: rest_elevation (4) + max(gravity_lift, hover_lift) = ?
```

**Which option makes most physical and UX sense?**

---

### 🔴 **2. ELEMENT CENTER POSITION**

**Problem:** `cursor_lift(element_center, cursor_position)` requires `element_center`

**Question: Is element position automatically available in Boon?**

**Option A: Automatic `element.center` property**
```boon
Element/button(
    element: [center: LINK]  // Automatically exposed?
    style: [
        transform: Theme/cursor_lift(element.center, cursor_position)
    ]
)
```

**Option B: Automatic `element.position` (top-left) + calculate center**
```boon
element_center: [
    x: element.position.x + (element.width / 2)
    y: element.position.y + (element.height / 2)
]
```

**Option C: Explicit bounds tracking**
```boon
Element/button(
    element: [bounds: LINK]  // { x, y, width, height }
)
```

**Which properties are available? Does this work reactively with layout changes?**

---

### 🔴 **3. PERFORMANCE STRATEGY**

**Problem:** TodoMVC with 100 todos = 100 gravity calculations per mouse move at 60fps = 6,000 calculations/second

**Current setting:** `spatial_partition: True` (quadtree optimization)

**Question: What performance strategy should we use?**

**Option A: Only calculate for elements within radius**
```
1. Cursor moves
2. Spatial query: "Which elements are within 120px?"
3. Calculate gravity only for those elements
```

**Option B: Throttle cursor updates**
```
Update gravity calculations at 30fps instead of 60fps
```

**Option C: Only affect visible elements**
```
Elements outside viewport ignore gravity
```

**Option D: Maximum affected elements**
```
Sort by distance, only affect closest 20 elements
```

**Option E: Combination of above**

**For TodoMVC demo, which optimizations are REQUIRED vs. NICE-TO-HAVE?**

---

### 🔴 **4. DISABLED ELEMENTS (Pattern 8)**

**Problem:** Pattern 8 makes disabled elements "ghosts" (opacity 0.3, recessed)

```boon
// Disabled button
is_disabled: True
style: [
    ...Theme/disabled_transform()  // opacity: 0.3, move_further: 2
]
```

**Question: Should disabled/ghost elements respond to cursor gravity?**

**Option A: Disabled elements ignore gravity**
```boon
gravity_lift: is_disabled |> WHEN {
    True => 0
    False => Theme/cursor_lift(...)
}
```

**Option B: Reduced gravity for disabled elements**
```boon
gravity_lift: Theme/cursor_lift(...) * (is_disabled |> WHEN {
    True => 0.3   // Subtle response
    False => 1.0
})
```

**Option C: Disabled elements fully respond (default behavior)**

**Expected UX: Disabled elements shouldn't feel interactive. Which option?**

---

### 🔴 **5. TRANSFORM COMPOSITION**

**Problem:** Elements have multiple transform sources:

```boon
-- Pattern 1: Material physics (interaction_transform)
-- Pattern 4: Text depth (move_further: -2 for hierarchy)
-- Pattern 6: Cursor gravity (move_closer: 3)
-- Custom positioning (move_left: 50, move_down: 14)
-- Rotation (rotate: 90)
```

**Question: How do we cleanly compose all these transforms?**

**Option A: Manual composition (current)**
```boon
transform: [
    ...Theme/interaction_transform(...)           // Pattern 1
    move_closer: Theme/cursor_lift(...),           // Pattern 6
    move_further: Theme/text_hierarchy_depth(...), // Pattern 4
    move_left: 50                                  // Custom
]
```

**Problem:** Conflicting `move_closer` values - which wins?

**Option B: Helper function**
```boon
transform: Theme/combine_transforms(
    interaction: [material: Button, state: [hovered: ..., pressed: ...]],
    gravity: [element_center: ..., cursor_position: ...],
    text_depth: Secondary,
    custom: [move_left: 50, rotate: 90]
)
```

**Option C: Additive elevation system**
```boon
-- All patterns ADD to base elevation
elevation: 0
    + interaction_elevation          // Pattern 1: +2 on hover
    + cursor_gravity_elevation       // Pattern 6: +3 from proximity
    + text_hierarchy_elevation       // Pattern 4: -2 for secondary text
    + custom_elevation              // Custom: +0

final_transform: [move_closer: total_elevation, ...custom_positioning]
```

**Which approach is cleanest and most maintainable?**

---

### 🔴 **6. API STYLE: GLOBAL vs. OPT-IN**

**Question: Should gravity be global (all elements) or opt-in (explicit)?**

**Option A: Global automatic (rendering engine applies to all interactive elements)**
```boon
-- Just store cursor position
cursor_position: Mouse/position()

-- All interactive elements automatically affected
-- No explicit code in element definitions
```
**Pros:** Clean, zero boilerplate
**Cons:** Less control, might affect unwanted elements

**Option B: Explicit opt-in per element**
```boon
Element/button(
    style: [
        transform: [
            ...Theme/interaction_transform(...)
            move_closer: Theme/cursor_lift(element.center, cursor_position)
        ]
    ]
)
```
**Pros:** Full control, clear what's affected
**Cons:** Verbose, repetitive, easy to forget

**Option C: Material property (theme-controlled)**
```boon
-- In theme
material_physics(Button).gravity_affected: True

-- Rendering engine applies gravity to elements with this property
```
**Pros:** Declarative, centralized control
**Cons:** Less per-instance flexibility

**Option D: Element type whitelist**
```boon
-- In theme
cursor_gravity_field().affected_elements: [Button, Checkbox, Input]
```

**Which fits best with Boon's philosophy?**

---

### 🔴 **7. TILT BEHAVIOR**

**Problem:** Tilt makes elements rotate to "look at" cursor

```boon
cursor_tilt(element.center, cursor_position) → [rotate_x: 2, rotate_y: 1]
```

**Potential issues:**
- **Text readability:** Tilted text harder to read
- **Buttons:** Tilted button icons might look broken
- **Chaos:** Multiple tilted elements might look messy

**Question: Which elements should tilt?**

**Option A: All elements with gravity tilt**
```boon
enable_tilt: True  // Global setting
```

**Option B: Opt-in per element type**
```boon
material_physics(Button).enable_tilt: True
material_physics(Input).enable_tilt: False  // Inputs don't tilt
```

**Option C: Only non-text elements tilt**
```boon
-- Buttons tilt, but Text/Label/Paragraph never tilt
```

**Option D: Disable tilt entirely (max_tilt: 0)**
```boon
-- Too subtle to be worth the complexity
```

**Should we keep tilt? If yes, which elements?**

---

### 🔴 **8. TOUCH DEVICES**

**Problem:** No cursor on touch devices (phones, tablets)

**Question: What happens on touch?**

**Option A: Disable gravity on touch devices**
```boon
cursor_gravity_field().enabled: Device/has_cursor()
```

**Option B: Create temporary gravity well at touch point**
```boon
-- On touch, create short-lived gravity field at touch location
-- Fade out after 300ms
```

**Option C: Different effect for touch**
```boon
-- Touch = ripple effect instead of gravity
```

**Option D: Do nothing (gravity just doesn't apply)**

**Should touch devices have special behavior?**

---

### 🔴 **9. ACCESSIBILITY**

**Problem:** Constant motion might trigger motion sensitivity

**Question: How do we respect `prefers-reduced-motion`?**

**Option A: Disable gravity entirely**
```boon
cursor_gravity_field().enabled: Media/prefers_reduced_motion() |> WHEN {
    True => False
    False => True
}
```

**Option B: Reduce effect strength**
```boon
gravity_strength: Media/prefers_reduced_motion() |> WHEN {
    True => 10    // Subtle effect
    False => 50   // Full effect
}
```

**Option C: Remove tilt, keep lift**
```boon
enable_tilt: Media/prefers_reduced_motion() |> Bool/not()
```

**Option D: User setting in theme options**
```boon
theme_options: [
    name: Professional
    mode: Light
    cursor_gravity: Disabled  // Or: Subtle, Normal, Aggressive
]
```

**Which approach is most respectful of user preferences?**

---

### 🔴 **10. SCOPE: WHICH ELEMENTS IN TODOMVC?**

**Question: In TodoMVC, which elements should respond to cursor gravity?**

**Interactive elements:**
- ✅ Buttons (clear, remove, filter buttons)
- ✅ Checkboxes (todo checkbox, toggle all checkbox)
- ❓ Text inputs (new todo input, editing input)
- ❓ Todo items (entire row)
- ❌ Text labels (not interactive)
- ❌ Containers (background, panels)

**Specific questions:**
1. Should **text inputs** respond to gravity? (They already have Pattern 1 interactions)
2. Should **entire todo rows** respond, or just the checkbox within?
3. Should **filter buttons** have gravity when already selected?
4. Should the **remove button** (which only appears on hover) have gravity?

**What's the right scope for the demo?**

---

### 🔴 **11. GRAVITY + PRESS STATE**

**Problem:** When element is pressed, should it still respond to cursor moving?

**Scenario:**
```
1. User hovers button (gravity lifts it)
2. User clicks and HOLDS (pressed: True)
3. While holding, user moves cursor away
```

**Question: Should pressed elements follow cursor or stay put?**

**Option A: Gravity disabled when pressed**
```boon
gravity_lift: element.pressed |> WHEN {
    True => 0
    False => Theme/cursor_lift(...)
}
```

**Option B: Pressed elements have stronger gravity (magnetic "stick")**
```boon
gravity_strength: element.pressed |> WHEN {
    True => 100   // Double strength
    False => 50
}
```

**Option C: Pressed elements use Pattern 1 only (ignore gravity)**

**Which feels most natural?**

---

### 🔴 **12. MULTIPLE NEARBY ELEMENTS**

**Scenario:** Cursor between two buttons 40px apart

```
   🔲          ⚫          🔲
 Button1    Cursor     Button2
   ↑                      ↑
  Lift: 4              Lift: 4
```

**Question: Is this desired behavior?**

**Concern:** Multiple elements lifting together might look chaotic

**Options:**
1. **Allow it** - natural physics, multiple elements can be in field
2. **Limit to closest N elements** - only closest 3 elements affected
3. **Strongest wins** - only element closest to cursor affected
4. **Gradient** - closer element lifts more (already handled by inverse square)

**Is multi-element gravity a feature or a bug?**

---

### 🔴 **13. DEBUGGING & VISUALIZATION**

**Problem:** Gravity field is invisible - hard to debug

**Question: Should there be a debug mode?**

**Potential debug features:**
```boon
cursor_gravity_field().debug_mode: True

-- Visualizations:
-- 1. Circle showing gravity radius
-- 2. Lines from cursor to affected elements
-- 3. Numbers showing force magnitude
-- 4. Highlight elements in range
```

**Should we include debug visualization in the theme? Or assume Boon dev tools handle it?**

---

## Implementation Complexity Estimate

### Minimal Implementation (Demo-ready)
- Store `cursor_position: Mouse/position()`
- Add gravity transform to 3-4 button elements explicitly
- No tilt, no optimization, simple distance calc
- **Complexity: Low**

### Full Implementation (Production-ready)
- Spatial partitioning for performance
- Material property system for affected elements
- Tilt behavior with per-type configuration
- Accessibility (prefers-reduced-motion)
- Touch device handling
- Transform composition helper
- Disabled element handling
- **Complexity: High**

---

## Recommendations for TodoMVC Demo

### Suggested Approach:
1. **Scope:** Only buttons and checkboxes (not inputs, not todo rows)
2. **API:** Explicit opt-in per element (Option B) for clarity in demo
3. **Combination:** Gravity REPLACES hover_lift when in range (Option B/C hybrid)
4. **Disabled:** Disabled elements ignore gravity (Option A)
5. **Tilt:** Disable entirely for simplicity (max_tilt: 0)
6. **Performance:** No optimization yet (TodoMVC is small)
7. **Accessibility:** Respect prefers-reduced-motion (Option A - disable gravity)
8. **Touch:** Disable on touch devices (Option A)

### Questions Remain:
- Element position API (Question 2)
- Transform composition strategy (Question 5)
- Pressed state behavior (Question 11)

---

## Summary of Critical Questions

Please answer these to finalize the API:

1. **How does gravity combine with Pattern 1 hover_lift?** (Additive, replace, disable, or max?)
2. **Is `element.center` or `element.position` available automatically in Boon?**
3. **Should disabled (Pattern 8) elements ignore gravity entirely?**
4. **How should we compose transforms from Pattern 1 + 4 + 6 + custom?** (Manual, helper, or additive?)
5. **API style: Global, opt-in, or material property?**
6. **Should we keep tilt behavior, or disable it?**
7. **For TodoMVC demo: which elements should have gravity?** (Just buttons/checkboxes, or also inputs/rows?)
8. **Should pressed elements still respond to gravity?**
9. **Accessibility: Fully disable gravity for prefers-reduced-motion?**
10. **Touch devices: Disable gravity, or create touch gravity wells?**

Once these are answered, I can implement a clean, well-documented Pattern 6 integration! 🎯
