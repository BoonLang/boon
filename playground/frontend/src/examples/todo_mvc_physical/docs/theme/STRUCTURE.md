# Theme System Structure

## File Organization

```
todo_mvc_physical/
├── todo_mvc_physical.bn          (Main app - references themes)
└── Theme/                        (Theme definitions)
    ├── README.md                  (Overview)
    ├── USAGE.md                   (Usage guide)
    ├── ARCHITECTURE.md            (System design)
    ├── STRUCTURE.md               (This file)
    ├── Professional.bn            (Default theme)
    ├── Neobrutalism.bn            (Bold theme)
    ├── Glassmorphism.bn           (Glass theme)
    └── Neumorphism.bn             (Soft theme)
```

## Naming Convention

### Folder: `Theme/` (capitalized)
- Capital `T` to indicate it's a module/namespace
- Matches Boon convention for folders that export definitions

### Files: `Professional.bn`, `Neobrutalism.bn`, etc. (capitalized)
- Capital first letter for theme names
- `.bn` extension for Boon code files

### Function: `theme(mode)` (lowercase)
- Consistent function name across all themes
- Each file exports exactly one function: `theme`

## Usage Pattern

The syntax follows: `Folder/File/function(args)`

```boon
theme: Theme/Professional/theme(mode: Light)
        ↑         ↑           ↑          ↑
     folder     file      function    argument
```

### Examples:

```boon
-- Professional theme, light mode
theme: Theme/Professional/theme(mode: Light)

-- Neobrutalism theme, dark mode
theme: Theme/Neobrutalism/theme(mode: Dark)

-- Glassmorphism theme, light mode
theme: Theme/Glassmorphism/theme(mode: Light)

-- Neumorphism theme, dark mode
theme: Theme/Neumorphism/theme(mode: Dark)
```

## Why This Structure?

### ✅ Clear namespace
- `Theme/Professional` is unambiguous
- No conflicts with other `Professional` definitions

### ✅ Consistent function name
- All themes export `theme(mode)`
- No need to remember different function names

### ✅ Clean syntax
- `Theme/Professional/theme(mode: Light)` is concise
- Easy to switch: just change the file name

### ✅ Discoverable
- IDE can autocomplete: `Theme/` → shows all available themes
- Folder structure makes it obvious what themes exist

### ✅ Follows patterns
- Similar to module systems in other languages
- Like `std::collections::HashMap::new()` in Rust
- Like `@material-ui/core/Button` in npm

## Theme File Structure

Each theme file (e.g., `Professional.bn`) contains:

```boon
-- Theme Name
-- Brief description of the theme's visual style

FUNCTION theme(mode) {
    [
        -- Lighting configuration
        lights: LIST { ... }

        -- Geometry settings
        geometry: [
            edge_radius: N
            bevel_angle: N
        ]

        -- Material presets
        materials: [
            panel: [...]
            button: [...]
            -- etc
        ]

        -- Elevation scale
        elevation: [
            card: N
            Button: N
            -- etc
        ]

        -- Depth scale
        depth: [
            Container: N
            Element: N
            -- etc
        ]

        -- Interaction physics
        interaction: [
            hover_lift: N
            press_depth: N
            -- etc
        ]

        -- Corner radius scale
        corners: [
            Edge: N
            Element: N
            -- etc
        ]

        -- Color palette (mode-dependent)
        colors: mode |> WHEN {
            Light => [...]
            Dark => [...]
        }
    ]
}
```

## Integration Example

```boon
-- todo_mvc_physical.bn

-- Define theme
theme: Theme/Professional/theme(mode: Light)

-- Apply theme to scene
scene: Scene/new(
    root: root_element(PASS: [store: store, theme: theme])
    lights: theme.lights
    geometry: theme.geometry
    materials: theme.materials
    elevation: theme.elevation
    depth: theme.depth
    interaction: theme.interaction
    corners: theme.corners
    colors: theme.colors
)

-- Elements access theme via PASSED context
FUNCTION my_button() {
    Element/button(
        style: [
            depth: PASSED.theme.depth.Element
            elevation: PASSED.theme.elevation.Button
            material: PASSED.theme.materials.button
            backgSoft: [color: PASSED.theme.colors.surface_variant]
        ]
        label: TEXT { Click me }
    )
}
```

## Adding New Themes

To create a new theme:

1. **Create file**: `Theme/MyTheme.bn`
2. **Define function**: `FUNCTION theme(mode) { [...] }`
3. **Fill in all properties**: lights, geometry, materials, etc.
4. **Test both modes**: Light and Dark
5. **Use it**: `theme: Theme/MyTheme/theme(mode: Light)`

## Comparison to Old Approach

### Old (before restructure):
```boon
-- Inconsistent:
theme: Professional(mode: Light)      -- Function call? Module?
theme: Neobrutalism(mode: Dark)       -- Where is this defined?
```

### New (current):
```boon
-- Clear namespace:
theme: Theme/Professional/theme(mode: Light)   -- Folder/File/function
theme: Theme/Neobrutalism/theme(mode: Dark)    -- Obvious structure
```

## Future Enhancements

### Custom Theme Paths
```boon
-- User-defined themes in different folder
theme: MyTheme/CorporateBrand/theme(mode: Light)
```

### Theme Composition
```boon
-- Merge themes
base: Theme/Professional/theme(mode: Light)
custom: Theme/MyCustom/theme(mode: Light)
theme: Theme/merge(base, custom)
```

### Theme Variants
```boon
-- Theme with preset mode
theme: Theme/Professional/light()  -- Shorthand for theme(mode: Light)
theme: Theme/Professional/dark()   -- Shorthand for theme(mode: Dark)
```
