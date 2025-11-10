# Pattern 4: Text Z-Position Implementation Plan

## Comprehensive Implementation Roadmap

---

## PHASE 1: Core Infrastructure (Foundation)

### 1.1 SDF Font Atlas Generation
**Goal:** Generate signed distance field texture atlas from font files

**Tasks:**
- [ ] **Research SDF generation libraries**
  - Evaluate msdfgen (multi-channel SDF)
  - Evaluate font-rs + custom SDF generation
  - Compare quality vs. performance trade-offs

- [ ] **Implement SDF atlas builder**
  - [ ] Load TTF/OTF font files
  - [ ] Extract glyph outlines
  - [ ] Generate SDF for each glyph (resolution: 64x64 base)
  - [ ] Pack glyphs into texture atlas (with padding for bleeding)
  - [ ] Store glyph metrics (advance, bearing, size)

- [ ] **Multi-resolution atlas generation**
  - [ ] Generate 3 LOD levels: high (64px), medium (32px), low (16px)
  - [ ] Store as mipmapped texture

- [ ] **Compression and caching**
  - [ ] Compress atlas textures (BC4/BC5 format)
  - [ ] Cache generated atlases to disk
  - [ ] Implement cache invalidation (font version check)

**Outputs:**
- `FontAtlas` struct with texture + glyph metadata
- Atlas generation CLI tool or build-time script
- Cached atlas files (.atlas format)

---

### 1.2 3D Text Mesh Generation
**Goal:** Convert text strings into 3D geometry with depth

**Tasks:**
- [ ] **Text layout engine**
  - [ ] Implement line breaking (word wrap)
  - [ ] Implement text alignment (left, center, right, justify)
  - [ ] Calculate bounding boxes
  - [ ] Handle Unicode (bidirectional text, combining characters)

- [ ] **Mesh generation**
  - [ ] Create quad for each glyph
  - [ ] Set UV coordinates from atlas
  - [ ] Apply 3D position (X, Y, **Z**) - Z is the depth!
  - [ ] Calculate normals (facing camera)

- [ ] **Vertex structure**
  ```rust
  struct TextVertex {
      position: [f32; 3],     // X, Y, Z (Z = depth!)
      uv: [f32; 2],           // Atlas texture coords
      color: [f32; 4],        // Base RGBA color
      material_id: u32,       // Index to material properties
      normal: [f32; 3],       // For lighting
  }
  ```

- [ ] **Batching**
  - [ ] Group glyphs by font/size/material
  - [ ] Create instanced rendering for repeated glyphs
  - [ ] Dynamic buffer updates for text changes

**Outputs:**
- `TextMesh` struct with vertices + indices
- `TextLayoutEngine` for positioning
- Batch rendering system

---

### 1.3 Depth-Based Lighting Shader
**Goal:** Shader that calculates lighting based on 3D text position

**Tasks:**
- [ ] **Vertex shader**
  ```glsl
  #version 450

  layout(location = 0) in vec3 position;
  layout(location = 1) in vec2 uv;
  layout(location = 2) in vec4 color;
  layout(location = 3) in uint material_id;

  layout(location = 0) out vec2 frag_uv;
  layout(location = 1) out vec4 frag_color;
  layout(location = 2) out vec3 world_pos;
  layout(location = 3) out flat uint frag_material_id;

  uniform mat4 view_projection;

  void main() {
      world_pos = position;
      gl_Position = view_projection * vec4(position, 1.0);
      frag_uv = uv;
      frag_color = color;
      frag_material_id = material_id;
  }
  ```

