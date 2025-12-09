# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Boon?

Boon is a reactive, dataflow-oriented programming language designed for building UIs, hardware descriptions, and durable state applications. It uses actors and streams for data flow, with constructs like `LATEST`, `WHEN`, `WHILE`, `THEN` for flow control, and `LINK` for event binding. Boon source files use the `.bn` extension.

## Project Structure

```
boon/
├── crates/boon/           # Core language implementation
│   └── src/
│       ├── parser/        # Lexer, parser, scope/persistence resolution
│       └── platform/
│           └── browser/   # Browser runtime (interpreter, evaluator, engine)
├── playground/            # Web playground (MoonZoon full-stack app)
│   ├── frontend/          # Zoon (Rust->WASM) + TypeScript (CodeMirror)
│   ├── backend/           # Moon server
│   └── shared/            # Shared types
└── tools/                 # Browser automation tools (standalone crate)
    ├── src/               # CLI + WebSocket server
    └── extension/         # Chrome extension for browser control
```

## Build Commands

### Running tests
```bash
cargo test -p boon
```

### Running the playground (development server)

**DO NOT** kill processes on port 8081 aggressively (e.g., `lsof -ti:8081 | xargs -r kill -9`).

Reasons:
1. This can kill the user's browser if it's using port 8081
2. MoonZoon (mzoon) supports **auto-reload** and **auto-compilation** - manual restarts are unnecessary
3. When you edit Rust files, mzoon will automatically recompile and hot-reload

**Start the playground using makers (correct way):**
```bash
cd playground && makers mzoon start &
```

This runs mzoon through the Makefile.toml configuration, which properly sets up the local mzoon binary.

**Alternative (if makers not available):**
```bash
cd playground && mzoon/bin/mzoon start &
```

Wait for compilation (1-2 minutes fresh, seconds incremental). Check if running:
```bash
curl -s http://localhost:8081 | head -5
```

**Stopping the playground (kill zombie processes on Linux):**
```bash
cd playground && makers kill
```

This is necessary because on Linux, process hierarchy auto-killing doesn't work properly, leaving zombie mzoon processes. The `kill` task gracefully terminates all mzoon-related processes and force-kills any that don't respond.

### TypeScript/CodeMirror (separate watcher)

MoonZoon does NOT auto-compile TypeScript. When editing TypeScript files:
```bash
cd playground/frontend/typescript/code_editor && ./node_modules/.bin/rolldown code_editor.ts --file ../bundles/code_editor.js --watch &
```

### Installing dependencies
```bash
cd playground && cargo make install
```

### Browser automation (boon-tools)

The `tools/` directory contains browser automation for testing and debugging. It uses a Chrome extension + WebSocket server architecture.

**Build boon-tools:**
```bash
cd tools && cargo build --release --target-dir ../target
```

**Quick start for debugging:**
```bash
# Terminal 1: Playground (if not already running)
cd playground && makers mzoon start &

# Terminal 2: WebSocket server with extension hot reload
cd tools && cargo run --release -- server start --watch ./extension

# Terminal 3: Load extension in Chrome (one-time manual setup)
# 1. Open chrome://extensions/
# 2. Enable "Developer mode"
# 3. Click "Load unpacked" → select tools/extension/
# 4. Navigate to http://localhost:8081

# Terminal 4: Execute commands
boon-tools exec status                    # Check connection
boon-tools exec inject "code here"        # Inject code into editor
boon-tools exec run                       # Trigger execution
boon-tools exec console                   # Get browser console logs
boon-tools exec preview                   # Get preview panel text
boon-tools exec screenshot -o test.png    # Capture page
```

See `tools/DEBUG_WITH_BROWSER.md` for full documentation.

## Architecture

