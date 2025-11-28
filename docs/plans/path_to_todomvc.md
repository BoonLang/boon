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

## Current State: Implementation Gap Analysis

### A. Parser - Statements (marked `todo()` in parser.rs)

| Statement | Status | Usage in todo_mvc_physical |
|-----------|--------|---------------------------|
| `WHEN { pattern => expr }` | `todo()` | Pattern matching, 15+ uses |
| `WHILE { pattern => expr }` | `todo()` | Reactive patterns, 10+ uses |
| `BLOCK { vars }` | `todo()` | Scoped variables, 20+ uses |

### B. Parser - Operators (tokens exist, Pratt parsing @TODO)

| Operator | Status | Usage |
|----------|--------|-------|
| `==`, `!=` | Tokens exist, Pratt @TODO | Equality comparisons |
| `<`, `>`, `<=`, `>=` | Tokens exist, Pratt @TODO | Comparisons |
| `+`, `-`, `*`, `/` | Tokens exist, Pratt @TODO | Arithmetic |
| `...` (spread) | NO TOKEN | Object spreading, 5+ uses |

### C. Parser - Syntax

| Syntax | Status | Usage |
|--------|--------|-------|
| `TEXT { content }` | NOT supported | All text (22+ uses) |
| `TEXT { {var} }` | @TODO comment | Interpolation (6+ uses) |
| `FLUSH { expr }` | NO TOKEN | Error handling in BUILD.bn |

### D. Evaluator - Expression Types (return "Not supported yet")

| Expression | Status |
|------------|--------|
| `Expression::When` | Error |
| `Expression::While` | Error |
| `Expression::Block` | Error |
| `Expression::Comparator` | Error |
| `Expression::ArithmeticOperator` | Error |
| `Expression::Map` | Error |
| `Expression::Function` (definitions) | Error |
| `Expression::LinkSetter` | Error |
| `Expression::Skip` | Error |
| `PASSED` aliases | Error |

### E. Evaluator - API Functions

**Currently implemented: 6 functions**
- `Document/new`, `Element/container`, `Element/stripe`, `Element/button`, `Math/sum`, `Timer/interval`

**Required but NOT implemented: 50+ functions**

| Namespace | Missing Functions |
|-----------|-------------------|
| **Element** | `text`, `text_input`, `checkbox`, `label`, `link`, `paragraph`, `stack`, `block` |
| **List** | `map`, `retain`, `append`, `every`, `any`, `count`, `not_empty`, `sort_by`, `latest` |
| **Bool** | `not`, `toggle` |
| **Text** | `trim`, `is_empty`, `is_not_empty`, `empty`, `join_lines` |
| **Router** | `route`, `go_to` |
| **Scene** | `new` |
| **Theme** | `material`, `font`, `depth`, `elevation`, `corners`, `lights`, `geometry`, `sizing`, `spacing`, `spring_range`, `text` |
| **Ulid** | `generate` |
| **File** | `read_text`, `write_text` |
| **Directory** | `entries` |
| **Url** | `encode` |
| **Log** | `info`, `error` |
| **Build** | `succeed`, `fail` |
| **Light** | `directional`, `ambient`, `spot` |
| **Color** | `contrast_ratio` |
| **Mouse** | `position` |

### F. User-Defined Functions

```boon
FUNCTION new_todo(title) { ... }  -- Line 77-124 in RUN.bn
```
**Status:** Parser accepts but evaluator throws `todo!("Function definitions are not supported yet")`

### G. FFI / External Function Mechanism

Current approach in `evaluator.rs`:
- Hardcoded `match` on function path
- Each function is a Rust closure
- No dynamic registration, no macro system
- Every new function requires Rust code + recompilation

**Needed:** Extensible FFI system for:
- BUILD.bn file operations (VFS)
- Future raybox integration
- Plugin/module system

### H. Playground Limitations

| Limitation | Current State | Required |
|------------|--------------|----------|
| Single-file only | `ExampleData { source_code: &str }` | Multi-file map |
| No imports | Interpreter takes one string | Module resolution |
| No BUILD.bn | No code generation | Build-time execution |
| Example count | 4 hardcoded examples | Include todo_mvc_physical |

---

## Phase 0: Parser Completion

### 0.1 Operators in Pratt Parser

Tokens already exist in lexer.rs. Complete the Pratt parser binding power and handling in parser.rs:

```rust
// Add binding power and infix handling for:
==, !=           // Equality (precedence 5)
<, >, <=, >=     // Comparison (precedence 5)
+, -             // Additive (precedence 7)
*, /             // Multiplicative (precedence 9)
```