- [ ] **Fragment shader**
  ```glsl
  #version 450

  layout(location = 0) in vec2 frag_uv;
  layout(location = 1) in vec4 frag_color;
  layout(location = 2) in vec3 world_pos;
  layout(location = 3) in flat uint frag_material_id;

  layout(location = 0) out vec4 out_color;

  uniform sampler2D sdf_atlas;
  uniform vec3 light_positions[4];
  uniform vec3 light_colors[4];
  uniform float light_intensities[4];
  uniform int num_lights;

  struct Material {
      float emissive;
      float shine;
      float roughness;
      float outline_width;
  };

  uniform Material materials[256];

  float sample_sdf(vec2 uv) {
      return texture(sdf_atlas, uv).a;
  }

  float calculate_depth_factor(float z) {
      // Raised text (+Z) catches more light
      // Recessed text (-Z) in shadow
      return z > 0.0 ? 1.0 + (z * 0.05) : 1.0 + (z * 0.08);
  }

  vec3 calculate_lighting(vec3 pos, vec3 normal, Material mat) {
      vec3 total_light = vec3(0.0);

      for (int i = 0; i < num_lights; i++) {
          vec3 light_dir = normalize(light_positions[i] - pos);
          float diffuse = max(dot(normal, light_dir), 0.0);
          float dist = length(light_positions[i] - pos);
          float attenuation = 1.0 / (dist * dist);

          total_light += light_colors[i] * diffuse * attenuation * light_intensities[i];
      }

      return total_light;
  }

  void main() {
      // SDF antialiasing
      float distance = sample_sdf(frag_uv);
      float smoothing = fwidth(distance);
      float alpha = smoothstep(0.5 - smoothing, 0.5 + smoothing, distance);

      if (alpha < 0.01) discard;

      Material mat = materials[frag_material_id];
      vec3 normal = vec3(0.0, 0.0, 1.0);  // Face camera

      // Lighting calculation
      vec3 lighting = calculate_lighting(world_pos, normal, mat);

      // Depth-based brightness adjustment
      float depth_factor = calculate_depth_factor(world_pos.z);

      // Combine
      vec3 lit_color = frag_color.rgb * lighting * depth_factor;

      // Add emissive
      lit_color += frag_color.rgb * mat.emissive;

      out_color = vec4(lit_color, alpha * frag_color.a);
  }
  ```

- [ ] **Shader variants**
  - [ ] Basic: SDF + simple lighting
  - [ ] Advanced: + outline + shadow
  - [ ] Performance: Simplified for LOD

**Outputs:**
- GLSL/WGSL shader code
- Shader compilation pipeline
- Uniform binding system

---

### 1.4 Integration with Scene Lighting
**Goal:** Text responds to all scene lights (directional, ambient, spotlight)

**Tasks:**
- [ ] **Pass scene lights to text shader**
  - [ ] Collect all Light types from scene
  - [ ] Convert to shader-friendly format
  - [ ] Upload as uniform buffer

- [ ] **Light type support**
  - [ ] Directional lights
  - [ ] Ambient lights
  - [ ] Point lights
  - [ ] Spotlights (for Pattern 5 focus)
  - [ ] Sweeping lights (for Pattern 9 loading)

- [ ] **Dynamic light updates**
  - [ ] Rebuild light buffer when lights change
  - [ ] Efficient update (only changed lights)
  - [ ] Frame-coherent updates

**Outputs:**
- Light uniform buffer system
- Integration with existing Scene/new lights

---

## PHASE 2: Material System (Visual Richness)

### 2.1 Text Material Properties
**Goal:** Rich material system for text (emissive, shine, outline, etc.)

**Tasks:**
- [ ] **Define TextMaterial struct**
  ```rust
  struct TextMaterial {
      base_color: [f32; 4],
      emissive_color: [f32; 3],
      emissive_intensity: f32,
      reflectivity: f32,      // How much light reflects
      roughness: f32,         // Surface roughness
      opacity: f32,
      outline_width: f32,
      outline_color: [f32; 4],
      shadow_offset: [f32; 2],
      shadow_color: [f32; 4],
  }
  ```

- [ ] **Implement in Theme/Professional.bn**
  ```boon
  FUNCTION text_material(importance, semantic) {
      BLOCK {
          base: importance |> WHEN {
              Hero => [emissive: 0.1, shine: 0.8, outline: 0]
              Primary => [emissive: 0, shine: 0.5, outline: 0]
              Secondary => [emissive: 0, shine: 0.3, outline: 0]
              Disabled => [emissive: 0, shine: 0.1, opacity: 0.6]
          }

          semantic |> WHEN {
              Error => [...base, emissive: 0.25, outline: 1]
              Success => [...base, emissive: 0.15, shine: 0.9]
              Default => base
          }
      }
  }
  ```

