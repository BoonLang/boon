# The LINK Pattern: Boon's Explicit Reactive Architecture

**Date**: 2025-11-12
**Status**: Core Language Concept
**Scope**: Reactive dataflow architecture in Boon

---

## Executive Summary

LINK is **not boilerplate** - it's Boon's explicit reactive architecture pattern. The three-step LINK pattern creates bidirectional reactive channels in the dataflow graph, making invisible dependencies visible and verifiable.

**The three steps are architecturally distinct:**
1. **Declare Architecture** - Document what reactive slots exist in the system
2. **Declare Interface** - Advertise what reactive streams a component provides
3. **Wire Connections** - Connect component streams to architectural slots

This pattern is similar to electrical wiring (socket → device → plug in) or network configuration (interface → service → bind). Each step serves a distinct purpose and enables powerful reactive capabilities.

---

## What LINK Actually Does: Reactive Plumbing

LINK creates **bidirectional reactive channels** in Boon's dataflow graph. It enables:

- **Multiple consumers** of the same event stream (local and remote access)
- **Cross-element coordination** through centralized reactive state
- **Dynamic element collections** with independent reactive channels per instance
- **Compile-time verification** of reactive topology

---

## The Three Steps Are Architecturally Distinct

### Step 1 - Declare Architecture (Store Level)

**Location**: Store declaration (RUN.bn lines 27-37, 90-95)

```boon
store: [
    elements: [
        new_todo_title_text_input: LINK  -- Reserve a reactive slot
        toggle_all_checkbox: LINK         -- Document what exists
        filter_buttons: [
            all: LINK
            active: LINK
            completed: LINK
        ]
    ]
]

FUNCTION new_todo(title) {
    [
        todo_elements: [
            remove_todo_button: LINK
            editing_todo_title_element: LINK
            todo_title_element: LINK
            todo_checkbox: LINK
        ]
    ]
}
```

**Purpose**: System architecture documentation.

**What you see at a glance:**
- What interactive elements exist in the system
- What can emit events and hold state
- What the reactive topology looks like
- The hierarchy of reactive entities

**Why this matters:**
- Documents the "reactive surface area" of your application
- Shows architectural intent before implementation
- Makes system structure greppable and analyzable
- Serves as API documentation for the reactive layer

---

### Step 2 - Declare Capabilities (Element Level)

**Location**: Element definition (RUN.bn lines 272-277, 388-390)

```boon
Element/text_input(
    element: [
        event: [
            change: LINK    -- This element emits change events
            key_down: LINK  -- This element emits key_down events
        ]
    ]
    style: [...]
    text: LATEST {
        ''
        element.event.change.text  -- Local access to own event
    }
)

Element/checkbox(
    element: [
        event: [click: LINK]  -- This element emits click events
        hovered: LINK         -- This element tracks hover state
    ]
    style: [...]
)
```

**Purpose**: Interface declaration.

**What the element says:**
- "I provide these reactive streams"
- "I maintain these reactive properties"
- "You can observe these state changes"

**Why this matters:**
- Self-documenting component API
- Compiler can verify that accessed events actually exist
- Clear contract between component and consumers
- Enables both local (within element) and remote (from store) access

---

### Step 3 - Wire Connections (Integration Level)

**Location**: Element instantiation (RUN.bn lines 246, 336, 362, 366)

```boon
-- Global element wiring
new_todo_title_text_input()
    |> LINK { PASSED.store.elements.new_todo_title_text_input }

toggle_all_checkbox()
    |> LINK { PASSED.store.elements.toggle_all_checkbox }

-- Per-instance element wiring (dynamic collections)
todo_checkbox(todo: todo)
    |> LINK { todo.todo_elements.todo_checkbox }

editing_todo_title_element(todo: todo)
    |> LINK { todo.todo_elements.editing_todo_title_element }
```

**Purpose**: Reactive plumbing - connect component streams to architectural slots.

**What this accomplishes:**
- Establishes bidirectional reactive channel
- Makes element's events accessible at the declared path
- Enables element state to be observed remotely
- Connects the dataflow graph

**Why this matters:**
- Explicit connection point (no magic)
- Clear data flow path from element to store
- Compiler can verify path exists and types match
- Debuggable reactive connections

