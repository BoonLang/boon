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
└── playground/            # Web playground (MoonZoon full-stack app)
    ├── frontend/          # Zoon (Rust->WASM) + TypeScript (CodeMirror)
    ├── backend/           # Moon server
    └── shared/            # Shared types
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

Start the playground:
```bash
cd playground && mzoon start &
```

Wait for compilation (1-2 minutes fresh, seconds incremental). Check if running:
```bash
curl -s http://localhost:8081 | head -5
```

### TypeScript/CodeMirror (separate watcher)

MoonZoon does NOT auto-compile TypeScript. When editing TypeScript files:
```bash
cd playground/frontend/typescript/code_editor && ./node_modules/.bin/rolldown code_editor.ts --file ../bundles/code_editor.js --watch &
```

### Installing dependencies
```bash
cd playground && cargo make install
```

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
- `initial |> LATEST state { body }` - Stateful transformation with self-reference
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

## Key Dependencies
- **chumsky** - Parser combinator library (with pratt parsing for operators)
- **ariadne** - Error reporting
- **zoon/moon** - MoonZoon framework (frontend/backend)
- **ulid** - Unique IDs for persistence