### Parser (`crates/boon/src/parser/`)
- `lexer.rs` - Tokenizes Boon source code (chumsky-based)
- `parser.rs` - Parses tokens into AST with expression types: `Variable`, `Literal`, `List`, `Object`, `TaggedObject`, `Map`, `Function`, `FunctionCall`, `Latest`, `LatestWithState`, `Then`, `When`, `While`, `Pipe`, `Block`, `TextLiteral`, etc.
- `scope_resolver.rs` - Resolves variable references within scopes
- `persistence_resolver.rs` - Assigns persistence IDs for durable state
- `static_expression.rs` - Static expression evaluation

### Browser Platform (`crates/boon/src/platform/browser/`)
- `interpreter.rs` - Main entry point, runs Boon code with state persistence
- `evaluator.rs` - Evaluates AST expressions into runtime values
- `engine.rs` - Contains `VirtualFilesystem` for module loading, registry management
- `bridge.rs` - Converts Boon objects to Zoon UI elements
- `api.rs` - Built-in functions (Math, Element, Document, List, etc.)

### Playground Frontend (`playground/frontend/`)
- `main.rs` - Playground UI (editor + preview panels), file management, state persistence
- `code_editor.rs` - CodeMirror integration wrapper
- `typescript/code_editor/` - CodeMirror setup with Boon syntax highlighting
- `src/examples/` - Example `.bn` files (counter, interval, todo_mvc, etc.)

### Language Constructs
- `LATEST { a, b, c }` - Merge multiple reactive streams
- `initial |> HOLD state { body }` - Stateful accumulator with self-reference (single-arm)
- `input |> THEN { body }` - Copy data when input arrives
- `input |> WHEN { pattern => body }` - Pattern match and copy on input
- `input |> WHILE { pattern => body }` - Continuous data flow while pattern matches
- `LINK` / `LINK { alias }` - Event binding for DOM elements
- `PASS: value` / `PASSED` - Implicit context passing through function calls
- `BLOCK { vars, output }` - Local variable bindings
- `TEXT { content with {interpolation} }` - Text with variable interpolation

## Language Documentation

Most Boon language features are documented in `docs/language/`:
- `BOON_SYNTAX.md` - Overall syntax reference
- `LATEST.md`, `LATEST_COMPILER_RULES.md` - LATEST combinator details
- `WHEN_VS_WHILE.md` - Difference between WHEN and WHILE
- `LINK_PATTERN.md` - Event binding patterns
- `ERROR_HANDLING.md`, `FLUSH.md` - Error handling with FLUSH
- `TEXT_SYNTAX.md` - Text interpolation syntax
- `LIST.md`, `BITS.md`, `BYTES.md`, `MEMORY.md` - Data types
- `PULSES.md`, `SPREAD_OPERATOR.md` - Iteration and spread
- `storage/` - Durable state and persistence research
- `gpu/` - GPU/HVM research and analysis

## Debugging the Engine

### Common Issues: Actor Drop Problems

The reactive engine uses actors (ValueActors) that maintain subscriptions via channels. A common bug pattern is "receiver is gone" errors, which occur when:
- A ValueActor is dropped while subscriptions are still active
- Subscriber actors are dropped before all events are processed
- `extra_owned_data` doesn't properly keep dependent actors alive

### Debug Logging

In `crates/boon/src/platform/browser/engine.rs`, there's a debug flag:

```rust
const LOG_DROPS_AND_LOOP_ENDS: bool = false;  // Set to true to debug
```

When enabled, it prints:
- `"Dropped: {construct_info}"` - when a ValueActor/Variable is deallocated
- `"Loop ended {construct_info}"` - when a ValueActor's internal loop exits

This helps trace the lifecycle of actors and identify premature drops.

### Key Patterns to Watch

1. **`flat_map(|actor| actor.subscribe())`** - The closure drops `actor` after `subscribe()`. If no one else holds a reference, the actor's task is cancelled and subscription fails.

2. **`extra_owned_data`** - Used with `ValueActor::new_arc_with_extra_owned_data()` to keep data alive while the actor runs. Check if the right data is being preserved.

3. **`output_valve_signal`** - If this stream ends, the ValueActor's loop breaks (see line ~1522 in engine.rs).

### Debugging Best Practice: Simplify Test Cases