### 0.2 WHEN Statement (replace `todo()`)

```boon
value |> WHEN {
    Pattern1 => result1
    Tag[field: var] => result2
    __ => default
}
```

Patterns: literals, tags, destructuring, wildcard `__`, list patterns

### 0.3 WHILE Statement (replace `todo()`)

```boon
signal |> WHILE {
    Pattern1 => continuous_result1
    Pattern2 => continuous_result2
}
```

Like WHEN but produces continuous reactive stream.

### 0.4 BLOCK Statement (replace `todo()`)

```boon
BLOCK {
    local_var: computation
    another: uses(local_var)
    result_expression
}
```

### 0.5 TEXT Literal Syntax

```boon
TEXT { Hello world }
TEXT { Count: {count} items }  -- interpolation
```

Add TEXT keyword, parse `TEXT { ... }` with `{var}` interpolation.

### 0.6 Spread Operator

Add `...` token:
```boon
[...base_object, override: value]
```

### 0.7 FLUSH Statement

```boon
error => FLUSH { error }
```

### 0.8 Hardware Types (parse only, no runtime)

**BITS:**
```boon
BITS[8] { 0b11110000 }
value |> Bits/get(index: 3)
value |> Bits/slice(high: 7, low: 4)
```

**MEMORY:**
```boon
MEMORY[256] { 0 }
memory |> Memory/read(address: addr)
memory |> Memory/write(address: addr, data: value)
```

**BYTES:**
```boon
BYTES { 0x48, 0x65, 0x6C, 0x6C, 0x6F }
```

**Fixed-size LIST:**
```boon
LIST[4] { True, False }  -- Fixed size with defaults
LIST[width, Bool]        -- Type annotation
```

**Files:** `lexer.rs`, `parser.rs`

### 0.9 Parser Tests

Write comprehensive tests for lexer and parser:
- Token tests for all keywords and operators
- Expression parsing tests
- Statement parsing tests (WHEN, WHILE, BLOCK, FUNCTION)
- Hardware syntax tests (BITS, MEMORY, BYTES)
- Error recovery tests

**Files:** `lexer.rs` tests, `parser.rs` tests

---

## Phase 1: Evaluator Expression Support

Implement evaluation for expressions that currently return "Not supported yet":

### 1.1 Comparators & Arithmetic
```rust
Expression::Comparator { left, op, right } => // compare values
Expression::ArithmeticOperator { left, op, right } => // compute
```

### 1.2 WHEN Evaluation
Match input against patterns, return first match result.

### 1.3 WHILE Evaluation
Create reactive stream that re-evaluates when input changes.

### 1.4 BLOCK Evaluation
Create local scope, evaluate bindings sequentially, return result.

### 1.5 User-Defined Functions
```rust
Expression::Function { name, params, body } => {
    // Register in scope, call evaluates body with bound params
}
```

### 1.6 PASSED Aliases
Support `PASSED.store.field` access through function call chain.

### 1.7 LinkSetter, Skip, Map
Implement remaining expression types.

**Files:** `evaluator.rs`

---

## Phase 2: MoonZoon Bridge & API Functions

### 2.1 Architecture Overview

Current bridge path: `evaluator.rs` → `api.rs` → `bridge.rs` → Zoon elements

**Currently bridged (3 elements):**
- `Element/container` → `Zoon El`
- `Element/stripe` → `Zoon Stripe`
- `Element/button` → `Zoon Button`

### 2.2 Element Functions for todo_mvc (Priority 1)

Bridge these to Zoon for classic TodoMVC:

| Boon Function | Zoon Element | Key Features |
|---------------|--------------|--------------|
| `Element/text_input` | `TextInput` | text, placeholder, focus, events (change, key_down, blur) |
| `Element/checkbox` | `Checkbox` | checked, icon, label, click event |
| `Element/label` | `Label` | label text, double_click event, hovered state |
| `Element/paragraph` | `Paragraph` | contents (list of text/links) |
| `Element/link` | `Link` | to (URL), label, new_tab, hovered state |

### 2.3 Core API Functions for todo_mvc (Priority 1)

**List functions:**
- `List/append(item)` - Add item to list reactively
- `List/retain(item, if)` - Filter list by predicate
- `List/map(old, new)` - Transform list items
- `List/latest()` - Get latest value from list of streams
- `List/every(item, if)` - All items match predicate
- `List/any(item, if)` - Any item matches predicate
- `List/count` - Count items
- `List/empty()` - Check if list is empty

