# Physical TodoMVC Plan

Implementation plan for making `todo_mvc_physical/` loadable in the playground with multi-file support, tabbed editor UI, and CSS-based physical rendering.

---

## Table of Contents

- [Background](#background)
- [Phase 0: Multi-File Example Infrastructure](#phase-0-multi-file-example-infrastructure)
- [Phase 1: Multi-Tab Editor UI](#phase-1-multi-tab-editor-ui)
- [Phase 2: CSS-Based Physical Rendering](#phase-2-css-based-physical-rendering)
- [Phase 3: VFS/Modules for DD and WASM Engines](#phase-3-vfsmodules-for-dd-and-wasm-engines)
- [Phase 4: Language Features (Deferred)](#phase-4-language-features-deferred)
- [Dependency Graph](#dependency-graph)
- [Critical Files](#critical-files)
- [Verification](#verification)

---

## Background

The `todo_mvc_physical/` example is a sophisticated 3D-styled TodoMVC demonstrating physically-based UI rendering with materials, lighting, and depth. It consists of **8 files totaling ~2,176 lines** across 4 themes:

| File | Lines | Purpose |
|------|-------|---------|
| `RUN.bn` | 784 | Main entry: state, UI hierarchy, Scene/new |
| `BUILD.bn` | 64 | Build script: generates Assets.bn from SVGs |
| `Theme/Theme.bn` | 20 | Theme dispatcher |
| `Theme/Professional.bn` | 320 | Professional theme (clean, corporate) |
| `Theme/Glassmorphism.bn` | 333 | Glassmorphism theme (frosted glass) |
| `Theme/Neobrutalism.bn` | 323 | Neobrutalism theme (bold borders) |
| `Theme/Neumorphism.bn` | 323 | Neumorphism theme (soft shadows) |
| `Generated/Assets.bn` | 9 | Auto-generated icon references |

**Current status:** Not loadable from the playground. The example loading system supports only single-file examples, and the physical rendering properties (`depth`, `gloss`, `Scene/new`, lights) have no bridge implementation.

**Key difference from regular `todo_mvc.bn`:** Uses `Scene/new()` instead of `Document/new()`, theme-based material system instead of inline Oklch colors, and 3D properties (depth, elevation, glow, relief) on every element.

---

## Phase 0: Multi-File Example Infrastructure

**Goal:** Make `todo_mvc_physical` loadable from the playground examples dropdown.

### 0.1 Add multi-file example data structure

**File:** `playground/frontend/src/main.rs`

The current `ExampleData` struct (line 206) holds a single file:

```rust
#[derive(Clone, Copy)]
struct ExampleData {
    filename: &'static str,
    source_code: &'static str,
}
```

Add a new struct for multi-file examples:

```rust
#[derive(Clone, Copy)]
struct MultiFileExampleData {
    /// Display name for the example button
    name: &'static str,
    /// List of (filename, source_code) pairs — all files in the project
    files: &'static [(&'static str, &'static str)],
    /// Entry point filename (e.g. "RUN.bn")
    main_file: &'static str,
}
```

### 0.2 Create include macro for multi-file examples

The existing `make_example_data!` macro (lines 211-218) uses `include_str!` for a single file:

```rust
macro_rules! make_example_data {
    ($name:literal) => {{
        ExampleData {
            filename: concat!($name, ".bn"),
            source_code: include_str!(concat!("examples/", $name, "/", $name, ".bn")),
        }
    }};
}
```

Add a new macro for multi-file examples. Since `include_str!` requires literal paths, each file must be listed explicitly:

```rust
macro_rules! make_multi_file_example_data {
    ($name:literal, main: $main:literal, files: [ $( $path:literal ),+ $(,)? ]) => {{
        MultiFileExampleData {
            name: $name,
            main_file: $main,
            files: &[
                $(
                    ($path, include_str!(concat!("examples/", $name, "/", $path))),
                )+
            ],
        }
    }};
}
```

Register `todo_mvc_physical`:

```rust
const MULTI_FILE_EXAMPLE_DATAS: [MultiFileExampleData; 1] = [
    make_multi_file_example_data!(
        "todo_mvc_physical",
        main: "RUN.bn",
        files: [
            "RUN.bn",
            "BUILD.bn",
            "Theme/Theme.bn",
            "Theme/Professional.bn",
            "Theme/Glassmorphism.bn",
            "Theme/Neobrutalism.bn",
            "Theme/Neumorphism.bn",
            "Generated/Assets.bn",
        ]
    ),
];
```

### 0.3 Multi-file example selection handler

When a multi-file example button is clicked, the handler must populate ALL files into `self.files` instead of just one. The existing `example_button` function (line 2148) creates a single-file map:

```rust
// Current single-file loading (lines 2192-2197)
let mut new_files = BTreeMap::new();
new_files.insert(
    example_data.filename.to_string(),
    example_data.source_code.to_string(),
);
files.set(Rc::new(new_files));
```

For multi-file examples, create a separate `multi_file_example_button` function (or extend the existing one) that:

1. Iterates `multi_file_data.files` and inserts ALL files into the `BTreeMap`
2. Sets `current_file` to `multi_file_data.main_file` (e.g. `"RUN.bn"`)
3. Sets `source_code` to the main file's content
4. Triggers the run command

**The runtime already supports this.** The `example_runner` function (line 2034) creates `VirtualFilesystem::with_files(...)` from ALL entries in `self.files`, executes BUILD.bn first if present, then runs the main file. No runtime changes needed.

### 0.4 Add to examples list UI

Add `todo_mvc_physical` to the examples panel. Options:

**Option A (preferred):** Add multi-file examples after the current `MAIN_EXAMPLES_COUNT` (currently 11, line 76) examples. Update the count or add a separate section header.

**Option B:** Create a separate "Projects" section in the examples panel, visually distinct from single-file examples.

Either way, the multi-file example button needs a slightly different visual indicator (e.g., a folder icon or "multi-file" label) so users know it contains multiple files.

---

## Phase 1: Multi-Tab Editor UI

**Goal:** Visual tabs for switching between files in multi-file projects.

### 1.1 Tab bar component

Add a tab bar above the CodeMirror editor, visible when `self.files` has >1 entry.

**Reactive signal:** Derive from `self.files.signal_ref(|f| f.len() > 1)` to show/hide the tab bar.

Each tab:
- Shows the filename (short form — see §1.3)
- Highlighted when matching `self.current_file`
- Clicking a tab: saves current editor content to `self.files`, switches `current_file`, loads new content into `source_code`

**Content sync already works:** The `_sync_source_to_files_task` (line 293) syncs `source_code` changes back to `self.files`, and `content_signal` pushes new content to the CodeMirror editor. The tab switch only needs to update `current_file` and `source_code`.

### 1.2 Tab switch logic

```rust
fn switch_to_file(&self, filename: &str) {
    // 1. Current source_code is already synced to files via _sync_source_to_files_task
    // 2. Update current_file
    self.current_file.set(filename.to_string());
    // 3. Load new file content into editor
    let files = self.files.lock_ref();
    if let Some(content) = files.get(filename) {
        self.source_code.set(Rc::new(Cow::Owned(content.clone())));
    }
}
```

### 1.3 Directory display in tabs

For files with directory paths like `Theme/Professional.bn`:
- Tab label: short name (`Professional.bn`)
- Tooltip: full path (`Theme/Professional.bn`)
- For `RUN.bn` and `BUILD.bn`: show as-is (no directory prefix)
- Group tabs by directory? Optional — could show `Theme/` prefix in a lighter color

### 1.4 File creation/deletion (minimal, optional)

- "+" button to add a new `.bn` file (prompt for filename)
- "x" button on non-main file tabs to delete
- Protect critical files: `RUN.bn`, `BUILD.bn` cannot be deleted

**This is optional for Phase 1.** The primary goal is viewing/editing existing multi-file examples.

---

## Phase 2: CSS-Based Physical Rendering

**Goal:** Implement `Scene/new()` rendering using CSS approximations. No WebGL.

### 2.1 Current Scene/new API

**File:** `crates/boon/src/platform/browser/api.rs` (lines 2478-2514)

`function_scene_new` currently takes 1 argument (a root element object) and returns a minimal object. It needs to:

1. Extract `lights` and `root_element` from the argument
2. Compute a `SceneContext` from the lights configuration
3. Pass `SceneContext` through element rendering

### 2.2 SceneContext — light-derived CSS parameters

The `todo_mvc_physical/RUN.bn` `main_scene()` function (line 254) uses:

```boon
Scene/new(
    lights: Lights/basic()
    root_element: root_element(PASS: [...])
)
```

`Lights/basic()` returns a standard lighting setup. Define `SceneContext` as a shared struct threaded through rendering:

```rust
struct SceneContext {
    /// Shadow X offset derived from Light/directional azimuth
    shadow_offset_x: f64,
    /// Shadow Y offset derived from Light/directional altitude
    shadow_offset_y: f64,
    /// Shadow blur derived from light spread
    shadow_softness: f64,
    /// Shadow color (dark, derived from ambient)
    shadow_color: String,
    /// Ambient light factor (0-1)
    ambient_factor: f64,
}
```

Default for `Lights/basic()`:

```rust
impl Default for SceneContext {
    fn default() -> Self {
        Self {
            shadow_offset_x: 4.0,
            shadow_offset_y: 6.0,
            shadow_softness: 12.0,
            shadow_color: "rgba(0, 0, 0, 0.25)".to_string(),
            ambient_factor: 0.3,
        }
    }
}
```

### 2.3 Physical style property → CSS mapping

The theme files define physical properties on elements. Map them to CSS:

| Boon Property | CSS Approximation | Notes |
|---|---|---|
| `depth: N` | Layered `box-shadow` simulating thickness | N * shadow_offset from SceneContext |
| `move_closer: N` | `transform: translateZ(Npx)` + `perspective` on parent | Also elevates shadow intensity |
| `move_further: N` | `transform: translateZ(-Npx)` + inset shadow hint | Depresses element |
| `material.gloss: 0-1` | `linear-gradient` overlay (white→transparent) | Higher = stronger specular highlight |
| `material.metal: 0-1` | Tint specular gradient with element's own color | Metallic reflection |
| `material.glow` | Colored `box-shadow` with large blur, no offset | Ambient glow effect |
| `material.shine` | Additional gradient overlay layer | Clearcoat reflection |
| `rounded_corners` | `border-radius` | Already supported in bridge |
| `borders` | `border` | Already supported in bridge |
| `spring_range` | CSS `transition` timing | Maps to CSS animation duration/easing |

**Text-specific properties:**
| Boon Property | CSS Approximation |
|---|---|
| `depth: N` on text | `text-shadow` with N * offset |
| `relief: Raised` | Light `text-shadow` above, dark below |
| `relief: Carved` | Dark `text-shadow` above, light below |

### 2.4 Bridge implementation strategy

**Start with Actors engine only:** `crates/boon/src/platform/browser/engine_actors/bridge.rs`

The bridge's `object_with_document_to_element_signal()` function (line 93) is the entry point for converting Boon objects to Zoon elements. Modify it to:

1. Detect whether the root object has a `scene` key (physical) vs `document` key (flat)
2. If `scene`, extract lights → build `SceneContext`
3. Thread `Option<Rc<SceneContext>>` through all element rendering functions

Create a shared `apply_physical_styles()` function:

```rust
fn apply_physical_styles(
    el: impl Element,
    style_settings: &Value,  // The Boon style object
    scene_ctx: &SceneContext,
) -> impl Element {
    // Extract depth, material, move_closer, etc. from style_settings
    // Apply CSS properties via Zoon's .style() methods
    el
}
```

This function would be called in each element renderer:
- `element_container` / `element_stripe`
- `element_button`
- `element_text_input`
- `element_checkbox`
- `element_label`
- `element_text` (for text-shadow)

### 2.5 Scene detection in bridge

The bridge currently looks for `document` key to find the root element. For physical rendering, `RUN.bn` uses:

```boon
scene: main_scene(PASS: [...])
```

where `main_scene` returns `Scene/new(lights: ..., root_element: ...)`.

Modify `object_with_document_to_element_signal()` to check:
1. If object has `document` → existing flat rendering path
2. If object has `scene` → extract `root_element` and `lights`, build SceneContext, render with physical styles

### 2.6 Lights API functions

Add to `api.rs`:
- `Lights/basic()` → returns a standard lighting configuration object
- `Light/directional(azimuth, altitude, intensity, spread)` → directional light spec
- `Light/ambient(intensity, color)` → ambient light spec

These are pure data constructors — they return Boon objects. The SceneContext computation happens in the bridge when it reads these objects.

---

## Phase 3: VFS/Modules for DD and WASM Engines

**Priority: Lower.** The Actors engine already has full VFS + module loading. Physical TodoMVC can run on Actors engine first.

### 3.1 DD Engine VFS

**File:** `crates/boon/src/platform/browser/engine_dd/mod.rs`

Currently `run_dd_reactive_with_persistence` takes a single source code string. Modify to accept the full `files: HashMap<String, String>` map. The DD compiler (`compile.rs`) needs module resolution against this file map.

Key changes:
- `compile.rs` line 106 area: add file map parameter to `compile()`
- Module import resolution: when encountering an import, look up the file map
- BUILD.bn execution: port the BUILD.bn preprocessing from the Actors engine

### 3.2 WASM Engine VFS

**File:** `crates/boon/src/platform/browser/engine_wasm/mod.rs`

Similar change to `compile_and_run`. Module resolution happens at IR lowering stage — when a module import is encountered during compilation, resolve it from the file map and compile it as a separate module.

### 3.3 Shared VFS abstraction (optional)

Currently each engine has its own file handling. A shared `VirtualFilesystem` abstraction could be extracted to reduce duplication. However, each engine's compilation pipeline is different enough that this may not be worth the effort.

---

## Phase 4: Language Features (Deferred)

The `RUN.bn` file does NOT use UNPLUGGED, `?.`, or partial pattern matching. These features (from `LANGUAGE_FEATURES_RESEARCH.md`) are future improvements, not blockers.

**Features `RUN.bn` actually uses (verify these work):**
- Spread operator (`...`) in objects — used extensively in theme application
- `PASS:` / `PASSED` context passing — theme and store are passed through function calls
- `HOLD state {}` — for todos, selected filter, editing state
- `LATEST {}` — combining multiple reactive inputs
- `WHEN {} / WHILE {}` — conditional rendering (edit mode, hover states)
- `THEN {}` — copy on event
- `TEXT { ... {interpolation} ... }` — active items count
- `LIST` operations — `append`, `retain`, `retain_by_key`
- Router API — `Router/url()`, route matching
- All Element APIs — `Element/stripe()`, `Element/button()`, `Element/text_input()`, etc.
- `Ulid/generate()` — todo IDs
- `Bool/not()` — toggle completed
- `Text/trim()`, `Text/is_not_empty()` — input validation

---

## Dependency Graph

```
Phase 0 (multi-file infra) ──→ Phase 1 (tab UI) ──→ Phase 2 (physical rendering)
                                                       ↗
Phase 3 (DD/WASM VFS) ─── independent, parallel ─────┘
Phase 4 (language features) ─── deferred
```

**Shortest path to visible result:** Phase 0 alone — load `todo_mvc_physical`, run on Actors engine, physical styles are ignored (flat rendering with standard CSS), but the multi-file infrastructure and BUILD.bn preprocessing are proven to work.

---

## Critical Files

| File | Changes | Phase |
|------|---------|-------|
| `playground/frontend/src/main.rs` | `MultiFileExampleData` struct, `make_multi_file_example_data!` macro, multi-file selection handler, tab bar UI | 0, 1 |
| `crates/boon/src/platform/browser/api.rs` | `Scene/new` structure, `Lights/basic()`, `Light/*` functions | 2 |
| `crates/boon/src/platform/browser/engine_actors/bridge.rs` | SceneContext, physical style extraction, Scene vs Document detection, `apply_physical_styles()` | 2 |
| `crates/boon/src/platform/browser/engine_dd/mod.rs` | VFS file map parameter | 3 |
| `crates/boon/src/platform/browser/engine_dd/core/compile.rs` | Module resolution from file map | 3 |
| `crates/boon/src/platform/browser/engine_wasm/mod.rs` | VFS file map parameter, module resolution | 3 |
| `playground/frontend/src/examples/todo_mvc_physical/*.bn` | Reference files — what the bridge must support | — |

---

## Verification

### Phase 0
1. Load `todo_mvc_physical` from examples dropdown
2. Verify `self.files` contains all 8 files
3. `current_file` is set to `RUN.bn`
4. Run on Actors engine — BUILD.bn executes, RUN.bn renders
5. `boon_console` — no runtime errors (physical styles silently ignored)

### Phase 1
1. Tab bar appears with 8 tabs
2. Click `Theme/Professional.bn` tab — editor shows that file's content
3. Edit content in a tab, switch away, switch back — edits preserved
4. Run after edits — changes reflected in preview

### Phase 2
1. Elements have `box-shadow` for depth
2. Hover states show material changes (gloss, glow)
3. Theme switching works (Professional → Neobrutalism → etc.)
4. `boon_screenshot_preview` — visual comparison across themes
5. No WebGL errors — pure CSS rendering

### Phase 3
1. Switch engine to DD — `todo_mvc_physical` still runs
2. Switch to WASM — same result
3. `boon_console` — no module resolution errors