---

## Why This Pattern Is Powerful

### 1. Multiple Consumers of Same Event Stream

LINK allows both **local** (within element) and **remote** (from store) access to the same reactive stream.

**Local access** (RUN.bn line 289):
```boon
FUNCTION new_todo_title_text_input() {
    Element/text_input(
        element: [
            event: [change: LINK, key_down: LINK]
        ]
        text: LATEST {
            ''
            element.event.change.text  -- Element uses its own event locally
            PASSED.store.title_to_save |> THEN { '' }
        }
    )
}
```

**Remote access** (RUN.bn line 50-58):
```boon
store: [
    title_to_save: elements.new_todo_title_text_input.event.key_down.key |> WHEN {
        Enter => BLOCK {
            new_todo_title: elements.new_todo_title_text_input.text |> Text/trim()
            new_todo_title
                |> Text/empty()
                |> Bool/not()
                |> WHEN { True => new_todo_title, False => SKIP }
        }
        __ => SKIP
    }
]
```

**Both consumers access the same reactive stream through the LINK!**

The element uses its own events for immediate UI updates, while the store uses the same events for application logic and data flow. This separation is clean and explicit.

---

### 2. Cross-Element Coordination

LINK enables centralized reactive state that coordinates multiple elements.

**Example: Toggle-all checkbox affects every todo** (RUN.bn line 110-118):
```boon
FUNCTION new_todo(title) {
    [
        completed:
            LATEST {
                False
                store.elements.toggle_all_checkbox.event.click |> THEN {
                    store.todos
                        |> List/every(item, if: item.completed)
                        |> Bool/not()
                }
            }
            |> Bool/toggle(when: todo_elements.todo_checkbox.event.click)
    ]
}
```

The `toggle_all_checkbox` affects **every todo's completed state** through the reactive graph. Each todo observes the toggle checkbox's events and coordinates its state accordingly.

**This is only possible because:**
- The checkbox's events are available at `store.elements.toggle_all_checkbox`
- The LINK makes the event stream accessible from anywhere in the store
- Multiple todos can subscribe to the same event stream
- The reactive graph handles coordination automatically

---

### 3. Dynamic Element Collections with Independent Channels

LINK handles dynamic collections where each instance has its own reactive channels.

**Per-todo reactive architecture** (RUN.bn lines 88-95, 361-374):
```boon
FUNCTION new_todo(title) {
    [
        todo_elements: [
            todo_checkbox: LINK           -- Each todo has its own checkbox
            remove_todo_button: LINK      -- Each todo has its own remove button
            editing_todo_title_element: LINK
            todo_title_element: LINK
        ]
        id: TodoId[id: Ulid/generate()]
        completed: LATEST {
            False
            -- Toggle on global checkbox click
            store.elements.toggle_all_checkbox.event.click |> THEN { ... }
        }
        |> Bool/toggle(when: todo_elements.todo_checkbox.event.click)
    ]
}

// Later, each todo instance is independently wired:
FUNCTION todo_element(todo) {
    Element/stripe(
        items: LIST {
            todo_checkbox(todo: todo)
                |> LINK { todo.todo_elements.todo_checkbox }

            todo_title_element(todo: todo)
                |> LINK { todo.todo_elements.todo_title_element }

            element.hovered |> WHILE {
                True => remove_todo_button()
                    |> LINK { todo.todo_elements.remove_todo_button }
                False => NoElement
            }
        }
    )
}
```

**Key insight:**
- Each todo in the list is a **separate reactive entity** with its own event streams
- The "tracking object" (`todo.todo_elements`) is the **per-instance reactive architecture**
- Each todo's checkbox can be toggled independently
- Each todo's remove button fires independently
- The LINK pattern handles this naturally without special collection handling