**Bool functions:**
- `Bool/not()` - Negate boolean
- `Bool/toggle(when)` - Toggle on event
- `Bool/or(that)` - Boolean OR

**Text functions:**
- `Text/trim()` - Remove whitespace
- `Text/is_not_empty()` - Check non-empty
- `Text/is_empty()` - Check empty
- `Text/empty` - Empty text constant

**Router functions:**
- `Router/route()` - Get current route
- `Router/go_to()` - Navigate to route

**Other:**
- `Ulid/generate()` - Generate unique ID

### 2.4 Additional Functions for todo_mvc_physical (Priority 2)

**Theme functions (stubs for DOM, real for raybox):**
- `Theme/material`, `Theme/font`, `Theme/depth`, `Theme/elevation`
- `Theme/corners`, `Theme/lights`, `Theme/geometry`, `Theme/sizing`
- `Theme/spacing`, `Theme/spring_range`, `Theme/text`

**Scene:**
- `Scene/new` - Create 3D scene (stub for DOM, raybox for 3D)

**Light (stubs):**
- `Light/directional`, `Light/ambient`, `Light/spot`

### 2.5 VFS Functions (for BUILD.bn)

```rust
// File operations map to VirtualFS
"File/read_text" => vfs.read(path)
"File/write_text" => vfs.write(path, content)
"Directory/entries" => vfs.list_dir(path)
```

**Files:** `evaluator.rs`, `api.rs`, `bridge.rs`

---

## Phase 3: Playground Projects & Multi-File Support

### 3.1 Project Concept

A **project** is a group of files that belong together:
- Entry point file (e.g., `RUN.bn`)
- Supporting modules (e.g., `Theme/*.bn`)
- Generated files (e.g., `Generated/Assets.bn`)
- Build script (e.g., `BUILD.bn`)

### 3.2 Update ExampleData → ProjectData Structure

**Current:**
```rust
struct ExampleData {
    filename: &'static str,
    source_code: &'static str,
}
```

**New:**
```rust
struct ExampleData {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],  // [(path, content), ...]
    entry_point: &'static str,  // Usually "RUN.bn"
}
```

**File:** `boon/playground/frontend/src/main.rs`

### 3.3 Update Example Loading Macro

**Current:**
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

**New - for single-file:**
```rust
macro_rules! make_example_single {
    ($name:literal) => {{
        ExampleData {
            name: $name,
            files: &[
                (concat!($name, ".bn"), include_str!(concat!("examples/", $name, "/", $name, ".bn")))
            ],
            entry_point: concat!($name, ".bn"),
        }
    }};
}
```

**New - for multi-file (todo_mvc_physical):**
```rust
macro_rules! make_example_multi {
    ($name:literal, [$( ($path:literal, $file:literal) ),* $(,)?], $entry:literal) => {{
        ExampleData {
            name: $name,
            files: &[
                $( ($path, include_str!(concat!("examples/", $name, "/", $file))) ),*
            ],
            entry_point: $entry,
        }
    }};
}
```

### 3.4 Add todo_mvc_physical to Examples

```rust
static EXAMPLE_DATAS: &[ExampleData] = &[
    make_example_single!("minimal"),
    make_example_single!("hello_world"),
    make_example_single!("interval"),
    make_example_single!("counter"),
    make_example_multi!("todo_mvc_physical", [
        ("RUN.bn", "RUN.bn"),
        ("Theme/Theme.bn", "Theme/Theme.bn"),
        ("Theme/Professional.bn", "Theme/Professional.bn"),
        ("Theme/Glassmorphism.bn", "Theme/Glassmorphism.bn"),
        ("Theme/Neumorphism.bn", "Theme/Neumorphism.bn"),
        ("Theme/Neobrutalism.bn", "Theme/Neobrutalism.bn"),
        ("Generated/Assets.bn", "Generated/Assets.bn"),
    ], "RUN.bn"),
];
```

### 3.5 Update Interpreter for Multi-File

**Current signature:**
```rust
pub fn run(
    filename: &str,
    source_code: &str,
    ...
) -> Option<(Arc<Object>, ConstructContext)>
```

**New signature:**
```rust
pub fn run(
    entry_point: &str,
    files: &[(&str, &str)],  // All files available
    ...
) -> Option<(Arc<Object>, ConstructContext)>
```

