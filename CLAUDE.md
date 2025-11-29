# Claude Code Guidelines for Boon

## Development Server (MoonZoon)

**DO NOT** kill processes on port 8081 aggressively (e.g., `lsof -ti:8081 | xargs -r kill -9`).

Reasons:
1. This can kill the user's browser if it's using port 8081
2. MoonZoon (mzoon) supports **auto-reload** and **auto-compilation** - manual restarts are unnecessary
3. When you edit Rust files, mzoon will automatically recompile and hot-reload

### Starting the playground

If the playground isn't running, start it with:
```bash
cd /home/martinkavik/repos/boon/playground && mzoon start &
```

Wait for compilation to complete (usually 1-2 minutes for fresh build, seconds for incremental).

### Checking if server is running

```bash
curl -s http://localhost:8081 | head -5
```

If it returns HTML, the server is running. No need to restart.

## TypeScript/CodeMirror (separate watcher)

MoonZoon does NOT auto-compile TypeScript. When editing TypeScript files (e.g., `boon-language.ts`), start the rolldown watcher:

```bash
cd /home/martinkavik/repos/boon/playground/frontend/typescript/code_editor && ./node_modules/.bin/rolldown code_editor.ts --file ../bundles/code_editor.js --watch &
```

This watches TypeScript files and rebuilds `bundles/code_editor.js` on changes.
