# Code Analysis and Improvements for RUN.bn

**Date**: 2025-11-12
**Status**: Analysis Complete - Ready for Implementation
**Scope**: RUN.bn comprehensive review for simplification and consistency

---

## Executive Summary

RUN.bn demonstrates strong emergent physical design with a well-structured architecture. Analysis revealed opportunities in three main areas:

1. **API Consistency** - Theme function signatures inconsistent
2. **Naming Clarity** - Depth/elevation terminology could be clearer
3. **Boilerplate Reduction** - LINK tracking patterns verbose

**Overall Grade**: B+ (Well-architected, needs polish)

---

## 🔴 Priority 1: Critical Issues

### Issue 1.1: Theme API Inconsistency

**Status**: 🔴 Critical - Breaks API predictability
**Location**: Throughout RUN.bn
**Impact**: Users must memorize which functions use `of:` parameter

**Current State**:
```boon
// Consistent - uses `of:` parameter:
Theme/material(of: Background)
Theme/font(of: Header)
Theme/border(of: Standard)
Theme/depth(of: Container)
Theme/elevation(of: Card)
Theme/corners(of: Comfort)
Theme/pointer_response(of: Checkbox)

// Inconsistent - missing `of:` parameter:
Theme/text_depth(Tertiary)              // Line 499, 500, 568, 671, etc.

// No parameters (correct for global state):
Theme/lights()                           // Line 159
Theme/geometry()                         // Line 160
```

**Proposed Solution**:
```boon
// Change:
transform: [move_further: Theme/text_depth(Tertiary)]

// To:
transform: [move_further: Theme/text_depth(of: Tertiary)]
```