**Implementation approach: Module Resolution**
- Parse entry point first
- When encountering `Module/function` call (e.g., `Theme/material`):
  - Look up `Module` in file map (e.g., `Theme/Theme.bn` or `Theme.bn`)
  - Parse that file if not already parsed
  - Resolve the function in that module's namespace
- Cache parsed modules to avoid re-parsing
- Support nested paths: `Theme/Professional.bn` → `Theme/Professional/...`

**File:** `boon/crates/boon/src/platform/browser/interpreter.rs`

### 3.6 Update Playground UI for Multi-File

**Changes needed:**
1. File tabs or tree view for multi-file examples
2. Store currently edited file separately
3. Update all files on save
4. Run button uses entry_point

**File:** `boon/playground/frontend/src/main.rs` (example_runner, example_button)

---

## Phase 4: BUILD.bn & Virtual Filesystem

BUILD.bn generates code (e.g., `Generated/Assets.bn` from SVG files). In WASM, we need a virtual filesystem.

### 4.1 Virtual Filesystem in WASM

```rust
struct VirtualFS {
    files: HashMap<String, Vec<u8>>,  // path -> content
}

impl VirtualFS {
    fn read(&self, path: &str) -> Result<&[u8], Error>;
    fn write(&mut self, path: &str, content: Vec<u8>);
    fn list_dir(&self, path: &str) -> Vec<String>;
    fn exists(&self, path: &str) -> bool;
}
```

**Population:**
- Project files loaded into VFS at startup
- Asset files (SVGs, images) also loaded
- Generated files written to VFS during build

### 4.2 BUILD.bn Execution

**When BUILD.bn runs:**
1. VFS is populated with project files + assets
2. BUILD.bn script executes
3. `File/read_text`, `Directory/entries` read from VFS
4. `File/write_text` writes to VFS
5. Generated files become available for RUN.bn

**Boon File API (evaluator):**
```boon
-- These map to VFS operations
Directory/entries(path)  -- VFS.list_dir(path)
File/read_text(path)     -- VFS.read(path) as string
File/write_text(path, content)  -- VFS.write(path, content)
```

### 4.3 Build Order

1. Load project files into VFS
2. If BUILD.bn exists, execute it first
3. Generated files now in VFS
4. Execute RUN.bn (entry point)

---

## Phase 5: Actor Runtime & Bridge Refactoring

*Note: Actor runtime and Boon-MoonZoon bridge were ad-hoc implementations. Open to refactoring for better API/architecture. Boon syntax stays unchanged. Major overhauls require user confirmation.*

### 5.1 Actor Runtime - Current Issues & Opportunities

**Current architecture:** `engine.rs` - ValueActor, ConstructStorage, combinators

**Known issues:**
- Clone overhead in LATEST combinator (line 622) - clones state on every iteration
- LocalStorage limitations (5-10MB, blocking I/O)
- Tight coupling between evaluation and DOM construction

**Potential improvements:**
- **Copy-on-write state**: Use `Rc<RefCell<>>` or `im` crate for persistent data structures
- **Lazy evaluation**: Only evaluate branches that are actually subscribed
- **Better stream composition**: Study `futures-signals` patterns but implement lock-free alternatives
- **Separate concerns**: Split engine into pure evaluation vs. side-effect handling

**Important constraint:** Do NOT use `futures-signals` crate directly. The Rust Actor runtime must be **lock-free** to support browser multithreading later. The `Mutable` types in futures-signals use locks that panic in browser web worker contexts.

### 5.2 MoonZoon Bridge - Current Issues & Opportunities

**Current architecture:** `api.rs` creates TaggedObjects → `bridge.rs` converts to Zoon elements

**Known issues:**
- Hardcoded function dispatch in `evaluator.rs` (large match statement)
- Each new Element requires changes in 3 files (api.rs, bridge.rs, evaluator.rs)
- Event handling is ad-hoc per element type

**Potential improvements:**
- **Trait-based element system**: `trait BoonElement { fn to_zoon(&self) -> impl Element; }`
- **Declarative event mapping**: Define events once, apply to all elements
- **Macro-based registration**: `register_element!(TextInput, text_input, [change, key_down, blur])`
- **Unified style system**: One style processor for all elements

### 5.3 Refactoring Strategy

**Approach:** Incremental refactoring alongside feature implementation
1. Add new features using current architecture
2. Identify pain points during implementation
3. Propose refactoring when patterns emerge
4. Confirm major changes with user before proceeding

**Non-goals:**
- No changes to Boon syntax
- No breaking existing working examples

### 5.4 Persistence Resolver Completion

