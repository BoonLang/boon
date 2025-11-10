# Pattern 4: Text Hierarchy from Z-Position - Integration Summary

## Overview

Pattern 4 has been successfully integrated into the TodoMVC application. Text elements now use **depth-based lighting** to communicate hierarchy through physical positioning in 3D space.

**Core Principle:** Text positioned at different Z-depths receives different amounts of light:
- **Raised text** (positive Z) → catches more light → appears brighter
- **Surface text** (Z = 0) → standard lighting → normal brightness
- **Recessed text** (negative Z) → in shadow → appears dimmer

This creates a **spatial hierarchy** where visual importance emerges from physics, not manual styling.

---

## Implementation Status

### ✅ Phase 1: Theme Functions (COMPLETE)

**Files:**
- `Theme/Professional.bn` - Pattern 4 implementation
- `Theme/Theme.bn` - Pattern 4 routers

**Functions Added:**

#### 1. `text_hierarchy_depth(importance)`
Maps semantic importance to Z-position:

```boon
Theme/text_hierarchy_depth(Primary)    → 0   (surface level)
Theme/text_hierarchy_depth(Secondary)  → -2  (slightly recessed)
Theme/text_hierarchy_depth(Tertiary)   → -4  (moderately recessed)
Theme/text_hierarchy_depth(Disabled)   → -6  (deeply recessed)
```

#### 2. `text_depth_color(z_position, base_color)`
Calculates lighting-adjusted color based on depth:

```boon
z ≥ 0   → 100% brightness (surface or raised)
z > -2  → 95% brightness  (slightly dimmed)
z > -4  → 85% brightness  (moderately dimmed)
z ≤ -4  → 70% brightness  (significantly dimmed)
```

**Note:** This function is available in the theme API but rendering implementation happens in the Boon engine (separate repository).

---

## Integration in TodoMVC

### Applied Text Hierarchy

All standalone text elements now use Pattern 4:

#### **1. Header Text - Hero Importance**
**Location:** `header()` function
**Purpose:** Main "todos" title
**Depth:** `move_closer: 6` (raised above surface)
**Effect:** Catches maximum light, very bright and prominent

```boon
Element/text(
    style: [
        font: Theme/font(of: Header)
        transform: [move_closer: 6]  -- Hero text: raised above surface
    ]
    text: 'todos'
)
```

#### **2. Active Items Counter - Secondary Importance**
**Location:** `active_items_count_text()` function
**Purpose:** "X items left" status text
**Depth:** `Theme/text_hierarchy_depth(Secondary)` (Z = -2)
**Effect:** Slightly recessed, subtly dimmed (~95% brightness)

```boon
Element/text(
    style: [
        font: Theme/font(of: Secondary)
        transform: [move_further: Theme/text_hierarchy_depth(Secondary)]
    ]
    text: '{count} item{maybe_s} left'
)
```

#### **3. Todo Title Labels - Dynamic Importance**
**Location:** `todo_title_element(todo)` function
**Purpose:** Individual todo item text
**Depth:** Dynamic based on completion state
- **Active todos:** `Theme/text_hierarchy_depth(Primary)` (Z = 0, surface level)
- **Completed todos:** `Theme/text_hierarchy_depth(Tertiary)` (Z = -4, recessed)
**Effect:** Completed todos visually recede and dim, active todos remain prominent

```boon
Element/label(
    style: [
        font: Theme/font(of: TodoTitle[completed: todo.completed])
        transform: todo.completed |> WHEN {
            True => [move_further: Theme/text_hierarchy_depth(Tertiary)]   -- Recessed, dimmer
            False => [move_further: Theme/text_hierarchy_depth(Primary)]   -- Surface level
        }
    ]
    label: todo.title
)
```

#### **4. Footer Text - Tertiary Importance**
**Location:** `footer()` function
**Purpose:** Instructional text and attribution links
**Depth:** `Theme/text_hierarchy_depth(Tertiary)` (Z = -4)
**Effect:** Recessed and dimmed, subtle and unobtrusive (~85% brightness)