When debugging complex issues, **always create simplified test cases first** to isolate the problem:

1. **Don't debug the full complex example** - Start with the smallest code that reproduces the issue
2. **Isolate components one by one** - Test each piece (HOLD, Stream/pulses, TEXT, etc.) separately
3. **Compare working vs broken cases** - e.g., if `10 |> Stream/pulses() |> Document/new()` works but `result: 10 |> Stream/pulses()` followed by `TEXT { {result} }` doesn't, the issue is in the latter pattern
4. **Eliminate variables** - Remove functions, simplify data, use constants instead of expressions

Example: Instead of debugging a full fibonacci implementation, first test:
```boon
// Test 1: Does Stream/pulses work directly?
document: 5 |> Stream/pulses() |> Document/new()

// Test 2: Does assignment + reference work?
x: 5 |> Stream/pulses()
document: x |> Document/new()

// Test 3: Does TEXT interpolation work?
x: 5 |> Stream/pulses()
document: TEXT { Value: {x} } |> Document/new()
```

This approach saves hours of debugging by quickly pinpointing the exact failing component.

### Browser Automation Rules

When debugging with browser automation (`boon-tools exec`):

1. **Prefer console/preview over screenshots** - Use `exec console` and `exec preview` for debugging instead of screenshots. Screenshots should only be used when visual inspection is absolutely necessary.

2. **Never use `exec reload`** - it disconnects the extension. Use `exec refresh` instead.

3. **When "debugger already attached" error occurs**:
   - Run `exec detach` FIRST - this is mandatory before retrying
   - Then retry the original command
   - Do NOT use `exec reload`
   - Do NOT try non-CDP fallbacks - always resolve debugger issues first

4. **Always use CDP commands** - they emulate real human interaction:
   - CDP creates trusted events (`isTrusted: true`)
   - Results are consistent and reproducible
   - Never mix CDP and non-CDP approaches in the same workflow

4. **One browser instance**: Keep a single Chromium instance running. Don't kill it.

5. **Auto-reload**:
   - mzoon auto-reloads WASM when Rust changes
   - WebSocket server `--watch` auto-reloads extension when JS changes
   - No manual restarts needed for most changes

**Error Resolution Order**:
1. Debugger conflict → `exec detach` → retry
2. Page state issues → `exec refresh` → retry
3. Extension disconnected → refresh browser tab manually
4. Complete failure (last resort) → kill browser, restart

## Stream Lifecycle Safety

ValueActors require **infinite streams** (streams that never terminate). If a stream terminates, the actor's internal loop exits, dropping all sender channels and causing "receiver is gone" errors for active subscribers.

### The Problem with `stream::once()`

```rust
// BROKEN - stream terminates after emitting one value:
ValueActor::new(construct_info, actor_context, stream::once(async move { value }));
```

### The Solution: `constant()`

```rust
// CORRECT - stream emits once, then stays alive forever:
ValueActor::new(construct_info, actor_context, constant(value));
```

The `constant()` function (in `engine.rs`) creates a stream that emits one value, then hangs forever using `stream::pending()`:

```rust
pub fn constant<T>(item: T) -> TypedStream<impl Stream<Item = T>, Infinite> {
    TypedStream::infinite(stream::once(future::ready(item)).chain(stream::once(future::pending())))
}
```

### TypedStream System

The codebase provides compile-time markers for stream lifecycle:

- **`TypedStream<S, Infinite>`** - Stream that never terminates (safe for ValueActor)
- **`TypedStream<S, Finite>`** - Stream that will terminate (needs conversion)
- **`finite_stream.keep_alive()`** - Converts Finite to Infinite by chaining with `pending()`

While not fully enforced on `ValueActor::new()` yet, these types serve as documentation and can be gradually adopted.

### Safe Patterns

| Use Case | Pattern |
|----------|---------|
| Single constant value | `constant(value)` |
| Subscription stream | Already infinite (receiver never closes first) |
| `stream::unfold()` | Usually infinite if the closure never returns `None` |
| Finite stream from external source | `TypedStream::finite(stream).keep_alive()` |

