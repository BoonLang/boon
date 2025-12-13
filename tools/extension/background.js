// Boon Browser Control - Background Service Worker
// Connects to WebSocket server and routes commands to content script

const WS_URL = 'ws://127.0.0.1:9222';
let ws = null;
let reconnectTimer = null;
let contentPort = null;
let pendingRequests = new Map();

// Exponential backoff for reconnection
let reconnectAttempts = 0;
const MAX_RECONNECT_DELAY = 30000; // 30 seconds max

// ============ CDP INFRASTRUCTURE ============
// Chrome DevTools Protocol for trusted events (isTrusted: true)

let debuggerAttached = new Map(); // tabId -> boolean
let cdpConsoleMessages = new Map(); // tabId -> messages[]

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
    if (messages.length > 100) messages.shift();
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
    if (messages.length > 100) messages.shift();
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

// ============ CDP OPERATIONS ============

// Click at coordinates (trusted event)
// NOTE: CDP mouse events may not trigger Zoon button handlers directly because
// Zoon uses pointer events. We use a hybrid approach: CDP for coordinates,
// then JS dispatch for pointer events.
// IMPORTANT: x,y can be either page coordinates (from DOM.getBoxModel) or viewport coordinates.
// CDP Input.dispatchMouseEvent expects viewport coordinates, so we convert if needed.
async function cdpClickAt(tabId, x, y) {
  await attachDebugger(tabId);

  // Get scroll offset to convert page coords to viewport coords for CDP events
  const scrollOffset = await cdpEvaluate(tabId, `({ scrollX: window.scrollX, scrollY: window.scrollY })`);
  const viewportX = x - (scrollOffset?.scrollX || 0);
  const viewportY = y - (scrollOffset?.scrollY || 0);

  // First, move mouse to position (some UIs require this)
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseMoved', x: viewportX, y: viewportY, button: 'none'
  });

  // Then send mouse press/release
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mousePressed', x: viewportX, y: viewportY, button: 'left', clickCount: 1
  });
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseReleased', x: viewportX, y: viewportY, button: 'left', clickCount: 1
  });

  // Also dispatch pointer events via JS since Zoon uses pointer events
  // This ensures Zoon button handlers are triggered
  // NOTE: x,y from DOM.getBoxModel are page coordinates, but elementFromPoint needs viewport coordinates
  await cdpEvaluate(tabId, `
    (function() {
      // Convert page coordinates to viewport coordinates
      const viewportX = ${x} - window.scrollX;
      const viewportY = ${y} - window.scrollY;
      const el = document.elementFromPoint(viewportX, viewportY);
      if (!el) {
        console.warn('[Boon] No element at viewport coords:', viewportX, viewportY, 'page coords:', ${x}, ${y});
        return;
      }

      // Dispatch pointer events (what Zoon listens to)
      ['pointerdown', 'pointerup'].forEach(type => {
        el.dispatchEvent(new PointerEvent(type, {
          bubbles: true, cancelable: true, view: window,
          clientX: viewportX, clientY: viewportY,
          pointerId: 1, pointerType: 'mouse', isPrimary: true
        }));
      });

      // Also dispatch click for good measure
      el.dispatchEvent(new MouseEvent('click', {
        bubbles: true, cancelable: true, view: window,
        clientX: viewportX, clientY: viewportY
      }));
    })()
  `);

  console.log(`[Boon] CDP: Clicked at (${x}, ${y}) with pointer events`);
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

// Type text (as if typing on keyboard - insertText is fast)
async function cdpTypeText(tabId, text) {
  await attachDebugger(tabId);
  await chrome.debugger.sendCommand({ tabId }, 'Input.insertText', { text });
}

// Press special key (Enter, Tab, Escape, etc.) using CDP Input.dispatchKeyEvent
// NOTE: This may not trigger JavaScript event listeners attached via web_sys
async function cdpPressKey(tabId, key, modifiers = 0) {
  await attachDebugger(tabId);

  const keyMap = {
    'Enter': { key: 'Enter', code: 'Enter', keyCode: 13, windowsVirtualKeyCode: 13 },
    'Tab': { key: 'Tab', code: 'Tab', keyCode: 9, windowsVirtualKeyCode: 9 },
    'Escape': { key: 'Escape', code: 'Escape', keyCode: 27, windowsVirtualKeyCode: 27 },
    'Backspace': { key: 'Backspace', code: 'Backspace', keyCode: 8, windowsVirtualKeyCode: 8 },
    'Delete': { key: 'Delete', code: 'Delete', keyCode: 46, windowsVirtualKeyCode: 46 },
    'a': { key: 'a', code: 'KeyA', keyCode: 65, windowsVirtualKeyCode: 65 },
  };

  const keyInfo = keyMap[key] || { key, code: key, keyCode: 0, windowsVirtualKeyCode: 0 };

  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', {
    type: 'keyDown', ...keyInfo, modifiers
  });
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchKeyEvent', {
    type: 'keyUp', ...keyInfo, modifiers
  });
}