**Outputs:**
- TextMaterial API
- Theme integration
- Material buffer system

---

### 2.2 Outline and Halo Effects
**Goal:** Shader-based outline and glow for readability

**Tasks:**
- [ ] **Outline implementation**
  ```glsl
  float sample_outline(vec2 uv, float width) {
      float dist = sample_sdf(uv);
      float outline_start = 0.5 - width;
      float outline_end = 0.5;
      return smoothstep(outline_start, outline_end, dist);
  }

  // In main():
  if (mat.outline_width > 0.0) {
      float outline_alpha = sample_outline(frag_uv, mat.outline_width);
      lit_color = mix(mat.outline_color.rgb, lit_color, outline_alpha);
  }
  ```

- [ ] **Halo/glow implementation**
  ```glsl
  float calculate_glow(float distance, float radius, float intensity) {
      return smoothstep(radius, 0.0, distance) * intensity;
  }

  // Apply glow around text
  float glow = calculate_glow(distance, glow_radius, glow_intensity);
  vec3 glow_color = mat.emissive_color * glow;
  lit_color += glow_color;
  ```

- [ ] **Shadow implementation**
  ```glsl
  // Sample SDF at offset position for drop shadow
  vec2 shadow_uv = frag_uv + mat.shadow_offset;
  float shadow_dist = sample_sdf(shadow_uv);
  float shadow_alpha = smoothstep(0.45, 0.55, shadow_dist);

  // Blend shadow behind text
  out_color = mix(
      vec4(mat.shadow_color.rgb, shadow_alpha),
      vec4(lit_color, alpha),
      alpha
  );
  ```

**Outputs:**
- Enhanced shader with outline/halo/shadow
- Material properties for each effect
- Performance variants (LOD)

---

### 2.3 Dynamic Material Switching
**Goal:** Change material based on state (hover, focus, error, etc.)

**Tasks:**
- [ ] **State-based material selection**
  ```boon
  Element/text(
      text: "Submit",
      style: [
          material: LIST { element.hovered, has_error } |> WHEN {
              LIST { _, True } => Theme/text_material(Primary, Error)
              LIST { True, False } => Theme/text_material(Primary, Hover)
              LIST { False, False } => Theme/text_material(Primary, Default)
          }
      ]
  )
  ```

- [ ] **Animated transitions**
  - [ ] Lerp between material properties
  - [ ] Smooth emissive fade in/out
  - [ ] Outline width animation

**Outputs:**
- State-based material system
- Transition animations
- Integration with Element state

---

## PHASE 3: Performance (Production Ready)

### 3.1 Text Baking System
**Goal:** Pre-compute lighting for static text

**Tasks:**
- [ ] **Identify static text**
  - [ ] Analyze text for changes (const vs. reactive)
  - [ ] Mark baakable text at compile time

- [ ] **Pre-compute lighting**
  - [ ] Run lighting shader offline
  - [ ] Store lit vertex colors
  - [ ] Cache to disk

- [ ] **Runtime rendering**
  - [ ] Use pre-lit colors (skip lighting calculation)
  - [ ] Re-bake only when lights change

- [ ] **Invalidation**
  - [ ] Detect light changes
  - [ ] Incremental re-baking
  - [ ] Background processing

**Outputs:**
- Baking pipeline
- Cache format
- Runtime loader

---

### 3.2 Instancing for Repeated Text
**Goal:** Single draw call for same text repeated multiple times

**Tasks:**
- [ ] **Instance buffer**
  ```rust
  struct TextInstance {
      transform: [[f32; 4]; 4],  // 4x4 matrix
      depth: f32,
      color_override: [f32; 4],
      material_id: u32,
  }
  ```

- [ ] **Instanced rendering**
  - [ ] Detect duplicate text
  - [ ] Build instance buffer
  - [ ] Draw all instances in one call

