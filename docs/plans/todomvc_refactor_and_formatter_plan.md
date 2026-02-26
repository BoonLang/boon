# TodoMVC Refactor & Boon Code Formatter Plan

Two independent work items: (A) conservative refactoring of `todo_mvc.bn`, and (B) implementing Boon's first code formatter with playground integration.

---

## Table of Contents

- [Part A: TodoMVC Refactoring](#part-a-todomvc-refactoring)
- [Part B: Boon Code Formatter](#part-b-boon-code-formatter)
- [Critical Files](#critical-files)
- [Verification](#verification)

---

## Part A: TodoMVC Refactoring

**File:** `playground/frontend/src/examples/todo_mvc/todo_mvc.bn` (689 lines)

Conservative refactoring — remove dead code, simplify factory pattern. No behavioral changes.

### A.1 Remove dead code: `new_todo_completed`

Lines 76-78 define a function that is never called anywhere in the file:

```boon
FUNCTION new_todo_completed(title) {
    new_todo_with_completed(title: title, initial_completed: True)
}
```

**Action:** Delete these 3 lines. Search confirms zero call sites — the function exists as a convenience wrapper that was never used.

### A.2 Inline `new_todo_with_completed` into `new_todo`

Currently there's a two-level factory:

```boon
FUNCTION new_todo(title) {
    new_todo_with_completed(title: title, initial_completed: False)
}

FUNCTION new_todo_with_completed(title, initial_completed) {
    [
        ...
        completed: initial_completed |> HOLD state {
            ...
        }
        ...
    ]
}
```

After removing `new_todo_completed`, the only caller of `new_todo_with_completed` is `new_todo` which always passes `initial_completed: False`.

**Action:** Inline the body of `new_todo_with_completed` into `new_todo`, hardcoding `False` where `initial_completed` was used:

```boon
FUNCTION new_todo(title) {
    [
        ...
        completed: False |> HOLD state {
            ...
        }
        ...
    ]
}
```

Then delete `new_todo_with_completed` entirely.

**Expected line reduction:** ~5-7 lines (function signature + call overhead).

### A.3 Verify refactored behavior

1. Load refactored `todo_mvc.bn` in playground
2. Test all core functionality:
   - Add a new todo (type + Enter)
   - Complete a todo (click checkbox)
   - Filter: All / Active / Completed
   - Edit a todo (double-click, modify, Enter)
   - Cancel edit (Escape)
   - Delete a todo (hover → X button)
   - Toggle all (checkbox in header)
   - Clear completed
   - URL routing (`/`, `/active`, `/completed`)
3. `boon_screenshot_preview` before/after — should be pixel-identical
4. `boon_console` — no new errors

---

## Part B: Boon Code Formatter

### B.1 Comment Architecture Analysis

**Problem:** Comments are stripped from the token stream in **8 locations** before parsing. The AST contains zero comment information:

| Location | File | Line |
|----------|------|------|
| Interpreter entry 1 | `interpreter.rs` | ~73 |
| Interpreter entry 2 | `interpreter.rs` | ~218 |
| Hot-reload compilation | `interpreter.rs` | ~321 |
| Actor engine evaluator | `evaluator.rs` | ~5399 |
| DD engine compiler | `compile.rs` | ~106 |
| WASM engine compiler | `engine_wasm/mod.rs` | ~157 |
| Parser test | `parser.rs` | ~1238 |

All use: `tokens.retain(|t| !matches!(t.node, Token::Comment(_)));`

**The `Token::Comment(&str)` variant** (lexer.rs line 14) stores the comment text WITHOUT the `--` prefix. The `Token::Newline` (line 23) is a separate token.

**Solution:** The formatter must use a **dual-pass approach**: lex for tokens (with comments) AND parse for AST structure, then merge them using span positions.

### B.2 Formatter Architecture

**New file:** `crates/boon/src/parser/formatter.rs`

**Entry point:**
```rust
pub fn format(source_code: &str) -> Result<String, FormatError>
```

**Algorithm:**

#### Step 1: Lex → token stream with comments

```rust
let tokens: Vec<Spanned<Token>> = lexer().parse(source_code).into_result()?;
// DO NOT strip comments — keep everything
```

#### Step 2: Build CommentMap

Associate each comment with the nearest code using span positions:

```rust
struct CommentMap {
    /// Comments that appear before a code line (leading comments)
    leading: HashMap<usize, Vec<String>>,   // byte_offset → comments
    /// Comments that appear after code on the same line (trailing comments)
    trailing: HashMap<usize, String>,        // byte_offset → comment
}
```

**Association rules:**
- **Leading comment:** A `Comment` token followed by `Newline`, where the next non-comment/non-newline token starts a code line. Associate with that code line's span start.
- **Trailing comment:** A `Comment` token on the same line as code (no `Newline` between the last code token and the comment). Associate with the preceding code's span end.
- **Comment block:** Multiple consecutive leading comments are grouped together.
- **Standalone comment at file end:** Preserved as trailing content.

#### Step 3: Parse → AST (for structural understanding)

```rust
let mut parse_tokens = tokens.clone();
parse_tokens.retain(|t| !matches!(t.node, Token::Comment(_)));
// Also strip newlines as the parser expects
parse_tokens.retain(|t| !matches!(t.node, Token::Newline));
reset_expression_depth();
let ast: Vec<Spanned<Expression>> = parser().parse(&parse_tokens).into_result()?;
```

#### Step 4: Walk AST, emit formatted code with comments

The formatter walks the AST recursively, emitting properly indented code. At each node, it checks the `CommentMap` for associated comments at the node's span position and inserts them.

### B.3 Formatter State

```rust
struct Formatter<'source> {
    source: &'source str,
    output: String,
    indent_level: usize,
    comment_map: CommentMap,
    /// Track which comments have been emitted (to avoid duplicates)
    emitted_comments: HashSet<usize>,
}

impl<'source> Formatter<'source> {
    fn indent(&self) -> String {
        "    ".repeat(self.indent_level)
    }

    fn emit(&mut self, text: &str) {
        self.output.push_str(text);
    }

    fn emit_line(&mut self, text: &str) {
        self.output.push_str(&self.indent());
        self.output.push_str(text);
        self.output.push('\n');
    }

    fn emit_leading_comments(&mut self, span_start: usize) { ... }
    fn emit_trailing_comment(&mut self, span_end: usize) { ... }
}
```

### B.4 Formatting Rules (Canonical Style)

Derived from all existing `.bn` example files and `docs/language/BOON_SYNTAX.md`:

#### Indentation
- **4 spaces** per level, no tabs
- Increase indent inside: `[]` (objects), `{}` (blocks, LATEST, HOLD, WHEN, WHILE, THEN, BLOCK), `()` (multi-line function calls)

#### Top-Level Declarations
- Variables separated by **blank line**
- Functions separated by **blank line**
- No blank line between object fields at the same level

#### Objects `[...]`
- **Empty object:** `[]` (no spaces)
- **Single short field:** `[tag: H1]` inline if total <80 chars
- **Multiple fields or long:** one field per line, indented:
  ```boon
  [
      name: value
      other: value
  ]
  ```

#### Tagged Objects
- Tag on same line as opening `[`:
  ```boon
  Oklch[lightness: 0.5, chroma: 0.1, hue: 250]
  ```
- If long, break fields:
  ```boon
  Oklch[
      lightness: 0.5
      chroma: 0.1
      hue: 250
  ]
  ```

#### Function Definitions
- Name and parameters on same line:
  ```boon
  FUNCTION todo_element(todo) {
      ...
  }
  ```
- Multi-parameter: all on same line if fits, otherwise one per line:
  ```boon
  FUNCTION new_todo_with_completed(title, initial_completed) {
      ...
  }
  ```

#### Function Calls
- Path/name directly precedes `(`:
  ```boon
  Element/stripe(
      style: [...]
      children: [...]
  )
  ```
- Named arguments: one per line if multi-line, indented
- Single short argument: inline: `Text/trim()`

#### LATEST
- Opening `{` on same line:
  ```boon
  LATEST {
      input_a
      input_b |> THEN { transformed }
  }
  ```
- One input per line, indented

#### HOLD
- Opening brace on same line:
  ```boon
  0 |> HOLD state {
      LATEST {
          ...
      }
  }
  ```

#### WHEN / WHILE
- Arms on separate lines, `=>` inline:
  ```boon
  |> WHEN {
      True => active_style
      False => inactive_style
  }
  ```
- Short single-arm: can be inline if <80 chars:
  ```boon
  |> THEN { True }
  ```

#### Pipe `|>`
- Continuation indented 4 spaces from start of chain:
  ```boon
  initial_value
      |> HOLD state { ... }
      |> Document/new()
  ```
- Short pipe chains inline if <80 chars:
  ```boon
  event.press |> THEN { counter + 1 }
  ```

#### TEXT Literals
- Inline with content: `TEXT { Hello, {name}! }`
- Space after `{` and before `}` in TEXT delimiters
- Interpolations with `{var}` — no spaces inside interpolation braces

#### BLOCK
```boon
BLOCK {
    local_var: computation
    output_value
}
```

#### Spread
- `...object` — no space after `...`

#### Comments
- **Leading comments:** Own line, same indent as following code
- **Trailing comments:** 2 spaces after code, then `-- comment`
- **Section separators:** Preserved as-is (e.g., `-- ─────────────`)

#### Line Width
- Target: **80 characters** soft limit
- Prefer breaking at natural points (after `:`, before `|>`, between arguments)
- Never break inside identifiers, keywords, or string literals

### B.5 Expression Formatting — All 25 Variants

Each `Expression` variant needs a formatting method:

| Variant | Formatting Rule |
|---------|----------------|
| `Variable` | `name: ` + format(value) |
| `Literal(Number)` | Numeric literal as-is |
| `Literal(Tag)` | PascalCase identifier |
| `Literal(Text)` | Quoted text (reconstruct from source spans) |
| `List` | `[item, item]` or multi-line |
| `Object` | `[field: val, ...]` — see object rules above |
| `TaggedObject` | `Tag[field: val]` |
| `Map` | `MAP { key => val, ... }` |
| `Function` | `FUNCTION name(params) { body }` |
| `FunctionCall` | `Path/name(args)` |
| `Alias` | Already has `Display` impl (parser.rs:1011-1030) |
| `LinkSetter` | `alias: LINK` |
| `Link` | `LINK` keyword |
| `Latest` | `LATEST { inputs... }` |
| `Hold` | `HOLD param { body }` |
| `Then` | `THEN { body }` |
| `Flush` | `FLUSH body` |
| `Spread` | `...value` |
| `When` | `WHEN { arms... }` |
| `While` | `WHILE { arms... }` |
| `Pipe` | `from \|> to` |
| `Skip` | `SKIP` keyword |
| `Block` | `BLOCK { vars... output }` |
| `Comparator` | `left op right` (==, !=, >, >=, <, <=) |
| `ArithmeticOperator` | `left op right` (+, -, *, /) or `-value` (negate) |
| `TextLiteral` | `TEXT { parts... }` — use source spans for content |
| `Bits` | `BITS(size)` |
| `Memory` | `MEMORY(address)` |
| `Bytes` | `BYTES(data)` |
| `FieldAccess` | `path.field.subfield` |

### B.6 TEXT Literal Special Handling

`TextLiteral` parts contain `TextPart::Text(&str)` and `TextPart::Interpolation { var }`. However, the text content comes from `Token::TextContent` which the lexer trims. For faithful reproduction:

1. Use the `Spanned` wrapper's span to extract raw text from the source string
2. Re-emit as `TEXT { raw_content_with_{interpolations} }`
3. Only reformat surrounding whitespace, not internal text content

### B.7 Inline vs Multi-Line Heuristics

The formatter needs to decide when to break constructs across lines:

```rust
fn should_inline(&self, expr: &Expression, available_width: usize) -> bool {
    let estimated_width = self.estimate_width(expr);
    estimated_width <= available_width && !self.has_nested_blocks(expr)
}
```

**Always multi-line:**
- Objects with >1 field (unless all fields are very short)
- LATEST with >1 input
- WHEN/WHILE with >1 arm
- Functions with body

**Always inline:**
- `SKIP`, `LINK`, single tags, numbers
- Empty objects `[]`
- Single-field short objects `[tag: H1]`

**Conditional (based on width):**
- Single-arm WHEN/WHILE: `|> THEN { value }` if <80 chars
- Function calls with 1 short argument
- Pipe chains with simple operations

### B.8 Idempotency Requirement

**`format(format(code)) == format(code)`** must hold for all valid Boon code.

This is verified by:
1. Format the input
2. Format the output
3. Assert string equality

If the formatter produces different output on the second pass, there's a bug in the formatting rules (likely an inconsistency between the inline/multi-line heuristic and the actual emission).

### B.9 Module Registration

**File:** `crates/boon/src/parser.rs`

Add to the module declarations (near line 1-10):

```rust
pub mod formatter;
```

The formatter uses `lexer()` and `parser()` from the parent module, plus the `Token`, `Expression`, `Spanned` types.

### B.10 Playground Format Button

**File:** `playground/frontend/src/main.rs`

Add a "Format" button in the controls area (near the engine selector).

**On click:**
1. Read `self.source_code` current value
2. Call `boon::parser::formatter::format(&source_code)` — runs synchronously in WASM
3. If `Ok(formatted)`: `self.source_code.set_neq(Rc::new(Cow::Owned(formatted)))` — editor updates reactively via `content_signal`
4. If `Err(e)`: log error to console or show a brief toast notification

The wiring pattern already exists — example selection uses `source_code.set_neq(...)` to update the editor content.

**Keyboard shortcut (optional):** Ctrl+Shift+F / Cmd+Shift+F — would require CodeMirror integration in the TypeScript layer.

---

## Implementation Phases

### Phase 1: Core Formatter (`formatter.rs`)

1. Create `crates/boon/src/parser/formatter.rs`
2. Implement `CommentMap` construction from token stream
3. Implement `Formatter` struct with `format_expression()` for each variant
4. Implement inline vs multi-line heuristics
5. Handle TEXT literal reconstruction from source spans
6. Add `pub mod formatter;` to `parser.rs`

**Estimated scope:** ~800-1200 lines for the formatter module.

### Phase 2: Tests

1. **Unit tests:** Format known inputs → expected outputs for each Expression variant
2. **Round-trip tests:** Format all example `.bn` files, verify they parse without errors
3. **Idempotency tests:** `format(format(x)) == format(x)` for all examples
4. **Comment preservation tests:** Verify comments from todo_mvc_physical (has `--` separators) survive formatting
5. **Edge cases:** Empty files, single-expression files, deeply nested objects, long pipe chains

Example test structure:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_simple_variable() {
        let input = "x:  5";
        let expected = "x: 5\n";
        assert_eq!(format(input).unwrap(), expected);
    }

    #[test]
    fn format_idempotent() {
        let input = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let first = format(input).unwrap();
        let second = format(&first).unwrap();
        assert_eq!(first, second);
    }
}
```

### Phase 3: Playground Integration

1. Add "Format" button to controls in `main.rs`
2. Wire button to `boon::parser::formatter::format()`
3. Handle success (update editor) and error (show message) cases
4. Test in playground — format button works, editor updates

### Phase 4: Polish

1. Format all example `.bn` files and commit canonical versions
2. Document formatting rules in `docs/language/FORMATTING.md`
3. Verify `todo_mvc_physical/` files format correctly (multi-file, comments, themes)

---

## Critical Files

| File | Changes | Part |
|------|---------|------|
| `playground/frontend/src/examples/todo_mvc/todo_mvc.bn` | Remove dead code, inline factory | A |
| `crates/boon/src/parser/formatter.rs` | **New file** — core formatter | B |
| `crates/boon/src/parser.rs` | Add `pub mod formatter;` | B |
| `crates/boon/src/parser/lexer.rs` | Reference — `Token::Comment`, `Token::Newline`, spans | B |
| `playground/frontend/src/main.rs` | Format button UI | B |

---

## Verification

### Part A — TodoMVC Refactoring

1. Load refactored `todo_mvc.bn` in playground
2. Test all functionality:
   - Add todo, complete, filter (All/Active/Completed)
   - Edit (double-click), cancel edit (Escape)
   - Delete, toggle-all, clear completed
   - URL routing (`/`, `/active`, `/completed`)
3. `boon_screenshot_preview` before/after — pixel-identical
4. `boon_console` — no errors

### Part B — Formatter

1. `cargo test -p boon` — all formatter unit tests pass
2. Format all example `.bn` files — all parse without errors after formatting
3. Idempotency: `format(format(x)) == format(x)` for every example
4. Comment preservation: todo_mvc_physical `--` separators survive formatting
5. Format button in playground — click formats code, editor updates
6. Format malformed code — shows error, doesn't crash
