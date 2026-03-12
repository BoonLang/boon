// Boon Browser Control - Background Service Worker
// Connects to WebSocket server and routes commands to content script

// Port convention: WS port = playground port + 1141
const WS_PORT_OFFSET = 1141;
const DEFAULT_PLAYGROUND_PORT = 8083;
const DEFAULT_WS_PORT = DEFAULT_PLAYGROUND_PORT + WS_PORT_OFFSET; // 9224

let ws = null;
let reconnectTimer = null;
let contentPort = null;
let pendingRequests = new Map();

// Exponential backoff for reconnection
let reconnectAttempts = 0;
const MAX_RECONNECT_DELAY = 30000; // 30 seconds max

// Dynamic port detection
let activePlaygroundPort = null;
let activeWsPort = null;

function extractPortFromUrl(url) {
  const match = url && url.match(/^http:\/\/localhost:(\d+)/);
  return match ? parseInt(match[1], 10) : null;
}

function isPlaygroundUrl(url) {
  return url && /^http:\/\/localhost:\d+/.test(url);
}

function deriveWsPort(playgroundPort) {
  return playgroundPort + WS_PORT_OFFSET;
}

// Get WS URL: check storage override, then derive from playground port, then default
async function getWsUrl() {
  // 1. Check chrome.storage for manual override
  try {
    const stored = await chrome.storage.local.get('wsPortOverride');
    if (stored.wsPortOverride) {
      const port = parseInt(stored.wsPortOverride, 10);
      if (!isNaN(port) && port > 0 && port < 65536) {
        return `ws://127.0.0.1:${port}`;
      }
    }
  } catch (e) {
    // storage not available, fall through
  }

  // 2. Derive from detected playground port
  if (activePlaygroundPort) {
    return `ws://127.0.0.1:${deriveWsPort(activePlaygroundPort)}`;
  }

  // 3. Fall back to default
  return `ws://127.0.0.1:${DEFAULT_WS_PORT}`;
}

// Restore persisted playground port (survives service worker restarts)
async function restorePersistedPort() {
  try {
    const stored = await chrome.storage.session.get('activePlaygroundPort');
    if (stored.activePlaygroundPort) {
      activePlaygroundPort = stored.activePlaygroundPort;
      activeWsPort = deriveWsPort(activePlaygroundPort);
      console.log(`[Boon] Restored persisted playground port: ${activePlaygroundPort}, WS: ${activeWsPort}`);
    }
  } catch (e) {
    // session storage not available
  }
}

async function persistPlaygroundPort(port) {
  activePlaygroundPort = port;
  activeWsPort = deriveWsPort(port);
  try {
    await chrome.storage.session.set({ activePlaygroundPort: port });
  } catch (e) {
    // session storage not available
  }
}

// ============ CDP INFRASTRUCTURE ============
// Chrome DevTools Protocol for trusted events (isTrusted: true)

let debuggerAttached = new Map(); // tabId -> boolean
let cdpConsoleMessages = new Map(); // tabId -> messages[]
let cachedPlaygroundTabId = null; // Cache tab ID for consistent targeting

async function attachDebugger(tabId) {
  if (debuggerAttached.get(tabId)) return;

  try {
    await chrome.debugger.attach({ tabId }, '1.3');
    debuggerAttached.set(tabId, true);
    console.log(`[Boon] CDP: Debugger attached to tab ${tabId}`);

    // Enable domains we need
    await chrome.debugger.sendCommand({ tabId }, 'DOM.enable');
    await chrome.debugger.sendCommand({ tabId }, 'Runtime.enable');
    await chrome.debugger.sendCommand({ tabId }, 'Page.enable');
  } catch (e) {
    if (e.message && e.message.includes('Another debugger is already attached')) {
      console.log('[Boon] CDP: Another debugger attached, trying to reuse...');
      // Mark as attached and try to use existing session
      debuggerAttached.set(tabId, true);
      try {
        await chrome.debugger.sendCommand({ tabId }, 'DOM.enable');
        await chrome.debugger.sendCommand({ tabId }, 'Runtime.enable');
        await chrome.debugger.sendCommand({ tabId }, 'Page.enable');
        console.log('[Boon] CDP: Reusing existing debugger session');
        return;
      } catch (e2) {
        debuggerAttached.delete(tabId);
        throw new Error('CDP debugger conflict. Run "boon-tools exec detach" or close Chrome DevTools.');
      }
    }
    console.error(`[Boon] CDP: Failed to attach debugger:`, e);
    throw e;
  }
}

// Handle debugger events (console messages and exceptions)
chrome.debugger.onEvent.addListener((source, method, params) => {
  if (method === 'Runtime.consoleAPICalled') {
    const messages = cdpConsoleMessages.get(source.tabId) || [];
    messages.push({
      level: params.type, // 'log', 'warn', 'error', etc.
      text: params.args.map(arg => arg.value || arg.description || '').join(' '),
      timestamp: Date.now()
    });
    if (messages.length > 2000) messages.shift();
    cdpConsoleMessages.set(source.tabId, messages);
  }
  // Capture uncaught exceptions (e.g., "Maximum call stack size exceeded")
  if (method === 'Runtime.exceptionThrown') {
    const messages = cdpConsoleMessages.get(source.tabId) || [];
    const exception = params.exceptionDetails;
    const text = exception.exception?.description ||
                 exception.text ||
                 'Unknown exception';
    messages.push({
      level: 'error',
      text: `[EXCEPTION] ${text}`,
      timestamp: Date.now()
    });
    if (messages.length > 2000) messages.shift();
    cdpConsoleMessages.set(source.tabId, messages);
  }
});

chrome.debugger.onDetach.addListener((source, reason) => {
  console.log(`[Boon] CDP: Debugger detached from tab ${source.tabId}, reason: ${reason}`);
  debuggerAttached.delete(source.tabId);
  cdpConsoleMessages.delete(source.tabId);
});

// Listen for tab updates (navigation, refresh) to clear stale debugger state
chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  // When a tab starts loading (navigation/refresh), the debugger session becomes invalid
  if (changeInfo.status === 'loading') {
    if (debuggerAttached.has(tabId)) {
      console.log(`[Boon] CDP: Tab ${tabId} navigating, clearing debugger state`);
      debuggerAttached.delete(tabId);
      cdpConsoleMessages.delete(tabId);
    }
  }
});

// Listen for tab removal to clear cached tab ID
chrome.tabs.onRemoved.addListener((tabId) => {
  if (tabId === cachedPlaygroundTabId) {
    console.log(`[Boon] Cached playground tab ${tabId} was closed`);
    cachedPlaygroundTabId = null;
  }
});

// ============ CDP OPERATIONS ============

// Click at viewport coordinates using real CDP mouse events (Input.dispatchMouseEvent).
// This mimics real user clicks: browser does hit-testing, generates the full event sequence
// (pointerdown → mousedown → pointerup → mouseup → click), and events are isTrusted: true.
// viewportX/viewportY are CSS pixel coordinates relative to the viewport.
async function cdpClickAtViewport(tabId, viewportX, viewportY) {
  await attachDebugger(tabId);

  // Move mouse to position first (compositor-level only — does NOT generate JS mouseenter/mouseleave)
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseMoved', x: viewportX, y: viewportY, button: 'none'
  });

  // mousePressed + mouseReleased = full click (browser generates the click event)
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mousePressed', x: viewportX, y: viewportY, button: 'left', clickCount: 1
  });
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseReleased', x: viewportX, y: viewportY, button: 'left', clickCount: 1
  });

  console.log(`[Boon] CDP: Real click at viewport (${viewportX}, ${viewportY})`);
}

// Click at page coordinates using real CDP mouse events.
// IMPORTANT: x,y are page coordinates (from DOM.getBoxModel). Converted to viewport internally.
async function cdpClickAt(tabId, x, y) {
  await attachDebugger(tabId);

  // Convert page coordinates to viewport coordinates
  const scrollOffset = await cdpEvaluate(tabId, `({ scrollX: window.scrollX, scrollY: window.scrollY })`);
  const viewportX = x - (scrollOffset?.scrollX || 0);
  const viewportY = y - (scrollOffset?.scrollY || 0);

  await cdpClickAtViewport(tabId, viewportX, viewportY);
  console.log(`[Boon] CDP: Real click at page (${x}, ${y}) -> viewport (${viewportX}, ${viewportY})`);
}

// Double-click at coordinates.
// Chromium's CDP clickCount-based sequence does not reliably produce a DOM
// `dblclick` for the Zoon label cells used by the playground, even though the
// hit target is correct. Dispatch the DOM double-click sequence directly on the
// resolved element instead.
async function cdpDoubleClickAt(tabId, x, y) {
  await attachDebugger(tabId);

  // Get scroll offset to convert page coords to viewport coords for CDP events
  const scrollOffset = await cdpEvaluate(tabId, `({ scrollX: window.scrollX, scrollY: window.scrollY })`);
  const viewportX = x - (scrollOffset?.scrollX || 0);
  const viewportY = y - (scrollOffset?.scrollY || 0);

  const result = await cdpEvaluate(tabId, `
    (function() {
      const x = ${JSON.stringify(viewportX)};
      const y = ${JSON.stringify(viewportY)};
      const target = document.elementFromPoint(x, y);
      if (!target) {
        return { ok: false, error: 'No element at target coordinates' };
      }

      if (typeof target.focus === 'function') {
        try { target.focus(); } catch (_) {}
      }

      const makeMouseEvent = (type, detail) => new MouseEvent(type, {
        bubbles: true,
        cancelable: true,
        composed: true,
        view: window,
        clientX: x,
        clientY: y,
        screenX: x,
        screenY: y,
        button: 0,
        buttons: type === 'mouseup' || type === 'click' || type === 'dblclick' ? 0 : 1,
        detail
      });

      const sequence = [
        ['mousedown', 1],
        ['mouseup', 1],
        ['click', 1],
        ['mousedown', 2],
        ['mouseup', 2],
        ['click', 2],
        ['dblclick', 2]
      ];

      for (const [type, detail] of sequence) {
        target.dispatchEvent(makeMouseEvent(type, detail));
      }

      const rect = target.getBoundingClientRect();
      return {
        ok: true,
        tag: target.tagName,
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom
      };
    })()
  `);

  if (!result?.ok) {
    throw new Error(result?.error || 'DOM double-click dispatch failed');
  }

  console.log(`[Boon] DOM dblclick at page (${x}, ${y}) -> viewport (${viewportX}, ${viewportY})`);
}

// Hover at coordinates using trusted CDP mouse movement only.
async function cdpHoverAt(tabId, x, y) {
  await attachDebugger(tabId);

  // Get scroll offset to convert page coords to viewport coords
  const scrollOffset = await cdpEvaluate(tabId, `({ scrollX: window.scrollX, scrollY: window.scrollY })`);
  const viewportX = x - (scrollOffset?.scrollX || 0);
  const viewportY = y - (scrollOffset?.scrollY || 0);

  // Move mouse to position via CDP (creates trusted mouse positioning)
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseMoved', x: viewportX, y: viewportY, button: 'none'
  });

  console.log(`[Boon] CDP: Trusted hover at (${x}, ${y})`);
}

// Get element bounding box via CDP
async function cdpGetElementBox(tabId, selector) {
  await attachDebugger(tabId);

  const { root } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getDocument');
  const { nodeId } = await chrome.debugger.sendCommand({ tabId }, 'DOM.querySelector', {
    nodeId: root.nodeId, selector
  });

  if (!nodeId) return null;

  const { model } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getBoxModel', { nodeId });
  const content = model.content; // [x1,y1, x2,y2, x3,y3, x4,y4]

  return {
    x: content[0],
    y: content[1],
    width: content[2] - content[0],
    height: content[5] - content[1],
    centerX: (content[0] + content[2]) / 2,
    centerY: (content[1] + content[5]) / 2
  };
}

// Click by selector (find element, then click at center)
async function cdpClickSelector(tabId, selector) {
  const box = await cdpGetElementBox(tabId, selector);
  if (!box) throw new Error(`Element not found: ${selector}`);

  await cdpClickAt(tabId, box.centerX, box.centerY);
  return { x: box.centerX, y: box.centerY };
}