- [ ] **Dynamic updates**
  - [ ] Update instance buffer for changes
  - [ ] Efficient partial updates

**Outputs:**
- Instanced rendering path
- Instance buffer management
- Batching system

---

### 3.3 LOD System
**Goal:** Reduce quality for distant text

**Tasks:**
- [ ] **Distance calculation**
  ```rust
  fn calculate_lod(camera_pos: Vec3, text_pos: Vec3, text_size: f32) -> LOD {
      let distance = (camera_pos - text_pos).length();
      let screen_size = text_size / distance;

      match screen_size {
          s if s > 0.05 => LOD::High,
          s if s > 0.02 => LOD::Medium,
          _ => LOD::Low,
      }
  }
  ```

- [ ] **LOD variants**
  - **High**: Full SDF + full lighting + outline
  - **Medium**: SDF + simplified lighting
  - **Low**: Flat color, no SDF (just quad)

- [ ] **Smooth transitions**
  - [ ] Fade between LODs
  - [ ] Hysteresis to prevent popping

**Outputs:**
- LOD selection system
- Shader variants for each LOD
- Transition system

---

### 3.4 Lighting Cache
**Goal:** Cache lighting calculations for text at same Z level

**Tasks:**
- [ ] **Build cache**
  ```rust
  struct LightingCache {
      entries: HashMap<i32, CachedLighting>,  // Z-level -> lighting
      last_update: Instant,
  }

  struct CachedLighting {
      diffuse: f32,
      depth_factor: f32,
      total_light: Vec3,
  }
  ```

- [ ] **Cache lookup**
  - [ ] Quantize Z position to cache level
  - [ ] Return cached lighting
  - [ ] Interpolate between levels for smooth falloff

- [ ] **Invalidation**
  - [ ] Invalidate when lights change
  - [ ] Incremental rebuild
  - [ ] LRU eviction for memory

**Outputs:**
- Lighting cache system
- Cache management
- Invalidation logic

---

### 3.5 Compute Shader Optimization
**Goal:** Batch lighting calculations on GPU

**Tasks:**
- [ ] **Compute shader**
  ```wgsl
  @compute @workgroup_size(256)
  fn calculate_text_lighting(
      @builtin(global_invocation_id) id: vec3<u32>
  ) {
      let text_idx = id.x;
      if (text_idx >= num_texts) { return; }

      let text = texts[text_idx];
      let lighting = calculate_lighting(text.position, lights);

      output[text_idx] = lighting;
  }
  ```

- [ ] **Dispatch**
  - [ ] Build text position buffer
  - [ ] Dispatch compute shader
  - [ ] Read back results
  - [ ] Update vertex buffer

**Outputs:**
- Compute shader pipeline
- GPU-side lighting calculation
- Integration with render loop

---

## PHASE 4: Accessibility (WCAG Compliance)

### 4.1 Contrast Ratio Calculation
**Goal:** Calculate and enforce WCAG contrast ratios

**Tasks:**
- [ ] **Implement WCAG formula**
  ```rust
  fn calculate_contrast_ratio(
      foreground: Color,
      background: Color
  ) -> f32 {
      let l1 = relative_luminance(foreground);
      let l2 = relative_luminance(background);

      let lighter = l1.max(l2);
      let darker = l1.min(l2);

      (lighter + 0.05) / (darker + 0.05)
  }

  fn relative_luminance(color: Color) -> f32 {
      let [r, g, b] = color.to_linear();
      0.2126 * r + 0.7152 * g + 0.0722 * b
  }
  ```

- [ ] **Check against standards**
  - [ ] AA: 4.5:1 for normal text, 3:1 for large text
  - [ ] AAA: 7:1 for normal text, 4.5:1 for large text

- [ ] **Report violations**
  - [ ] Warn in dev mode
  - [ ] Log in console
  - [ ] Optional strict mode (fail on violation)

**Outputs:**
- Contrast calculation utility
- WCAG compliance checker
- Dev mode warnings

---

### 4.2 Auto-Adjustment for Compliance
**Goal:** Automatically adjust depth/material to meet contrast requirements