// Press special key using JavaScript dispatchEvent (triggers web_sys event listeners)
async function jsDispatchKeyEvent(tabId, key) {
  const keyMap = {
    'Enter': { key: 'Enter', code: 'Enter', keyCode: 13, which: 13 },
    'Tab': { key: 'Tab', code: 'Tab', keyCode: 9, which: 9 },
    'Escape': { key: 'Escape', code: 'Escape', keyCode: 27, which: 27 },
    'Backspace': { key: 'Backspace', code: 'Backspace', keyCode: 8, which: 8 },
    'Delete': { key: 'Delete', code: 'Delete', keyCode: 46, which: 46 },
  };

  const keyInfo = keyMap[key] || { key, code: key, keyCode: 0, which: 0 };

  // Dispatch keydown event on the focused element
  const script = `
    (function() {
      const target = document.activeElement || document.body;
      const event = new KeyboardEvent('keydown', {
        key: '${keyInfo.key}',
        code: '${keyInfo.code}',
        keyCode: ${keyInfo.keyCode},
        which: ${keyInfo.which},
        bubbles: true,
        cancelable: true,
        composed: true
      });
      target.dispatchEvent(event);
      return { target: target.tagName, key: '${keyInfo.key}' };
    })()
  `;

  return await cdpEvaluate(tabId, script);
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

  const { root } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getDocument');
  const { nodeIds } = await chrome.debugger.sendCommand({ tabId }, 'DOM.querySelectorAll', {
    nodeId: root.nodeId, selector
  });

  const elements = [];
  for (const nodeId of nodeIds) {
    try {
      const { model } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getBoxModel', { nodeId });
      const content = model.content;

      // Get node info
      const { outerHTML } = await chrome.debugger.sendCommand({ tabId }, 'DOM.getOuterHTML', { nodeId });

      elements.push({
        nodeId,
        x: content[0],
        y: content[1],
        width: content[2] - content[0],
        height: content[5] - content[1],
        centerX: (content[0] + content[2]) / 2,
        centerY: (content[1] + content[5]) / 2,
        html: outerHTML.substring(0, 200)
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
function connect() {
  // Check both CONNECTING and OPEN states to avoid race conditions
  if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) {
    return;
  }

  console.log('[Boon] Connecting to WebSocket server...');

  try {
    ws = new WebSocket(WS_URL);
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
    // Get the active tab with localhost:8081
    const tabs = await chrome.tabs.query({ url: 'http://localhost:8081/*' });

    if (tabs.length === 0) {
      return { type: 'error', message: 'No Boon Playground tab found' };
    }

    const tab = tabs[0];

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

      case 'type':
        // Use CDP: focus element, then type
        try {
          await cdpFocusElement(tab.id, command.selector);
          await cdpKeyboardShortcut(tab.id, 'a', true); // Ctrl+A to select all
          await cdpTypeText(tab.id, command.text);
          return { type: 'success', data: null };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'key':
        // Press a special key (Enter, Tab, Escape, etc.)
        // Use JavaScript dispatchEvent to properly trigger web_sys event listeners
        try {
          const result = await jsDispatchKeyEvent(tab.id, command.key);
          return { type: 'success', data: result };
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
        try {
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

      case 'getPreviewText':
        // Use CDP Runtime.evaluate
        try {
          const text = await cdpEvaluate(tab.id,
            `document.querySelector('[data-boon-panel="preview"]')?.textContent || ''`);
          return { type: 'previewText', text };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'getPreviewElements':
        // Use CDP to get elements with bounding boxes
        try {
          const elements = await cdpQuerySelectorAll(tab.id,
            '[data-boon-panel="preview"] [role="button"], [data-boon-panel="preview"] button');
          return { type: 'previewElements', data: { elements } };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'clearStates':
        // Use CDP Runtime.evaluate to find and click the Clear saved states button
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              // Find all elements that might be buttons
              const elements = document.querySelectorAll('[role="button"], button');
              for (const el of elements) {
                const text = (el.textContent || '').toLowerCase();
                if (text.includes('clear') && text.includes('saved') && text.includes('states')) {
                  // Found the button - dispatch pointer events (Zoon uses pointer events)
                  const rect = el.getBoundingClientRect();
                  const x = rect.left + rect.width / 2;
                  const y = rect.top + rect.height / 2;

                  el.dispatchEvent(new PointerEvent('pointerdown', {
                    bubbles: true,
                    cancelable: true,
                    view: window,
                    clientX: x,
                    clientY: y,
                    button: 0,
                    pointerType: 'mouse'
                  }));
                  el.dispatchEvent(new PointerEvent('pointerup', {
                    bubbles: true,
                    cancelable: true,
                    view: window,
                    clientX: x,
                    clientY: y,
                    button: 0,
                    pointerType: 'mouse'
                  }));
                  return { clicked: true, text: text.trim() };
                }
              }
              return { clicked: false, found: elements.length };
            })()
          `);
          if (result && result.clicked) {
            return { type: 'success', data: { method: 'cdp-evaluate', text: result.text } };
          }
          return { type: 'error', message: `Clear saved states button not found (searched ${result?.found || 0} elements)` };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'selectExample':
        // Select an example by name (e.g., "todo_mvc.bn", "counter.bn")
        // Uses CDP Runtime.evaluate to find and click the tab reliably
        try {
          const exampleName = command.name;
          const result = await cdpEvaluate(tab.id, `
            (function() {
              // Find all example tabs in the header
              const tabs = document.querySelectorAll('[role="button"], button');
              for (const tab of tabs) {
                const text = (tab.textContent || '').trim();
                if (text === '${exampleName}' || text === '${exampleName.replace('.bn', '')}') {
                  // Found the tab - dispatch pointer events (Zoon uses pointer events)
                  const rect = tab.getBoundingClientRect();
                  const x = rect.left + rect.width / 2;
                  const y = rect.top + rect.height / 2;

                  ['pointerdown', 'pointerup'].forEach(type => {
                    tab.dispatchEvent(new PointerEvent(type, {
                      bubbles: true, cancelable: true, view: window,
                      clientX: x, clientY: y,
                      pointerId: 1, pointerType: 'mouse', isPrimary: true
                    }));
                  });

                  tab.dispatchEvent(new MouseEvent('click', {
                    bubbles: true, cancelable: true, view: window,
                    clientX: x, clientY: y
                  }));

                  return { found: true, text: text };
                }
              }
              return { found: false, available: Array.from(tabs).map(t => (t.textContent || '').trim()).filter(t => t.endsWith('.bn')) };
            })()
          `);

          if (result && result.found) {
            return { type: 'success', data: { selected: result.text } };
          } else {
            const available = result?.available?.join(', ') || 'unknown';
            return { type: 'error', message: `Example '${exampleName}' not found. Available: ${available}` };
          }
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
  el.dispatchEvent(new Event('input', { bubbles: true }));
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
    return { type: 'previewText', text: preview.textContent || '' };
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

// Clear saved states by clicking the "Clear saved states" button
function clearSavedStates() {
  // Normalize text for comparison (trim and collapse whitespace)
  function normalizeText(text) {
    return (text || '').replace(/\s+/g, ' ').trim().toLowerCase();
  }

  // Look for the clear states button
  const clearButton = document.querySelector('[data-action="clear-states"]') ||
                      Array.from(document.querySelectorAll('button')).find(btn => {
                        const text = normalizeText(btn.textContent);
                        return text === 'clear saved states' || text.includes('clear saved states');
                      });
  if (clearButton) {
    clearButton.click();
    return { type: 'success', data: null };
  }

  // Fallback: try to find by partial match on any clickable element
  const allElements = document.querySelectorAll('button, [role="button"], [onclick]');
  for (const el of allElements) {
    const text = normalizeText(el.textContent);
    if (text.includes('clear') && text.includes('states')) {
      el.click();
      return { type: 'success', data: { foundBy: 'partial match', text: el.textContent.trim() } };
    }
  }

  return { type: 'error', message: 'Could not find Clear saved states button' };
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
      matches: ['http://localhost:8081/*'],
      js: ['console-capture.js'],
      runAt: 'document_start',
      world: 'MAIN'
    }]);
    console.log('[Boon] Early console capture registered');
  } catch (e) {
    console.log('[Boon] Could not register early console capture:', e.message);
  }
}

// Initialize on service worker load
console.log('[Boon] Service worker loading...');
connect();
registerEarlyConsoleCapture();
setupKeepAliveAlarm();