// Type text character by character using CDP Input.dispatchKeyEvent
// This simulates real keyboard typing behavior - each key generates keyDown, char, keyUp events
async function cdpTypeTextCharByChar(tabId, text) {
  await attachDebugger(tabId);

  for (const char of text) {
    // Prefer direct DOM input for focused editable elements in the preview.
    // CDP key events alone do not reliably trigger `input` handlers for the
    // Zoon-managed text inputs used by the playground.
    const insertedViaDom = await chrome.debugger.sendCommand(
      { tabId },
      'Runtime.evaluate',
      {
        expression: `(function() {
          const preview = document.querySelector('[data-boon-panel="preview"]');
          let focused = document.activeElement;
          if ((!focused || focused === document.body) && preview) {
            focused =
              preview.querySelector(':focus') ||
              preview.querySelector('[data-boon-focused="true"]') ||
              preview.querySelector('[focused="true"]') ||
              preview.querySelector('[autofocus]');
          }
          if (!focused) return false;

          const text = ${JSON.stringify(char)};
          const isTextControl =
            focused instanceof HTMLInputElement ||
            focused instanceof HTMLTextAreaElement;

          if (isTextControl) {
            if (document.activeElement !== focused && typeof focused.focus === 'function') {
              focused.focus();
            }
            const start = focused.selectionStart ?? focused.value.length;
            const end = focused.selectionEnd ?? start;
            focused.value =
              focused.value.slice(0, start) +
              text +
              focused.value.slice(end);
            const caret = start + text.length;
            if (focused.setSelectionRange) {
              focused.setSelectionRange(caret, caret);
            }
            focused.dispatchEvent(
              new InputEvent('input', {
                bubbles: true,
                composed: true,
                data: text,
                inputType: 'insertText'
              })
            );
            return true;
          }

          if (focused.isContentEditable) {
            if (document.activeElement !== focused && typeof focused.focus === 'function') {
              focused.focus();
            }
            focused.textContent = (focused.textContent || '') + text;
            focused.dispatchEvent(
              new InputEvent('input', {
                bubbles: true,
                composed: true,
                data: text,
                inputType: 'insertText'
              })
            );
            return true;
          }

          return false;
        })()`,
        returnByValue: true
      }
    ).then(({ result }) => Boolean(result?.value)).catch(() => false);

    if (insertedViaDom) {
      continue;
    }

    // Get the correct key code for the character
    let keyCode;
    let codeStr;

    const upperCode = char.toUpperCase().charCodeAt(0);
    if (upperCode >= 65 && upperCode <= 90) {
      // Letters A-Z
      keyCode = upperCode;
      codeStr = `Key${char.toUpperCase()}`;
    } else if (char === ' ') {
      // Space key
      keyCode = 32;
      codeStr = 'Space';
    } else if (char >= '0' && char <= '9') {
      // Digits 0-9
      keyCode = char.charCodeAt(0);
      codeStr = `Digit${char}`;
    } else {
      // Other characters (punctuation, etc.)
      keyCode = char.charCodeAt(0);
      codeStr = ''; // Many punctuation keys have complex codes, but char event handles them
    }

    // keyDown - don't include text, just key info
    await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', {
      type: 'keyDown',
      key: char,
      code: codeStr,
      windowsVirtualKeyCode: keyCode,
      nativeVirtualKeyCode: keyCode
    });

    // char event - this is what actually inserts the character
    await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', {
      type: 'char',
      text: char
    });

    // keyUp
    await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', {
      type: 'keyUp',
      key: char,
      code: codeStr,
      windowsVirtualKeyCode: keyCode,
      nativeVirtualKeyCode: keyCode
    });
  }
}