**Currently unimplemented (~40% missing):**
- Block expressions
- Map expressions
- When/While combinators
- Arithmetic operators
- Comparators
- Function definitions
- Link setters

**Implementation:**
- Add `set_persistence()` cases for each missing expression type
- Ensure structural matching preserves state across code changes
- Test hot-reload with each expression type

### 5.5 Actor Runtime Tests

```rust
#[test]
fn test_value_actor_subscription() { ... }

#[test]
fn test_latest_combinator_deduplication() { ... }

#[test]
fn test_then_combinator_impulse() { ... }

#[test]
fn test_persistence_across_reload() { ... }

#[test]
fn test_output_valve_gating() { ... }
```

**Files:**
- `boon/crates/boon/src/platform/browser/engine.rs`
- `boon/crates/boon/src/parser/persistence_resolver.rs`

---

## Phase 6: CodeMirror TEXT {} Syntax Highlighting

### 6.1 Update Boon Grammar for CodeMirror

The playground uses CodeMirror for editing. Need to add TEXT {} highlighting.

**Current location:** Likely in `playground/frontend/` TypeScript code

**Grammar additions:**
```javascript
// TEXT keyword
{ tag: "keyword", match: /\bTEXT\b/ },

// TEXT { ... } block with interpolation
{
  tag: "string",
  begin: /TEXT\s*\{/,
  end: /\}/,
  contains: [
    // Interpolation {var}
    { tag: "variable", match: /\{[a-z_][a-z0-9_]*\}/i }
  ]
}
```

### 6.2 Deprecation Warning for ''

- Parser accepts `'string'` but emits deprecation warning
- Editor could show strikethrough or warning highlight on `''`
- Eventually remove `''` support entirely

---

## Critical Files Summary

| File | Changes |
|------|---------|
| `boon/crates/boon/src/parser/lexer.rs` | TEXT, FLUSH, spread tokens; deprecate `''` |
| `boon/crates/boon/src/parser.rs` | TextLiteral with interpolation, Flush, Spread expressions |
| `boon/crates/boon/src/platform/browser/evaluator.rs` | Evaluate new expressions, VFS File/Directory APIs |
| `boon/crates/boon/src/platform/browser/interpreter.rs` | Module resolution, VFS integration |
| `boon/playground/frontend/src/main.rs` | ProjectData, project UI, file tabs |
| `boon/playground/frontend/` (TypeScript) | CodeMirror grammar for TEXT {} highlighting |
| `boon/crates/boon/src/vfs.rs` (new) | Virtual filesystem implementation |

---

## Testing Strategy

### Lexer Tests (`lexer.rs` tests)
- Token recognition for all keywords: LIST, MAP, FUNCTION, LINK, LATEST, THEN, WHEN, WHILE, SKIP, BLOCK, PASS, PASSED, TEXT, FLUSH, BITS, MEMORY, BYTES
- Operator tokens: `|>`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `+`, `-`, `*`, `/`, `...`
- Literal tokens: numbers, text, binary (`0b`), hex (`0x`)
- Comment handling (`--`)
- Error cases: invalid tokens, unterminated strings

### Parser Tests (`parser.rs` tests)
- Expression parsing: variables, function calls, pipes, objects, lists
- Operators: precedence, associativity
- WHEN patterns: literals, tags, destructuring, wildcard, list patterns
- WHILE patterns: same as WHEN
- BLOCK: scoped variable binding, result expression
- TEXT: simple, with interpolation, Unicode
- Hardware syntax: BITS[N], MEMORY[N], BYTES, LIST[N]
- FUNCTION definitions: params, body
- Error recovery: partial parses, helpful messages

### Evaluator Tests (runtime tests)
**Expression evaluation:**
- Comparators: `==`, `!=`, `<`, `>` with numbers, text, bools
- Arithmetic: `+`, `-`, `*`, `/` with numbers
- WHEN: pattern matching, default case
- WHILE: reactive re-evaluation
- BLOCK: local scope, shadowing

**API function tests:**
- List: map, retain, append, count, every, any
- Bool: not, toggle
- Text: trim, is_empty, join_lines
- Element: text, text_input, checkbox (verify DOM output)

### VFS Tests
- File read/write operations
- Directory listing
- Path resolution
- Isolated per-project

### Playground Integration Tests
**Multi-file:**
- Load project with multiple files
- Switch between files
- Edit file, verify saved
- Cross-file function resolution

**Syntax highlighting:**
- TEXT {} highlighted as string
- Interpolation {var} highlighted
- All keywords highlighted