**Access pattern for todo-specific events** (RUN.bn lines 64-65, 73-78):
```boon
todos: LIST {}
    |> List/append(item: title_to_save |> new_todo())
    |> List/retain(item, if: LATEST {
        True
        -- Each item observes its own remove button
        item.todo_elements.remove_todo_button.event.press
            |> THEN { False }
        -- Global remove completed button
        elements.remove_completed_button.event.press
            |> THEN { item.completed |> Bool/not() }
    })

selected_todo_id: LATEST {
    None
    todos
        |> List/map(old, new: LATEST {
            -- Each old todo observes its own elements
            old.todo_elements.editing_todo_title_element.event.key_down.key
                |> WHEN { Escape => None, __ => SKIP }
            old.title_to_update
                |> THEN { None }
            old.todo_elements.todo_title_element.event.double_click
                |> THEN { old.id }
        })
        |> List/latest()
}
```

Each `item` in the list has its own `item.todo_elements.remove_todo_button` - completely independent reactive channels.

---

### 4. Explicit Data Flow Paths

The path structure makes data flow explicit and traceable.

```boon
PASSED.store.elements.new_todo_title_text_input.event.key_down.key
│      │     │         │                         │     │         └── Data: the key value
│      │     │         │                         │     └── Event type: key_down
│      │     │         │                         └── Event namespace
│      │     │         └── Element name
│      │     └── Element collection
│      └── Store location
└── Context: passed from parent
```

**Each segment has semantic meaning:**
- `PASSED.store` - Where the data comes from (passed context)
- `.elements` - What layer it's in (element tracking)
- `.new_todo_title_text_input` - What specific element
- `.event.key_down` - What capability/event type
- `.key` - What specific data

**This explicitness enables:**
- **Greppability**: Search for all usages of an element's events
- **Refactoring**: Rename element, compiler catches all references
- **Debugging**: Trace exactly where data flows from/to
- **Documentation**: The path itself documents the architecture

---

## What I Got Wrong Initially

In the original CODE_ANALYSIS_AND_IMPROVEMENTS.md, I characterized LINK as "boilerplate" (Issue 2.1). This was a misunderstanding of the pattern's purpose. Here's what I got wrong:

### Mistake 1: "Every todo instance needs its own tracking object" ← Framed as a problem

**Reality**: This is **correct design**!

Each todo IS a separate reactive entity with its own event streams. The "tracking object" is the **per-instance reactive architecture**. Just as each HTML element has its own event listeners, each Boon element has its own reactive channels.

Without per-instance channels, you couldn't:
- Toggle individual todos independently
- Remove individual todos
- Edit individual todos
- Track which specific todo was double-clicked

---

### Mistake 2: "Manual tracking structure must mirror UI hierarchy" ← Framed as duplication

**Reality**: The structure **documents the hierarchy**.

The store structure shows exactly what reactive capabilities exist in the system. It's architectural self-documentation, not duplication.

```boon
store: [
    elements: [
        filter_buttons: [
            all: LINK
            active: LINK
            completed: LINK
        ]
    ]
]
```

Looking at this, you immediately know:
- There are filter buttons
- There are three of them (all, active, completed)
- Each can emit events independently
- They're organized hierarchically under filter_buttons

This is **documentation as code**, not boilerplate.

---

### Mistake 3: "Verbose path references" ← Framed as boilerplate

**Reality**: Explicit paths show data flow.

```boon
PASSED.store.elements.new_todo_title_text_input.event.key_down.key
```

This explicitly shows:
- **Where**: `PASSED.store` (from parent context)
- **Layer**: `.elements` (reactive element layer)
- **Element**: `.new_todo_title_text_input` (specific element)
- **Capability**: `.event.key_down` (event type)
- **Data**: `.key` (the actual value)

This is **explicit data flow**, not verbosity. Compare to implicit alternatives:

```javascript
// JavaScript - where does this come from?
event.key

// React - is this local state? Props? Context?
value

// Svelte - is this a store? Local? Prop?
$value
```

Boon's paths make it obvious. You can also use local aliases to reduce repetition when needed:

```boon
input: PASSED.store.elements.new_todo_title_text_input
title_to_save: input.event.key_down.key |> WHEN { ... }
text: input.text |> Text/trim()
```

---

### Mistake 4: Proposed "solutions" that break the model

**ID-based references** (from original analysis):
```boon
// PROPOSED (BAD):
Element/text_input(
    element: [id: 'new-todo-input']
)

// Access by ID anywhere
store.element('new-todo-input').event.change
```

