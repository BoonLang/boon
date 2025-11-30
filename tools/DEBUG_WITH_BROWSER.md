# Debugging Boon Playground with Browser Automation

This document covers browser automation for testing and debugging the Boon Playground.

## Architecture

```
+----------------+      WebSocket      +------------------+                    +------------------+
|   boon-tools   | <----------------> |  Rust WebSocket  | <----------------> |  Chrome Extension|
|   CLI          |    localhost:9222  |  Server (tokio)  |                    |  (Manifest V3)   |
+----------------+                    +------------------+                    +------------------+
                                                                                      |
                                                                                      | DOM Access
                                                                                      v
                                                                             +------------------+
                                                                             |  Boon Playground |
                                                                             |  (localhost:8081)|
                                                                             +------------------+
```

The browser control stack consists of:
1. **boon-tools CLI** - Sends commands via WebSocket
2. **WebSocket Server** - Rust server using tokio-tungstenite, routes commands
3. **Chrome Extension** - Connects to server, executes commands in browser context
4. **Playground API** - `window.boonPlayground` JavaScript API exposed by the playground

## Quick Start

### Terminal 1: Start the Playground
```bash
cd playground && makers mzoon start
```
Wait for compilation (~1-2 minutes). Server runs on http://localhost:8081

### Terminal 2: Start the WebSocket Server with Hot Reload
```bash
cd tools && cargo run --release -- server start --port 9222 --watch ./extension
```
Server listens on ws://127.0.0.1:9222 and watches for extension file changes.

### Terminal 3: Open Browser and Load Extension

**Recommended: Use Chrome Canary** (can run alongside regular Chrome):
```bash
google-chrome-canary \
  --user-data-dir=/tmp/boon-canary \
  --no-first-run \
  --no-default-browser-check \
  http://localhost:8081
```

Then manually load the extension (one-time setup):
1. Open `chrome://extensions/` in Canary
2. Enable "Developer mode" (top right toggle)
3. Click "Load unpacked"
4. Select the `tools/extension/` directory
5. Navigate to http://localhost:8081

**Alternative: Regular Chrome with isolated profile:**
```bash
google-chrome \
  --user-data-dir=/tmp/boon-automation \
  --no-first-run \
  --no-default-browser-check \
  http://localhost:8081
```

**Note:** The `--load-extension` flag is often silently ignored by Chrome. Manual extension loading is more reliable.

**Chrome flags explained:**
- `--user-data-dir=/tmp/...` - Isolated profile, doesn't affect main browser
- `--no-first-run` - Skip Chrome's "Welcome" setup wizard
- `--no-default-browser-check` - Don't ask to be default browser

### Terminal 4: Execute Commands
```bash
# Check connection status
boon-tools exec status

# Inject and run code
boon-tools exec inject "document: Document/new(root: 123)"
boon-tools exec run

# Take screenshot
boon-tools exec screenshot --output test.png

# Get preview text
boon-tools exec preview

# Full test: inject, run, verify
boon-tools exec test "document: Document/new(root: 123)" --expect "123" --screenshot test.png
```

## Extension Installation

### Option 1: Unpacked Extension (Development)
1. Open Chrome and go to `chrome://extensions/`
2. Enable "Developer mode" (top right)
3. Click "Load unpacked"
4. Select the `tools/extension/` directory
5. Navigate to http://localhost:8081

### Option 2: Command Line with Isolated Profile
```bash
google-chrome --user-data-dir=/tmp/boon-automation \
              --load-extension=$(pwd)/tools/extension \
              http://localhost:8081
```

This creates an isolated Chrome profile that won't affect your main browser.

## CLI Commands Reference

### Server Commands
```bash
# Start WebSocket server
boon-tools server start --port 9222

# Start with extension hot reload (watches for file changes)
boon-tools server start --port 9222 --watch ./extension
```

### Exec Commands (via Extension)
```bash
# Check if extension is connected
boon-tools exec status

# Inject code into editor
boon-tools exec inject "code here"
boon-tools exec inject @filename.bn  # Read from file

# Trigger run (Shift+Enter equivalent)
boon-tools exec run

# Take screenshot
boon-tools exec screenshot --output screenshot.png

# Get preview panel text
boon-tools exec preview

# Click element by CSS selector
boon-tools exec click ".some-button"

# Type text into element
boon-tools exec type "input.search" "search text"

# Full test cycle
boon-tools exec test "code" --expect "expected text" --screenshot output.png

# Manually reload extension (triggers chrome.runtime.reload())
boon-tools exec reload

# Get console messages from browser
boon-tools exec console

# Scroll preview panel
boon-tools exec scroll --to-bottom          # Scroll to bottom
boon-tools exec scroll --y 100              # Scroll to absolute position
boon-tools exec scroll --delta 50           # Scroll by relative amount
```

