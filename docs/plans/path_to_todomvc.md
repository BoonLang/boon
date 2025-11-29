# Plan: Path to TodoMVC in Boon Playground

## Goal
Make both TodoMVC examples fully working in the Boon playground:
1. **todo_mvc** (classic) - 570 lines, standard TodoMVC with MoonZoon/Zoon rendering
2. **todo_mvc_physical** - 784 lines + themes, 3D physical UI (MoonZoon first, then raybox)

## Strategy: MoonZoon-First
MoonZoon/Zoon is the **primary renderer** during development, not a fallback:
- Get all Boon language features working with immediate visual feedback
- Validate todo_mvc (classic) works completely with Zoon
- Then tackle todo_mvc_physical with Zoon (DOM rendering)
- Raybox becomes a renderer swap later, not a blocker

## Milestones
1. **todo_mvc works** - Classic TodoMVC renders and functions via MoonZoon
2. **todo_mvc_physical works (DOM)** - Physical TodoMVC renders via MoonZoon (flat, no 3D)
3. **todo_mvc_physical works (3D)** - Raybox integration (future, separate repo)

---

## Current Status (Updated 2025-11-29)

### Implementation Summary

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 0 | Parser Completion | ✅ COMPLETE |
| Phase 1 | Evaluator Expression Support | ✅ COMPLETE |
| Phase 2 | MoonZoon Bridge & API Functions | ✅ COMPLETE |
| Phase 3 | Multi-File Support | ✅ COMPLETE |
| Phase 4 | BUILD.bn & Virtual Filesystem | ✅ COMPLETE |
| Phase 5 | Actor Runtime & Persistence | ✅ COMPLETE |
| Phase 6 | CodeMirror Syntax Highlighting | ✅ COMPLETE |

---

## Phase 0: Parser Completion ✅ COMPLETE

### 0.1 Operators in Pratt Parser ✅
All operators implemented: `==`, `!=`, `<`, `>`, `<=`, `>=`, `+`, `-`, `*`, `/`

### 0.2 WHEN Statement ✅
Pattern matching with literals, tags, destructuring, wildcard `__`, list patterns.

### 0.3 WHILE Statement ✅
Reactive patterns producing continuous streams.

### 0.4 BLOCK Statement ✅
Scoped variable binding with result expression.

### 0.5 TEXT Literal Syntax ✅
`TEXT { content }` with `{var}` interpolation. Old `'string'` syntax removed.

### 0.6 Spread Operator ✅
`...` token for object spreading: `[...base_object, override: value]`

### 0.7 FLUSH Statement ✅
`FLUSH { expr }` for fail-fast error handling.

### 0.8 Hardware Types ✅ (parse only)
BITS, MEMORY, BYTES tokens and parsing implemented. Runtime evaluation deferred.

---

## Phase 1: Evaluator Expression Support ✅ COMPLETE