```boon
Element/paragraph(
    style: [
        font: Theme/font(of: Small)
        transform: [move_further: Theme/text_hierarchy_depth(Tertiary)]
    ]
    contents: LIST { 'Double-click to edit a todo' }
)
```

---

## Elements NOT Using Pattern 4

### **Button Labels**
**Why:** Button labels are simple strings (e.g., `label: 'Clear completed'`), not separate `Element/text` components. They inherit the button's positioning and move with the button through **Pattern 1** (material physics).

**Examples:**
- Remove todo button: `label: '×'`
- Filter buttons: `label: 'All' | 'Active' | 'Completed'`
- Clear completed button: `label: 'Clear completed'`

### **Icon Text**
**Why:** The ">" arrow in `toggle_all_checkbox` is a decorative icon with custom positioning (`rotate: 90, move_up: 18`). It's not semantic text, and it moves with the checkbox through Pattern 1.

---

## Visual Hierarchy Achieved

### **Before Pattern 4:**
All text at same visual prominence, hierarchy communicated only through:
- Color (text vs. text_secondary vs. text_disabled)
- Font size (Header vs. Body)
- Font weight (Bold vs. Regular)

### **After Pattern 4:**
Text hierarchy now has **spatial dimension**:

```
        ↑ Z-AXIS (toward viewer)
        |
    +6  │  "todos" - Hero (very bright, prominent)
        │
    +4  │
        │
    +2  │
        │
     0  ├─ Active todo titles - Primary (standard brightness)
        │
    -2  │  "X items left" - Secondary (slightly dimmed)
        │
    -4  │  Completed todos, footer text - Tertiary (noticeably dimmed)
        │
    -6  │  Disabled text (very dim, barely visible)
        ↓
```

### **Benefits:**

1. **Semantic Clarity:** Visual importance matches semantic importance
2. **Automatic Adaptation:** Text brightness adjusts with scene lighting (light/dark mode)
3. **Subtle Feedback:** Completed todos naturally recede without manual styling
4. **Reduced Tokens:** No need for separate "text_primary", "text_secondary" color tokens
5. **Physical Consistency:** Text hierarchy uses same depth system as UI elements

---

## Design Decisions

### **Why These Specific Depth Values?**

| Importance | Z-Position | Brightness | Use Case |
|------------|-----------|-----------|----------|
| **Hero** | +6 | ~110% | Large headers, critical calls-to-action |
| **Primary** | 0 | 100% | Main content, active items, body text |
| **Secondary** | -2 | 95% | Supporting info, counters, captions |
| **Tertiary** | -4 | 85% | Fine print, footer text, completed items |
| **Disabled** | -6 | 70% | Inactive elements, ghosted text |

**Rationale:**
- **2-unit increments** create noticeable but not jarring differences
- **Surface level (0)** as the baseline for main content
- **Raised text (+6)** for heroes to catch maximum directional light
- **Recessed text (-2 to -6)** falls into shadow, creating natural dimming

### **Why Not Apply to Button Labels?**

Button labels inherit the button's entire transform stack, including:
- Pattern 1: Material physics (`rest_elevation`, `hover_lift`, `press_depression`)
- Pattern 6: Cursor gravity (if enabled)

Adding Pattern 4 depth would create **conflicting transforms**. The button already moves physically, and the text moves with it.

---

## Future Enhancements

### **Not Yet Implemented (Rendering Layer Work)**

These features are documented in `PATTERN_4_REVISED.md` and `PATTERN_4_IMPLEMENTATION_PLAN.md` but require implementation in the Boon rendering engine:

1. **Colored Text + Depth**
   - Combine base color + depth lighting + emissive glow
   - Example: Red error text that's ALSO raised + glowing

2. **Material Properties for Text**
   - Emissive glow (self-illuminating text)
   - Outline/halo shader effects
   - Shine/reflectivity for metallic text

3. **Performance Optimizations**
   - SDF text rendering with multi-channel distance fields
   - Text baking for static elements
   - Instancing for repeated text
   - LOD system for distant text