## Playground JavaScript API

The playground exposes `window.boonPlayground` with these methods:

```javascript
// Check if API is ready
window.boonPlayground.isReady()  // returns boolean

// Set editor content
window.boonPlayground.setCode("document: Document/new(root: 123)")

// Get editor content
window.boonPlayground.getCode()  // returns string

// Trigger code execution
window.boonPlayground.run()

// Get preview panel text
window.boonPlayground.getPreview()  // returns string
```

You can test these directly in the browser console.

## Extension Hot Reload

The WebSocket server supports automatic extension reloading during development:

1. **Start server with `--watch`**: The server monitors the extension directory for changes
2. **On file change**: Server sends a `reload` command to the extension
3. **Extension reloads**: Calls `chrome.runtime.reload()` which restarts the service worker
4. **Playground refreshes**: The tab is also refreshed to ensure clean state

**How it works:**
- File watcher uses the `notify` crate with 500ms debouncing
- Only reacts to `.js`, `.json`, `.html`, `.css` file changes
- Extension sends keep-alive pings every 20s to prevent MV3 service worker sleep
- After reload, extension automatically reconnects to the WebSocket server

**Manual reload:**
```bash
boon-tools exec reload
```

## Why Chrome Extension Instead of CDP?

We tried CDP (Chrome DevTools Protocol) first, but it couldn't trigger Zoon's reactive system:
- Synthetic DOM events from CDP are ignored by Zoon
- CDP mouse/keyboard events don't trigger Zoon handlers
- wasm_bindgen closures setting Mutable don't trigger reactive updates

The Chrome Extension solution works because:
- Scripts execute in the actual page context (MAIN world)
- Can call `window.boonPlayground` which runs within Zoon's reactive system
- Native DOM operations are trusted by the browser

**Note:** CDP support was removed from boon-tools to simplify the codebase. All browser automation now goes through the extension.

## Troubleshooting

### Extension Not Connecting
1. Check if WebSocket server is running: `ps aux | grep boon-tools`
2. Check Chrome extension page for errors: `chrome://extensions/`
3. Reload extension and refresh playground page

### Server Logs
The WebSocket server prints connection status:
```
WebSocket server listening on ws://127.0.0.1:9222
Waiting for Chrome extension to connect...
Extension connected!
```

### Killing Zombie Processes
On Linux, mzoon processes may not terminate properly:
```bash
cd playground && makers kill
```

### Force Rebuild
If auto-reload isn't working:
```bash
cd playground && makers kill && makers mzoon start
```

## Protocol Reference

### Commands (CLI -> Extension)
```json
{ "id": 1, "command": { "type": "injectCode", "code": "..." } }
{ "id": 2, "command": { "type": "triggerRun" } }
{ "id": 3, "command": { "type": "screenshot" } }
{ "id": 4, "command": { "type": "getPreviewText" } }
{ "id": 5, "command": { "type": "click", "selector": ".btn" } }
{ "id": 6, "command": { "type": "type", "selector": "input", "text": "..." } }
{ "id": 7, "command": { "type": "ping" } }
{ "id": 8, "command": { "type": "getStatus" } }
{ "id": 9, "command": { "type": "reload" } }
{ "id": 10, "command": { "type": "getConsole" } }
{ "id": 11, "command": { "type": "scroll", "y": 100 } }
{ "id": 12, "command": { "type": "scroll", "delta": 50 } }
{ "id": 13, "command": { "type": "scroll", "toBottom": true } }
```

### Responses (Extension -> CLI)
```json
{ "id": 1, "response": { "type": "success", "data": null } }
{ "id": 2, "response": { "type": "error", "message": "..." } }
{ "id": 3, "response": { "type": "screenshot", "base64": "..." } }
{ "id": 4, "response": { "type": "previewText", "text": "..." } }
{ "id": 5, "response": { "type": "status", "connected": true, "pageUrl": "...", "apiReady": true } }
{ "id": 6, "response": { "type": "console", "messages": [{"level": "log", "text": "...", "timestamp": 123}] } }
```

---
*Last updated: 2025-11-30*