**Tasks:**
- [ ] **Contrast-aware depth adjustment**
  ```boon
  FUNCTION ensure_contrast(text_config, background, min_ratio) {
      BLOCK {
          current_contrast: calculate_contrast(text_config.color, background)

          current_contrast < min_ratio |> WHEN {
              True => BLOCK {
                  -- Adjust depth to change brightness
                  required_brightness: (min_ratio * background.lightness) - 0.05
                  current_brightness: text_config.color.lightness
                  brightness_delta: required_brightness - current_brightness

                  -- Depth adjustment: +1 depth = +5% brightness
                  depth_adjustment: brightness_delta / 0.05

                  [
                      ...text_config,
                      depth: text_config.depth + depth_adjustment,
                      adjusted: True
                  ]
              }
              False => text_config
          }
      }
  }
  ```

- [ ] **Material boost**
  - [ ] Increase emissive if depth adjustment not enough
  - [ ] Add outline for definition
  - [ ] Increase reflectivity

**Outputs:**
- Auto-adjustment system
- Contrast enforcement
- Fallback strategies

---

### 4.3 High Contrast Mode
**Goal:** Special rendering for high contrast accessibility preference

**Tasks:**
- [ ] **Detect high contrast mode**
  ```boon
  high_contrast_enabled: System/prefers_high_contrast()
  ```

- [ ] **Override materials**
  ```boon
  high_contrast_enabled |> WHEN {
      True => [
          emissive: 0.3,       -- Self-illuminating
          reflectivity: 0.9,   -- Very responsive to light
          outline: 2,          -- Thick outline
          depth: depth + 4     -- Raise significantly
      ]
      False => standard_material
  }
  ```

- [ ] **Simplified rendering**
  - [ ] Black/white only in extreme mode
  - [ ] No subtle effects (shadows, glows)
  - [ ] Maximum contrast

**Outputs:**
- High contrast mode detection
- Override material system
- Simplified renderer variant

---

### 4.4 HTML Overlay Integration
**Goal:** Semantic HTML for screen readers

**Tasks:**
- [ ] **Generate HTML overlay**
  ```rust
  fn generate_html_overlay(text_elements: &[TextElement]) -> String {
      let mut html = String::new();

      for element in text_elements {
          html.push_str(&format!(
              r#"<div style="position: absolute; left: {}px; top: {}px; opacity: 0;">
                  {}
              </div>"#,
              element.position.x,
              element.position.y,
              element.text
          ));
      }

      html
  }
  ```

- [ ] **Sync with WebGPU rendering**
  - [ ] Position HTML elements to match 3D positions
  - [ ] Update on text changes
  - [ ] Invisible but accessible (opacity: 0)

- [ ] **Semantic markup**
  - [ ] `<button>` for button text
  - [ ] `<label>` for input labels
  - [ ] `<h1>`, `<h2>` for headers
  - [ ] ARIA attributes for state

**Outputs:**
- HTML overlay generator
- Position synchronization
- Semantic markup system

---

## PHASE 5: Theme Integration (Production API)

### 5.1 Theme API for Text Importance
**Goal:** Simple API for common text hierarchy

**Tasks:**
- [ ] **Implement in Theme/Theme.bn**
  ```boon
  FUNCTION text_hierarchy_depth(importance) {
      PASSED.theme_options.name |> WHEN {
          Professional => importance |> Professional/text_hierarchy_depth()
          Glassmorphism => importance |> Glassmorphism/text_hierarchy_depth()
          Neobrutalism => importance |> Neobrutalism/text_hierarchy_depth()
          Neumorphism => importance |> Neumorphism/text_hierarchy_depth()
      }
  }

  FUNCTION text_material(importance, semantic) {
      PASSED.theme_options.name |> WHEN {
          Professional => Professional/text_material(importance: importance, semantic: semantic)
          ...
      }
  }
  ```

- [ ] **Implement in Professional.bn**
  ```boon
  FUNCTION text_hierarchy_depth(importance) {
      importance |> WHEN {
          Hero => 6
          Primary => 0
          Secondary => -2
          Tertiary => -4
          Disabled => -6
      }
  }

  FUNCTION text_material(importance, semantic) {
      -- See Phase 2.1 for full implementation
      ...
  }
  ```