// Press special key (Enter, Tab, Escape, etc.) using CDP Input.dispatchKeyEvent
// NOTE: This may not trigger JavaScript event listeners attached via web_sys
async function cdpPressKey(tabId, key, modifiers = 0, retryCount = 0) {
  const withTimeout = (promise, label) => Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timeout`)), 5000))
  ]);

  try {
    await withTimeout(attachDebugger(tabId), 'attachDebugger');

    const keyMap = {
      'Enter': { key: 'Enter', code: 'Enter', keyCode: 13, windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 },
      'Tab': { key: 'Tab', code: 'Tab', keyCode: 9, windowsVirtualKeyCode: 9, nativeVirtualKeyCode: 9 },
      'Escape': { key: 'Escape', code: 'Escape', keyCode: 27, windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27 },
      'Backspace': { key: 'Backspace', code: 'Backspace', keyCode: 8, windowsVirtualKeyCode: 8, nativeVirtualKeyCode: 8 },
      'Delete': { key: 'Delete', code: 'Delete', keyCode: 46, windowsVirtualKeyCode: 46, nativeVirtualKeyCode: 46 },
      'End': { key: 'End', code: 'End', keyCode: 35, windowsVirtualKeyCode: 35, nativeVirtualKeyCode: 35 },
      'Home': { key: 'Home', code: 'Home', keyCode: 36, windowsVirtualKeyCode: 36, nativeVirtualKeyCode: 36 },
    };

    let keyInfo = keyMap[key];
    if (!keyInfo) {
      if (/^[a-zA-Z]$/.test(key)) {
        const upper = key.toUpperCase();
        const keyCode = upper.charCodeAt(0);
        keyInfo = {
          key,
          code: `Key${upper}`,
          keyCode,
          windowsVirtualKeyCode: keyCode,
          nativeVirtualKeyCode: keyCode,
        };
      } else if (/^[0-9]$/.test(key)) {
        const keyCode = key.charCodeAt(0);
        keyInfo = {
          key,
          code: `Digit${key}`,
          keyCode,
          windowsVirtualKeyCode: keyCode,
          nativeVirtualKeyCode: keyCode,
        };
      } else {
        keyInfo = { key, code: key, keyCode: 0, windowsVirtualKeyCode: 0, nativeVirtualKeyCode: 0 };
      }
    }

    const keyDownEvent = { type: 'keyDown', ...keyInfo, modifiers };
    if (keyInfo.key === 'Enter') {
      keyDownEvent.text = '\r';
      keyDownEvent.unmodifiedText = '\r';
    }

    await withTimeout(
      chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', keyDownEvent),
      'Input.dispatchKeyEvent(keyDown)'
    );
    await withTimeout(
      chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', {
        type: 'keyUp', ...keyInfo, modifiers
      }),
      'Input.dispatchKeyEvent(keyUp)'
    );
  } catch (e) {
    if (retryCount === 0 && (
      e.message?.includes('timeout') ||
      e.message?.includes('Target closed') ||
      e.message?.includes('Cannot find context') ||
      e.message?.includes('Debugger is not attached') ||
      e.message?.includes('No tab with given id')
    )) {
      console.log(`[Boon] CDP: Key press session issue for ${key}, retrying once...`);
      debuggerAttached.delete(tabId);
      try {
        await chrome.debugger.detach({ tabId });
      } catch (_) {}
      return cdpPressKey(tabId, key, modifiers, retryCount + 1);
    }
    throw e;
  }
}

// Keyboard shortcut (Ctrl+A, Ctrl+V, etc.)
async function cdpKeyboardShortcut(tabId, key, ctrl = false, shift = false, alt = false) {
  let modifiers = 0;
  if (ctrl) modifiers |= 2;
  if (shift) modifiers |= 8;
  if (alt) modifiers |= 1;

  await cdpPressKey(tabId, key, modifiers);
}

// Screenshot via CDP
async function cdpScreenshot(tabId) {
  await attachDebugger(tabId);

  const { data } = await chrome.debugger.sendCommand({ tabId }, 'Page.captureScreenshot', {
    format: 'png'
  });
  return data; // base64 encoded
}

// Scroll via mouse wheel
async function cdpScroll(tabId, x, y, deltaX = 0, deltaY = 0) {
  await attachDebugger(tabId);

  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseWheel', x, y, deltaX, deltaY
  });
}

// Get console messages (captured via Runtime.consoleAPICalled)
function cdpGetConsole(tabId) {
  return cdpConsoleMessages.get(tabId) || [];
}

// Execute JavaScript via CDP (only when CDP doesn't have equivalent)
// Includes retry logic for stale sessions
async function cdpEvaluate(tabId, expression, retryCount = 0) {
  await attachDebugger(tabId);

  try {
    const { result, exceptionDetails } = await chrome.debugger.sendCommand(
      { tabId }, 'Runtime.evaluate', { expression, returnByValue: true }
    );

    if (exceptionDetails) {
      throw new Error(exceptionDetails.exception?.description || 'Evaluation failed');
    }

    return result.value;
  } catch (e) {
    // Handle stale debugger session - clear state and retry once
    if (retryCount === 0 && (
      e.message?.includes('Target closed') ||
      e.message?.includes('Cannot find context') ||
      e.message?.includes('Debugger is not attached') ||
      e.message?.includes('No tab with given id')
    )) {
      console.log(`[Boon] CDP: Stale session detected, re-attaching...`);
      debuggerAttached.delete(tabId);
      return cdpEvaluate(tabId, expression, retryCount + 1);
    }
    throw e;
  }
}

// Focus element via CDP
async function cdpFocusElement(tabId, selector) {
  await attachDebugger(tabId);

  const { root } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getDocument');
  const { nodeId } = await chrome.debugger.sendCommand({ tabId }, 'DOM.querySelector', {
    nodeId: root.nodeId, selector
  });

  if (!nodeId) throw new Error(`Element not found: ${selector}`);

  await chrome.debugger.sendCommand({ tabId }, 'DOM.focus', { nodeId });
}

// Get all elements matching selector with their boxes
async function cdpQuerySelectorAll(tabId, selector) {
  await attachDebugger(tabId);

  // Force a fresh DOM tree by requesting depth: -1 (full tree)
  // This prevents stale cached DOM issues when the page has changed
  const { root } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getDocument', { depth: -1 });
  const { nodeIds } = await chrome.debugger.sendCommand({ tabId }, 'DOM.querySelectorAll', {
    nodeId: root.nodeId, selector
  });

  const elements = [];
  for (const nodeId of nodeIds) {
    try {
      const { model } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getBoxModel', { nodeId });
      const content = model.content;

      // Get node info - use full outerHTML to include text content
      const { outerHTML } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getOuterHTML', { nodeId });

      // Extract text content from HTML (strip tags)
      const text = outerHTML.replace(/<[^>]*>/g, '').trim();

      elements.push({
        nodeId,
        x: content[0],
        y: content[1],
        width: content[2] - content[0],
        height: content[5] - content[1],
        centerX: (content[0] + content[2]) / 2,
        centerY: (content[1] + content[5]) / 2,
        html: outerHTML.substring(0, 200),
        text: text.substring(0, 100)
      });
    } catch (e) {
      // Element might be invisible or have no layout
    }
  }

  return elements;
}

// Safe send that handles errors
function safeSend(message) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    try {
      ws.send(typeof message === 'string' ? message : JSON.stringify(message));
      return true;
    } catch (e) {
      console.error('[Boon] Send failed:', e);
      scheduleReconnect();
      return false;
    }
  }
  return false;
}

// Connect to WebSocket server
async function connect() {
  // Check both CONNECTING and OPEN states to avoid race conditions
  if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) {
    return;
  }

  const wsUrl = await getWsUrl();
  console.log(`[Boon] Connecting to WebSocket server at ${wsUrl}...`);

  try {
    ws = new WebSocket(wsUrl);
  } catch (e) {
    console.error('[Boon] WebSocket constructor error:', e);
    scheduleReconnect();
    return;
  }

  ws.onopen = () => {
    console.log('[Boon] Connected to WebSocket server');
    // Reset reconnect attempts on successful connection
    reconnectAttempts = 0;
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    // Identify as extension to the server
    safeSend({ clientType: 'extension' });
  };

  ws.onclose = () => {
    console.log('[Boon] WebSocket connection closed');
    ws = null;
    scheduleReconnect();
  };

  ws.onerror = (error) => {
    console.error('[Boon] WebSocket error:', error);
  };

  ws.onmessage = async (event) => {
    try {
      const request = JSON.parse(event.data);
      console.log('[Boon] Received request:', request);

      const response = await handleCommand(request.id, request.command);

      // Some commands (like reload) return null to indicate no response needed
      if (response !== null) {
        safeSend({
          id: request.id,
          response: response
        });
        console.log('[Boon] Sent response:', response);
      }
    } catch (e) {
      console.error('[Boon] Error handling message:', e);
    }
  };
}

function scheduleReconnect() {
  if (reconnectTimer) return;

  // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s (max)
  const delay = Math.min(1000 * Math.pow(2, reconnectAttempts), MAX_RECONNECT_DELAY);
  reconnectAttempts++;

  console.log(`[Boon] Scheduling reconnect in ${delay}ms (attempt ${reconnectAttempts})`);

  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, delay);
}

// Handle incoming commands
async function handleCommand(id, command) {
  const type = command.type;

  try {
    // Get the active tab with any localhost playground
    // Use cached tab ID if valid, otherwise find and cache a new one
    let tab = null;

    if (cachedPlaygroundTabId !== null) {
      try {
        tab = await chrome.tabs.get(cachedPlaygroundTabId);
        if (!isPlaygroundUrl(tab.url)) {
          console.log('[Boon] Cached tab no longer on playground, finding new tab');
          cachedPlaygroundTabId = null;
          tab = null;
        }
      } catch (e) {
        // Tab no longer exists
        console.log('[Boon] Cached tab no longer exists, finding new tab');
        cachedPlaygroundTabId = null;
        tab = null;
      }
    }

    if (tab === null) {
      const tabs = await chrome.tabs.query({ url: 'http://localhost/*' });
      if (tabs.length === 0) {
        return { type: 'error', message: 'No Boon Playground tab found on localhost' };
      }
      // Prefer the most recently accessed tab, or the active one
      tab = tabs.find(t => t.active) || tabs.sort((a, b) => (b.lastAccessed || 0) - (a.lastAccessed || 0))[0];
      cachedPlaygroundTabId = tab.id;

      // Update active playground port for WS derivation
      const detectedPort = extractPortFromUrl(tab.url);
      if (detectedPort && detectedPort !== activePlaygroundPort) {
        console.log(`[Boon] Detected playground port: ${detectedPort}, WS port: ${deriveWsPort(detectedPort)}`);
        await persistPlaygroundPort(detectedPort);
        // Reconnect WS if port changed
        if (ws) { ws.close(); ws = null; }
        connect();
      }

      console.log(`[Boon] Selected playground tab ${tab.id} (${tabs.length} tabs found)`);
    }

    switch (type) {
      case 'ping':
        return { type: 'pong' };

      case 'getStatus':
        return {
          type: 'status',
          connected: true,
          pageUrl: tab.url,
          apiReady: await checkApiReady(tab.id)
        };

      // ============ CDP-BASED COMMANDS (trusted events) ============

      case 'click':
        // Use CDP for trusted click events
        try {
          const clickPos = await cdpClickSelector(tab.id, command.selector);
          return { type: 'success', data: { ...clickPos, method: 'cdp' } };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'clickAt':
        // Use CDP for trusted click at coordinates
        await cdpClickAt(tab.id, command.x, command.y);
        return { type: 'success', data: { x: command.x, y: command.y, method: 'cdp' } };

      case 'hoverAt':
        // Use CDP to move mouse without clicking (trigger hover)
        await cdpHoverAt(tab.id, command.x, command.y);
        return { type: 'success', data: { x: command.x, y: command.y, method: 'cdp' } };

      case 'doubleClickAt':
        // Use CDP for trusted double-click at coordinates
        await cdpDoubleClickAt(tab.id, command.x, command.y);
        return { type: 'success', data: { x: command.x, y: command.y, method: 'cdp' } };

      case 'type':
        // Use trusted keyboard-like CDP events only.
        try {
          await cdpFocusElement(tab.id, command.selector);
          await cdpKeyboardShortcut(tab.id, 'a', true); // Ctrl+A to select all
          await cdpPressKey(tab.id, 'Backspace');
          await cdpTypeTextCharByChar(tab.id, command.text);
          return { type: 'success', data: null };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'key':
        // Press a special key (Enter, Tab, Escape, etc.)
        // Also supports modifier combinations like "Ctrl+A", "Shift+Tab", "Ctrl+Shift+Z"
        try {
          const keyStr = command.key;

          // Parse modifier keys
          const hasCtrl = keyStr.includes('Ctrl+');
          const hasShift = keyStr.includes('Shift+');
          const hasAlt = keyStr.includes('Alt+');

          // Extract the actual key (last part after modifiers)
          const actualKey = keyStr.replace(/Ctrl\+/g, '').replace(/Shift\+/g, '').replace(/Alt\+/g, '');

          if (hasCtrl || hasShift || hasAlt) {
            // Use CDP keyboard shortcut for modifier combinations
            await cdpKeyboardShortcut(tab.id, actualKey.toLowerCase(), hasCtrl, hasShift, hasAlt);
            return { type: 'success', data: { key: actualKey, ctrl: hasCtrl, shift: hasShift, alt: hasAlt, method: 'cdp' } };
          } else {
            await cdpPressKey(tab.id, actualKey);
            return { type: 'success', data: { key: actualKey, method: 'cdp' } };
          }
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'screenshot':
        // Use CDP for screenshot
        try {
          const base64 = await cdpScreenshot(tab.id);
          return { type: 'screenshot', base64 };
        } catch (e) {
          return { type: 'error', message: `Screenshot failed: ${e.message}` };
        }

      case 'getConsole':
        // Use CDP console capture (automatic via Runtime.consoleAPICalled)
        return { type: 'console', messages: cdpGetConsole(tab.id) };

      case 'setupConsole':
        // CDP handles console capture automatically when attached
        await attachDebugger(tab.id);
        return { type: 'success', data: 'CDP console capture enabled' };

      case 'scroll':
        // Use CDP mouse wheel for scrolling
        try {
          const previewBox = await cdpGetElementBox(tab.id, '[data-boon-panel="preview"]');
          if (!previewBox) return { type: 'error', message: 'Preview panel not found' };
          const deltaY = command.toBottom ? 10000 : (command.delta || command.y || 0);
          await cdpScroll(tab.id, previewBox.centerX, previewBox.centerY, 0, deltaY);
          return { type: 'success', data: null };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      // ============ CDP Runtime.evaluate for playground API ============

      case 'injectCode':
        // Use CDP Runtime.evaluate (still CDP, minimal JS injection)
        // If filename is provided, set it first so code is saved under the correct name
        try {
          if (command.filename) {
            await cdpEvaluate(tab.id, `window.boonPlayground.setCurrentFile(${JSON.stringify(command.filename)})`);
          }
          await cdpEvaluate(tab.id, `window.boonPlayground.setCode(${JSON.stringify(command.code)})`);
          return { type: 'success', data: null };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'triggerRun':
        // Use CDP Runtime.evaluate for playground API
        try {
          await cdpEvaluate(tab.id, 'window.boonPlayground.run()');
          return { type: 'success', data: null };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'format':
        // Use CDP Runtime.evaluate for playground API
        try {
          await cdpEvaluate(tab.id, 'window.boonPlayground.format()');
          return { type: 'success', data: null };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'getPreviewText':
        // Use CDP Runtime.evaluate
        try {
          const text = await cdpEvaluate(tab.id,
            `document.querySelector('[data-boon-panel="preview"]')?.textContent || ''`);
          return { type: 'previewText', text };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'runAndCaptureInitial':
        // ATOMIC: Run code and capture initial preview BEFORE any async events (timers) fire
        // This is critical for testing initial state before timer-based updates
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              // Run the code synchronously
              if (typeof window.boonPlayground === 'undefined' || !window.boonPlayground.run) {
                return { error: 'boonPlayground API not available' };
              }

              // Trigger run - this compiles and renders synchronously
              window.boonPlayground.run();

              // Immediately capture the preview BEFORE any setTimeout/timers fire
              // JavaScript event loop guarantees sync code completes before async callbacks
              const preview = document.querySelector('[data-boon-panel="preview"]');
              const initialText = preview ? preview.textContent || '' : '';

              return {
                success: true,
                initialPreview: initialText,
                timestamp: Date.now()
              };
            })()
          `);

          if (result.error) {
            return { type: 'error', message: result.error };
          }
          return { type: 'runAndCaptureInitial', ...result };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'getPreviewElements':
        // Use CDP to get ALL visible elements with bounding boxes (like raybox approach)
        try {
          const elements = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { elements: [], error: 'Preview panel not found' };

              const allElements = preview.querySelectorAll('*');
              const results = [];

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                // Skip invisible elements
                if (rect.width === 0 || rect.height === 0) return;

                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                // Get direct text content (not from children)
                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                // Get full text content
                const fullText = (el.textContent || '').trim().substring(0, 100);

                // Determine element type
                const role = el.getAttribute('role');
                const tagName = el.tagName.toLowerCase();
                let elementType = tagName;
                if (role) elementType = role;
                if (tagName === 'input') elementType = 'input:' + (el.type || 'text');

                results.push({
                  tagName,
                  role,
                  elementType,
                  x: Math.round(rect.x),
                  y: Math.round(rect.y),
                  width: Math.round(rect.width),
                  height: Math.round(rect.height),
                  centerX: Math.round(rect.x + rect.width / 2),
                  centerY: Math.round(rect.y + rect.height / 2),
                  directText,
                  fullText,
                  className: el.className || ''
                });
              });

              return { elements: results };
            })()
          `);
          return { type: 'previewElements', data: elements };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'clearStates':
        // Click the "Clear saved states" button using trusted pointer events.
        try {
          const button = await cdpEvaluate(tab.id, `
            (function() {
              const normalize = (text) => (text || '').replace(/\\s+/g, ' ').trim().toLowerCase();
              const candidates = Array.from(document.querySelectorAll('button, [role="button"], [data-action="clear-states"]'));
              const target = candidates.find((el) => {
                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return false;
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return false;
                const text = normalize(el.textContent);
                return text === 'clear saved states' || text.includes('clear saved states');
              });
              if (!target) return { found: false };
              const rect = target.getBoundingClientRect();
              return {
                found: true,
                text: (target.textContent || '').trim(),
                x: Math.round(rect.x + rect.width / 2),
                y: Math.round(rect.y + rect.height / 2)
              };
            })()
          `);

          if (button && button.found) {
            await cdpClickAtViewport(tab.id, button.x, button.y);
            return {
              type: 'success',
              data: {
                method: 'cdp-click',
                text: button.text || 'Clear saved states',
              },
            };
          }

          // Fallback: if button not found, clear localStorage directly.
          // This is less user-like but keeps recovery behavior available.
          const fallbackResult = await cdpEvaluate(tab.id, `
              (function() {
                const preserveKeys = ['boon-playground-engine-type'];
                const preserved = {};
                for (const key of preserveKeys) {
                  const val = localStorage.getItem(key);
                  if (val !== null) preserved[key] = val;
                }
                const keyCount = localStorage.length;
                localStorage.clear();
                for (const [key, val] of Object.entries(preserved)) {
                  localStorage.setItem(key, val);
                }
                return { cleared: keyCount, preserved: Object.keys(preserved), warning: 'Button not found, timers may not be invalidated' };
              })()
            `);
          return { type: 'success', data: { method: 'fallback-clear', ...fallbackResult } };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'navigateTo':
        // Navigate to a specific route using history.pushState and trigger popstate
        // This is essential for resetting Router state in tests
        try {
          const path = command.path;
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const path = '${path}';
              // Use history.pushState to change URL without reload
              history.pushState(null, '', path);
              // Dispatch popstate event so Router/route() picks up the change
              window.dispatchEvent(new PopStateEvent('popstate', { state: null }));
              return { navigated: path, currentPath: location.pathname };
            })()
          `);
          return { type: 'success', data: result };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'selectExample':
        // Call the WASM-exported selectExample(name) which does the same as clicking the example tab
        try {
          const exampleName = command.name.replace('.bn', '');
          const found = await cdpEvaluate(tab.id, `window.boonPlayground.selectExample(${JSON.stringify(exampleName)})`);
          if (!found) {
            return { type: 'error', message: `Example '${exampleName}' not found` };
          }
          return { type: 'success', data: { v: 2, selected: exampleName + '.bn' } };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'getEditorCode':
        // Get current editor code via boonPlayground API
        try {
          const code = await cdpEvaluate(tab.id, `window.boonPlayground.getCode()`);
          return { type: 'editorCode', code: code || '' };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'screenshotElement':
        // Take screenshot of a specific element by clipping to its bounds
        // Note: Screenshot will be at device pixel ratio. Use ImageMagick to resize if needed.
        try {
          const box = await cdpGetElementBox(tab.id, command.selector);
          if (!box) return { type: 'error', message: `Element not found: ${command.selector}` };

          await attachDebugger(tab.id);
          const { data } = await chrome.debugger.sendCommand({ tabId: tab.id }, 'Page.captureScreenshot', {
            format: 'png',
            clip: {
              x: box.x,
              y: box.y,
              width: box.width,
              height: box.height,
              scale: 1
            }
          });
          return { type: 'screenshot', base64: data };
        } catch (e) {
          return { type: 'error', message: `Element screenshot failed: ${e.message}` };
        }

      case 'getAccessibilityTree':
        // Get accessibility tree of preview pane via CDP Accessibility domain
        try {
          await attachDebugger(tab.id);
          await chrome.debugger.sendCommand({ tabId: tab.id }, 'Accessibility.enable');

          // Get the preview pane node first
          const { root } = await chrome.debugger.sendCommand({ tabId: tab.id }, 'DOM.getDocument');
          const { nodeId } = await chrome.debugger.sendCommand({ tabId: tab.id }, 'DOM.querySelector', {
            nodeId: root.nodeId,
            selector: '[data-boon-panel="preview"]'
          });

          if (!nodeId) {
            return { type: 'error', message: 'Preview pane not found' };
          }

          // Get accessibility tree for this node
          const { nodes } = await chrome.debugger.sendCommand({ tabId: tab.id }, 'Accessibility.getPartialAXTree', {
            nodeId: nodeId,
            fetchRelatives: true
          });

          // Format the tree nicely
          const formattedNodes = nodes.map(node => ({
            role: node.role?.value,
            name: node.name?.value,
            value: node.value?.value,
            description: node.description?.value,
            children: node.childIds?.length || 0
          })).filter(n => n.role || n.name);

          return { type: 'accessibilityTree', tree: formattedNodes };
        } catch (e) {
          return { type: 'error', message: `Accessibility tree failed: ${e.message}` };
        }

      case 'clickCheckbox':
        // Click a checkbox by index (0-indexed) in the preview pane
        // Boon/Zoon checkboxes are identified by:
        // 1. [role="checkbox"] attribute (standard)
        // 2. id starting with "cb-" (Boon bridge convention from bridge_v2.rs)
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { error: 'Preview panel not found' };

              // Find checkboxes by multiple methods:
              // 1. Standard role="checkbox"
              // 2. Boon's cb-* ID convention (bridge_v2.rs assigns id="cb-{slot_index}")
              const roleCheckboxes = Array.from(preview.querySelectorAll('[role="checkbox"]'));
              const idCheckboxes = Array.from(preview.querySelectorAll('[id^="cb-"]'));

              // Merge and dedupe (prefer elements found by both methods)
              const seen = new Set();
              const allCheckboxes = [];

              // First add role-based checkboxes (they're most reliable)
              roleCheckboxes.forEach(el => {
                seen.add(el);
                allCheckboxes.push(el);
              });

              // Then add id-based checkboxes not already found
              idCheckboxes.forEach(el => {
                if (!seen.has(el)) {
                  seen.add(el);
                  allCheckboxes.push(el);
                }
              });

              // Sort by vertical position (top to bottom), then horizontal (left to right) as tiebreaker
              allCheckboxes.sort((a, b) => {
                const rectA = a.getBoundingClientRect();
                const rectB = b.getBoundingClientRect();
                const dy = rectA.top - rectB.top;
                if (Math.abs(dy) > 2) return dy;  // 2px threshold for "same row"
                return rectA.left - rectB.left;
              });

              const results = [];

              // Log all found checkboxes for debugging
              console.log('[Boon Debug] Found', allCheckboxes.length, 'checkboxes (role:', roleCheckboxes.length, ', id:', idCheckboxes.length, ')');

              allCheckboxes.forEach((el, i) => {
                const rect = el.getBoundingClientRect();
                const style = window.getComputedStyle(el);

                // Log each checkbox details
                console.log('[Boon Debug] Checkbox', i, ':', {
                  id: el.id,
                  role: el.getAttribute('role'),
                  rect: { x: rect.x, y: rect.y, w: rect.width, h: rect.height },
                  display: style.display,
                  visibility: style.visibility
                });

                if (rect.width === 0 || rect.height === 0) return;
                if (style.display === 'none' || style.visibility === 'hidden') return;

                results.push({
                  id: el.id,
                  centerX: rect.x + rect.width / 2,
                  centerY: rect.y + rect.height / 2,
                  text: (el.textContent || '').trim().substring(0, 50),
                  width: rect.width,
                  height: rect.height
                });
              });

              return { checkboxes: results, totalFound: allCheckboxes.length };
            })()
          `);

          if (result.error) {
            return { type: 'error', message: result.error };
          }

          const checkboxes = result.checkboxes || [];
          const checkboxIndex = command.index;
          if (checkboxIndex >= checkboxes.length) {
            return { type: 'error', message: `Checkbox index ${checkboxIndex} out of range (found ${checkboxes.length} checkboxes)` };
          }
          const checkbox = checkboxes[checkboxIndex];

          // Click using real CDP mouse events at the checkbox center coordinates.
          // getBoundingClientRect() returns viewport coordinates, which is what cdpClickAtViewport expects.
          await cdpClickAtViewport(tab.id, checkbox.centerX, checkbox.centerY);

          return { type: 'success', data: {
            index: checkboxIndex,
            id: checkbox.id,
            text: checkbox.text,
            x: checkbox.centerX,
            y: checkbox.centerY,
            width: checkbox.width,
            height: checkbox.height
          } };
        } catch (e) {
          return { type: 'error', message: `Click checkbox failed: ${e.message}` };
        }

      case 'clickButton':
        // Click a button by index (0-indexed) in the preview pane
        // First try elements with role="button", then fall back to heuristics
        try {
          const clickResult = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { error: 'Preview panel not found' };

              const buttonIndex = ${command.index};

              // First try: elements with role="button"
              let buttons = Array.from(preview.querySelectorAll('[role="button"]'));

              // Filter visible buttons
              buttons = buttons.filter(el => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return false;
                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return false;
                return true;
              });

              // If not enough role="button" elements, fall back to heuristics
              if (buttons.length <= buttonIndex) {
                const allElements = preview.querySelectorAll('*');
                const heuristicButtons = [];

                allElements.forEach((el) => {
                  // Skip if already in buttons array
                  if (el.getAttribute('role') === 'button') return;

                  const rect = el.getBoundingClientRect();
                  if (rect.width === 0 || rect.height === 0) return;

                  const style = window.getComputedStyle(el);
                  if (style.display === 'none' || style.visibility === 'hidden') return;

                  let directText = '';
                  for (const node of el.childNodes) {
                    if (node.nodeType === Node.TEXT_NODE) {
                      directText += node.textContent;
                    }
                  }
                  directText = directText.trim();

                  const hasPointerCursor = style.cursor === 'pointer';
                  const isSmall = rect.width < 300 && rect.height < 100;
                  const hasText = directText.length > 0 && directText.length < 50;

                  if (hasText && isSmall && hasPointerCursor) {
                    heuristicButtons.push(el);
                  }
                });

                buttons = buttons.concat(heuristicButtons);
              }

              if (buttonIndex >= buttons.length) {
                return { error: 'Button index ' + buttonIndex + ' out of range (found ' + buttons.length + ' buttons)' };
              }

              const button = buttons[buttonIndex];
              const rect = button.getBoundingClientRect();
              const text = (button.textContent || '').trim().substring(0, 50);

              const centerX = rect.x + rect.width / 2;
              const centerY = rect.y + rect.height / 2;

              // Return coordinates — click will be dispatched via real CDP mouse events
              return {
                success: true,
                index: buttonIndex,
                text: text,
                rect: { x: rect.x, y: rect.y, w: rect.width, h: rect.height },
                role: button.getAttribute('role'),
                centerX: centerX,
                centerY: centerY
              };
            })()
          `);

          if (clickResult.error) {
            return { type: 'error', message: clickResult.error };
          }

          // Click using real CDP mouse events at the button center coordinates
          await cdpClickAtViewport(tab.id, clickResult.centerX, clickResult.centerY);

          return { type: 'success', data: clickResult };
        } catch (e) {
          return { type: 'error', message: `Click button failed: ${e.message}` };
        }

      case 'clickByText':
        // Click any element by its text content (more flexible than clickButton)
        // IMPORTANT: Find and click in a SINGLE cdpEvaluate call to avoid timing issues
        // where the DOM may be rebuilt by Boon's reactive system between find and click
        try {
          const searchText = command.text;
          const exact = command.exact || false;

          const result = await cdpEvaluate(tab.id, `
            (function() {
              const searchText = ${JSON.stringify(searchText)};
              const exact = ${exact};
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              // Find all elements and look for matching text
              const allElements = preview.querySelectorAll('*');
              let bestMatch = null;
              let bestMatchElement = null;
              let bestMatchSize = Infinity;

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return;

                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                // Check direct text content
                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                // Check for match
                let matches = false;
                if (exact) {
                  matches = directText === searchText;
                } else {
                  matches = directText.includes(searchText);
                }

                if (matches) {
                  // Prefer smaller elements (more specific match)
                  const size = rect.width * rect.height;
                  if (size < bestMatchSize) {
                    bestMatchSize = size;
                    bestMatchElement = el;
                    bestMatch = {
                      text: directText,
                      x: Math.round(rect.x),
                      y: Math.round(rect.y),
                      width: Math.round(rect.width),
                      height: Math.round(rect.height),
                      centerX: Math.round(rect.x + rect.width / 2),
                      centerY: Math.round(rect.y + rect.height / 2)
                    };
                  }
                }
              });

              if (!bestMatchElement) {
                return { found: false, error: 'No element found with text: ' + searchText };
              }

              // Return coordinates — click will be dispatched via real CDP mouse events
              return { found: true, element: bestMatch };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Element not found' };
          }

          // Click using real CDP mouse events at the element center coordinates
          await cdpClickAtViewport(tab.id, result.element.centerX, result.element.centerY);

          return { type: 'success', data: { text: result.element.text, x: result.element.centerX, y: result.element.centerY } };
        } catch (e) {
          return { type: 'error', message: `Click by text failed: ${e.message}` };
        }

      case 'clickButtonNearText':
        // Click a button (e.g., ×) that's in the same row as an element containing the target text
        // This handles hover-dependent buttons by first hovering the row to make the button appear
        try {
          const searchText = command.text;
          const buttonText = command.buttonText || '×';  // Default to × for delete buttons

          // Step 1: Find the target element and its row container
          const findResult = await cdpEvaluate(tab.id, `
            (function() {
              const searchText = ${JSON.stringify(searchText)};
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              // Find the element containing the target text
              const allElements = preview.querySelectorAll('*');
              let targetElement = null;
              let smallestSize = Infinity;

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return;

                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                if (directText === searchText) {
                  const size = rect.width * rect.height;
                  if (size < smallestSize) {
                    smallestSize = size;
                    targetElement = el;
                  }
                }
              });

              if (!targetElement) {
                return { found: false, error: 'No element found with text: ' + searchText };
              }

              // Find the row container (walk up to find a reasonable parent)
              let container = targetElement.parentElement;
              let depth = 0;
              while (container && depth < 3) {
                // Look for a div that seems like a row container
                if (container.style && container.children.length > 1) {
                  break;
                }
                container = container.parentElement;
                depth++;
              }

              if (!container) container = targetElement.parentElement;

              const rect = container.getBoundingClientRect();
              return {
                found: true,
                centerX: Math.round(rect.x + rect.width / 2),
                centerY: Math.round(rect.y + rect.height / 2)
              };
            })()
          `);

          if (!findResult.found) {
            return { type: 'error', message: findResult.error || 'Target element not found' };
          }

          // Step 2: Hover over the row to make hover-dependent buttons appear
          await cdpHoverAt(tab.id, findResult.centerX, findResult.centerY);

          // Small delay to let the hover state update the DOM
          await new Promise(resolve => setTimeout(resolve, 100));

          // Step 3: Find AND click the button in a single atomic operation
          // This prevents timing issues where DOM is rebuilt between find and click
          const clickResult = await cdpEvaluate(tab.id, `
            (function() {
              const searchText = ${JSON.stringify(searchText)};
              const buttonText = ${JSON.stringify(buttonText)};
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              // Find the element containing the target text again
              const allElements = preview.querySelectorAll('*');
              let targetElement = null;
              let smallestSize = Infinity;

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return;

                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                if (directText === searchText) {
                  const size = rect.width * rect.height;
                  if (size < smallestSize) {
                    smallestSize = size;
                    targetElement = el;
                  }
                }
              });

              if (!targetElement) {
                return { found: false, error: 'Target element not found after hover' };
              }

              // Find the row container and look for the button
              let container = targetElement.parentElement;
              let foundButton = null;
              let maxDepth = 5;

              while (container && maxDepth > 0) {
                const buttons = container.querySelectorAll('[role="button"]');
                for (const btn of buttons) {
                  let btnText = '';
                  for (const node of btn.childNodes) {
                    if (node.nodeType === Node.TEXT_NODE) {
                      btnText += node.textContent;
                    }
                  }
                  btnText = btnText.trim();

                  if (btnText === buttonText) {
                    const btnRect = btn.getBoundingClientRect();
                    // Allow buttons that exist in DOM even if hidden by CSS (opacity:0, etc.)
                    // This handles Zoon's hover-dependent buttons that may not respond to synthetic hover
                    if (btnRect.width > 0 && btnRect.height > 0) {
                      const btnStyle = window.getComputedStyle(btn);
                      // Only reject display:none (element truly removed from layout)
                      // Allow opacity:0, visibility:hidden (element exists but invisible)
                      if (btnStyle.display !== 'none') {
                        foundButton = btn;
                        break;
                      }
                    }
                  }
                }

                if (foundButton) break;
                container = container.parentElement;
                maxDepth--;
              }

              if (!foundButton) {
                return { found: false, error: 'No button "' + buttonText + '" found near text: ' + searchText + ' (button may not appear on hover)' };
              }

              // Return coordinates — click will be dispatched via real CDP mouse events
              const btnRect = foundButton.getBoundingClientRect();
              const centerX = btnRect.x + btnRect.width / 2;
              const centerY = btnRect.y + btnRect.height / 2;

              return {
                found: true,
                text: searchText,
                buttonText: buttonText,
                centerX: Math.round(centerX),
                centerY: Math.round(centerY),
                buttonRect: { x: btnRect.x, y: btnRect.y, w: btnRect.width, h: btnRect.height }
              };
            })()
          `);

          if (!clickResult.found) {
            return { type: 'error', message: clickResult.error || 'Button not found after hover' };
          }

          // Click using real CDP mouse events at the button center coordinates
          await cdpClickAtViewport(tab.id, clickResult.centerX, clickResult.centerY);

          return { type: 'success', data: clickResult };
        } catch (e) {
          return { type: 'error', message: `Click button near text failed: ${e.message}` };
        }

      case 'focusInput':
        // Focus an input element in the preview panel by index
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              const inputs = preview.querySelectorAll('input, textarea, [contenteditable="true"]');
              const inputIndex = ${command.index};
              if (inputIndex >= inputs.length) {
                return { found: false, error: 'Input index ' + inputIndex + ' out of range (found ' + inputs.length + ' inputs)' };
              }

              const input = inputs[inputIndex];

              const rect = input.getBoundingClientRect();
              return {
                found: true,
                index: inputIndex,
                type: input.type || input.tagName.toLowerCase(),
                x: Math.round(rect.x + rect.width / 2),
                y: Math.round(rect.y + rect.height / 2)
              };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Input not found' };
          }

          // Also click via real CDP mouse events to ensure focus,
          // but only if the element has a visible rect (hidden/zero-size
          // elements return 0,0 and would steal focus from the intended target).
          if (result.x > 0 || result.y > 0) {
            await cdpClickAtViewport(tab.id, result.x, result.y);
          }
          return { type: 'success', data: result };
        } catch (e) {
          return { type: 'error', message: `Focus input failed: ${e.message}` };
        }

      case 'typeText':
        // Type text into the currently focused element using trusted key events.
        try {
          await cdpTypeTextCharByChar(tab.id, command.text);
          return { type: 'success', data: { text: command.text, method: 'char-by-char' } };
        } catch (e) {
          return { type: 'error', message: `Type text failed: ${e.message}` };
        }

      case 'typeTextCharByChar':
        // Type text character by character using CDP dispatchKeyEvent
        // This simulates real keyboard typing - each character generates keyDown, char, keyUp
        try {
          await cdpTypeTextCharByChar(tab.id, command.text);
          return { type: 'success', data: { text: command.text, method: 'char-by-char' } };
        } catch (e) {
          return { type: 'error', message: `Type text char-by-char failed: ${e.message}` };
        }

      case 'pressKey':
        // Press a special key using trusted CDP keyboard events.
        try {
          await cdpPressKey(tab.id, command.key);
          return { type: 'success', data: { key: command.key, method: 'cdp' } };
        } catch (e) {
          return { type: 'error', message: `Press key failed: ${e.message}` };
        }

      // ============ Legacy commands still using executeScript ============

      case 'getDOM':
        return await executeInTab(tab.id, getDOMStructure, command.selector || null, command.depth || 4);

      case 'detach':
        // Detach CDP debugger to resolve "debugger already attached" conflicts
        try {
          if (debuggerAttached.get(tab.id)) {
            await chrome.debugger.detach({ tabId: tab.id });
            debuggerAttached.delete(tab.id);
            cdpConsoleMessages.delete(tab.id);
            console.log(`[Boon] CDP: Debugger detached from tab ${tab.id}`);
          }
          return { type: 'success', data: 'Debugger detached' };
        } catch (e) {
          // Even if detach fails, clear our state
          debuggerAttached.delete(tab.id);
          cdpConsoleMessages.delete(tab.id);
          return { type: 'success', data: `Cleared state (detach error: ${e.message})` };
        }

      case 'refresh':
        // Refresh the tab WITHOUT reloading the extension (safer than reload)
        console.log('[Boon] Refreshing page...');
        // Clear debugger state BEFORE refresh (tab listener also does this but be explicit)
        debuggerAttached.delete(tab.id);
        cdpConsoleMessages.delete(tab.id);
        await chrome.tabs.reload(tab.id);
        // Wait for page to load and API to be ready
        let attempts = 0;
        while (attempts < 30) {
          await new Promise(r => setTimeout(r, 500));
          if (await checkApiReady(tab.id)) {
            // Pre-attach debugger so subsequent commands work immediately
            try {
              await attachDebugger(tab.id);
              console.log('[Boon] CDP: Pre-attached debugger after refresh');
            } catch (e) {
              console.log('[Boon] CDP: Could not pre-attach debugger:', e.message);
            }
            return { type: 'success', data: 'Page refreshed, API ready' };
          }
          attempts++;
        }
        return { type: 'error', message: 'Page refreshed but API not ready after 15s' };

      case 'reload':
        // Hot reload: send response FIRST, then reload
        console.log('[Boon] Reloading extension...');
        // Send success response before we terminate
        safeSend({ id, response: { type: 'success', data: null } });
        // Small delay to allow response to send
        await new Promise(resolve => setTimeout(resolve, 100));
        // Refresh the playground tab
        try {
          await chrome.tabs.reload(tab.id);
        } catch (e) {
          console.log('[Boon] Could not refresh tab:', e);
        }
        // Now reload the extension - this will terminate the service worker
        chrome.runtime.reload();
        // Return null to indicate response already sent
        return null;

      case 'getLocalStorage':
        return await executeInTab(tab.id, (pattern) => {
          const entries = {};
          for (let i = 0; i < localStorage.length; i++) {
            const key = localStorage.key(i);
            if (!pattern || key.includes(pattern)) {
              entries[key] = localStorage.getItem(key);
            }
          }
          return { type: 'localStorage', entries };
        }, command.pattern || null);

      case 'getFocusedElement':
        // Get information about the currently focused element
        // NOTE: document.activeElement can return body in automated environments
        // even when an element is properly focused at the DOM level.
        // We check multiple fallbacks for reliable detection.
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              let focused = document.activeElement;

              // Debug logging
              const boonFocused = preview ? preview.querySelector('[data-boon-focused="true"]') : null;
              const inputs = preview ? preview.querySelectorAll('input') : [];
              console.log('[getFocusedElement] activeElement:', focused?.tagName, 'boonFocused:', boonFocused?.tagName, 'inputs:', inputs.length);
              if (inputs.length > 0) {
                console.log('[getFocusedElement] input attrs:', Array.from(inputs[0].attributes).map(a => a.name + '=' + a.value).join(', '));
              }

              // If activeElement is body, try fallback detection methods.
              // Do not treat [autofocus] as currently focused; it only marks
              // mount-time intent and caused false positives after rerenders.
              if (!focused || focused === document.body) {
                if (preview) {
                  // Try :focus pseudo-class
                  focused = preview.querySelector(':focus');
                  // Try data-boon-focused attribute (set by Boon's bridge_v2.rs)
                  if (!focused) {
                    focused = preview.querySelector('[data-boon-focused="true"]');
                  }
                  // Try elements with focused="true" attribute (dominator may set this)
                  if (!focused) {
                    focused = preview.querySelector('[focused="true"]');
                  }
                }
              }

              if (!focused || focused === document.body) {
                return { tag_name: null, input_type: null, input_index: null };
              }

              const tag_name = focused.tagName;
              const input_type = focused.type || null;

              // Find the input index within the preview pane
              let input_index = null;
              if (preview && (focused.tagName === 'INPUT' || focused.tagName === 'TEXTAREA')) {
                const inputs = preview.querySelectorAll('input, textarea, [contenteditable="true"]');
                for (let i = 0; i < inputs.length; i++) {
                  if (inputs[i] === focused) {
                    input_index = i;
                    break;
                  }
                }
              }

              return { tag_name, input_type, input_index };
            })()
          `);
          return { type: 'focusedElement', ...result };
        } catch (e) {
          return { type: 'error', message: `Get focused element failed: ${e.message}` };
        }

      case 'getInputProperties':
        // Get properties of an input element by index
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              const inputs = preview.querySelectorAll('input, textarea, [contenteditable="true"]');
              const index = ${command.index};

              if (index >= inputs.length) {
                return { found: false, error: 'Input index ' + index + ' out of range (found ' + inputs.length + ' inputs)' };
              }

              const input = inputs[index];
              return {
                found: true,
                placeholder: input.placeholder || input.getAttribute('placeholder') || null,
                value: input.value || input.textContent || null,
                inputType: input.type || input.tagName.toLowerCase()
              };
            })()
          `);
          return { type: 'inputProperties', ...result };
        } catch (e) {
          return { type: 'error', message: `Get input properties failed: ${e.message}` };
        }

      case 'getCurrentUrl':
        // Get the current page URL
        return { type: 'currentUrl', url: tab.url };

      case 'doubleClickByText':
        // Double-click an element by its text content
        // IMPORTANT: Dispatch dblclick directly on the found element (like clickByText)
        // to avoid viewport/scroll issues with CDP coordinate-based clicking
        try {
          const searchText = command.text;
          const exact = command.exact || false;

          const result = await cdpEvaluate(tab.id, `
            (function() {
              const searchText = ${JSON.stringify(searchText)};
              const exact = ${exact};
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              const allElements = preview.querySelectorAll('*');
              let bestMatch = null;
              let bestMatchElement = null;
              let bestMatchSize = Infinity;

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return;

                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                let matches = exact ? directText === searchText : directText.includes(searchText);

                if (matches) {
                  const size = rect.width * rect.height;
                  if (size < bestMatchSize) {
                    bestMatchSize = size;
                    bestMatchElement = el;
                    bestMatch = {
                      text: directText,
                      x: Math.round(rect.x),
                      y: Math.round(rect.y),
                      width: Math.round(rect.width),
                      height: Math.round(rect.height)
                    };
                  }
                }
              });

              if (!bestMatchElement) {
                return { found: false, error: 'No element found with text: ' + searchText };
              }

              // Return coordinates — double-click will be dispatched via real CDP mouse events
              const rect = bestMatchElement.getBoundingClientRect();
              const centerX = rect.x + rect.width / 2;
              const centerY = rect.y + rect.height / 2;

              bestMatch.centerX = Math.round(centerX);
              bestMatch.centerY = Math.round(centerY);
              return { found: true, element: bestMatch };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Element not found' };
          }

          // Double-click using real CDP mouse events at the element center coordinates.
          // cdpDoubleClickAt expects page coordinates, so add scroll offset.
          const scrollOffset = await cdpEvaluate(tab.id, `({ scrollX: window.scrollX, scrollY: window.scrollY })`);
          const pageX = result.element.centerX + (scrollOffset?.scrollX || 0);
          const pageY = result.element.centerY + (scrollOffset?.scrollY || 0);
          await cdpDoubleClickAt(tab.id, pageX, pageY);

          return { type: 'success', data: { text: result.element.text, x: result.element.centerX, y: result.element.centerY } };
        } catch (e) {
          return { type: 'error', message: `Double-click by text failed: ${e.message}` };
        }

      case 'hoverByText':
        // Hover over an element by its text content
        // Uses scrollIntoView + CDP hover for real pointer events (Zoon needs real mouse events)
        try {
          const searchText = command.text;
          const exact = command.exact || false;

          const result = await cdpEvaluate(tab.id, `
            (function() {
              const searchText = ${JSON.stringify(searchText)};
              const exact = ${exact};
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              const allElements = preview.querySelectorAll('*');
              let bestMatch = null;
              let bestMatchElement = null;
              let bestMatchSize = Infinity;

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return;

                const style = window.getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                let matches = exact ? directText === searchText : directText.includes(searchText);

                if (matches) {
                  const size = rect.width * rect.height;
                  if (size < bestMatchSize) {
                    bestMatchSize = size;
                    bestMatchElement = el;
                    bestMatch = { text: directText };
                  }
                }
              });

              if (!bestMatchElement) {
                return { found: false, error: 'No element found with text: ' + searchText };
              }

              // Scroll into view, then get viewport coordinates for CDP hover
              bestMatchElement.scrollIntoView({ block: 'center', behavior: 'instant' });
              const rect = bestMatchElement.getBoundingClientRect();
              bestMatch.centerX = Math.round(rect.x + rect.width / 2);
              bestMatch.centerY = Math.round(rect.y + rect.height / 2);
              return { found: true, element: bestMatch };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Element not found' };
          }

          const element = result.element;
          // Small delay after scroll for layout to settle
          await new Promise(r => setTimeout(r, 50));
          await cdpHoverAt(tab.id, element.centerX, element.centerY);
          return { type: 'success', data: { text: element.text, x: element.centerX, y: element.centerY } };
        } catch (e) {
          return { type: 'error', message: `Hover by text failed: ${e.message}` };
        }

      case 'verifyInputTypeable':
        // Verify input is actually typeable (not disabled/readonly/hidden)
        try {
          const inputIndex = command.index;
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { typeable: false, reason: 'Preview panel not found', disabled: false, readonly: false, hidden: true };

              const inputs = preview.querySelectorAll('input, textarea');
              if (${inputIndex} >= inputs.length) {
                return { typeable: false, reason: 'Input ' + ${inputIndex} + ' not found (only ' + inputs.length + ' inputs)', disabled: false, readonly: false, hidden: true };
              }

              const input = inputs[${inputIndex}];
              const style = window.getComputedStyle(input);
              const rect = input.getBoundingClientRect();

              const disabled = input.disabled || input.getAttribute('aria-disabled') === 'true';
              const readonly = input.readOnly || input.getAttribute('aria-readonly') === 'true';
              const hidden = style.display === 'none' || style.visibility === 'hidden' || rect.width === 0 || rect.height === 0;

              let reason = null;
              if (disabled) reason = 'Input is disabled';
              else if (readonly) reason = 'Input is readonly';
              else if (hidden) reason = 'Input is hidden (display:none or zero-size)';

              return {
                typeable: !disabled && !readonly && !hidden,
                disabled,
                readonly,
                hidden,
                reason
              };
            })()
          `);
          return { type: 'inputTypeableStatus', ...result };
        } catch (e) {
          return { type: 'error', message: `Verify input typeable failed: ${e.message}` };
        }

      case 'getCheckboxState':
        // Get the checked state of a checkbox by index (0-indexed) in the preview pane
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, checked: false };

              // Find checkboxes using same logic as clickCheckbox
              const roleCheckboxes = Array.from(preview.querySelectorAll('[role="checkbox"]'));
              const idCheckboxes = Array.from(preview.querySelectorAll('[id^="cb-"]'));
              const seen = new Set();
              const allCheckboxes = [];

              roleCheckboxes.forEach(el => {
                seen.add(el);
                allCheckboxes.push(el);
              });

              idCheckboxes.forEach(el => {
                if (!seen.has(el)) {
                  seen.add(el);
                  allCheckboxes.push(el);
                }
              });

              // Sort by vertical position
              allCheckboxes.sort((a, b) => {
                const rectA = a.getBoundingClientRect();
                const rectB = b.getBoundingClientRect();
                return rectA.top - rectB.top;
              });

              const checkboxIndex = ${command.index};
              if (checkboxIndex >= allCheckboxes.length) {
                return { found: false, checked: false };
              }

              const checkbox = allCheckboxes[checkboxIndex];

              // Determine checked state:
              // 1. aria-checked attribute (most reliable for role="checkbox")
              // 2. data-checked attribute
              // 3. checked property (for native checkboxes)
              const ariaChecked = checkbox.getAttribute('aria-checked');
              const dataChecked = checkbox.getAttribute('data-checked');
              const nativeChecked = checkbox.checked;

              let checked = false;
              if (ariaChecked !== null) {
                checked = ariaChecked === 'true';
              } else if (dataChecked !== null) {
                checked = dataChecked === 'true';
              } else if (nativeChecked !== undefined) {
                checked = !!nativeChecked;
              }

              return { found: true, checked };
            })()
          `);

          return { type: 'checkboxState', found: result.found, checked: result.checked };
        } catch (e) {
          return { type: 'error', message: `Get checkbox state failed: ${e.message}` };
        }

      case 'assertButtonHasOutline':
        // Check if a button with the given text has a visible outline
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, hasOutline: false, error: 'Preview panel not found' };

              const buttonText = ${JSON.stringify(command.text)};

              // Find buttons (button tags and role="button" elements)
              const allButtons = Array.from(preview.querySelectorAll('button, [role="button"]'));

              // Find button with matching text
              const button = allButtons.find(btn => {
                const text = btn.textContent.trim();
                return text === buttonText;
              });

              if (!button) {
                return { found: false, hasOutline: false, error: 'Button with text "' + buttonText + '" not found' };
              }

              // Check if button has a visible outline
              const style = window.getComputedStyle(button);
              const outline = style.outline;
              const outlineWidth = style.outlineWidth;
              const outlineStyle = style.outlineStyle;
              const boxShadow = style.boxShadow;

              // Outline is visible if:
              // 1. outlineStyle is not 'none' and outlineWidth > 0, OR
              // 2. box-shadow contains 'inset' (inner outline rendered as inset box-shadow)
              const hasOutlineCSS = outlineStyle !== 'none' &&
                                    outlineWidth !== '0px' &&
                                    outlineWidth !== '0';
              const hasInsetShadow = boxShadow && boxShadow !== 'none' && boxShadow.includes('inset');
              const hasOutline = hasOutlineCSS || hasInsetShadow;

              return {
                found: true,
                hasOutline: hasOutline,
                outline: outline,
                outlineWidth: outlineWidth,
                outlineStyle: outlineStyle,
                boxShadow: boxShadow
              };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Button not found' };
          }
          if (!result.hasOutline) {
            return { type: 'error', message: `Button "${command.text}" does not have a visible outline. Got: outline="${result.outline}", outlineWidth="${result.outlineWidth}", outlineStyle="${result.outlineStyle}", boxShadow="${result.boxShadow}"` };
          }
          return { type: 'success', data: { outline: result.outline, outlineWidth: result.outlineWidth } };
        } catch (e) {
          return { type: 'error', message: `Assert button has outline failed: ${e.message}` };
        }

      case 'getElementStyle':
        // Get computed style property of an element found by text content
        try {
          const styleResult = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, value: '', error: 'Preview panel not found' };

              const searchText = ${JSON.stringify(command.text)};
              const property = ${JSON.stringify(command.property)};

              // Find deepest element containing the text
              const allElements = preview.querySelectorAll('*');
              let bestMatch = null;
              for (const el of allElements) {
                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                if (directText.trim() === searchText) {
                  bestMatch = el;
                }
              }

              if (!bestMatch) {
                // Fallback: check textContent contains
                for (const el of allElements) {
                  if (el.textContent.trim() === searchText && el.children.length === 0) {
                    bestMatch = el;
                    break;
                  }
                }
              }

              if (!bestMatch) {
                return { found: false, value: '', error: 'Element with text "' + searchText + '" not found' };
              }

              const style = window.getComputedStyle(bestMatch);
              return { found: true, value: style.getPropertyValue(property) };
            })()
          `);

          return { type: 'elementStyle', found: styleResult.found, value: styleResult.value || '' };
        } catch (e) {
          return { type: 'error', message: 'Get element style failed: ' + e.message };
        }

      case 'assertToggleAllDarker':
        // Check if the toggle all checkbox icon is dark (all todos completed)
        // The icon should have Oklch lightness ~0.40 when dark, ~0.75 when light
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              // Find the toggle all checkbox - it's the first checkbox (index 0) containing the ❯ chevron
              const checkboxes = preview.querySelectorAll('[role="checkbox"]');
              if (checkboxes.length === 0) {
                return { found: false, error: 'No checkboxes found' };
              }

              // The toggle all is the first checkbox
              const toggleAll = checkboxes[0];

              // Find the element containing ❯ (the icon) - look for deepest element with that text
              let iconEl = null;
              const allElements = toggleAll.querySelectorAll('*');
              for (const el of allElements) {
                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                if (directText.trim() === '❯') {
                  iconEl = el;
                  break;
                }
              }

              if (!iconEl) {
                // Fallback: check the toggle all element itself or its first child
                iconEl = toggleAll.querySelector('*') || toggleAll;
              }

              const style = window.getComputedStyle(iconEl);
              const color = style.color;

              // Parse the color to calculate luminance (without regex to avoid CDP issues)
              // Color will be like "rgb(102, 102, 102)" or "oklch(40% 0 0)"
              let actualLuminance = null;

              if (color.indexOf('oklch') === 0) {
                // Parse oklch(40% 0 0) or oklch(0.4 0 0) without regex
                // Extract the first number after 'oklch('
                const start = color.indexOf('(') + 1;
                const end = color.indexOf('%') !== -1 ? color.indexOf('%') : color.indexOf(' ', start);
                if (start > 0 && end > start) {
                  const value = parseFloat(color.substring(start, end));
                  actualLuminance = value > 1 ? value / 100 : value;
                }
              } else if (color.indexOf('rgb') === 0) {
                // Parse rgb(102, 102, 102) without regex
                const inner = color.substring(color.indexOf('(') + 1, color.indexOf(')'));
                const parts = inner.split(',').map(function(s) { return parseInt(s.trim()); });
                if (parts.length >= 3) {
                  const r = parts[0] / 255;
                  const g = parts[1] / 255;
                  const b = parts[2] / 255;
                  // sRGB to relative luminance (simplified)
                  actualLuminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                }
              }

              return {
                found: true,
                color: color,
                luminance: actualLuminance
              };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Toggle all icon not found' };
          }

          if (result.luminance === null) {
            return { type: 'error', message: 'Could not parse color from: ' + result.color };
          }

          // For Oklch colors, we use lightness directly (not converted luminance)
          // Dark = Oklch lightness 0.40, Light = Oklch lightness 0.75
          // Threshold: if lightness < 0.55, it's dark enough (midpoint between 0.4 and 0.75 is ~0.575)
          const isDark = result.luminance < 0.55;
          if (!isDark) {
            return { type: 'error', message: 'Toggle all icon is NOT dark. Lightness: ' + result.luminance.toFixed(3) + ', color: ' + result.color + ' (expected lightness < 0.55)' };
          }

          return { type: 'success', data: { luminance: result.luminance, color: result.color } };
        } catch (e) {
          return { type: 'error', message: 'Assert toggle all darker failed: ' + e.message };
        }

      case 'assertCheckboxClickable':
        // Verify that a checkbox is ACTUALLY clickable by real user (not obscured by other elements)
        // This uses elementFromPoint() to check what element would receive a real click
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              const checkboxIndex = ${command.index};

              // Find checkboxes using same method as clickCheckbox
              const roleCheckboxes = Array.from(preview.querySelectorAll('[role="checkbox"]'));
              const idCheckboxes = Array.from(preview.querySelectorAll('[id^="cb-"]'));

              // Merge and dedupe
              const seen = new Set();
              const allCheckboxes = [];
              roleCheckboxes.forEach(el => { seen.add(el); allCheckboxes.push(el); });
              idCheckboxes.forEach(el => { if (!seen.has(el)) { seen.add(el); allCheckboxes.push(el); } });

              // Sort by vertical position for consistent ordering
              allCheckboxes.sort((a, b) => {
                const rectA = a.getBoundingClientRect();
                const rectB = b.getBoundingClientRect();
                return rectA.top - rectB.top;
              });

              if (checkboxIndex >= allCheckboxes.length) {
                return {
                  found: false,
                  clickable: false,
                  error: 'Checkbox index ' + checkboxIndex + ' not found (only ' + allCheckboxes.length + ' checkboxes exist)'
                };
              }

              const checkbox = allCheckboxes[checkboxIndex];
              const rect = checkbox.getBoundingClientRect();

              // Calculate center point
              const centerX = rect.left + rect.width / 2;
              const centerY = rect.top + rect.height / 2;

              // Use elementFromPoint to check what element would receive a REAL user click
              const topElement = document.elementFromPoint(centerX, centerY);

              if (!topElement) {
                return {
                  found: true,
                  clickable: false,
                  error: 'No element at checkbox center (' + centerX.toFixed(1) + ', ' + centerY.toFixed(1) + ')'
                };
              }

              // Element is clickable if it IS the checkbox OR is a descendant of the checkbox
              const isClickable = checkbox.contains(topElement) || topElement === checkbox;

              if (!isClickable) {
                return {
                  found: true,
                  clickable: false,
                  error: 'Checkbox ' + checkboxIndex + ' is OBSCURED. At center (' +
                         centerX.toFixed(1) + ', ' + centerY.toFixed(1) +
                         '), click would hit <' + topElement.tagName.toLowerCase() +
                         (topElement.id ? ' id="' + topElement.id + '"' : '') +
                         (topElement.className ? ' class="' + topElement.className + '"' : '') +
                         '> instead of the checkbox'
                };
              }

              return {
                found: true,
                clickable: true,
                centerX: centerX,
                centerY: centerY,
                actualElement: topElement.tagName.toLowerCase()
              };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Checkbox not found' };
          }
          if (!result.clickable) {
            return { type: 'error', message: result.error || 'Checkbox not clickable' };
          }
          return { type: 'success', data: { centerX: result.centerX, centerY: result.centerY, actualElement: result.actualElement } };
        } catch (e) {
          return { type: 'error', message: 'Assert checkbox clickable failed: ' + e.message };
        }

      case 'screenshotPreview':
        // Take screenshot of preview pane at specified dimensions (v3)
        // Uses PreviewOnly panel layout mode for clean screenshots
        try {
          const width = command.width || 700;
          const height = command.height || 700;
          const useHidpi = command.hidpi || false;

          // 1. Get devicePixelRatio and save current panel layout
          const dpr = await cdpEvaluate(tab.id, `window.devicePixelRatio || 1`);
          const originalLayout = await cdpEvaluate(tab.id, `
            window.boonPlayground.getPanelLayout()
          `);

          // 2. Switch to PreviewOnly mode for clean screenshot styling
          await cdpEvaluate(tab.id, `
            window.boonPlayground.setPanelLayout('preview')
          `);
          await new Promise(r => setTimeout(r, 50));

          // 3. Force preview to requested size
          await cdpEvaluate(tab.id, `
            window.boonPlayground.setPreviewSize(${width}, ${height})
          `);
          await new Promise(r => setTimeout(r, 100));

          // 4. Verify preview fits in viewport
          const verification = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { error: 'Preview panel not found' };

              const rect = preview.getBoundingClientRect();
              const viewportWidth = window.innerWidth;
              const viewportHeight = window.innerHeight;

              // Check if preview is fully visible (not clipped by viewport)
              if (rect.right > viewportWidth || rect.bottom > viewportHeight) {
                return {
                  error: 'Browser window too small. Preview would be clipped. Needed: ' +
                         Math.ceil(rect.right) + 'x' + Math.ceil(rect.bottom) +
                         ', Viewport: ' + viewportWidth + 'x' + viewportHeight
                };
              }

              // Check if preview is positioned off-screen
              if (rect.left < 0 || rect.top < 0) {
                return { error: 'Preview is positioned off-screen' };
              }

              return {
                ok: true,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height
              };
            })()
          `);

          if (verification.error) {
            // Reset and fail
            await cdpEvaluate(tab.id, `window.boonPlayground.resetPreviewSize()`);
            await cdpEvaluate(tab.id, `window.boonPlayground.setPanelLayout('${originalLayout}')`);
            return { type: 'error', message: verification.error };
          }

          // 5. Take screenshot with HiDPI normalization
          // scale: 1/dpr normalizes output to CSS pixels (700x700 CSS -> 700x700 px)
          // scale: 1 gives native device resolution (700x700 CSS -> 1400x1400 px on 2x)
          await attachDebugger(tab.id);
          const scale = useHidpi ? 1 : (1 / dpr);
          const { data } = await chrome.debugger.sendCommand({ tabId: tab.id }, 'Page.captureScreenshot', {
            format: 'png',
            clip: {
              x: verification.x,
              y: verification.y,
              width: verification.width,
              height: verification.height,
              scale: scale
            }
          });

          // 6. Reset preview size and restore original panel layout
          await cdpEvaluate(tab.id, `window.boonPlayground.resetPreviewSize()`);
          await cdpEvaluate(tab.id, `window.boonPlayground.setPanelLayout('${originalLayout}')`);

          // 7. Return with metadata
          const outputWidth = useHidpi ? Math.round(width * dpr) : width;
          const outputHeight = useHidpi ? Math.round(height * dpr) : height;

          return {
            type: 'screenshot',
            base64: data,
            width: outputWidth,
            height: outputHeight,
            dpr: dpr
          };
        } catch (e) {
          // Always try to reset on error
          try {
            await cdpEvaluate(tab.id, `window.boonPlayground.resetPreviewSize()`);
            await cdpEvaluate(tab.id, `window.boonPlayground.setPanelLayout('normal')`);
          } catch {}
          return { type: 'error', message: 'Screenshot preview failed: ' + e.message };
        }

      case 'getEngine':
        // Get the currently selected engine type from the playground
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              if (typeof window.boonPlayground === 'undefined') {
                return { error: 'boonPlayground not available' };
              }
              if (typeof window.boonPlayground.getEngine !== 'function') {
                return { error: 'getEngine API not available' };
              }
              return window.boonPlayground.getEngine();
            })()
          `);
          if (result && result.error) {
            return { type: 'error', message: result.error };
          }
          return { type: 'engineInfo', engine: result.engine, switchable: result.switchable };
        } catch (e) {
          return { type: 'error', message: 'GetEngine failed: ' + e.message };
        }

      case 'setEngine':
        // Set the engine type and trigger re-run
        try {
          const engineToSet = command.engine;
          if (engineToSet !== 'Actors' && engineToSet !== 'DD' && engineToSet !== 'Wasm' && engineToSet !== 'WasmPro') {
            return { type: 'error', message: `Invalid engine '${engineToSet}'. Must be 'Actors', 'DD', 'Wasm', or 'WasmPro'` };
          }
          const result = await cdpEvaluate(tab.id, `
            (function() {
              if (typeof window.boonPlayground === 'undefined') {
                return { error: 'boonPlayground not available' };
              }
              if (typeof window.boonPlayground.setEngine !== 'function') {
                return { error: 'setEngine API not available' };
              }
              return window.boonPlayground.setEngine('${engineToSet}');
            })()
          `);
          if (result && result.error) {
            return { type: 'error', message: result.error };
          }
          return { type: 'success', data: { engine: result.engine, previous: result.previous } };
        } catch (e) {
          return { type: 'error', message: 'SetEngine failed: ' + e.message };
        }

      case 'getPersistence':
        // Get persistence state from boonPlayground API
        try {
          const persistResult = await cdpEvaluate(tab.id, `
            (function() {
              if (typeof window.boonPlayground === 'undefined' ||
                  typeof window.boonPlayground.getPersistence !== 'function') {
                return { error: 'getPersistence API not available' };
              }
              return { enabled: window.boonPlayground.getPersistence() };
            })()
          `);
          if (persistResult && persistResult.error) {
            return { type: 'error', message: persistResult.error };
          }
          return { type: 'success', data: { enabled: persistResult.enabled } };
        } catch (e) {
          return { type: 'error', message: 'GetPersistence failed: ' + e.message };
        }

      case 'setPersistence':
        // Set persistence state via boonPlayground API
        try {
          const enabledVal = command.enabled ? 'true' : 'false';
          const setPersistResult = await cdpEvaluate(tab.id, `
            (function() {
              if (typeof window.boonPlayground === 'undefined' ||
                  typeof window.boonPlayground.setPersistence !== 'function') {
                return { error: 'setPersistence API not available' };
              }
              const result = window.boonPlayground.setPersistence(${enabledVal});
              return { enabled: result };
            })()
          `);
          if (setPersistResult && setPersistResult.error) {
            return { type: 'error', message: setPersistResult.error };
          }
          return { type: 'success', data: { enabled: setPersistResult.enabled } };
        } catch (e) {
          return { type: 'error', message: 'SetPersistence failed: ' + e.message };
        }

      case 'getElementStyle':
        // Get computed CSS styles of an element found by text content
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              const searchText = ${JSON.stringify(command.text)};
              const properties = ${JSON.stringify(command.properties)};
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { found: false, error: 'Preview panel not found' };

              // Find element by text content (prefer smallest/most specific match)
              const allElements = preview.querySelectorAll('*');
              let bestMatch = null;
              let bestMatchSize = Infinity;

              allElements.forEach((el) => {
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0) return;

                let directText = '';
                for (const node of el.childNodes) {
                  if (node.nodeType === Node.TEXT_NODE) {
                    directText += node.textContent;
                  }
                }
                directText = directText.trim();

                if (directText.includes(searchText)) {
                  const size = rect.width * rect.height;
                  if (size < bestMatchSize) {
                    bestMatchSize = size;
                    bestMatch = el;
                  }
                }
              });

              if (!bestMatch) {
                return { found: false, error: 'No element found with text: ' + searchText };
              }

              // Get computed styles for requested properties
              // For non-inherited properties (background-color, transform, etc.),
              // walk up the ancestor chain to find the nearest styled element.
              const NON_INHERITED_DEFAULTS = {
                'background-color': ['rgba(0, 0, 0, 0)', 'transparent'],
                'transform': ['none'],
                'box-shadow': ['none'],
                'border-top': ['none'],
                'border-bottom': ['none'],
                'border-left': ['none'],
                'border-right': ['none'],
                'padding': ['0px'],
                'padding-top': ['0px'],
                'padding-bottom': ['0px'],
                'padding-left': ['0px'],
                'padding-right': ['0px'],
                'border-radius': ['0px'],
              };
              const styles = {};
              for (const prop of properties) {
                let el = bestMatch;
                let value = window.getComputedStyle(el).getPropertyValue(prop);
                const defaults = NON_INHERITED_DEFAULTS[prop];
                if (defaults) {
                  while (defaults.includes(value) && el.parentElement && el.parentElement !== preview) {
                    el = el.parentElement;
                    value = window.getComputedStyle(el).getPropertyValue(prop);
                  }
                }
                styles[prop] = value;
              }
              return { found: true, styles: styles };
            })()
          `);

          if (!result.found) {
            return { type: 'elementStyle', found: false, error: result.error || 'Element not found' };
          }
          return { type: 'elementStyle', found: true, styles: result.styles };
        } catch (e) {
          return { type: 'elementStyle', found: false, error: 'GetElementStyle failed: ' + e.message };
        }

      case 'evalJs':
        // Evaluate arbitrary JavaScript in the page context
        try {
          const evalResult = await cdpEvaluate(tab.id, command.expression);
          return { type: 'success', data: evalResult };
        } catch (e) {
          return { type: 'error', message: `EvalJs failed: ${e.message}` };
        }

      default:
        return { type: 'error', message: `Unknown command: ${type}` };
    }
  } catch (e) {
    console.error('[Boon] Command error:', e);
    return { type: 'error', message: e.message || String(e) };
  }
}

// Execute function in tab context (MAIN world to access page's window object)
async function executeInTab(tabId, func, ...args) {
  try {
    const results = await chrome.scripting.executeScript({
      target: { tabId },
      world: 'MAIN',  // Run in page's main world to access window.boonPlayground
      func: func,
      args: args
    });

    if (results && results[0]) {
      return results[0].result;
    }
    return { type: 'error', message: 'No result from script' };
  } catch (e) {
    return { type: 'error', message: e.message || String(e) };
  }
}

// Check if boonPlayground API is available
async function checkApiReady(tabId) {
  try {
    const results = await chrome.scripting.executeScript({
      target: { tabId },
      world: 'MAIN',  // Run in page's main world to access window.boonPlayground
      func: () => {
        return typeof window.boonPlayground !== 'undefined' &&
               typeof window.boonPlayground.isReady === 'function' &&
               window.boonPlayground.isReady();
      }
    });
    return results && results[0] && results[0].result === true;
  } catch (e) {
    return false;
  }
}

// Capture screenshot
async function captureScreenshot(tabId) {
  try {
    const dataUrl = await chrome.tabs.captureVisibleTab(null, { format: 'png' });
    // Remove the data:image/png;base64, prefix
    const base64 = dataUrl.replace(/^data:image\/png;base64,/, '');
    return { type: 'screenshot', base64 };
  } catch (e) {
    return { type: 'error', message: `Screenshot failed: ${e.message}` };
  }
}

// Functions to be injected into page context
function clickElement(selector) {
  let el = null;
  let foundBy = 'selector';
  const preview = document.querySelector('[data-boon-panel="preview"]');

  // First try exact CSS selector in whole document
  try {
    el = document.querySelector(selector);
  } catch (e) {
    // Invalid selector, will try text matching below
  }

  // If not found, try within the preview panel
  if (!el && preview) {
    try {
      el = preview.querySelector(selector);
    } catch (e) {}

    // Boon/Zoon renders buttons as <div role="button">, not <button> elements
    // If selector is "button" and not found, try [role="button"]
    if (!el && selector === 'button') {
      el = preview.querySelector('[role="button"]');
      if (el) foundBy = 'role=button';
    }

    // Try finding by text content in preview panel (for Boon UI elements)
    // This works now with DOM and will need adaptation for WebGPU later
    if (!el) {
      const searchText = selector.toLowerCase().trim();
      const candidates = preview.querySelectorAll('[role="button"], [tabindex], [onclick]');

      // First pass: look for exact text match (most specific)
      for (const candidate of candidates) {
        const text = (candidate.textContent || '').trim().toLowerCase();
        if (text === searchText) {
          el = candidate;
          foundBy = 'text content (exact)';
          break;
        }
      }

      // Second pass: look for partial match if no exact match found
      if (!el) {
        for (const candidate of candidates) {
          const text = (candidate.textContent || '').trim().toLowerCase();
          if (text.includes(searchText)) {
            el = candidate;
            foundBy = 'text content (partial)';
            break;
          }
        }
      }
    }
  }

  // Final fallback: try [role="button"] anywhere
  if (!el && selector === 'button') {
    el = document.querySelector('[role="button"]');
    if (el) foundBy = 'role=button (document)';
  }

  if (!el) {
    return { type: 'error', message: `Element not found: ${selector}. Try using 'elements' command to get bounding boxes, then 'click-at x y'.` };
  }

  // Click at center of element's bounding box
  const rect = el.getBoundingClientRect();
  const x = rect.left + rect.width / 2;
  const y = rect.top + rect.height / 2;

  // Zoon/MoonZoon uses pointer events, dispatch both pointer and mouse events for compatibility
  const pointerEvents = ['pointerdown', 'pointerup'];
  for (const eventType of pointerEvents) {
    const event = new PointerEvent(eventType, {
      bubbles: true,
      cancelable: true,
      view: window,
      clientX: x,
      clientY: y,
      pointerId: 1,
      pointerType: 'mouse',
      isPrimary: true
    });
    el.dispatchEvent(event);
  }

  // Also dispatch mouse events and click for general compatibility
  const mouseEvents = ['mousedown', 'mouseup', 'click'];
  for (const eventType of mouseEvents) {
    const event = new MouseEvent(eventType, {
      bubbles: true,
      cancelable: true,
      view: window,
      clientX: x,
      clientY: y
    });
    el.dispatchEvent(event);
  }

  return { type: 'success', data: {
    tag: el.tagName.toLowerCase(),
    text: (el.textContent || '').trim().substring(0, 50),
    foundBy,
    bounds: { x: Math.round(rect.left), y: Math.round(rect.top), width: Math.round(rect.width), height: Math.round(rect.height) }
  }};
}

function typeInElement(selector, text) {
  const el = document.querySelector(selector);
  if (!el) {
    return { type: 'error', message: `Element not found: ${selector}` };
  }
  el.focus();
  el.value = text;
  // Dispatch both 'input' (for real-time updates) and 'change' (for Zoon's on_change)
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return { type: 'success', data: null };
}

function injectCode(code) {
  if (typeof window.boonPlayground !== 'undefined' && window.boonPlayground.setCode) {
    window.boonPlayground.setCode(code);
    return { type: 'success', data: null };
  }

  // Fallback: try to access CodeMirror directly
  const cmElement = document.querySelector('.cm-content');
  if (cmElement) {
    // Try to find the CodeMirror view
    const view = cmElement.cmView?.view;
    if (view) {
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: code }
      });
      return { type: 'success', data: null };
    }
  }

  return { type: 'error', message: 'Could not inject code: boonPlayground API not available' };
}

function triggerRun() {
  if (typeof window.boonPlayground !== 'undefined' && window.boonPlayground.run) {
    window.boonPlayground.run();
    return { type: 'success', data: null };
  }

  // Fallback: try to find and click run button
  const runButton = document.querySelector('[data-action="run"]') ||
                    document.querySelector('.run-button') ||
                    document.querySelector('button:has-text("Run")');
  if (runButton) {
    runButton.click();
    return { type: 'success', data: null };
  }

  return { type: 'error', message: 'Could not trigger run: boonPlayground API not available' };
}

function getConsoleMessages() {
  // Check if we have captured console messages
  if (typeof window.__boonCapturedConsole !== 'undefined') {
    return { type: 'console', messages: window.__boonCapturedConsole };
  }

  // Fallback: check if boonPlayground has getConsole
  if (typeof window.boonPlayground !== 'undefined' && window.boonPlayground.getConsole) {
    const messages = window.boonPlayground.getConsole();
    return { type: 'console', messages };
  }

  return { type: 'console', messages: [] };
}

// Set up console capture in MAIN world - call this on page load
function setupConsoleCapture() {
  if (typeof window.__boonCapturedConsole !== 'undefined') {
    return { type: 'success', data: 'Already set up' };
  }

  window.__boonCapturedConsole = [];
  const maxMessages = 100;

  const originalConsole = {
    log: console.log,
    warn: console.warn,
    error: console.error,
    info: console.info
  };

  function captureConsole(level, ...args) {
    const text = args.map(arg => {
      if (typeof arg === 'object') {
        try {
          return JSON.stringify(arg);
        } catch (e) {
          return String(arg);
        }
      }
      return String(arg);
    }).join(' ');

    window.__boonCapturedConsole.push({
      level,
      text,
      timestamp: Date.now()
    });

    if (window.__boonCapturedConsole.length > maxMessages) {
      window.__boonCapturedConsole.shift();
    }

    originalConsole[level].apply(console, args);
  }

  console.log = (...args) => captureConsole('log', ...args);
  console.warn = (...args) => captureConsole('warn', ...args);
  console.error = (...args) => captureConsole('error', ...args);
  console.info = (...args) => captureConsole('info', ...args);

  return { type: 'success', data: 'Console capture set up' };
}

function getPreviewText() {
  const preview = document.querySelector('[data-boon-panel="preview"]');
  if (preview) {
    const limit = 8192;
    const walker = document.createTreeWalker(preview, NodeFilter.SHOW_TEXT);
    let text = '';
    let node = walker.nextNode();
    while (node) {
      text += node.textContent || '';
      if (text.length >= limit) {
        text = text.substring(0, limit);
        break;
      }
      node = walker.nextNode();
    }
    return { type: 'previewText', text };
  }
  return { type: 'error', message: 'Could not get preview text' };
}

function getPreviewElements() {
  const preview = document.querySelector('[data-boon-panel="preview"]');
  if (!preview) {
    return { type: 'error', message: 'Preview panel not found' };
  }

  const panelRect = preview.getBoundingClientRect();
  const elements = [];

  // Find all interactive/significant elements in preview
  const interactiveSelectors = [
    '[role="button"]',
    'button',
    'a',
    'input',
    'select',
    'textarea',
    '[onclick]',
    '[tabindex]'
  ];

  const allInteractive = preview.querySelectorAll(interactiveSelectors.join(','));

  allInteractive.forEach((el, index) => {
    const rect = el.getBoundingClientRect();
    // Get relative position within preview panel
    const relX = rect.left - panelRect.left;
    const relY = rect.top - panelRect.top;

    elements.push({
      index,
      tag: el.tagName.toLowerCase(),
      text: (el.textContent || '').trim().substring(0, 50),
      role: el.getAttribute('role'),
      classes: el.className,
      // Bounding box relative to preview panel
      bbox: {
        x: Math.round(relX),
        y: Math.round(relY),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
        // Absolute position for clicking
        absX: Math.round(rect.left + rect.width / 2),
        absY: Math.round(rect.top + rect.height / 2)
      },
      // Generate a unique selector
      selector: generateSelector(el, preview)
    });
  });

  // Also get all text nodes with their positions
  const textNodes = [];
  const walker = document.createTreeWalker(preview, NodeFilter.SHOW_TEXT, null, false);
  let node;
  while (node = walker.nextNode()) {
    const text = node.textContent.trim();
    if (text) {
      const range = document.createRange();
      range.selectNodeContents(node);
      const rect = range.getBoundingClientRect();
      if (rect.width > 0 && rect.height > 0) {
        textNodes.push({
          text: text.substring(0, 100),
          bbox: {
            x: Math.round(rect.left - panelRect.left),
            y: Math.round(rect.top - panelRect.top),
            width: Math.round(rect.width),
            height: Math.round(rect.height)
          }
        });
      }
    }
  }

  return {
    type: 'previewElements',
    data: {
      panelSize: {
        width: Math.round(panelRect.width),
        height: Math.round(panelRect.height)
      },
      elements,
      textNodes,
      html: preview.innerHTML.substring(0, 2000)
    }
  };
}

// Helper to generate a unique selector for an element
function generateSelector(el, container) {
  // Try ID first
  if (el.id) return `#${el.id}`;

  // Try unique class within container
  if (el.className && typeof el.className === 'string') {
    const classes = el.className.split(' ').filter(c => c && !c.startsWith('_'));
    for (const cls of classes) {
      if (container.querySelectorAll(`.${cls}`).length === 1) {
        return `.${cls}`;
      }
    }
  }

  // Try role + text content
  const role = el.getAttribute('role');
  if (role) {
    const text = (el.textContent || '').trim();
    if (text && text.length < 30) {
      // This is just informational - actual click should use coordinates
      return `[role="${role}"] containing "${text}"`;
    }
    return `[role="${role}"]`;
  }

  // Try data-boon-panel child path
  const boonPanel = el.closest('[data-boon-panel="preview"]');
  if (boonPanel) {
    return `[data-boon-panel="preview"] ${el.tagName.toLowerCase()}`;
  }

  return el.tagName.toLowerCase();
}

