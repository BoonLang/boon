# Pattern 4: Text Hierarchy from Z-Position

**Status:** ✅ Theme API Complete | ⏳ Renderer Implementation Pending

This document consolidates the analysis, implementation plan, and current integration status for Pattern 4.

---

## Table of Contents

1. [Quick Reference](#quick-reference)
2. [Capabilities & Design](#capabilities--design)
3. [Implementation Roadmap](#implementation-roadmap)
4. [Current Status](#current-status)

---

## Quick Reference

### Current TodoMVC Usage

Pattern 4 is implemented in the theme API and used in 4 locations in TodoMVC:

#### 1. Header Text - Hero Importance
**Location:** `header()` function
**Purpose:** Main "todos" title
**Depth:** `move_closer: 6` (raised above surface)

```boon
Element/text(
    style: [
        font: Theme/font(of: Header)
        transform: [move_closer: 6]  -- Hero text: raised above surface
    ]
    text: 'todos'
)
```

#### 2. Active Items Counter - Secondary Importance
**Location:** `active_items_count_text()` function
**Purpose:** "X items left" status text
**Depth:** `Theme/text_hierarchy_depth(Secondary)` (Z = -2)

```boon
Element/text(
    style: [
        font: Theme/font(of: Secondary)
        transform: [move_further: Theme/text_hierarchy_depth(Secondary)]
    ]
    text: '{count} item{maybe_s} left'
)
```

#### 3. Todo Title Labels - Dynamic Importance
**Location:** `todo_title_element(todo)` function
**Purpose:** Individual todo item text
**Depth:** Dynamic based on completion state

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

#### 4. Footer Text - Tertiary Importance
**Location:** `footer()` function
**Purpose:** Instructional text and attribution links
**Depth:** `Theme/text_hierarchy_depth(Tertiary)` (Z = -4)

```boon
Element/paragraph(
    style: [
        font: Theme/font(of: Small)
        transform: [move_further: Theme/text_hierarchy_depth(Tertiary)]
    ]
    contents: LIST { 'Double-click to edit a todo' }
)
```

### Theme API

```boon
-- Map semantic importance to Z-position
Theme/text_hierarchy_depth(Primary)    → 0   (surface level)
Theme/text_hierarchy_depth(Secondary)  → -2  (slightly recessed)
Theme/text_hierarchy_depth(Tertiary)   → -4  (moderately recessed)
Theme/text_hierarchy_depth(Disabled)   → -6  (deeply recessed)

-- Calculate lighting-adjusted color based on depth
Theme/text_depth_color(z_position, base_color)
```

### Visual Hierarchy Achieved

```
        ↑ Z-AXIS (toward viewer)
        |
    +6  │  "todos" - Hero (very bright, prominent)
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

---

## Capabilities & Design

### Core Principle

Text positioned at different Z-depths receives different amounts of light:
- **Raised text** (positive Z) → catches more light → appears brighter
- **Surface text** (Z = 0) → standard lighting → normal brightness
- **Recessed text** (negative Z) → in shadow → appears dimmer

This creates a **spatial hierarchy** where visual importance emerges from physics, not manual styling.

### Depth + Color Combinations

Pattern 4 isn't just "recessed = dimmer". It's a complete spatial text hierarchy system that combines multiple properties:

```boon
-- Error text: RED + RAISED + GLOWING
Element/text(
    style: [
        font: [color: danger_red]
        transform: [move_closer: 4]      -- Raised: catches light
        material: [emissive: 0.2]        -- Glows from within
    ]
)

-- Success text: GREEN + RAISED + SHINY
Element/text(
    style: [
        font: [color: success_green]
        transform: [move_closer: 2]      -- Slightly raised
        material: [shine: 0.8]           -- Shiny surface
    ]
)

-- Disabled text: GRAY + RECESSED
Element/text(
    style: [
        font: [color: text_gray]
        transform: [move_further: 4]     -- Recessed: in shadow
        material: [opacity: 0.6]         -- Also semi-transparent
    ]
)
```

### Material Properties for Text

Text can have material properties just like UI elements:

```boon
FUNCTION text_material(properties) {
    [
        base_color: properties.color,
        emissive_color: properties.glow_color,
        emissive_intensity: properties.glow,
        reflectivity: properties.shine,
        roughness: 1.0 - properties.gloss,
        opacity: properties.opacity,

        -- Special text properties
        outline_width: properties.outline,
        outline_color: properties.outline_color,
        shadow_offset: properties.shadow,
        shadow_color: properties.shadow_color
    ]
}
```

### Dynamic Lighting Response

Text automatically responds to:
- **Scene lighting changes** (theme mode switch: light/dark)
- **Focus spotlights** (focused input text glows - Pattern 5 integration)
- **Hover effects** (button text brightens on hover - Pattern 1 integration)
- **Animated lights** (loading sweep illuminates text - Pattern 9 integration)

### Integration with Other Patterns

#### Pattern 5 (Focus Spotlight)
```boon
// When input focused, spotlight illuminates text inside
Element/text_input(
    style: [
        font: [color: text_color],
        transform: [move_closer: 0]  -- Surface level
    ]
)

// Focus spotlight makes text BRIGHTER without depth change!
```

#### Pattern 10 (Emissive States)
```boon
Element/text(
    text: "Error: Invalid input",
    style: [
        font: [color: danger_red],
        transform: [move_closer: 4],        -- Raised
        material: has_error |> WHEN {
            True => Theme/text_material(Error)   -- Emissive red glow
            False => Theme/text_material(Primary)
        }
    ]
)

// Error text: Red + Raised + Glowing = VERY visible!
```

#### Pattern 1 (Material Physics)
```boon
// Button text lifts with button on hover
Element/button(
    style: [
        transform: Theme/interaction_transform(...)
    ],
    label: Element/text(
        text: "Click me",
        style: [
            // Text inherits parent transform - lifts with button!
            transform: [move_closer: 0]  // Relative to button surface
        ]
    )
)
```

### Readability Strategies

#### Strategy 1: Contrast-Aware Depth Adjustment
```boon
FUNCTION ensure_contrast(text_color, background_color, min_ratio) {
    BLOCK {
        current_contrast: Color/contrast_ratio(text_color, background_color)

        current_contrast < min_ratio |> WHEN {
            True => BLOCK {
                -- Need more contrast: adjust depth to change brightness
                required_brightness: calculate_brightness_for_contrast(min_ratio)
                depth_adjustment: brightness_to_depth(required_brightness)

                [
                    depth: depth_adjustment,
                    warning: "Depth adjusted for WCAG compliance"
                ]
            }
            False => [depth: 0, warning: None]
        }
    }
}
```

#### Strategy 2: Adaptive Material Properties
```boon
-- High-contrast mode: Increase material response
high_contrast_mode |> WHEN {
    True => [
        reflectivity: 0.9,      -- More responsive to light
        emissive: 0.3,          -- Self-illuminating
        outline: 1,             -- Add outline for definition
    ]
    False => standard_material
}
```

#### Strategy 3: Outline/Halo Shader
```glsl
// In fragment shader
float outline = sample_sdf_outline(uv, outline_width);
vec3 outlined = mix(outline_color, text_color, outline);

// Or halo glow
float glow = smoothstep(glow_radius, 0.0, distance);
vec3 glowing = mix(text_color, glow_color, glow * glow_intensity);
```

#### Strategy 4: Dynamic Range Compression
```boon
-- Prevent text from getting TOO dim when deeply recessed
FUNCTION compress_brightness_range(brightness, min_acceptable) {
    brightness |> Math/max(min_acceptable)
}

-- Example: Never darker than 60% brightness
compressed: calculate_brightness(...) |> Math/max(0.6)
```

### Design Decisions

#### Why These Specific Depth Values?

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

#### Why Not Apply to Button Labels?

Button labels inherit the button's entire transform stack, including:
- Pattern 1: Material physics (`rest_elevation`, `hover_lift`, `press_depression`)
- Pattern 6: Cursor gravity (if enabled)

Adding Pattern 4 depth would create **conflicting transforms**. The button already moves physically, and the text moves with it.

---

## Implementation Roadmap

### Phase 1: Core Infrastructure (Renderer Work)
**Estimated Time:** 3-4 weeks

1. **SDF Font Atlas Generation**
   - Pre-compute signed distance fields for all glyphs
   - Multi-channel SDF for better quality (MSDF)
   - Multiple resolution levels (LOD)
   - Compress and cache

2. **3D Text Mesh Generation**
   ```rust
   struct TextVertex {
       position: vec3,      // 3D position (includes Z depth!)
       uv: vec2,            // Texture coordinates for SDF atlas
       color: vec4,         // Base color + alpha
       material: u32,       // Material properties index
   }
   ```

3. **Depth-Based Lighting Shader**
   ```glsl
   // 1. Sample SDF for anti-aliased edge
   float distance = texture(sdf_atlas, uv).a;
   float alpha = smoothstep(0.5 - smoothing, 0.5 + smoothing, distance);

   // 2. Calculate lighting based on 3D position
   vec3 normal = vec3(0, 0, 1);  // Text faces camera
   vec3 light_dir = normalize(light_position - world_position);
   float diffuse = max(dot(normal, light_dir), 0.0);

   // 3. Apply depth-based dimming
   float depth_factor = calculate_depth_lighting(world_position.z);

   // 4. Combine
   vec3 lit_color = base_color * diffuse * depth_factor;
   fragColor = vec4(lit_color, alpha);
   ```

4. **Integration with Scene Lighting**
   ```boon
   FUNCTION calculate_text_lighting(z_position, base_color, lights, material) {
       BLOCK {
           -- Accumulate lighting from all scene lights
           total_light: lights
               |> List/map(light, new: calculate_light_contribution(
                   position: [x: text.x, y: text.y, z: z_position],
                   normal: [x: 0, y: 0, z: 1],  -- Faces camera
                   light: light,
                   material: material
               ))
               |> List/sum()

           -- Apply depth factor (recessed text in shadow)
           depth_factor: z_position |> WHEN {
               z if z > 0 => 1.0 + (z * 0.05)     -- Raised: brighter
               z if z < 0 => 1.0 + (z * 0.08)     -- Recessed: dimmer
               _ => 1.0
           }

           -- Combine with base color
           [
               color: base_color * total_light * depth_factor,
               opacity: calculate_opacity(z_position, material)
           ]
       }
   }
   ```

### Phase 2: Material System (Renderer Work)
**Estimated Time:** 2-3 weeks

5. **Text Material Properties**
   - Emissive glow (self-illuminating text)
   - Reflectivity/shine
   - Roughness/gloss
   - Opacity

6. **Outline and Halo Effects**
   - SDF-based outline rendering
   - Gaussian blur for halo
   - Multi-pass compositing

7. **Dynamic Material Switching**
   - State-based materials (Error, Success, Warning)
   - Smooth transitions between states

### Phase 3: Performance (Renderer Work)
**Estimated Time:** 2-3 weeks

8. **Text Baking System**
   ```boon
   -- Pre-compute lighting at build time for static text
   baked_text_cache: [
       "Submit button": [
           vertices: [...],
           lit_colors: [...]  -- Pre-lit vertices
       ]
   ]
   ```

9. **Instancing for Repeated Text**
   ```rust
   // Single draw call for all items with same text
   struct TextInstance {
       transform: mat4,
       depth: f32,
       color_override: vec4,
   }

   draw_instanced(glyph_mesh, instances);
   ```

10. **LOD System**
    ```boon
    FUNCTION text_lod(distance_to_camera, text_size) {
        distance_to_camera |> WHEN {
            d if d < 100 => HighQuality    -- Full SDF + lighting
            d if d < 500 => MediumQuality  -- Simplified lighting
            d => LowQuality                -- Flat color, no 3D
        }
    }
    ```

11. **Lighting Cache**
    ```boon
    -- Cache lighting calculations per frame
    text_lighting_cache: Map {
        z_level: [
            -6 => precomputed_lighting(-6),
            -4 => precomputed_lighting(-4),
            -2 => precomputed_lighting(-2),
             0 => precomputed_lighting(0),
            +2 => precomputed_lighting(2),
            +4 => precomputed_lighting(4),
        ]
    }
    ```

### Phase 4: Accessibility (Renderer Work)
**Estimated Time:** 1-2 weeks

12. **Contrast Ratio Calculation**
    - WCAG compliance checking
    - Runtime contrast validation

13. **Auto-Adjustment for WCAG**
    - Automatic depth adjustment to meet contrast requirements
    - Warning system for non-compliant combinations

14. **High Contrast Mode**
    - Increased material response
    - Stronger emissive glow
    - Optional outlines

15. **HTML Overlay Integration**
    - Screen reader compatibility
    - Text selection support
    - Copy/paste functionality

### Phase 5: Theme Integration (Boon App Code)
**Estimated Time:** 1 week

16. **Theme API for Text Importance**
    ```boon
    FUNCTION text_importance_config(importance) {
        importance |> WHEN {
            Hero => [
                depth: 6,           -- Very raised
                emissive: 0.1,      -- Slight glow
                shine: 0.8,         -- Shiny
                outline: 0          -- No outline needed
            ]
            Primary => [depth: 0, emissive: 0, shine: 0.5, outline: 0]
            Secondary => [depth: -2, emissive: 0, shine: 0.3, outline: 0]
            Tertiary => [depth: -4, emissive: 0, shine: 0.2, outline: 0]
            Disabled => [depth: -6, emissive: 0, shine: 0.1, outline: 0, opacity: 0.6]
        }
    }
    ```

17. **Semantic Text Configs**
    ```boon
    FUNCTION text_semantic_config(semantic, importance) {
        BLOCK {
            base: text_importance_config(importance)

            semantic |> WHEN {
                Error => [
                    ...base,
                    depth: base.depth + 4,      -- Raise for visibility
                    emissive: 0.25,             -- Red glow
                    outline: 1,                 -- Add definition
                ]
                Success => [
                    ...base,
                    depth: base.depth + 2,
                    emissive: 0.15,             -- Green glow
                    shine: 0.9                  -- Shiny success!
                ]
                Warning => [
                    ...base,
                    depth: base.depth + 2,
                    emissive: 0.2,              -- Yellow glow
                    outline: 1
                ]
                Default => base
            }
        }
    }
    ```

18. **Integration with Existing Patterns**
    - Pattern 5 (Focus Spotlight): Text responds to focus light
    - Pattern 10 (Emissive States): Error/success text glows
    - Pattern 1 (Material Physics): Button text moves with button

### Success Metrics

**Visual Quality:**
- ✅ Smooth anti-aliasing (SDF-based)
- ✅ Crisp at any zoom level
- ✅ Consistent lighting with scene
- ✅ Natural depth perception

**Performance:**
- ✅ 60 FPS with 10,000+ text elements
- ✅ < 1ms per frame for text rendering
- ✅ Minimal memory overhead vs. flat text

**Accessibility:**
- ✅ WCAG AAA contrast ratios met
- ✅ Screen reader compatible (HTML overlay)
- ✅ High contrast mode support
- ✅ User preferences respected

**Developer Experience:**
- ✅ Simple API for common cases
- ✅ Full control when needed
- ✅ Works with existing patterns
- ✅ Clear documentation

### Timeline Summary

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| Phase 1 | 3-4 weeks | Core 3D text rendering with depth lighting |
| Phase 2 | 2-3 weeks | Material system and effects |
| Phase 3 | 2-3 weeks | Performance optimizations |
| Phase 4 | 1-2 weeks | Accessibility compliance |
| Phase 5 | 1 week | Theme integration |
| **Total** | **9-13 weeks** | Production-ready Pattern 4 |

---

## Current Status

### ✅ Completed (Boon App Code)

**Theme API:**
- `text_hierarchy_depth()` function implemented in all 4 themes
- `text_depth_color()` function available (not actively used yet)
- Semantic importance levels defined (Hero, Primary, Secondary, Tertiary, Disabled)

**TodoMVC Integration:**
- 4 text elements using Pattern 4
- Dynamic depth based on completion state (completed todos recede)
- Visual hierarchy established through Z-positioning

**Documentation:**
- File header in `todo_mvc_physical.bn` updated with Pattern 4 reference
- Theme files document depth values
- Usage examples in place

### ⏳ Pending (Renderer Implementation)

**Required for Full Pattern 4:**
- SDF text rendering pipeline
- 3D text mesh generation
- Depth-based lighting shader
- Material properties for text (emissive, shine, outline)
- Performance optimizations (baking, instancing, LOD)
- Accessibility features (contrast checking, HTML overlay)

**Repository:**
- Renderer work happens in **separate repository** (not this Boon app repo)
- This documentation serves as specification for renderer team

### Usage Guidelines

#### When to Use Each Importance Level

**Hero:**
- Main page titles
- Critical call-to-action buttons (when using text elements)
- Large promotional text

**Primary:**
- Body text
- Active list items
- Main navigation labels
- Input text

**Secondary:**
- Supporting information
- Counters and badges
- Captions
- Subtle hints

**Tertiary:**
- Fine print
- Footer text
- Completed/archived items
- Legal disclaimers

**Disabled:**
- Inactive form inputs
- Unavailable options
- Ghosted text

### Theme API Examples

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

**Last Updated:** 2025-11-13
**Next Review:** After renderer implementation complete