**Outputs:**
- Theme API functions
- All 4 themes implemented
- Documentation

---

### 5.2 Semantic Text Configs
**Goal:** Pre-configured materials for Error, Success, Warning, Info

**Tasks:**
- [ ] **Implement semantic configs**
  ```boon
  FUNCTION text_semantic_config(semantic, importance) {
      BLOCK {
          base: text_importance_config(importance)

          semantic |> WHEN {
              Error => [
                  ...base,
                  depth: base.depth + 4,         -- Raise for visibility
                  color: danger_red,
                  emissive: 0.25,                -- Red glow
                  outline: 1,
                  outline_color: dark_red
              ]

              Success => [
                  ...base,
                  depth: base.depth + 2,
                  color: success_green,
                  emissive: 0.15,                -- Green glow
                  shine: 0.9                     -- Shiny!
              ]

              Warning => [
                  ...base,
                  depth: base.depth + 2,
                  color: warning_yellow,
                  emissive: 0.2,                 -- Yellow glow
                  outline: 1
              ]

              Info => [
                  ...base,
                  color: info_blue,
                  emissive: 0.05                 -- Subtle blue glow
              ]

              Default => base
          }
      }
  }
  ```

**Outputs:**
- Semantic text API
- Pre-configured materials
- All semantic states

---

### 5.3 Integration with Pattern 5 (Focus Spotlight)
**Goal:** Text brightens when parent element focused

**Tasks:**
- [ ] **Detect parent focus**
  ```boon
  Element/text_input(
      element: [focused: LINK],
      label: Element/text(
          text: "Email",
          style: [
              -- Text inherits parent's focus spotlight!
              -- No special handling needed - lighting system does it
          ]
      )
  )
  ```

- [ ] **Optional explicit brightening**
  ```boon
  Element/text(
      text: "Focused text",
      style: [
          transform: parent.focused |> WHEN {
              True => [move_closer: 2]     -- Raise when parent focused
              False => [move_closer: 0]
          }
      ]
  )
  ```

**Outputs:**
- Automatic integration with spotlight
- Optional explicit depth changes
- Examples in documentation

---

### 5.4 Integration with Pattern 10 (Emissive)
**Goal:** Combine depth + emissive for powerful effects

**Tasks:**
- [ ] **Error text example**
  ```boon
  Element/text(
      text: "Error: Invalid input",
      style: [
          font: [color: Theme/font(of: Error).color],
          transform: [move_closer: 4],                 -- Raised
          material: Theme/text_material(Primary, Error) -- Emissive red
      ]
  )
  ```

- [ ] **Success animation**
  ```boon
  Element/text(
      text: "Saved!",
      style: [
          transform: [move_closer: saved |> WHEN {
              True => 6      -- Pop up on success
              False => 0
          }],
          material: Theme/text_material(Primary, Success)
      ]
  )
  ```

**Outputs:**
- Combined pattern examples
- Best practices guide
- Demo scene

---

### 5.5 Integration with Pattern 1 (Material Physics)
**Goal:** Text moves with parent element interactions

**Tasks:**
- [ ] **Button text lifts with button**
  ```boon
  Element/button(
      style: [
          transform: Theme/interaction_transform(
              material: Button,
              state: [hovered: element.hovered, pressed: element.pressed]
          )
      ],
      label: Element/text(
          text: "Click me",
          style: [
              -- Text position relative to button surface
              -- Automatically lifts with button!
              transform: [move_closer: 0]
          ]
      )
  )
  ```

- [ ] **Independent text animation**
  ```boon
  Element/text(
      text: "Hover me",
      style: [
          transform: element.hovered |> WHEN {
              True => [move_closer: 2]     -- Text lifts independently
              False => [move_closer: 0]
          }
      ]
  )
  ```

**Outputs:**
- Parent-child transform composition
- Independent text animations
- Examples for both approaches

---

## Success Criteria