function clickAtCoordinates(x, y) {
  const element = document.elementFromPoint(x, y);
  if (!element) {
    return { type: 'error', message: `No element at coordinates (${x}, ${y})` };
  }

  // Create and dispatch proper mouse events
  const events = ['mousedown', 'mouseup', 'click'];
  for (const eventType of events) {
    const event = new MouseEvent(eventType, {
      bubbles: true,
      cancelable: true,
      view: window,
      clientX: x,
      clientY: y
    });
    element.dispatchEvent(event);
  }

  return {
    type: 'success',
    data: {
      clickedTag: element.tagName.toLowerCase(),
      clickedText: (element.textContent || '').trim().substring(0, 50)
    }
  };
}

function getDOMStructure(selector, maxDepth = 4) {
  const root = selector ? document.querySelector(selector) : document.body;
  if (!root) {
    return { type: 'error', message: `Element not found: ${selector}` };
  }

  function describeElement(el, depth = 0) {
    if (depth > maxDepth) return '...';
    if (el.nodeType === Node.TEXT_NODE) {
      const text = el.textContent.trim();
      return text ? `"${text.substring(0, 50)}${text.length > 50 ? '...' : ''}"` : null;
    }
    if (el.nodeType !== Node.ELEMENT_NODE) return null;

    const tag = el.tagName.toLowerCase();
    const id = el.id ? `#${el.id}` : '';
    const classes = el.className && typeof el.className === 'string'
      ? '.' + el.className.split(' ').filter(c => c).join('.')
      : '';
    const attrs = [];
    for (const attr of el.attributes) {
      if (attr.name !== 'id' && attr.name !== 'class' && attr.name !== 'style') {
        attrs.push(`${attr.name}="${attr.value.substring(0, 30)}"`);
      }
    }
    const attrStr = attrs.length ? ` [${attrs.join(', ')}]` : '';

    const children = [];
    for (const child of el.childNodes) {
      const desc = describeElement(child, depth + 1);
      if (desc) children.push(desc);
    }

    const childStr = children.length
      ? (children.length <= 3
          ? ` { ${children.join(', ')} }`
          : ` { ${children.slice(0, 3).join(', ')}, ...(${children.length} total) }`)
      : '';

    return `${tag}${id}${classes}${attrStr}${childStr}`;
  }

  const structure = describeElement(root);
  return { type: 'dom', structure };
}