**Problems:**
- ❌ String IDs can have typos (no compile-time checking)
- ❌ Unclear scoping (global IDs vs local?)
- ❌ No architectural documentation in store
- ❌ Magic lookup - where is this element in the tree?
- ❌ Loses hierarchy information

**Auto-references** (from original analysis):
```boon
// PROPOSED (BAD):
Element/text_input(
    element: [ref: auto]  // Compiler generates stable reference
)

// Access via query
todos |> List/map(todo =>
    todo.query(Element/text_input).event.key_down
)
```

**Problems:**
- ❌ Too much magic - where does the reference live?
- ❌ Query syntax is vague - which text_input if there are multiple?
- ❌ No architectural declaration
- ❌ Runtime overhead for queries

**Current LINK pattern has none of these problems:**
- ✅ Compile-time verified paths
- ✅ Clear scoping (explicit path from root)
- ✅ Architectural documentation in store
- ✅ Zero runtime overhead (direct references)
- ✅ Preserves hierarchy information

---

## How LINK Could Be Improved (Without Changing The Model)

The LINK pattern is fundamentally sound, but there are quality-of-life improvements that preserve the explicit model:

### 1. Compiler Verification and Error Messages

The three steps should be type-checked with helpful error messages:

```boon
// Step 1: Declared in store
store: [elements: [my_button: LINK]]

// Step 2: Element provides events
Element/button(element: [event: [press: LINK, click: LINK]])

// Step 3: Connected
my_button() |> LINK { PASSED.store.elements.my_button }

// Step 4: Used
store.elements.my_button.event.press  -- ✓ Valid
store.elements.my_button.event.hover  -- ✗ Error: element doesn't declare hover event
```

**Possible compiler errors:**
- ✗ "LINK declared at store.elements.X but never connected"
- ✗ "Element with LINK events not wired to store (did you forget |> LINK?)"
- ✗ "Accessing .event.press but element only declares [click: LINK]"
- ✗ "LINK target PASSED.store.elements.unknown_button doesn't exist in store"
- ✗ "Element connected to wrong path: declared as X, connected to Y"

**Benefits:**
- Catch wiring errors at compile time
- Verify event access is valid
- Detect unused LINKs
- Guide developers to fix issues

---

### 2. Syntactic Sugar for Common Pattern (Name-Based Convention)

When the function name matches the store path exactly, reduce repetition:

```boon
// Current (explicit):
new_todo_title_text_input()
    |> LINK { PASSED.store.elements.new_todo_title_text_input }

// Sugar (when names match exactly):
new_todo_title_text_input()
    |> LINK_AUTO[from: PASSED.store.elements]

// Expands to the explicit form at compile time
```

**Benefits:**
- ✅ Reduces repetition for the 80% case (single elements with matching names)
- ✅ Still explicit about which scope to link from
- ✅ Compiler verifies name match exists in scope
- ✅ Opt-in (explicit form always available)

**Limitations:**
- Only works for flat element hierarchies (not `filter_buttons.all`)
- Only works when names match exactly
- Doesn't work for per-instance wiring (like todos)

**Alternative syntax:**
```boon
// Destructured wiring for multiple elements
LINK_AUTO[from: PASSED.store.elements] {
    new_todo_title_text_input()
    toggle_all_checkbox()
    remove_completed_button()
}

// Expands to:
new_todo_title_text_input() |> LINK { PASSED.store.elements.new_todo_title_text_input }
toggle_all_checkbox() |> LINK { PASSED.store.elements.toggle_all_checkbox }
remove_completed_button() |> LINK { PASSED.store.elements.remove_completed_button }
```

---

### 3. Path Aliases for Reducing Repetition

Boon already supports local aliases - document this as the recommended pattern:

```boon
// Current (repetitive):
title_to_save: elements.new_todo_title_text_input.event.key_down.key |> WHEN { ... }
new_title: elements.new_todo_title_text_input.text |> Text/trim()

// Recommended (with alias):
input: PASSED.store.elements.new_todo_title_text_input
title_to_save: input.event.key_down.key |> WHEN { ... }
new_title: input.text |> Text/trim()
```

