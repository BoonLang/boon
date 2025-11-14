# TEXT Syntax Specification

**Date**: 2025-11-14
**Status**: Final Design
**Scope**: String literals and text templates in Boon

---

## Executive Summary

TEXT provides a unified syntax for string literals in Boon, supporting both inline and multiline strings with visual indentation, interpolation, and hash escaping. No escape sequences needed - multiline TEXT handles newlines naturally, hash escaping handles literal braces, and Unicode works directly.

**Three modes determined by character after opening `{`:**
- `}` → Empty mode
- ` ` (space) → Inline mode (requires padding)
- `\n` (newline) → Multiline mode (visual indentation)

---

## Table of Contents

1. [Basic Syntax & Mode Detection](#basic-syntax--mode-detection)
2. [Spacing Rules (Strict)](#spacing-rules-strict)
3. [Interpolation](#interpolation)
4. [Multiline & Visual Indentation](#multiline--visual-indentation)
5. [Hash Escaping](#hash-escaping)
6. [No Escape Sequences](#no-escape-sequences)
7. [Text Module API](#text-module-api)
8. [Complete Examples](#complete-examples)

---

## Basic Syntax & Mode Detection

### Three Modes

The character immediately after `TEXT {` determines the mode:

```boon
TEXT {}              -- } after { → Empty mode
TEXT { content }     -- space after { → Inline mode
TEXT {               -- newline after { → Multiline mode
    content
}
```

### Lexer Algorithm

```rust
fn lex_text() {
    expect("TEXT");

    // Count hashes (for escaping)
    let hash_count = consume_while('#').len();

    expect('{');

    // Next character determines mode
    match peek() {
        '}' => EmptyText { hash_count },
        ' ' => InlineText { consume_padded_content(), hash_count },
        '\n' => MultilineText { consume_multiline_content(), hash_count },
        _ => error("Expected }, space, or newline after TEXT {")
    }
}
```

---

## Spacing Rules (Strict)

**Only two valid patterns:**

1. **Empty:** `TEXT {}`
2. **With content:** `TEXT { content }` (exactly one space padding on each side)

### Empty String

```boon
TEXT {}           -- ✅ Only way to represent empty string
TEXT #{}          -- ✅ Empty with hash (rarely needed)
```

### Inline Strings (MUST have padding)

```boon
-- Single character:
TEXT { + }        -- ✅ Valid (space padding required)
TEXT {+}          -- ❌ Error: "Missing padding. Use TEXT { + }"

-- Multiple characters:
TEXT { Hello }    -- ✅ Valid (space padding required)
TEXT {Hello}      -- ❌ Error: "Missing padding. Use TEXT { Hello }"

-- With symbols:
TEXT { A + B }    -- ✅ Valid
TEXT {A + B}      -- ❌ Error: "Missing padding"
```

### Edge Case: Single Space

```boon
TEXT { }          -- ❌ Error: only 1 char between braces (not enough for padding)
TEXT {   }        -- ⚠️  Technically valid: 3 chars (pad + space + pad), but use constant instead

-- Use constant instead:
Text/space        -- ✅ Correct way (same as TEXT {   } with 3 chars)
```

### Multiple Spaces

Content between padding is literal:

```boon
TEXT {    }       -- ✅ Two spaces of content
                  --    (4 chars total between braces: 1 pad + 2 content + 1 pad)
TEXT {     }      -- ✅ Three spaces of content
                  --    (5 chars total between braces: 1 pad + 3 content + 1 pad)

-- Alternative using Text/repeat() - clearer than counting spaces:
Text/repeat(Text/space, 2)  -- Creates 2 spaces
Text/repeat(Text/space, 3)  -- Creates 3 spaces
```

**Rule:** Exactly ONE space padding on each side, everything between is content.

### Multiline (No padding - newline marks mode)

```boon
TEXT {
    Line 1
    Line 2
}
```

Newline after `{` triggers multiline mode - no space padding used.

---

## Interpolation

### Simple Variable References Only

```boon
-- ✅ Valid:
TEXT { Hello {name}! }
TEXT { User: {user.email} }
TEXT { Data: {PASSED.store.title} }
TEXT { Color: {config.ui.theme.primary} }

-- ❌ Invalid:
TEXT { Count: { count } }      -- No spaces around variable
TEXT { Sum: {x + y} }           -- No expressions
TEXT { Value: {Math/sum(x)} }   -- No function calls
TEXT { Check: {x > 10} }        -- No comparisons
```

**Syntax:** `{variable}`, `{object.field}`, `{object.field.nested}`, `{PASSED.path.to.value}`

**Rules:**
- No spaces inside braces: `{var}` not `{ var }`
- Only variable references and field access
- No expressions, operators, or function calls

---

## Multiline & Visual Indentation

### Basic Multiline

```boon
TEXT {
    Line 1
    Line 2
    Line 3
}
```

### Visual Indentation for Interpolation

When `{variable}` appears **alone on its line**, its position determines output indentation:

```boon
TEXT {
    outer: [
        {content}
    ]
}
```

**Algorithm:**
1. Closing `}` column = C
2. Minimum required indent = C + 4
3. `{content}` column = M
4. Extra indent = M - (C + 4)
5. Apply extra indent to each line of content

**Example:**

```boon
FUNCTION generate_class(methods) {
    methods
        |> Text/join_lines()
        |> WHEN { code => TEXT {
            class User {
                {code}
            }
        } }
}
```

- Closing `}` at column 8
- Minimum required: 8 + 4 = 12
- `{code}` at column 16
- Extra indent: 16 - 12 = 4 spaces
- Each line of `code` content gets +4 indent

**Benefits:**
- ✅ WYSIWYG - template shows final structure
- ✅ No pre-processing needed (no `Text/indent()` calls)
- ✅ Intuitive - visual position = output position

---

## Hash Escaping

Use hash prefix when content contains literal braces. **Hash count goes between TEXT and `{`:**

```boon
TEXT { ... }       -- No hash
TEXT #{ ... }      -- One hash
TEXT ##{ ... }     -- Two hashes
```

### How Hashes Work

The hash count determines the interpolation pattern:

| Hash Count | Interpolation | Literal Braces |
|------------|---------------|----------------|
| 0 (none) | `{var}` | None allowed |
| 1 | `#{var}` | `{` and `}` |
| 2 | `##{var}` | `#{` and `{` |
| N | `#`×N + `{var}` | All patterns with fewer `#` |

### No Hash (Default)

```boon
TEXT { User: {name}, Score: {score} }
```

- Interpolation: `{var}`
- Literal braces: Not allowed (would conflict)

### One Hash

```boon
TEXT #{ function() { return #{value}; } }
```

- Interpolation: `#{var}`
- Literal: `{` and `}` (with no hash prefix)

### Two Hashes

```boon
TEXT ##{ CSS: a[href^="#{url}"] { color: ##{color}; } }
```

- Interpolation: `##{var}`
- Literal: `#{` and `{` (with fewer than 2 hashes)

### Works with All Modes

```boon
-- Empty:
TEXT #{}

-- Inline:
TEXT #{ function() { return #{x}; } }

-- Multiline:
TEXT #{
    function process() {
        return #{value};
    }
}
```

### Closing is Always `}`

**Asymmetric design** - hash only at opening:

```boon
TEXT { content }       -- Close with }
TEXT #{ content }      -- Close with } (not }#)
TEXT ##{ content }     -- Close with } (not }##)
```

Closing is always just `}` (no hash). Lexer tracks brace depth and interpolation patterns to find correct closing.

---

## No Escape Sequences

### Newlines → Use Multiline TEXT

```boon
-- No need for \n:
TEXT {
    Line 1
    Line 2
}
```

### Tabs → Use Spaces

Boon style uses spaces, not tabs:

```boon
TEXT { Name:    Value }
```

### Literal Braces → Use Hash

```boon
TEXT #{ Code: { literal } and #{interpolation} }
```

### Unicode → Just Type It

```boon
TEXT { Hello 🌍 }
TEXT { Price: €50 }
TEXT { Arrow: → }
TEXT { 你好 {name} }
```

No `\u{...}` escape sequences needed!

---

## Text Module API

### Constants

Defined in the Text module using TEXT syntax:

```boon
-- Newline character (multiline TEXT with empty content):
newline: TEXT {

}

-- Single space (TEXT { } with 1 char is compiler error, use this):
space: TEXT {   }  -- 3 chars between braces (1 pad + 1 space + 1 pad)

-- Tab character (actual tab, not spaces):
tab: Text/character(9)
```

### Functions

```boon
-- Create single character from code point:
Text/character(code: Number) -> Text

-- Repeat text N times:
Text/repeat(text: Text, count: Number) -> Text

-- Join list with custom separator:
Text/join(list: List, separator: Text) -> Text

-- Join list with newlines (sugar for join with Text/newline):
Text/join_lines(list: List) -> Text

-- Remove leading and trailing whitespace:
Text/trim(text: Text) -> Text

-- Check if text is empty:
Text/is_empty(text: Text) -> Bool

-- Check if text is not empty:
Text/is_not_empty(text: Text) -> Bool

-- Indent text (build-time only):
Text/indent(text: Text, spaces: Number) -> Text
```

### Usage Examples

```boon
-- Join lines (most common):
lines |> Text/join_lines()

-- Join with space:
words |> Text/join(Text/space)

-- Join with tab (for TSV):
fields |> Text/join(Text/tab)

-- Custom separator:
items |> Text/join(TEXT { | })

-- Repeat text:
Text/repeat(TEXT { - }, 10)           -- Creates "----------"
Text/repeat(Text/space, 4)            -- Creates 4 spaces
Text/repeat(TEXT { Hello }, 3)        -- Creates "HelloHelloHello"

-- Empty check:
text |> Text/is_empty() |> WHEN { True => ..., False => ... }

-- Not empty check (cleaner than Bool/not):
text |> Text/is_not_empty() |> WHEN { True => ..., False => ... }

-- Rare control characters:
bell: Text/character(7)
escape: Text/character(27)
```

### Why These Names?

**Predicates use `is_` prefix:**
- `Text/is_empty()` not `Text/empty()` - avoids confusion with empty string literal
- `Text/is_not_empty()` not `Text/not_empty()` - consistent with `is_empty`

**No abbreviations:**
- `Text/character()` not `Text/char()` - Boon prefers full words

**Pattern consistency:**
- Same naming pattern as `List/is_empty()` and `List/is_not_empty()`

---

## Complete Examples

### Empty and Spaces

```boon
empty: TEXT {}
one_space: Text/space        -- TEXT { } is compiler error
two_spaces: TEXT {   }       -- 3 chars total = 1 space content
single_char: TEXT { - }      -- Single char must have padding
```

### Simple Inline

```boon
name: TEXT { Alice }
greeting: TEXT { Hello {name}! }
path: TEXT { ./assets/icons }
extension: TEXT { svg }
```

### Interpolation

```boon
message: TEXT { Found {count} items }
user_info: TEXT { {user.name} ({user.email}) }
config: TEXT { Theme: {PASSED.theme.name} }
```

### Multiline Templates

```boon
template: TEXT {
    Dear {name},

    Welcome to {project}!

    Best regards
}
```

### Visual Indentation

```boon
code: TEXT {
    class User {
        {methods}

        function process() {
            {body}
        }
    }
}
```

Where `{methods}` and `{body}` positions determine their output indentation.

### Hash Escaping

```boon
-- JavaScript code:
js: TEXT #{
    function process(data) {
        return data.map(x => #{transform});
    }
}

-- CSS:
css: TEXT #{
    a {
        color: #{color};
    }
}

-- Nested hash patterns:
docs: TEXT ##{
    Use #{pattern} for literals
    Use ##{value} for interpolation
}
```

### BUILD.bn Example

```boon
FUNCTION module_code(svg_files) {
    svg_files
        |> List/map(old, new: icon_code(file: old))
        |> Text/join_lines()
        |> WHEN { code => TEXT {
            -- Generated from {icons_directory}

            icon: [
                {code}
            ]

        } }
}

FUNCTION icon_code(file) {
    file.file_stem
        |> WHEN { name =>
            file.path
                |> File/read_text()
                |> Url/encode()
                |> WHEN { encoded => TEXT { {name}: 'data:image/svg+xml;utf8,{encoded}' } }
        }
}
```

### TodoMVC Validation Example

```boon
new_todo_title: elements.new_todo_title_text_input.text
    |> Text/trim()

title_to_save: new_todo_title
    |> Text/is_not_empty()
    |> WHEN { True => new_todo_title, False => SKIP }
```

Using `Text/is_not_empty()` is cleaner than `Text/is_empty() |> Bool/not()`.

---

## Future: ANSI Module (Terminal Control)

For terminal applications, a dedicated ANSI module could provide high-level functions:

```boon
-- Future API (not part of core TEXT syntax):
ANSI/color(text: Text, color: Color) -> Text
ANSI/bold(text: Text) -> Text
ANSI/clear_screen() -> Text
ANSI/move_cursor(row: Number, col: Number) -> Text
ANSI/bell() -> Text

-- Usage:
error_text: ANSI/color(text: TEXT { Error }, color: Red)
important: ANSI/bold(text: TEXT { Warning })
```

This would hide escape sequences and provide semantic terminal control. Can be added when terminal applications become common in Boon.

---

## Design Principles

1. **Explicit over implicit** - Content is literal, spacing is visible
2. **Visual structure** - Multiline templates show output structure
3. **No escape hell** - Natural solutions (multiline, hash, Unicode)
4. **Flexible** - Inline to complex multiline, simple to hash-escaped
5. **Consistent** - Same syntax scales from empty string to code generation

---

## Migration from Old Syntax

### Before (with quotes and escapes)

```boon
text: 'Hello {name}!'
text: 'I don\'t like it'
text: 'Line 1\nLine 2\nLine 3'
code: 'function() { return {x}; }'
empty: ''
check: text |> Text/trim() |> Text/empty() |> Bool/not()
```

### After (with TEXT)

```boon
text: TEXT { Hello {name}! }
text: TEXT { I don't like it }
text: TEXT {
    Line 1
    Line 2
    Line 3
}
code: TEXT #{ function() { return #{x}; } }
empty: TEXT {}
check: text |> Text/trim() |> Text/is_not_empty()
```

### Key Changes

- Apostrophes work without escaping
- Multiline instead of `\n`
- Hash escaping for literal braces
- `TEXT {}` for empty string
- `Text/is_empty()` and `Text/is_not_empty()` predicates
- `Text/newline`, `Text/space`, `Text/tab` constants

---

## Summary Table

| Feature | Syntax | Example |
|---------|--------|---------|
| **Mode Detection** | Next char after `{` | `}`, space, or newline |
| Empty | `TEXT {}` | `TEXT {}` |
| Single space | Use constant | `Text/space` |
| Inline | `TEXT { content }` | `TEXT { Hello }` (padding required) |
| Single char | `TEXT { x }` | `TEXT { + }` (padding required) |
| Interpolation | `{var}` (no spaces) | `TEXT { Hello {name}! }` |
| Multiline | Newline after `{` | `TEXT {\n    Line\n}` |
| Visual indent | Variable position | `{code}` at column N adds indent |
| Hash escape | Between TEXT and `{` | `TEXT #{ { } and #{x} }` |
| Hash modes | All three modes | Empty, inline, multiline with hashes |
| Asymmetric close | Always `}` | `TEXT #{ ... }` not `TEXT #{ ... }#` |
| No escapes | Natural solutions | Multiline, hash, Unicode |
| Constants | `Text/newline/space/tab` | Defined with TEXT syntax |
| Character code | `Text/character(code)` | For control chars |
| Repeat text | `Text/repeat(text, n)` | `Text/repeat(TEXT { - }, 10)` |
| Predicates | `is_` prefix | `is_empty`, `is_not_empty` |
| Join lines | `Text/join_lines()` | `lines \|> Text/join_lines()` |

### Strict Spacing Rules Summary

```boon
-- Valid patterns:
TEXT {}              ✅ Empty
TEXT { x }           ✅ Content with padding (exactly one space each side)
TEXT {               ✅ Multiline (newline after {)
    content
}

-- Invalid patterns:
TEXT {x}             ❌ Missing padding (only 1 char, need 3+ for inline mode)
TEXT {  x  }         ❌ Extra padding (need exactly 1 space each side)
TEXT { }             ❌ Only 1 char between braces (use Text/space constant)
TEXT{ x }            ❌ Space before { not allowed
```

---

## Compiler Error Messages

Clear, helpful errors guide developers to correct syntax:

```boon
TEXT {hello}
-- Error: Missing padding. Content must have exactly one space on each side.
-- Help: Use TEXT { hello }

TEXT {  hello  }
-- Error: Extra padding detected. Use exactly one space on each side.
-- Help: Use TEXT { hello }

TEXT { }
-- Error: Use Text/space constant for single space character.
-- Help: Replace with Text/space

TEXT {hello
-- Error: Newline must be immediately after opening { for multiline mode.
-- Help: Use TEXT { hello } for inline or TEXT {\n    hello\n} for multiline

TEXT{ hello }
-- Error: Unexpected character after TEXT keyword.
-- Help: Did you mean TEXT { hello }?
```

---

## Common Questions (FAQ)

### **Q: Why can't I use `TEXT {hello}` without spaces?**

**A:** The space padding makes mode detection unambiguous and prevents confusion with interpolation. After `TEXT {`, the next character determines the mode:
- `}` → empty
- ` ` (space) → inline with padding
- `\n` (newline) → multiline

Without this rule, `TEXT {x}` could be confused with interpolation syntax.

### **Q: How do I represent a single space character?**

**A:** Use the `Text/space` constant. While `TEXT {   }` (3 chars between braces: 1 pad + 1 space content + 1 pad) is technically valid, it's visually confusing. The `Text/space` constant is defined as `TEXT {   }` in the Text module, but developers should use the constant for clarity.

### **Q: How do I include literal braces in my text?**

**A:** Use hash escaping:
```boon
TEXT #{ code with { } and #{interpolation} }
```

The hash goes between `TEXT` and `{`. Inside, use `#{var}` for interpolation and `{` for literal braces.

### **Q: Can I use expressions in interpolation?**

**A:** No, only simple variable references:
```boon
TEXT { Hello {name}! }              -- ✅ Variable
TEXT { User: {user.email} }         -- ✅ Field access
TEXT { Config: {PASSED.store.title} } -- ✅ Nested path
TEXT { Count: {count + 1} }         -- ❌ Expression not allowed
```

### **Q: Why is `TEXT { }` (single space) a compiler error?**

**A:** The TEXT syntax only allows `TEXT {}` (empty) or `TEXT { content }` (with padding). When a space appears after `{`, it triggers inline mode where the spaces are consumed as padding, not content.

Inline mode requires minimum 3 characters between braces:
- 1 space (left padding - consumed)
- 1+ characters (content)
- 1 space (right padding - consumed)

With only 1-2 characters total between braces, there isn't enough for both padding and content.

### **Q: How does visual indentation work in multiline TEXT?**

**A:** When `{variable}` appears alone on its line, its column position determines extra indentation applied to the variable's content:

```boon
TEXT {
    outer: [
        {content}    -- At column 8
    ]
}
```

If closing `}` is at column 0, strip amount is 4. The `{content}` at column 8 means extra indent = 8 - 4 = 4 spaces added to each line of content.

### **Q: Can I nest TEXT blocks?**

**A:** No, but you can use interpolation to compose strings:
```boon
inner: TEXT { inner text }
outer: TEXT { outer {inner} text }
```

### **Q: What's the difference between `Text/trim()` and manual whitespace removal?**

**A:** `Text/trim()` removes leading and trailing whitespace. For validation, it's common to trim first then check if empty:
```boon
user_input |> Text/trim() |> Text/is_not_empty()
```

---

## Anti-Patterns to Avoid

### **❌ Don't use `Text/character()` for common characters**

```boon
-- Bad:
space: Text/character(32)
newline: Text/character(10)

-- Good:
space: Text/space
newline: Text/newline
```

Use the provided constants for readability.

### **❌ Don't chain `is_empty() |> Bool/not()`**

```boon
-- Bad:
text |> Text/is_empty() |> Bool/not()

-- Good:
text |> Text/is_not_empty()
```

Use the dedicated predicate function.

### **❌ Don't forget padding in inline TEXT**

```boon
-- Bad (compiler error):
label: TEXT {+}
message: TEXT {Hello}

-- Good:
label: TEXT { + }
message: TEXT { Hello }
```

All inline TEXT must have exactly one space padding on each side.

### **❌ Don't use TEXT for single space**

```boon
-- Bad (visually confusing - hard to count spaces):
separator: TEXT {   }    -- 3 chars total = 1 space of content

-- Good:
separator: Text/space
```

Use the constant for clarity.

### **❌ Don't put spaces in interpolation**

```boon
-- Bad:
TEXT { Hello { name }! }
--           ^     ^
--           spaces not allowed

-- Good:
TEXT { Hello {name}! }
```

Interpolation `{var}` must have no spaces inside braces.

### **❌ Don't mix content and newline after opening `{`**

```boon
-- Bad (error):
TEXT {Hello
    World
}

-- Good (multiline):
TEXT {
    Hello
    World
}

-- Good (inline):
TEXT { Hello World }
```

Newline must be **immediately** after `{` for multiline mode.

### **❌ Don't use hash escaping when not needed**

```boon
-- Bad (unnecessary):
TEXT #{ Hello #{name} }
--   ^
--   Hash not needed, no literal braces in content

-- Good:
TEXT { Hello {name} }
```

Only use hash escaping when you have literal braces in content.

### **❌ Don't use expressions in interpolation**

```boon
-- Bad:
TEXT { Total: {price * quantity} }
TEXT { Name: {Text/trim(name)} }

-- Good:
total: price * quantity
TEXT { Total: {total} }

clean_name: name |> Text/trim()
TEXT { Name: {clean_name} }
```

Compute values first, then interpolate simple variable references.

---

**Last Updated:** 2025-11-14