function scrollPreview(y, delta, toBottom) {
  // Find the preview panel - try various selectors
  const preview = document.querySelector('.preview-panel') ||
                  document.querySelector('[data-panel="preview"]') ||
                  document.querySelector('#preview') ||
                  document.querySelector('.preview');

  if (!preview) {
    return { type: 'error', message: 'Could not find preview panel to scroll' };
  }

  if (toBottom) {
    preview.scrollTop = preview.scrollHeight;
    return { type: 'success', data: { scrollTop: preview.scrollTop } };
  }

  if (y !== undefined && y !== null) {
    preview.scrollTop = y;
    return { type: 'success', data: { scrollTop: preview.scrollTop } };
  }

  if (delta !== undefined && delta !== null) {
    preview.scrollBy(0, delta);
    return { type: 'success', data: { scrollTop: preview.scrollTop } };
  }

  return { type: 'success', data: { scrollTop: preview.scrollTop, scrollHeight: preview.scrollHeight } };
}

// Service worker lifecycle handlers - critical for MV3 reliability
chrome.runtime.onStartup.addListener(() => {
  console.log('[Boon] Service worker started (onStartup), connecting...');
  connect();
});

chrome.runtime.onInstalled.addListener(() => {
  console.log('[Boon] Extension installed/updated (onInstalled), connecting...');
  connect();
  // Set up the alarm for keep-alive
  setupKeepAliveAlarm();
});

