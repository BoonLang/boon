# BUILD.bn System: Design, Best Practices, and API

**Date**: 2025-11-12
**Status**: Design Document
**Scope**: BUILD.bn code generation system and browser compatibility

---

## Executive Summary

BUILD.bn is Boon's build-time code generation system. It allows projects to generate code before compilation, similar to Rust's build.rs or code-generation macros.

**Current capabilities:**
- ✅ Generate code from file system resources (e.g., inline SVG assets)
- ✅ Flat, declarative structure (no `main()` needed)
- ✅ Incremental rebuilds (rerun-if-changed)
- ✅ Works in Node.js environment

**Goals for this document:**
1. Define BUILD.bn API and best practices
2. Design BuildFS abstraction for browser/Wasm compatibility
3. Document simple routing pattern for single source of truth
4. Enable TodoMVC to work in Boon Playground (browser)

---

## Table of Contents

1. [BUILD.bn Fundamentals](#buildbn-fundamentals)
2. [API Reference](#api-reference)
3. [Browser Compatibility (BuildFS)](#browser-compatibility)
4. [Simple Routing Pattern](#simple-routing-pattern)
5. [Best Practices](#best-practices)
6. [Migration Guide](#migration-guide)

---

## BUILD.bn Fundamentals

### What is BUILD.bn?

BUILD.bn is a special Boon file that runs **before compilation** to generate code. It's similar to:
- Rust's `build.rs` (build scripts)
- Rust procedural macros (but simpler - string-based)
- JavaScript build tools (webpack, vite)

### When does BUILD.bn run?

```
┌─────────────┐
│  Source     │
│  Files      │
└──────┬──────┘
       │
       v
┌─────────────┐
│  BUILD.bn   │◄─── Runs FIRST
│  (codegen)  │
└──────┬──────┘
       │
       v
┌─────────────┐
│  Generated  │
│  Code       │
└──────┬──────┘
       │
       v
┌─────────────┐
│  Boon       │
│  Compiler   │
└──────┬──────┘
       │
       v
┌─────────────┐
│  Output     │
└─────────────┘
```

**Rebuild triggers:**
- BUILD.bn file changes
- Watched files change (via `Build/rerun_if_changed()`)
- Watched directories change (via `Build/rerun_if_dir_changed()`)

### Structure

BUILD.bn uses a flat, declarative structure - no `main()` function needed.

```boon
-- BUILD.bn

------------------------------------------------------------------------
-- CONFIGURATION (at top)
------------------------------------------------------------------------

input_dir: './assets/icons'
output_file: './Generated/Assets.bn'

------------------------------------------------------------------------
-- HELPER FUNCTIONS (if needed)
------------------------------------------------------------------------

FUNCTION generate_icon_entry(file) {
    icon_name: file.name |> Text/trim_suffix('.svg')
    svg_content: BuildFS/read_string(file.path)
    data_url: 'data:image/svg+xml;utf8,' ++ URL/encode(svg_content)
    Result: '{icon_name}: \'{data_url}\''
}

------------------------------------------------------------------------
-- BUILD LOGIC (flat, order-independent)
------------------------------------------------------------------------

-- Read inputs
svg_files: BuildFS/read_dir(input_dir)
    |> List/retain(item, if: item.name |> Text/ends_with('.svg'))

-- Generate code
generated_code: [
    '-- GENERATED CODE'
    ''
    'icon: ['
    svg_files
        |> List/map(file => generate_icon_entry(file))
        |> Text/join('\n')
        |> Text/indent(spaces: 4)
    ']'
] |> Text/join('\n')

-- Write output
BuildFS/create_dir_all('./Generated')
BuildFS/write_string(output_file, generated_code)

-- Register rebuild triggers
Build/rerun_if_changed('BUILD.bn')
Build/rerun_if_dir_changed(input_dir)
```

**Key points:**
- Flat structure - everything at top level
- Order doesn't matter - Boon's reactive model handles dependencies
- Configuration at top for visibility
- Helper functions defined when needed
- No need to call `main()` or run anything explicitly

---

## API Reference

### Build Module

Controls build system behavior and provides build-time utilities.

#### `Build/info(message: String)`

Log informational message to build output.

```boon
Build/info('Generated 42 icons to Generated/Assets.bn')
```

#### `Build/warning(message: String)`

Log warning message (non-fatal).

```boon
Build/warning('Icon directory is empty')
```

#### `Build/error(message: String)`

Log error and fail the build.

```boon
Build/error('Required file not found: config.toml')
```

#### `Build/rerun_if_changed(path: String)`

Register file dependency - rebuild if file changes.

```boon
Build/rerun_if_changed('BUILD.bn')
Build/rerun_if_changed('routes.config.bn')
```

#### `Build/rerun_if_dir_changed(path: String)`

Register directory dependency - rebuild if any file in directory changes.

```boon
Build/rerun_if_dir_changed('./assets/icons')
Build/rerun_if_dir_changed('./theme')
```

#### `Build/env(name: String) -> String | UNPLUGGED`

Access build-time environment variables.

```boon
api_endpoint: Build/env('API_ENDPOINT')? |> WHEN {
    UNPLUGGED => 'http://localhost:3000'
    value => value
}
```

---

### File Module (Node.js Backend)

**⚠️ Warning**: These APIs only work in Node.js. For browser compatibility, use `BuildFS` module (see Browser Compatibility section).

#### `File/read_string(path: String) -> String`

Read file as UTF-8 string.

```boon
svg_content: File/read_string('./assets/icon.svg')
```

#### `File/write_string(path: String, content: String)`

Write string to file (UTF-8).

```boon
File/write_string('./Generated/Assets.bn', generated_code)
```

#### `File/read_dir(path: String) -> List[FileEntry]`

Read directory entries.

```boon
files: File/read_dir('./assets')
    |> List/retain(item, if: item.name |> Text/ends_with('.svg'))
```

**FileEntry**:
```boon
[
    name: String       -- Filename (e.g., "icon.svg")
    path: String       -- Full path (e.g., "./assets/icon.svg")
    is_file: Bool      -- True if file, False if directory
    is_dir: Bool       -- True if directory, False if file
]
```

#### `File/exists(path: String) -> Bool`

Check if file or directory exists.

```boon
File/exists('./config.toml') |> WHEN {
    True => read_config()
    False => default_config()
}
```

#### `File/create_dir_all(path: String)`

Create directory and all parent directories (like `mkdir -p`).

```boon
File/create_dir_all('./Generated/Routes')
```

---

### Time Module

Provides build-time timestamps.

#### `Time/now() -> Timestamp`

Get current timestamp.

```boon
timestamp: Time/now()
```

#### `Time/format_iso(timestamp: Timestamp) -> String`

Format timestamp as ISO 8601 string.

```boon
generated_at: Time/now() |> Time/format_iso()
-- Result: "2025-11-12T14:30:00Z"
```

---

### URL Module

URL encoding utilities.

#### `URL/encode(text: String) -> String`

URL-encode string (percent-encoding).

```boon
data_url: 'data:image/svg+xml;utf8,' ++ URL/encode(svg_content)
```

---

### Text Module (Extended for BUILD.bn)

Additional text utilities for code generation.

#### `Text/indent(text: String, spaces: Number) -> String`

Indent each line by N spaces.

```boon
body: function_body |> Text/indent(spaces: 4)

Result: [
    'FUNCTION foo() {'
    body
    '}'
] |> Text/join('\n')
```

#### `Text/trim_suffix(text: String, suffix: String) -> String`

Remove suffix if present.

```boon
'checkbox_completed.svg' |> Text/trim_suffix('.svg')
-- Result: "checkbox_completed"
```

#### `Text/trim_prefix(text: String, prefix: String) -> String`

Remove prefix if present.

```boon
'./assets/icon.svg' |> Text/trim_prefix('./')
-- Result: "assets/icon.svg"
```

---

## Browser Compatibility

### The Problem

Current BUILD.bn uses `File/` APIs that don't work in browser:
- `File/read_dir()` - No filesystem in browser
- `File/write_string()` - No filesystem writes in browser
- `File/exists()` - No filesystem in browser

### Solution: BuildFS Abstraction Layer

Introduce `BuildFS` module that works in both Node.js and browser.

#### Architecture

```
┌─────────────────────────────────────┐
│           BUILD.bn Code             │
│  (uses BuildFS/* instead of File/*) │
└──────────────┬──────────────────────┘
               │
               v
        ┌──────────────┐
        │   BuildFS    │  (abstraction)
        └──────┬───────┘
               │
       ┌───────┴────────┐
       v                v
┌─────────────┐  ┌─────────────┐
│   Node.js   │  │   Browser   │
│  (File API) │  │  (Virtual)  │
└─────────────┘  └─────────────┘
```

#### Node.js Implementation

```boon
-- Delegates to real filesystem
BuildFS/read_string(path) => File/read_string(path)
BuildFS/write_string(path, content) => File/write_string(path, content)
```

#### Browser Implementation

```boon
-- Uses in-memory virtual filesystem
BuildFS/read_string(path) => VirtualFS/read(path)
BuildFS/write_string(path, content) => VirtualFS/write(path, content)
```

The browser implementation:
1. Maintains virtual filesystem in memory (Map or object)
2. Pre-populated with project files
3. Generated files added to virtual FS
4. Compiler reads from virtual FS

---

### BuildFS API

#### `BuildFS/read_string(path: String) -> String`

Read file as string.

**Node.js**: Reads from real filesystem
**Browser**: Reads from virtual filesystem (loaded project files)

```boon
content: BuildFS/read_string('./assets/icon.svg')
```

#### `BuildFS/write_string(path: String, content: String)`

Write file as string.

**Node.js**: Writes to real filesystem
**Browser**: Writes to virtual filesystem (available to compiler)

```boon
BuildFS/write_string('./Generated/Router.bn', router_code)
```

#### `BuildFS/read_dir(path: String) -> List[FileEntry]`

List directory contents.

**Node.js**: Reads from real filesystem
**Browser**: Reads from virtual filesystem directory structure

```boon
icons: BuildFS/read_dir('./assets/icons')
    |> List/retain(item, if: item.name |> Text/ends_with('.svg'))
```

#### `BuildFS/exists(path: String) -> Bool`

Check if file exists.

```boon
BuildFS/exists('./config.bn') |> WHEN {
    True => load_config()
    False => default_config()
}
```

#### `BuildFS/create_dir_all(path: String)`

Create directory (and parents).

**Node.js**: Creates real directories
**Browser**: Creates virtual directory entries

```boon
BuildFS/create_dir_all('./Generated/Routes')
```

---

### Migration Path

**Phase 1**: Introduce BuildFS alongside File
```boon
-- Both work in Node.js
File/read_string(path)     -- Old (Node.js only)
BuildFS/read_string(path)  -- New (Node.js + Browser)
```

**Phase 2**: Migrate existing BUILD.bn files
```diff
- File/read_dir('./assets/icons')
+ BuildFS/read_dir('./assets/icons')

- File/write_string(output_file, code)
+ BuildFS/write_string(output_file, code)
```

**Phase 3**: Deprecate File/* in BUILD.bn context

---

## Simple Routing Pattern

### The Problem

Route strings often appear in multiple places:
1. Route parsing (URL → state)
2. Route generation (state → URL)
3. Labels/display text

This creates duplication and maintenance burden.

### The Solution: Simple Data Structure

Use a flat record at the top of your file as the single source of truth:

**RUN.bn**:
```boon
------------------------------------------------------------------------
-- FILTER ROUTES - Single source of truth
------------------------------------------------------------------------

filter_routes: [
    all: '/'
    active: '/active'
    completed: '/completed'
]

------------------------------------------------------------------------
-- STORE AND STATE
------------------------------------------------------------------------

store: [
    elements: [
        filter_buttons: [all: LINK, active: LINK, completed: LINK]
        remove_completed_button: LINK
        toggle_all_checkbox: LINK
        new_todo_title_text_input: LINK
    ]

    -- Route parsing: paths only appear in filter_routes
    selected_filter: Router/route() |> WHEN {
        filter_routes.active => Active
        filter_routes.completed => Completed
        __ => All
    }

    -- Route generation: paths only appear in filter_routes
    go_to_result:
        LATEST {
            filter_buttons.all.event.press |> THEN { filter_routes.all }
            filter_buttons.active.event.press |> THEN { filter_routes.active }
            filter_buttons.completed.event.press |> THEN { filter_routes.completed }
        }
        |> Router/go_to()

    todos: LIST {} ...
]

------------------------------------------------------------------------
-- FILTER BUTTON
------------------------------------------------------------------------

FUNCTION filter_button(filter) {
    BLOCK {
        selected: PASSED.store.selected_filter = filter

        Element/button(
            element: [event: [press: LINK], hovered: LINK]
            style: [...]
            label: Element/text(
                style: Theme/text(of: ButtonFilter)
                -- Labels can stay as simple WHEN - they're just UI strings
                text: filter |> WHEN {
                    All => 'All'
                    Active => 'Active'
                    Completed => 'Completed'
                }
            )
        )
    }
}
```

### What This Achieves

**Before** (duplication):
```boon
-- Parsing
selected_filter: Router/route() |> WHEN {
    '/active' => Active          -- String appears here
    '/completed' => Completed    -- String appears here
    __ => All
}

-- Generation
go_to_result: LATEST {
    filter_buttons.all.event.press |> THEN { '/' }              -- String appears here
    filter_buttons.active.event.press |> THEN { '/active' }     -- String appears here
    filter_buttons.completed.event.press |> THEN { '/completed' } -- String appears here
}
```

**After** (single source):
```boon
-- Routes defined once
filter_routes: [
    all: '/'
    active: '/active'
    completed: '/completed'
]

-- Parsing uses filter_routes
selected_filter: Router/route() |> WHEN {
    filter_routes.active => Active
    filter_routes.completed => Completed
    __ => All
}

-- Generation uses filter_routes
go_to_result: LATEST {
    filter_buttons.all.event.press |> THEN { filter_routes.all }
    filter_buttons.active.event.press |> THEN { filter_routes.active }
    filter_buttons.completed.event.press |> THEN { filter_routes.completed }
}
```

### Benefits

✅ **Single source of truth**: All route paths in one place
✅ **Maximum simplicity**: Just a flat record, no code generation
✅ **Easy to change**: Modify route = change one place
✅ **Easy to add**: New route = add one field to record
✅ **Clear**: All routes visible at top of file
✅ **Type-safe**: Pattern matching still works normally

### When to Use BUILD.bn for Routing

For simple routing (3-10 routes), use the data structure pattern above. BUILD.bn is not needed.

BUILD.bn becomes useful when:
- You have 20+ routes with complex patterns
- Routes need to be shared across multiple files
- You need to generate additional code (tests, docs, validation)
- Routes come from external data sources

For TodoMVC and similar apps, the simple data structure pattern is perfect.

---

## Best Practices

### 1. Always Use BuildFS for File Operations

✅ **Do**:
```boon
content: BuildFS/read_string('./config.bn')
BuildFS/write_string('./Generated/Output.bn', code)
```

❌ **Don't**:
```boon
content: File/read_string('./config.bn')  -- Won't work in browser
File/write_string('./Generated/Output.bn', code)
```

**Why**: BuildFS works in both Node.js and browser (Playground).

---

### 2. Generate Code to `Generated/` Directory

✅ **Do**:
```boon
BuildFS/create_dir_all('./Generated')
BuildFS/write_string('./Generated/Assets.bn', code)
```

❌ **Don't**:
```boon
BuildFS/write_string('./src/assets.bn', code)  -- Mixes generated with source
```

**Why**:
- Clear separation of generated vs handwritten code
- Generated/ can be gitignored
- Easy to clean (delete directory)

---

### 3. Add Header Comments to Generated Files

✅ **Do**:
```boon
code: [
    '-- GENERATED CODE - DO NOT EDIT'
    '-- Generated by BUILD.bn'
    '-- Generated at: {Time/now() |> Time/format_iso()}'
    ''
    actual_code
] |> Text/join('\n')
```

**Why**:
- Warns developers not to edit
- Documents generation source
- Helps with debugging

---

### 4. Register Rebuild Triggers

✅ **Do**:
```boon
FUNCTION main() {
    -- ... generation logic ...

    Build/rerun_if_changed('BUILD.bn')
    Build/rerun_if_dir_changed('./assets')
}
```

**Why**: Enables incremental builds - only regenerate when inputs change.

---

### 5. Keep Configuration at Top

✅ **Do**:
```boon
-- BUILD.bn

------------------------------------------------------------------------
-- CONFIGURATION
------------------------------------------------------------------------

icons_dir: './assets/icons'
output_dir: './Generated'
output_file: output_dir ++ '/Assets.bn'

------------------------------------------------------------------------
-- BUILD LOGIC
------------------------------------------------------------------------

-- Ensure Generated directory exists
BuildFS/create_dir_all(output_dir)

-- Read all SVG files
svg_files: BuildFS/read_dir(icons_dir)
    |> List/retain(item, if: item.name |> Text/ends_with('.svg'))

-- Generate code
generated_code: generate_assets_module(svg_files)

-- Write output
BuildFS/write_string(output_file, generated_code)
```

**Why**:
- Configuration is visible and easy to modify
- No need for `main()` function - flat structure
- Dependencies handled by Boon's reactive model

---

### 6. Validate Inputs Early

✅ **Do**:
```boon
-- Check for empty directories
svg_files |> List/count() = 0 |> WHEN {
    True => Build/warning('No SVG files found in {icons_dir}')
    False => SKIP
}

-- Check for duplicates
duplicates: svg_files
    |> List/group_by(file, key: file.name)
    |> List/retain(group, if: group |> List/count() > 1)

duplicates |> List/count() > 0 |> WHEN {
    True => Build/error('Duplicate filenames: {duplicates}')
    False => SKIP
}
```

**Why**: Fail fast with clear error messages.

---

## Migration Guide

### Migrating TodoMVC to Simple Routing Pattern

Apply the single source of truth pattern to eliminate route string duplication.

**Step 1**: Add route definitions at top of RUN.bn

```boon
------------------------------------------------------------------------
-- FILTER ROUTES - Single source of truth
------------------------------------------------------------------------

filter_routes: [
    all: '/'
    active: '/active'
    completed: '/completed'
]
```

**Step 2**: Update route parsing to use filter_routes

```diff
  store: [
      elements: [ ... ]
-     selected_filter: Router/route() |> WHEN {
-         '/active' => Active
-         '/completed' => Completed
-         __ => All
-     }
+     selected_filter: Router/route() |> WHEN {
+         filter_routes.active => Active
+         filter_routes.completed => Completed
+         __ => All
+     }
```

**Step 3**: Update route generation to use filter_routes

```diff
      go_to_result:
          LATEST {
-             filter_buttons.all.event.press |> THEN { '/' }
-             filter_buttons.active.event.press |> THEN { '/active' }
-             filter_buttons.completed.event.press |> THEN { '/completed' }
+             filter_buttons.all.event.press |> THEN { filter_routes.all }
+             filter_buttons.active.event.press |> THEN { filter_routes.active }
+             filter_buttons.completed.event.press |> THEN { filter_routes.completed }
          }
          |> Router/go_to()
  ]
```

**Done!** No build step needed, route strings now appear in exactly one place.

---

### Benefits

| Aspect | Before | After |
|--------|--------|-------|
| **Route strings** | 2 places | 1 place (filter_routes) |
| **Lines of code** | 12 lines | 9 lines |
| **Adding filter** | 2 changes | 1 change (add to filter_routes) |
| **Complexity** | Simple | Simple |
| **Build step** | None | None |

---

## Conclusion

BUILD.bn provides powerful code generation capabilities for Boon projects.

**Key Benefits:**
1. **Browser Compatible**: BuildFS abstraction works in Node.js and Wasm
2. **Incremental Builds**: Only rebuilds when inputs change
3. **Pure Boon**: No external tools or languages needed
4. **Flat Structure**: No need for `main()` - just define data and transformations

**For Routing:**
- For simple apps (3-10 routes): Use data structure pattern (no BUILD.bn needed)
- For complex apps (20+ routes): Use BUILD.bn for code generation
- Single source of truth principle applies to both approaches

**Next Steps:**
1. Implement BuildFS abstraction layer for browser compatibility
2. Apply simple routing pattern to TodoMVC
3. Test in browser Playground

---

**Related Documents:**
- `.code/BOON_SYNTAX.md` - Core Boon syntax
- `.code/LINK_PATTERN.md` - Reactive architecture patterns
- `playground/frontend/src/examples/todo_mvc_physical/BUILD.bn` - Example build script

**Last Updated:** 2025-11-12