All expression types evaluate correctly:
- ✅ Comparators (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- ✅ Arithmetic (`+`, `-`, `*`, `/`)
- ✅ WHEN evaluation with pattern matching
- ✅ WHILE evaluation with reactive streams
- ✅ BLOCK evaluation with local scope
- ✅ User-defined FUNCTION support
- ✅ PASSED aliases
- ✅ LinkSetter, Skip, Map, Spread, Flush

---

## Phase 2: MoonZoon Bridge & API Functions ✅ COMPLETE

### Built-in Functions Implemented

**Element functions:**
- ✅ `Element/stripe`, `Element/button`, `Element/text_input`
- ✅ `Element/checkbox`, `Element/label`, `Element/paragraph`, `Element/link`

**List functions:**
- ✅ `List/append`, `List/retain`, `List/map`, `List/latest`
- ✅ `List/every`, `List/any`, `List/count`, `List/empty`, `List/not_empty`, `List/sort_by`

**Bool functions:**
- ✅ `Bool/not`, `Bool/toggle`, `Bool/or`

**Text functions:**
- ✅ `Text/trim`, `Text/is_empty`, `Text/is_not_empty`, `Text/empty`

**Router functions:**
- ✅ `Router/route`, `Router/go_to`

**Other:**
- ✅ `Ulid/generate`, `Math/sum`, `Timer/interval`
- ✅ `Document/new`, `Scene/new`
- ✅ `Log/info`, `Log/error`
- ✅ `Build/succeed`, `Build/fail`
- ✅ `File/read_text`, `File/write_text`, `Directory/entries`
- ✅ `Theme/background_color`, `Theme/text_color`, `Theme/accent_color` (stubs)

### User-Defined Functions (via Module Resolution)
Theme functions (`Theme/material`, `Theme/font`, etc.) are user-defined in `Theme/*.bn` files, resolved via ModuleLoader - NOT built-in functions.

---

## Phase 3: Multi-File Support ✅ COMPLETE

### ModuleLoader ✅
- Parses and caches modules on demand
- Resolves `Module/function` calls to `.bn` files
- Supports nested paths: `Theme/Professional.bn`

### Playground UI ✅
- File tabs row implemented (`file_tabs_row()`)
- Files stored in `BTreeMap<String, String>`
- Current file tracking with `current_file` Mutable

### VirtualFilesystem ✅
- `VirtualFilesystem` struct in `engine.rs`
- Files loaded into VFS at startup
- Used for module resolution and file operations

---

## Phase 4: BUILD.bn & Virtual Filesystem ✅ COMPLETE

### Build Order ✅
1. Project files loaded into VFS
2. BUILD.bn executes first (if exists)
3. Generated files available in VFS
4. RUN.bn executes

### File API ✅
- `File/read_text`, `File/write_text` map to VFS
- `Directory/entries` lists VFS directory

---

## Phase 5: Actor Runtime & Persistence ✅ COMPLETE

### Persistence Resolver ✅
All expression types handled:
- Variable, Object, TaggedObject, FunctionCall
- Block, List, Map, Latest, Then
- When, While, Pipe
- ArithmeticOperator, Comparator
- Function, LinkSetter, Alias, Literal
- Link, Skip, TextLiteral, LatestWithState
- Flush, Pulses, Spread
- Bits, Memory, Bytes

---

## Phase 6: CodeMirror Syntax Highlighting ✅ COMPLETE

- ✅ TEXT {} highlighted as TextContent
- ✅ Interpolation `{var}` highlighted
- ✅ All keywords highlighted
- ✅ Old `'string'` syntax removed (Token::Text removed from lexer)

---

## Remaining Work

### Milestone 1: todo_mvc Works

**Status: Ready for Testing**

All language features and API functions needed by todo_mvc are implemented:
- [x] Parser: WHEN, WHILE, BLOCK, TEXT, operators
- [x] Evaluator: All expression types
- [x] API: List/*, Bool/*, Text/*, Router/*, Ulid/generate
- [x] Elements: text_input, checkbox, label, paragraph, link

**Next Step:** Load and test todo_mvc in playground to find runtime bugs.

### Milestone 2: todo_mvc_physical Works (DOM)

**Status: Ready for Testing**

All features needed are implemented:
- [x] Multi-file with ModuleLoader
- [x] File tabs in playground UI
- [x] Theme/* functions (user-defined in Theme/*.bn)
- [x] VirtualFS and BUILD.bn execution
- [x] Spread operator `...`
- [x] Persistence resolver complete

**Next Step:** Load and test todo_mvc_physical to find runtime bugs.

### Milestone 3: todo_mvc_physical Works (3D via Raybox)

*Future - handled in raybox repo*
- [ ] Raybox integrated as library crate
- [ ] Scene/new creates EmergentScene
- [ ] Theme functions produce real materials/geometry
- [ ] Full 3D rendering with emergent raymarching

---

## Optional/Low Priority

These are NOT blocking todo_mvc or todo_mvc_physical:

| Item | Status | Notes |
|------|--------|-------|
| `Text/join_lines` | Not implemented | Not used in todo_mvc_physical |
| `Color/contrast_ratio` | Not implemented | Future 3D feature |
| `Mouse/position` | Not implemented | Future 3D feature |
| `Url/encode` | Not implemented | May not be needed |
| `Light/directional`, etc. | Not implemented | Future 3D/raybox features |

---

## Critical Files Summary

| File | Status |
|------|--------|
| `crates/boon/src/parser/lexer.rs` | ✅ All tokens including TEXT, FLUSH, spread, hardware types |
| `crates/boon/src/parser.rs` | ✅ All expressions and statements |
| `crates/boon/src/parser/static_expression.rs` | ✅ All expression types |
| `crates/boon/src/parser/persistence_resolver.rs` | ✅ All expression types handled |
| `crates/boon/src/platform/browser/evaluator.rs` | ✅ All expressions evaluate, ModuleLoader integrated |
| `crates/boon/src/platform/browser/engine.rs` | ✅ VirtualFilesystem, List combinators |
| `crates/boon/src/platform/browser/api.rs` | ✅ All needed API functions |
| `playground/frontend/src/main.rs` | ✅ Multi-file UI with file tabs |
| `playground/frontend/typescript/.../boon-language.ts` | ✅ TEXT {} highlighting, old string removed |

---

## Next Actions

1. **Test todo_mvc** - Load in playground, verify all interactions work
2. **Test todo_mvc_physical** - Load with all Theme/*.bn files, verify rendering
3. **Fix runtime bugs** - Any issues discovered during testing
4. **Raybox integration** - After DOM rendering works (separate effort)

---

**Last Updated:** 2025-11-29