## Key Dependencies
- **chumsky** - Parser combinator library (with pratt parsing for operators)
- **ariadne** - Error reporting
- **zoon/moon** - MoonZoon framework (frontend/backend)
- **ulid** - Unique IDs for persistence

## Engine Architecture Rules

### Parallel Processing
- **Only Actors (ValueActor) should be parallel processing units** in the Boon engine
- API functions should return **pure streams** that get wrapped into actors by the evaluator
- Never spawn `Task::start_droppable` outside of actor/engine infrastructure (e.g., not in api.rs stream functions)

### Interior Mutability
- **No `Rc<RefCell>`** in engine code - it fails in multi-threaded environments (WebWorkers)
- Actor-local state should be owned by the actor's async loop
- Use channels (mpsc) for communication between actors

### Stream Functions Pattern
API functions that return streams should:
1. Return `impl Stream<Item = Value>` (a pure stream)
2. Use `stream::iter()` for synchronous initial values, `stream::unfold()` for stateful iteration
3. Let the caller (evaluator) wrap the stream in a ValueActor
4. Keep input actors alive via the stream's closure (Arc<ValueActor> is Clone), NOT via Rc

### No Synchronous Operations (Async-Only Architecture)
- **Everything must be async** - Boon actors may live in different WebWorkers, clusters, or distributed systems
- **Never block or poll synchronously** - Use `.await` for all waiting, never spin-loops or blocking calls
- **No "sync processing" hacks** - Don't try to process initial values synchronously before returning a stream
- **Use channels for synchronous-like behavior** - If you need to ensure ordering or immediate processing, use mpsc channels and let the async runtime handle scheduling
- **Reason**: Sync operations assume single-threaded execution. Boon's actor model must work across threads, processes, and network boundaries

### Lazy vs Eager Evaluation (LazyValueActor)

**The Problem**: ValueActor eagerly polls its source stream in an internal loop, decoupling producers from consumers. This breaks sequential state updates in HOLD:

```boon
[count: 0] |> HOLD state {
    3 |> Stream/pulses() |> THEN { [count: state.count + 1] }
}
```

With eager evaluation, all 3 THEN bodies see `state.count = 0` because they're evaluated before HOLD updates state.

**The Solution**: LazyValueActor provides demand-driven evaluation:
- Only polls source stream when a subscriber requests a value
- Uses channels for subscriber ↔ actor communication
- Buffers values for multiple subscribers with cursor tracking

**When to Use Each Mode**:

| Context | Mode | Reason |
|---------|------|--------|
| HOLD body | **Lazy** | Sequential state updates |
| THEN/WHEN/WHILE body | Lazy | Only evaluate when needed |
| Top-level variables | Eager | Reactive updates |
| LATEST inputs | Eager | Must detect any change |
| Stream/interval | Eager | Time-driven |

**Implementation Details**:

1. **`use_lazy_actors` flag** in `ActorContext` - signals lazy evaluation mode
2. **`new_arc_lazy()`** constructor - creates ValueActor with lazy delegate
3. **`subscribe_boxed()`** method - returns lazy subscription if delegate exists
4. **HOLD sets `use_lazy_actors: true`** for body evaluation (evaluator.rs ~line 2756)

**Key Files**:
- `engine.rs`: `LazyValueActor` struct, `subscribe_boxed()` method
- `evaluator.rs`: `build_hold_actor()` uses `subscribe_boxed()` for body

**State Reference in HOLD Body**:
The `state` variable is a regular eager ValueActor - it reads current stored value. HOLD updates it after each lazy pull from body. This is why lazy body + eager state reference works correctly.

**HOLD Value Flow**: Initial value stored via `store_value_directly()`, then body values flow through `state_update_stream`. The `hold_state_update_callback` only updates `state_actor` and releases backpressure permit - it does NOT store to output (that's `state_update_stream`'s job). This separation ensures each value emits exactly once.