// Set up chrome.alarms for reliable keep-alive (survives service worker restart)
function setupKeepAliveAlarm() {
  // Create alarm that fires every 24 seconds (under the 30s MV3 limit)
  chrome.alarms.create('boonKeepAlive', { periodInMinutes: 0.4 });
  console.log('[Boon] Keep-alive alarm created');
}

// Handle alarm events
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === 'boonKeepAlive') {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      console.log('[Boon] Alarm: WebSocket not connected, reconnecting...');
      connect();
    } else {
      // Send keep-alive ping
      safeSend({ type: 'keepAlive' });
      console.log('[Boon] Sent keep-alive ping via alarm');
    }
  }
});

// Register content script that runs at document_start to capture console early
// This uses the scripting API to register a MAIN world script
async function registerEarlyConsoleCapture() {
  try {
    // First unregister any existing scripts to avoid duplicates
    try {
      await chrome.scripting.unregisterContentScripts({ ids: ['boon-console-capture'] });
    } catch (e) {
      // Ignore if not registered
    }

    await chrome.scripting.registerContentScripts([{
      id: 'boon-console-capture',
      matches: ['http://localhost/*'],
      js: ['console-capture.js'],
      runAt: 'document_start',
      world: 'MAIN'
    }]);
    console.log('[Boon] Early console capture registered');
  } catch (e) {
    console.log('[Boon] Could not register early console capture:', e.message);
  }
}

// Monitor tab activations to detect playground port changes
chrome.tabs.onActivated.addListener(async (activeInfo) => {
  try {
    const tab = await chrome.tabs.get(activeInfo.tabId);
    const port = extractPortFromUrl(tab.url);
    if (port && port !== activePlaygroundPort) {
      console.log(`[Boon] Tab activated with playground port ${port}, WS port: ${deriveWsPort(port)}`);
      await persistPlaygroundPort(port);
      // Reconnect WS if port changed
      if (ws) { ws.close(); ws = null; }
      connect();
    }
  } catch (e) {
    // Tab may have been closed
  }
});

// Initialize on service worker load
console.log('[Boon] Service worker loading...');
// Restore persisted port before connecting
restorePersistedPort().then(() => {
  connect();
});
registerEarlyConsoleCapture();
setupKeepAliveAlarm();