**This is already valid Boon!** Just document it as a best practice.

---

### 4. Visual Tooling / Documentation Generation

The LINK structure is **architectural metadata** that should be leveraged for tooling:

**Reactive graph visualization:**
```
store.elements.toggle_all_checkbox.event.click
    ├─> todo[0].completed (affects)
    ├─> todo[1].completed (affects)
    ├─> todo[2].completed (affects)
    └─> toggle_all_checkbox.checked (updates)

store.elements.new_todo_title_text_input.event.key_down.key
    ├─> store.title_to_save (when Enter)
    └─> store.todos (appends new todo)
```

**API documentation generation:**
```markdown
## Element: new_todo_title_text_input

**Location**: `store.elements.new_todo_title_text_input`

**Events**:
- `event.change` - Fires when text changes
  - `.text: String` - The current text value
- `event.key_down` - Fires when key is pressed
  - `.key: Key` - The key that was pressed

**State**:
- `.text: String` - Current input text

**Used by**:
- `store.title_to_save` (reacts to key_down.Enter)
- `new_todo_title_text_input()` (local change handler)
```

**Dependency analysis:**
```
store.elements.toggle_all_checkbox.event.click
  └─ Affects 3 downstream computations:
     - todos[*].completed (all todo items)
```

**Dead code detection:**
```
⚠ Warning: store.elements.remove_completed_button declared but never used
```

---

### 5. Convention Documentation

Document the LINK pattern as a standardized architecture with naming conventions:

**Recommended structure:**
```boon
store: [
    // Global singleton elements
    elements: [
        widget_name: LINK
        another_widget: LINK

        // Grouped elements (same lifecycle/purpose)
        filter_buttons: [
            all: LINK
            active: LINK
            completed: LINK
        ]
    ]
]

// Per-instance elements (for dynamic collections)
FUNCTION new_item(data) {
    [
        item_elements: [
            checkbox: LINK
            delete_button: LINK
        ]
        // ... other item data
    ]
}
```

**Naming conventions:**
- Store collection: `elements`, `item_elements`, `todo_elements`
- Element names: match function names when possible
- Grouped elements: use descriptive group names (filter_buttons, nav_items)

---

## Core Insight: LINK Is Architectural, Not Boilerplate

The three-step LINK pattern is analogous to other architectural patterns:

| Domain | Declare | Provide | Connect |
|--------|---------|---------|---------|
| **LINK** | `store: [X: LINK]` | `event: [E: LINK]` | `\|> LINK { store.X }` |
| **Electrical** | Socket on wall | Device with plug | Plug in cable |
| **Type system** | `var x: Type` | `function(): Type` | `x = function()` |
| **Networks** | Interface eth0 | Service on port 80 | Bind to interface |
| **Dependency Injection** | Declare dependency | Provide implementation | Wire together |

In each case, the three steps serve **distinct architectural purposes**:

1. **Architecture Declaration**: What exists in the system?
2. **Component Interface**: What does this component provide?
3. **Integration**: How are they connected?

You wouldn't say "declaring a variable and assigning it is boilerplate - let's just have implicit globals." The explicitness is the feature.

---

## Best Practices: The LINK Pattern

**LINK is Boon's explicit reactive architecture pattern.**

### Step 1 - Architecture (Store)
```boon
store: [elements: [widget_name: LINK]]  -- Document reactive topology
```

**Purpose**: Declare what reactive entities exist in your system.

**Guidelines:**
- Place at store root for global elements
- Place inside item constructors for per-instance elements
- Use descriptive names that match element function names
- Group related elements (filter_buttons, nav_items)

---

### Step 2 - Interface (Element)
```boon
element: [
    event: [action: LINK]  -- Advertise event capabilities
    hovered: LINK          -- Advertise state properties
]
```

**Purpose**: Declare what reactive streams and properties this element provides.

**Guidelines:**
- Declare all events that will be observed externally
- Declare all state that will be accessed remotely
- Keep declarations close to element creation
- Use semantic event names (press, click, change, blur)

---

### Step 3 - Wiring (Connection)
```boon
widget() |> LINK { PASSED.store.elements.widget_name }  -- Wire reactive channels
```