### Visual Quality ✅
- [ ] Smooth anti-aliasing at all scales
- [ ] Consistent with scene lighting
- [ ] Natural depth perception
- [ ] No visible artifacts (banding, aliasing, bleeding)

### Performance ✅
- [ ] 60 FPS with 10,000 text elements
- [ ] < 1ms per frame for text rendering
- [ ] < 10MB memory overhead
- [ ] Smooth LOD transitions

### Accessibility ✅
- [ ] WCAG AAA contrast ratios met
- [ ] Screen reader compatible
- [ ] High contrast mode works
- [ ] Keyboard navigation preserved

### Developer Experience ✅
- [ ] Simple API for 80% use cases
- [ ] Full control for 20% edge cases
- [ ] Clear error messages
- [ ] Comprehensive documentation
- [ ] Examples for all patterns

### Integration ✅
- [ ] Works with all other patterns (1, 2, 5, 10)
- [ ] Consistent with theme system
- [ ] No breaking changes to existing API
- [ ] Migration guide provided

---

## Dependencies & Prerequisites

### External Libraries
- [ ] msdfgen (or alternative) for SDF generation
- [ ] harfbuzz for text shaping (complex scripts)
- [ ] rusttype or ttf-parser for font parsing
- [ ] wgpu for WebGPU rendering

### Internal Systems
- [ ] Scene lighting system (Pattern 5)
- [ ] Material system (Pattern 10)
- [ ] Theme system (Professional, etc.)
- [ ] Element system (Element/text API)

---

## Timeline Estimate

### Phase 1: Core Infrastructure (4-6 weeks)
- Week 1-2: SDF atlas generation
- Week 3-4: 3D text mesh generation
- Week 5: Depth lighting shader
- Week 6: Scene integration

### Phase 2: Material System (3-4 weeks)
- Week 1: Text material properties
- Week 2: Outline/halo effects
- Week 3: Dynamic materials
- Week 4: Testing & refinement

### Phase 3: Performance (3-4 weeks)
- Week 1: Text baking
- Week 2: Instancing & LOD
- Week 3: Lighting cache
- Week 4: Compute shader optimization

### Phase 4: Accessibility (2-3 weeks)
- Week 1: Contrast calculation
- Week 2: Auto-adjustment & high contrast
- Week 3: HTML overlay

### Phase 5: Theme Integration (2 weeks)
- Week 1: Theme API & semantic configs
- Week 2: Pattern integration & examples

**Total: 14-19 weeks (~3.5 - 5 months)**

---

## Risk Mitigation

### Technical Risks
- **SDF quality issues**: Have multiple generation algorithms ready
- **Performance bottlenecks**: Profile early, optimize incrementally
- **WebGPU compatibility**: Test on multiple browsers/devices
- **Text shaping complexity**: Start with simple ASCII, add Unicode later

### Design Risks
- **Readability concerns**: Implement contrast checking early
- **Overwhelming effect**: Provide subtle default values
- **Learning curve**: Excellent documentation & examples
- **Theme consistency**: Regular design reviews

### Schedule Risks
- **Scope creep**: Stick to phased approach
- **Dependencies**: Identify early, have fallbacks
- **Testing time**: Allocate 20% for testing/refinement
- **Documentation**: Write as you go, not at end

---

## Next Steps

1. **Review & Approval**: Get stakeholder sign-off on plan
2. **Spike Research**: 1-week spike on SDF generation (evaluate libraries)
3. **Prototype**: Minimal viable prototype (Phase 1.1-1.3) in 2 weeks
4. **Demo**: Show working prototype to get feedback
5. **Iterate**: Refine based on feedback
6. **Full Implementation**: Execute phases 1-5

---

## Conclusion

Pattern 4 transforms from "optional/experimental" to **core capability** when properly understood. This implementation plan provides:

✅ **Complete technical roadmap** (phases, tasks, code examples)
✅ **Realistic timeline** (14-19 weeks)
✅ **Clear success criteria** (visual, performance, accessibility)
✅ **Risk mitigation** strategies
✅ **Integration** with all other patterns

**Pattern 4 is ready for implementation!** 🚀