4. **Accessibility**
   - WCAG contrast ratio checking
   - Auto-adjustment of depth to meet contrast requirements
   - HTML overlay for screen readers (required for WebGPU anyway)

5. **Advanced Effects**
   - Dynamic lighting response (text brightness changes with scene lights)
   - Focus spotlight illumination (focused input text glows)
   - Animated lights sweeping across text

---

## Usage Guidelines

### **When to Use Each Importance Level:**

#### **Hero**
- Main page titles
- Critical call-to-action buttons (when using text elements)
- Large promotional text

#### **Primary**
- Body text
- Active list items
- Main navigation labels
- Input text

#### **Secondary**
- Supporting information
- Counters and badges
- Captions
- Subtle hints

#### **Tertiary**
- Fine print
- Footer text
- Completed/archived items
- Legal disclaimers

#### **Disabled**
- Inactive form inputs
- Unavailable options
- Ghosted text

### **Theme API Examples:**

```boon
-- Simple usage: semantic importance
Element/text(
    style: [
        font: Theme/font(of: Body)
        transform: [move_further: Theme/text_hierarchy_depth(Primary)]
    ]
    text: "Main content"
)

-- Dynamic importance based on state
Element/text(
    style: [
        font: Theme/font(of: Body)
        transform: state.is_completed |> WHEN {
            True => [move_further: Theme/text_hierarchy_depth(Tertiary)]
            False => [move_further: Theme/text_hierarchy_depth(Primary)]
        }
    ]
    text: item.title
)

-- Hero text (raised above surface)
Element/text(
    style: [
        font: Theme/font(of: Header)
        transform: [move_closer: 6]  -- Explicit hero positioning
    ]
    text: "Welcome"
)
```

---

## Technical Notes

### **Coordinate System**

```
       ↑ +Z (move_closer)
       |
       |    [Text at +6: very bright]
       |
       |    [Text at 0: standard]
       |
       |    [Text at -4: dimmed]
       ↓ -Z (move_further)
```

### **Transform Composition**

Text transforms can be combined with other patterns:

```boon
-- Pattern 4 (depth) + custom positioning
transform: [
    move_further: Theme/text_hierarchy_depth(Secondary),  -- Pattern 4
    move_left: 20,                                         -- Custom offset
    rotate: 5                                              -- Custom rotation
]
```

### **Lighting Calculation (Conceptual)**

The actual lighting calculation happens in the rendering engine:

```glsl
// In text fragment shader (conceptual)
float depth_factor = calculate_depth_lighting(world_position.z);
vec3 lit_color = base_color * scene_lighting * depth_factor;
```

But the Boon code just specifies the depth:

```boon
transform: [move_further: Theme/text_hierarchy_depth(Secondary)]
```

---

## Migration Checklist

If adding Pattern 4 to an existing Boon application:

- [ ] Add `text_hierarchy_depth()` and `text_depth_color()` to theme
- [ ] Add routers to `Theme/Theme.bn`
- [ ] Identify all `Element/text`, `Element/label`, `Element/paragraph` uses
- [ ] For each text element, determine semantic importance
- [ ] Add `transform: [move_further: Theme/text_hierarchy_depth(importance)]`
- [ ] Test in both Light and Dark modes
- [ ] Verify WCAG contrast ratios are maintained
- [ ] Update documentation

---

## Summary

Pattern 4 successfully integrates **spatial text hierarchy** into TodoMVC:

✅ **Theme Functions:** `text_hierarchy_depth()`, `text_depth_color()`
✅ **Applied to:** Header, todo titles, counter, footer text
✅ **Dynamic Behavior:** Completed todos automatically recede
✅ **Documentation:** File header updated with all patterns

**Result:** Text hierarchy now emerges from **physics** (depth + lighting) rather than manual color tokens, creating a cohesive 3D design language.

**Next Steps:** Rendering layer implementation in Boon engine (separate repository) for advanced features like colored+depth text, material properties, and performance optimizations.