**Purpose**: Connect element's reactive streams to declared architectural slot.

**Guidelines:**
- Wire immediately at element instantiation
- Use explicit paths for clarity
- Consider local aliases for deeply nested paths
- Verify path matches declared LINK location

---

## Benefits of the LINK Pattern

### 📊 Self-Documenting Architecture
The store structure shows your reactive topology at a glance. New developers can understand what interactive elements exist without reading implementation.

### 🔍 Explicit Data Flow
Paths like `store.elements.button.event.press` make it obvious where data comes from. No hidden reactivity or magic subscriptions.

### ✅ Compile-Time Checkable
All three steps can be verified by the compiler:
- LINK declared? ✓
- Events advertised? ✓
- Wiring connected? ✓
- Access path valid? ✓

### 🔌 Multiple Consumers
Same LINK accessed locally (within element) and remotely (from store). No special multi-cast setup needed.

### 📦 Scales Naturally
Works for:
- Single global elements (new_todo_title_text_input)
- Grouped elements (filter_buttons.all)
- Dynamic collections (todos[*].todo_elements.checkbox)

### 🐛 Debuggable
- Grep for element name to find all usages
- Trace reactive flow through explicit paths
- Visualize reactive graph from LINK declarations
- Detect unused or unwired elements

### 🔧 Refactor-Friendly
- Rename element: compiler catches all references
- Move element: update one LINK path
- Remove element: compiler shows dangling references
- Add event: declare in interface, use anywhere

---

## Examples From TodoMVC

### Example 1: Global Singleton Element

**New todo input** (RUN.bn lines 36, 245-246, 270-298):

```boon
-- Step 1: Declare in store
store: [
    elements: [
        new_todo_title_text_input: LINK
    ]
]

-- Step 2: Element provides interface
FUNCTION new_todo_title_text_input() {
    Element/text_input(
        element: [
            event: [
                change: LINK
                key_down: LINK
            ]
        ]
        text: LATEST {
            ''
            element.event.change.text  -- Local access
        }
    )
}

-- Step 3: Wire at instantiation
new_todo_title_text_input()
    |> LINK { PASSED.store.elements.new_todo_title_text_input }

-- Step 4: Use from store (remote access)
store: [
    title_to_save: elements.new_todo_title_text_input.event.key_down.key |> WHEN {
        Enter => elements.new_todo_title_text_input.text |> Text/trim()
    }
]
```

**Both local (line 289) and remote (line 50) access the same reactive stream!**

---

### Example 2: Grouped Elements

**Filter buttons** (RUN.bn lines 29-32, 44-48, 586-592):

```boon
-- Step 1: Declare group in store
store: [
    elements: [
        filter_buttons: [
            all: LINK
            active: LINK
            completed: LINK
        ]
    ]
]

-- Step 2 & 3: Create and wire each button
filter_button(All) |> LINK { PASSED.store.elements.filter_buttons.all }
filter_button(Active) |> LINK { PASSED.store.elements.filter_buttons.active }
filter_button(Completed) |> LINK { PASSED.store.elements.filter_buttons.completed }

-- Step 4: Use button events for navigation
go_to_result: LATEST {
    filter_buttons.all.event.press |> THEN { '/' }
    filter_buttons.active.event.press |> THEN { '/active' }
    filter_buttons.completed.event.press |> THEN { '/completed' }
} |> Router/go_to()
```

**The hierarchical structure (filter_buttons.all) mirrors the conceptual grouping.**

---

### Example 3: Dynamic Collection (Per-Instance Elements)

**Todo items** (RUN.bn lines 90-95, 365-371):