**Implementation Checklist**:
- [ ] Update all `Theme/text_depth(...)` calls in RUN.bn to use `of:` parameter
- [ ] Update Theme/*.bn files to accept `of:` parameter in `text_depth()` function
- [ ] Verify no other theme functions missing `of:` parameter

**Rationale**: Consistent APIs are easier to learn and use. The `of:` parameter pattern is established across 7 other theme functions.

---

### Issue 1.2: Text Depth vs Geometric Depth Naming Confusion

**Status**: 🟡 Medium - Conceptual clarity issue
**Location**: Lines 242, 284, 358, 499-500, 568, 671
**Impact**: Two different "depth" concepts use similar naming

**Current State**:

**Geometric Depth** (extrusion thickness):
```boon
depth: Theme/depth(of: Container)  // 8 units thick
depth: Theme/depth(of: Element)    // 6 units thick
depth: Theme/depth(of: Detail)     // 2 units thick
depth: Theme/depth(of: Hero)       // 10 units thick
```

**Text Z-Position** (no thickness, only lighting):
```boon
transform: [move_further: Theme/text_depth(Primary)]    // Surface level
transform: [move_further: Theme/text_depth(Secondary)]  // Slightly recessed
transform: [move_further: Theme/text_depth(Tertiary)]   // More recessed
```

**Problem**: `text_depth` is misleading - text has no extrusion depth, only Z-position for lighting effects.

**Proposed Solution A** (Rename function):
```boon
// Old:
Theme/text_depth(of: Primary)

// New:
Theme/text_level(of: Primary)
```

**Proposed Solution B** (Keep but document clearly):
```boon
// Document that text_depth returns Z-offset, not extrusion depth
// Add comment explaining the distinction
```

**Recommendation**: **Solution A** - Rename to `Theme/text_level()` for clarity.

**Implementation Checklist**:
- [ ] Rename `text_depth()` to `text_level()` in Theme/*.bn files
- [ ] Update all calls in RUN.bn (lines 499, 500, 568, 671, 679, 693)
- [ ] Update documentation to clarify distinction between geometric depth and Z-level

---

### Issue 1.3: Transform API Axis Terminology

**Status**: 🟡 Medium - Potential confusion
**Location**: Lines 224, 243, 285, 411, 439
**Impact**: Mixing Z-axis and Y-axis movement terms

**Current State**:
```boon
transform: [move_closer: 6]                              // Z+ (toward viewer)
transform: [move_closer: Theme/elevation(of: Card)]      // Z+ (toward viewer)
transform: [move_further: Theme/elevation(of: Inset)]    // Z- (away from viewer)
transform: [rotate: 90, move_up: 18]                     // Rotation + Y? or Z?
transform: [move_closer: 24]                             // Z+ (toward viewer)
```

**Question**: What does `move_up` mean?
- Y-axis screen space translation (up on screen)?
- Z-axis movement toward viewer?

**Context Analysis**: Line 411 uses `rotate: 90, move_up: 18` together. Likely:
- `rotate` = rotation around Z-axis (into screen)
- `move_up` = Y-axis screen space (move upward on screen)

**Problem**: The mix of semantic Z-axis (`move_closer`/`move_further`) with positional Y-axis (`move_up`) in same transform API could be confusing.

**Proposed Solution A** (Explicit axis naming):
```boon
transform: [
    z: 6                    // Toward viewer (positive Z)
    z: -4                   // Away from viewer (negative Z)
    rotate_z: 90            // Rotation around Z-axis
    y: 18                   // Screen-space Y translation
]
```

**Proposed Solution B** (Keep semantic but document):
```boon
// Document clearly:
// - move_closer/move_further: Z-axis (depth)
// - move_up/move_down/move_left/move_right: X/Y-axis (screen space)
// - rotate: Z-axis rotation (degrees)
```

**Recommendation**: **Solution B** - Keep semantic names but ensure documentation is clear.

**Implementation Checklist**:
- [ ] Document transform API axis semantics
- [ ] Verify all transform uses follow conventions
- [ ] Consider adding validation/warnings for mixing axes incorrectly

---

## 🟠 Priority 2: Significant Improvements

### Issue 2.1: LINK Boilerplate Pattern

**Status**: 🟠 Significant - Verbose but functional
**Location**: Throughout RUN.bn (store declaration, element creation, linking)
**Impact**: Every interactive element requires 3-step boilerplate

**Current Pattern**:

**Step 1** - Declare in store (lines 27-37):
```boon
store: [
    elements: [
        filter_buttons: [
            all: LINK
            active: LINK
            completed: LINK
        ]
        remove_completed_button: LINK
        toggle_all_checkbox: LINK
        new_todo_title_text_input: LINK
    ]
]
```

**Step 2** - Create element function:
```boon
FUNCTION new_todo_title_text_input() {
    Element/text_input(
        element: [
            event: [change: LINK, key_down: LINK]
        ]
        ...
    )
}
```

**Step 3** - Link when using (line 249):
```boon
new_todo_title_text_input()
    |> LINK { PASSED.store.elements.new_todo_title_text_input }
```

**Same pattern in todos** (lines 90-95):
```boon
todo_elements: [
    remove_todo_button: LINK
    editing_todo_title_element: LINK
    todo_title_element: LINK
    todo_checkbox: LINK
]
```

**Pain Points**:
1. Manual tracking structure must mirror UI hierarchy
2. Easy to forget linking step
3. Verbose path references: `todo.todo_elements.editing_todo_title_element`
4. Every todo instance needs its own tracking object
5. Store structure grows with UI complexity

**Proposed Solution A** (ID-based references):
```boon
// Step 1: Tag elements with IDs
Element/text_input(
    element: [
        id: 'new-todo-input'
        event: [change: LINK, key_down: LINK]
    ]
    ...
)

// Step 2: Reference by ID anywhere
store.element('new-todo-input').event.change
```

**Pros**:
- Simpler (2 steps instead of 3)
- Familiar pattern (like HTML IDs)
- No manual store structure maintenance

**Cons**:
- String IDs can have typos (no compile-time checking)
- Unclear scoping (global IDs vs local?)

**Proposed Solution B** (Auto-references):
```boon
// Compiler automatically generates references for elements with events
Element/text_input(
    element: [
        ref: auto  // Compiler generates stable reference
        event: [change: LINK, key_down: LINK]
    ]
    ...
)

// Access via query
todos |> List/map(todo =>
    todo.query(Element/text_input).event.key_down
)
```

**Pros**:
- Minimal boilerplate
- Type-safe (compiler-checked)

**Cons**:
- More "magic" (less explicit)
- Query syntax might be verbose

**Proposed Solution C** (Syntactic sugar for current pattern):
```boon
// Wrapper that handles LINK automatically
tracked(
    path: store.elements.new_todo_input
    element: new_todo_title_text_input()
)
```

**Pros**:
- Keeps explicit model
- Reduces boilerplate slightly

**Cons**:
- Still requires manual store structure

**Recommendation**: **Defer decision** - This is a language-level pattern. Document current pattern clearly, consider language improvements in future.

**Implementation Checklist**:
- [ ] Document LINK pattern in LANGUAGE_FEATURES_RESEARCH.md
- [ ] Create examples showing best practices
- [ ] Consider language-level improvements for future Boon versions

---

### Issue 2.2: Router/Filter Configuration Redundancy

**Status**: 🟠 Significant - DRY violation
**Location**: Lines 38-49, 624-628
**Impact**: Filter metadata defined in 3 separate places

**Current State**:

**Location 1** - Route parsing (lines 38-42):
```boon
selected_filter: Router/route() |> WHEN {
    '/active' => Active
    '/completed' => Completed
    __ => All
}
```

**Location 2** - Route generation (lines 43-49):
```boon
go_to_result:
    LATEST {
        filter_buttons.all.event.press |> THEN { '/' }
        filter_buttons.active.event.press |> THEN { '/active' }
        filter_buttons.completed.event.press |> THEN { '/completed' }
    }
    |> Router/go_to()
```

**Location 3** - Filter labels (lines 624-628):
```boon
label: filter |> WHEN {
    All => 'All'
    Active => 'Active'
    Completed => 'Completed'
}
```

**Problem**: Changing a route requires updating 2-3 places. Adding a filter requires 3 updates.

**Proposed Solution** (Single source of truth):
```boon
-- Define once at top level
filter_config: [
    All: [route: '/', label: 'All']
    Active: [route: '/active', label: 'Active']
    Completed: [route: '/completed', label: 'Completed']
]

-- Derive route parsing
selected_filter: BLOCK {
    current_route: Router/route()
    filter_config
        |> Dict/find_first(filter, if: filter.route = current_route)
        |> Option/map(entry => entry.key)
        |> Option/default(All)
}

-- Derive navigation (still needs button references)
go_to_result: LATEST {
    filter_buttons.all.event.press |> THEN { filter_config.All.route }
    filter_buttons.active.event.press |> THEN { filter_config.Active.route }
    filter_buttons.completed.event.press |> THEN { filter_config.Completed.route }
} |> Router/go_to()

-- Derive label
label: filter_config[filter].label
```

**Pros**:
- Single source of truth
- Easy to add new filters
- No inconsistencies

**Cons**:
- Slightly more complex setup
- Requires Dict/Option APIs

**Recommendation**: **Implement if Dict/Option APIs available**, otherwise document as known duplication.

**Implementation Checklist**:
- [ ] Verify Dict/Option APIs exist in Boon
- [ ] Create filter_config structure
- [ ] Update route parsing to use config
- [ ] Update navigation to use config
- [ ] Update labels to use config

---

## 🟢 Priority 3: Minor Improvements

### Issue 3.1: Magic Numbers Should Be Semantic Tokens

**Status**: 🟢 Minor - Maintainability improvement
**Location**: Throughout RUN.bn
**Impact**: Hardcoded values make theme customization harder

**Examples**:
```boon
padding: [column: 19, left: 60, right: 16]              // Line 282
width: [sizing: Fill, minimum: 230, maximum: 550]       // Line 192
gap: 65                                                  // Line 200
height: 130                                              // Line 218
padding: [row: 27, column: 6]                           // Line 409
size: 40                                                 // Line 462
width: 60                                                // Line 334, 396
```

**Proposed Solution**:
```boon
// Add to theme system:
FUNCTION spacing(of) {
    of |> WHEN {
        InputPaddingColumn => 19
        InputPaddingLeft => 60
        InputPaddingRight => 16
        ContentMinWidth => 230
        ContentMaxWidth => 550
        SectionGap => 65
        HeaderHeight => 130
        CheckboxSize => 40
        CheckboxWidth => 60
        ToggleCheckboxPadding => [row: 27, column: 6]
    }
}

// Usage:
padding: [
    column: Theme/spacing(of: InputPaddingColumn)
    left: Theme/spacing(of: InputPaddingLeft)
    right: Theme/spacing(of: InputPaddingRight)
]
width: [
    sizing: Fill
    minimum: Theme/spacing(of: ContentMinWidth)
    maximum: Theme/spacing(of: ContentMaxWidth)
]
```

**Pros**:
- Easier to maintain consistency
- Themes can override spacing
- Self-documenting

**Cons**:
- More verbose
- Many small tokens to manage

**Recommendation**: **Implement for key repeated values** (CheckboxSize: 40, CheckboxWidth: 60), **defer for unique values**.

**Implementation Checklist**:
- [ ] Identify truly repeated spacing values
- [ ] Add Theme/spacing() function to theme system
- [ ] Update RUN.bn to use spacing tokens for repeated values
- [ ] Document when to use tokens vs hardcoded values

---

### Issue 3.2: Conditional Rendering Clarity

**Status**: 🟢 Minor - Readability improvement
**Location**: Lines 252-263, 372-376, 546-550
**Impact**: Some conditionals use inverted logic

**Example 1** - Inverted logic (lines 252-263):
```boon
PASSED.store.todos
    |> List/empty()
    |> WHILE {
        True => NoElement      // If empty, show nothing
        False => Element/stripe(...)  // If NOT empty, show list
    }
```

**Example 2** - Normal logic (lines 372-376):
```boon
element.hovered |> WHILE {
    True => remove_todo_button()
    False => NoElement
}
```

**Problem**: Mixing normal and inverted conditionals reduces readability.

**Proposed Solution A** (Add UNLESS combinator):
```boon
// Instead of:
todos |> List/empty() |> WHILE {
    True => NoElement
    False => Element/stripe(...)
}

// Use:
todos |> List/not_empty() |> UNLESS { Element/stripe(...) }
// Or:
Element/stripe(...) |> show_if(todos |> List/not_empty())
```

**Proposed Solution B** (Use List/not_empty):
```boon
PASSED.store.todos
    |> List/not_empty()
    |> WHILE {
        True => Element/stripe(...)
        False => NoElement
    }
```

**Recommendation**: **Solution B (use List/not_empty)** - no language changes needed, clearer logic.

**Implementation Checklist**:
- [ ] Replace inverted conditionals with `List/not_empty()`
- [ ] Standardize on True = show, False = hide pattern

---

### Issue 3.3: Pointer Response vs Material Naming Alignment

**Status**: 🟢 Minor - Consistency improvement
**Location**: Throughout RUN.bn
**Impact**: Related concepts use different naming

**Current Mapping**:

| Pointer Response | Material | Alignment |
|-----------------|----------|-----------|
| `Checkbox` | `ToggleCheckbox`, `TodoCheckbox` | ❌ Mismatch |
| `Button` | `Button`, `ButtonEmphasis`, `ButtonClear` | ⚠️ One-to-many |
| `ButtonDestructive` | `ButtonDelete` | ❌ Different terms |
| `ButtonFilter` | `ButtonFilter` | ✅ Match |

**Examples**:
```boon
// Line 400-401: ToggleCheckbox
pointer_response: Theme/pointer_response(of: Checkbox)
material: Theme/material(of: ToggleCheckbox[checked: ..., hover: ...])

// Line 465-466: TodoCheckbox
pointer_response: Theme/pointer_response(of: Checkbox)
material: Theme/material(of: TodoCheckbox[checked: ..., hover: ...])

// Line 521-522: ButtonDelete
pointer_response: Theme/pointer_response(of: ButtonDestructive)
material: Theme/material(of: ButtonDelete[hover: ...])
```

**Question**: Should pointer_response and material use the same names?

**Proposed Solution A** (Align names exactly):
```boon
// Change pointer_response to match materials:
pointer_response: Theme/pointer_response(of: ToggleCheckbox)
material: Theme/material(of: ToggleCheckbox[...])

pointer_response: Theme/pointer_response(of: ButtonDelete)
material: Theme/material(of: ButtonDelete[...])
```

**Proposed Solution B** (Use categories + variants):
```boon
// Pointer response = behavior category
// Material = specific appearance variant
pointer_response: Theme/pointer_response(of: Checkbox)  // Generic checkbox behavior
material: Theme/material(of: CheckboxToggle[...])      // Specific toggle appearance

pointer_response: Theme/pointer_response(of: Button)      // Generic button behavior
material: Theme/material(of: ButtonDestructive[...])     // Specific destructive appearance
```

**Recommendation**: **Clarify the distinction** in documentation. Pointer response = behavior category, Material = appearance variant. Current mismatch is intentional.

**Implementation Checklist**:
- [ ] Document pointer_response vs material distinction
- [ ] Clarify that pointer_response is behavior, material is appearance
- [ ] Verify naming makes semantic sense for each use case

---

### Issue 3.4: Empty Array as Signal Pattern

**Status**: 🟢 Minor - Documentation needed
**Location**: Lines 97-103, 122-123
**Impact**: Unclear idiom for temporal triggers

**Current Pattern**:
```boon
title_to_update:
    LATEST {
        todo_elements.editing_todo_title_element.event.blur
            |> THEN { [] }
        todo_elements.editing_todo_title_element.event.key_down.key
            |> WHEN { Enter => [], __ => SKIP }
    }
    |> THEN { todo_elements.editing_todo_title_element.text }
```

**Question**: What does `[]` represent here?
- Empty array/object as trigger signal?
- Unit type (void)?
- Temporal update without payload?

**Observation**: `THEN` seems to respond to any value change, so `[]` is just a simple value to emit.

**Proposed Solution**:
```boon
// Option 1: Explicit trigger type
|> THEN { Trigger }

// Option 2: Unit type syntax
|> THEN { () }

// Option 3: Keep [] but document clearly
|> THEN { [] }  -- Empty value triggers next THEN
```

**Recommendation**: **Document current pattern** - `[]` is a simple value that triggers downstream reactions. Consider adding explicit `Trigger` or `Unit` type in future language versions.

**Implementation Checklist**:
- [ ] Document empty array/object as trigger pattern
- [ ] Add examples to LANGUAGE_FEATURES_RESEARCH.md
- [ ] Consider language-level Trigger/Unit type for future

---

## Text Styling Implementation: Unified Theme/text() API

**Status**: ✅ Implemented
**Date**: 2025-11-12

### Overview

Based on Issue 1.2 (Text Depth vs Geometric Depth), we implemented a unified `Theme/text()` function that returns all text-related properties in one call:

```boon
Theme/text(of: Header) => [
    font: [size: 100, color: ..., weight: Hairline]
    depth: 6              // Geometric thickness of 3D text
    transform: [move_closer: 6]  // Z-position for hierarchy
    text_mode: Emboss     // Rendering mode (Emboss | Deboss)
]
```

This replaces the previous pattern of separate `Theme/font()` and `Theme/text_depth()` calls:

```boon
-- Old (deprecated):
font: Theme/font(of: Header)
transform: [move_further: Theme/text_depth(Primary)]

-- New (recommended):
style: Theme/text(of: Header)
```

### Implementation Coverage

Out of 9 text instances in RUN.bn:

| Location | Element Type | Uses Theme/text() | Notes |
|----------|-------------|-------------------|-------|
| Line 220 | Header | ✅ Yes | Clean usage |
| Line 561 | Item counter | ✅ Yes | Clean usage |
| Line 485 | Todo title | ✅ Yes | Uses Element/text wrapper |
| Line 512 | Remove button | ✅ Yes | Uses Element/text wrapper |
| Line 606 | Filter buttons | ✅ Yes | Uses Element/text wrapper |
| Line 638 | Clear button | ✅ Yes | Uses Element/text wrapper |
| Line 661-685 | Footer paragraphs (3x) | ✅ Yes | Clean usage |
| Line 405 | Checkbox icon | ⚠️ **Special** | Mixed layout + text |
| Line 689 | Footer link | ⚠️ **Special** | Minimal override only |

**Success Rate**: 7 of 9 cases (78%) use the unified API cleanly.

### Special Case 1: Checkbox Icon (Mixed Layout and Text)

**Location**: RUN.bn lines 405-414

**Current Implementation**:
```boon
icon: Element/text(
    style: [
        height: 34                          // Layout property
        padding: [row: 27, column: 6]       // Layout property
        font: Theme/font(of: ButtonIcon[checked: checked])  // Text property
        transform: [rotate: 90, move_up: 18]  // Layout transforms
    ]
    text: '>'
)
```

**Why It's Special**:
- Mixes **layout properties** (height, padding, rotate, move_up) with **text properties** (font)
- The rotation and positioning are specific to the icon's visual design, not semantic text hierarchy
- Using Theme/text() would incorrectly apply semantic depth/embossing meant for readable text

**Pattern**: When text needs custom layout transforms (rotation, positioning) specific to its role as a visual icon, use direct property specification rather than theme function.

**When to Use This Pattern**:
- Icons or decorative text with custom transforms
- Text used as UI geometry rather than content
- Cases where layout and text styling are inseparable

### Special Case 2: Footer Link (Minimal Style Override)

**Location**: RUN.bn lines 689-706

**Current Implementation**:
```boon
FUNCTION footer_link(label) {
    Element/link(
        element: [hovered: LINK]
        style: [
            font: [line: [underline: element.hovered]]  // Only override underline
        ]
        label: label
    )
}
```

**Why It's Special**:
- Only needs to override **one property** (underline on hover)
- All other text properties inherited from context
- Creating full Theme/text() case for single property override is overkill
- The link relies on paragraph's font styling, only adding interaction state

**Pattern**: For minimal overrides of a single font property based on interaction state, use direct property specification.

**When to Use This Pattern**:
- Single property overrides (underline, weight, color)
- Interaction-driven styling changes
- Inheriting most styling from parent/context

### Architecture Decision: Layout vs Semantic Styling

The unified `Theme/text()` API is designed for **semantic text content** with hierarchy (Header, Primary, Secondary, etc.). It bundles properties that should change together:

- Font properties (size, color, weight)
- 3D thickness (depth)
- Z-position (transform)
- Rendering mode (Emboss/Deboss)

For **layout-driven text** (icons, decorative elements) or **minimal overrides** (links), direct property specification is more appropriate.

**Decision Tree**:

```
Is this readable text content?
├─ YES: Does it represent semantic hierarchy?
│  ├─ YES: Use Theme/text(of: SemanticLevel)  ✅
│  └─ NO: Does it need special layout transforms?
│     ├─ YES: Use direct properties  ⚠️ (Special Case 1)
│     └─ NO: Use Theme/text(of: closest match)
└─ NO: Is it a visual icon/decoration?
   └─ YES: Use direct properties  ⚠️ (Special Case 1)

Is this a minimal style override?
└─ YES (1-2 properties): Use direct properties  ⚠️ (Special Case 2)
```

### Implementation Files

The unified `Theme/text()` function is implemented in:

- `Theme/Professional.bn` (lines 313-427)
- `Theme/Neumorphism.bn` (lines 193-270)
- `Theme/Neobrutalism.bn` (lines 192-269)
- `Theme/Glassmorphism.bn` (lines 226-303)

Router added in `Theme/Theme.bn` (lines 29-37).

### Benefits

1. **Consistency**: All text styling properties bundled together
2. **Clarity**: Clear separation of geometric depth vs Z-position
3. **Maintainability**: Single function to update for theme changes
4. **Type Safety**: Semantic levels (Header, Secondary, etc.) document intent

---

## Implementation Strategy

### Phase 1: Quick Wins (Priority 1)
1. **Theme API Consistency** - Add `of:` parameter to `Theme/text_depth()`
2. **Text Depth Rename** - Change to `Theme/text_level()` for clarity
3. **Document transform axes** - Clarify `move_closer` vs `move_up`

**Estimated Effort**: 1-2 hours
**Impact**: High (improves consistency and clarity)

### Phase 2: Significant Improvements (Priority 2)
4. **Router/Filter DRY** - Single source of truth for filter config
5. **Document LINK pattern** - Best practices and alternatives

**Estimated Effort**: 2-3 hours
**Impact**: Medium (reduces duplication, improves documentation)

### Phase 3: Polish (Priority 3)
6. **Spacing tokens** - For repeated values only
7. **Conditional rendering** - Use `List/not_empty()` pattern
8. **Document naming conventions** - Pointer response vs materials
9. **Document trigger patterns** - Empty array usage

**Estimated Effort**: 2-4 hours
**Impact**: Low-Medium (improves maintainability and documentation)

---

## Deferred for Language Design Discussion

The following issues require language-level consideration:

1. **LINK Boilerplate Reduction** (Issue 2.1)
   - Auto-references vs ID-based vs syntactic sugar
   - Needs broader Boon language discussion

2. **Conditional Rendering Sugar** (Issue 3.2)
   - UNLESS combinator
   - show_if/hide_if helpers
   - Can be addressed with existing primitives for now

3. **Trigger/Unit Type** (Issue 3.4)
   - Explicit signal types
   - Language-level feature consideration

---

## Conclusion

RUN.bn demonstrates strong architectural patterns with emergent physical design. The main opportunities are:

**Must Fix**:
- Theme API consistency (`of:` parameter)
- Naming clarity (text_level vs depth)

**Should Improve**:
- Filter configuration DRY
- Documentation of patterns

**Nice to Have**:
- Spacing tokens for repeated values
- Naming convention documentation

The code is fundamentally sound - these are polish and consistency improvements that will enhance maintainability and learnability.

---

**Next Steps**: Review findings with user, prioritize implementation, proceed problem by problem.