**BUILD.bn execution:**
- VFS populated
- BUILD.bn runs before RUN.bn
- Generated files available

### todo_mvc_physical Validation (before Raybox)

**Parse test:** Verify all .bn files parse without errors:
```rust
#[test]
fn test_todo_mvc_physical_parses() {
    let files = ["RUN.bn", "Theme/Theme.bn", "Theme/Professional.bn", ...];
    for file in files {
        let result = parser::parse(load_file(file));
        assert!(result.is_ok(), "Failed to parse {}: {:?}", file, result);
    }
}
```

**Evaluate test (partial):** Run evaluation up to the point where raybox is needed:
- Module resolution works
- Functions resolve across files
- Theme functions return stub values
- Element tree is built (even if not rendered)

**MoonZoon DOM rendering test:** Before raybox, use existing MoonZoon FFI:
- Element functions → MoonZoon/Zoon elements via existing FFI bridge
- Verify todo_mvc_physical renders functional DOM (even if not 3D)
- Test interactions (click, input, checkbox toggle) work through Zoon

---

## Success Criteria

### Milestone 1: todo_mvc Works (Classic TodoMVC via MoonZoon)

**Parser (Phase 0):**
- [ ] `==`, `>` comparators work
- [ ] WHEN statement with pattern matching
- [ ] WHILE statement with reactive patterns
- [ ] BLOCK statement with scoped variables
- [ ] TEXT {} with interpolation
- [ ] Lexer and parser tests

**Evaluator (Phase 1):**
- [ ] Comparators evaluate correctly
- [ ] WHEN/WHILE/BLOCK evaluate correctly
- [ ] User-defined FUNCTION works
- [ ] PASSED aliases work

**MoonZoon Bridge (Phase 2):**
- [ ] Element/text_input → Zoon TextInput (with events)
- [ ] Element/checkbox → Zoon Checkbox
- [ ] Element/label → Zoon Label (with hovered, double_click)
- [ ] Element/paragraph → Zoon Paragraph
- [ ] Element/link → Zoon Link

**API Functions (Phase 2):**
- [ ] List: append, retain, map, latest, every, any, count, empty
- [ ] Bool: not, toggle, or
- [ ] Text: trim, is_empty, is_not_empty, empty
- [ ] Router: route, go_to
- [ ] Ulid: generate

**Validation:**
- [ ] todo_mvc.bn parses without errors
- [ ] todo_mvc.bn evaluates without errors
- [ ] todo_mvc.bn renders working TodoMVC in browser
- [ ] All interactions work (add, edit, delete, filter, toggle)

### Milestone 2: todo_mvc_physical Works (DOM via MoonZoon)

**Multi-File (Phase 3):**
- [ ] ProjectData with multiple files
- [ ] File tabs in playground UI
- [ ] Module resolution for Theme/*.bn imports

**BUILD.bn (Phase 4):**
- [ ] VirtualFS in WASM
- [ ] File/read_text, File/write_text, Directory/entries
- [ ] BUILD.bn generates Assets.bn

**Additional Parser (Phase 0):**
- [ ] Spread operator `...`
- [ ] FLUSH statement
- [ ] BITS, MEMORY, BYTES (parse only)

**Theme Functions (Phase 2 - stubs):**
- [ ] Theme/* functions return placeholder values
- [ ] Scene/new returns DOM element (not 3D)

**Actor Runtime (Phase 5):**
- [ ] LATEST combinator optimized
- [ ] Persistence resolver complete
- [ ] Actor runtime tests pass

**Syntax Highlighting (Phase 6):**
- [ ] CodeMirror highlights TEXT {}
- [ ] Interpolation {var} highlighted

**Validation:**
- [ ] All todo_mvc_physical/*.bn files parse
- [ ] todo_mvc_physical renders (flat, not 3D)
- [ ] Theme switching works
- [ ] State persists across reload

### Milestone 3: todo_mvc_physical Works (3D via Raybox)

*Future - handled in raybox repo*
- [ ] Raybox integrated as library crate
- [ ] Scene/new creates EmergentScene
- [ ] Theme functions produce real materials/geometry
- [ ] Full 3D rendering with emergent raymarching

---

## Future: Raybox Integration

Once Phases 0-1 are complete, raybox integration becomes possible:
- Raybox as library crate in boon playground
- `Scene/new` API bridges Boon → EmergentScene
- 3D rendering replaces DOM-based rendering

(Raybox development continues in `/home/martinkavik/repos/raybox/`)