```boon
-- Step 1: Declare in constructor
FUNCTION new_todo(title) {
    [
        todo_elements: [
            todo_checkbox: LINK
            todo_title_element: LINK
            remove_todo_button: LINK
            editing_todo_title_element: LINK
        ]
        completed: LATEST {
            False
            -- Observe global checkbox
            store.elements.toggle_all_checkbox.event.click |> THEN { ... }
        }
        -- Observe own checkbox
        |> Bool/toggle(when: todo_elements.todo_checkbox.event.click)
    ]
}

-- Step 2 & 3: Create and wire per instance
FUNCTION todo_element(todo) {
    Element/stripe(
        items: LIST {
            todo_checkbox(todo: todo)
                |> LINK { todo.todo_elements.todo_checkbox }

            todo_title_element(todo: todo)
                |> LINK { todo.todo_elements.todo_title_element }

            remove_todo_button()
                |> LINK { todo.todo_elements.remove_todo_button }
        }
    )
}

-- Step 4: Use per-instance events
todos: LIST {}
    |> List/retain(item, if: LATEST {
        True
        -- Each item observes its own remove button
        item.todo_elements.remove_todo_button.event.press
            |> THEN { False }
    })
```

**Each todo has completely independent reactive channels. The `item.todo_elements` gives access to that specific todo's elements.**

---

### Example 4: Cross-Element Coordination

**Toggle-all affects every todo** (RUN.bn lines 35, 110-118, 383-414):

```boon
-- Global toggle checkbox
store: [
    elements: [
        toggle_all_checkbox: LINK
    ]
]

-- Each todo observes the global checkbox
FUNCTION new_todo(title) {
    [
        completed: LATEST {
            False
            -- Observe global element from individual todo
            store.elements.toggle_all_checkbox.event.click |> THEN {
                store.todos
                    |> List/every(item, if: item.completed)
                    |> Bool/not()
            }
        }
        -- Also observe own checkbox
        |> Bool/toggle(when: todo_elements.todo_checkbox.event.click)
    ]
}
```

**The LINK makes the toggle checkbox's events accessible to every todo, enabling coordination through the reactive graph.**

---

## Comparison to Other Reactive Systems

### React (Implicit Event Flow)
```jsx
function TodoInput() {
  const [value, setValue] = useState('');

  // Where does handleSave come from? Props? Context?
  const handleKeyDown = (e) => {
    if (e.key === 'Enter') handleSave(value);
  };

  return <input value={value} onChange={e => setValue(e.target.value)}
                onKeyDown={handleKeyDown} />;
}
```

**Problems:**
- Implicit: Where does `handleSave` come from?
- Magic: `onChange` auto-wires to internal state
- No architecture doc: Can't see reactive topology

---

### Svelte (Store-based)
```svelte
<script>
  import { inputValue } from './stores.js';

  function handleKeyDown(e) {
    if (e.key === 'Enter') {
      // Do something with $inputValue
    }
  }
</script>

<input bind:value={$inputValue} on:keydown={handleKeyDown}>
```

**Problems:**
- Magic `$` syntax for store access
- `bind:` is implicit two-way binding
- Store structure not declared upfront

---

### Boon (Explicit LINK)
```boon
-- Architecture declared upfront
store: [elements: [new_todo_input: LINK]]

-- Element declares interface
Element/text_input(element: [event: [change: LINK, key_down: LINK]])

-- Explicit wiring
new_todo_input() |> LINK { PASSED.store.elements.new_todo_input }

-- Explicit usage
store.elements.new_todo_input.event.key_down.key |> WHEN { Enter => ... }
```

**Benefits:**
- ✅ Architecture visible upfront (store structure)
- ✅ Interface explicit (event: [change: LINK, ...])
- ✅ Wiring explicit (|> LINK { path })
- ✅ Usage explicit (full path)
- ✅ Compile-time verifiable (all paths)

---

## Conclusion

**LINK is not boilerplate - it's architectural clarity.**

The three-step pattern makes Boon's reactive dataflow:
- **Visible** (documented in store)
- **Explicit** (no magic subscriptions)
- **Verifiable** (compile-time checked)
- **Scalable** (works for singles and collections)
- **Powerful** (multiple consumers, cross-element coordination)

Don't try to eliminate the pattern - **embrace it** as a core architectural principle. The explicitness is the feature, not a bug to be optimized away.

---

**Related Documentation:**
- `../language/BOON_SYNTAX.md` - Core Boon syntax rules
- `playground/frontend/src/examples/todo_mvc_physical/docs/CODE_ANALYSIS_AND_IMPROVEMENTS.md` - TodoMVC code analysis

**Last Updated:** 2025-11-12
