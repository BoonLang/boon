// Boon Browser Control - Background Service Worker
// Connects to WebSocket server and routes commands to content script

const WS_URL = 'ws://127.0.0.1:9223';
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

// ============ CDP OPERATIONS ============

// Click at coordinates using JavaScript events
// NOTE: We do NOT use CDP Input.dispatchMouseEvent for clicks because CDP mouse events
// trigger click handlers via the browser, AND we also dispatch JS click events for
// cross-browser compatibility. This causes double-firing for Zoon buttons.
// Instead, we just dispatch a single JS click event like clickButton does.
// IMPORTANT: x,y are page coordinates (from DOM.getBoxModel).
async function cdpClickAt(tabId, x, y) {
  await attachDebugger(tabId);

  // Dispatch only a JavaScript click event on the element at the coordinates.
  // This matches the approach used by clickButton which works correctly.
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

      // Dispatch a single click event (no CDP mouse events to avoid double-firing)
      el.dispatchEvent(new MouseEvent('click', {
        bubbles: true, cancelable: true, view: window,
        clientX: viewportX, clientY: viewportY,
        detail: 1
      }));
      console.log('[Boon] Dispatched JS click event on:', el.tagName, el.className || '(no class)');
    })()
  `);

  console.log(`[Boon] Clicked at (${x}, ${y})`);
}

// Double-click at coordinates (trusted event)
async function cdpDoubleClickAt(tabId, x, y) {
  await attachDebugger(tabId);

  // Get scroll offset to convert page coords to viewport coords for CDP events
  const scrollOffset = await cdpEvaluate(tabId, `({ scrollX: window.scrollX, scrollY: window.scrollY })`);
  const viewportX = x - (scrollOffset?.scrollX || 0);
  const viewportY = y - (scrollOffset?.scrollY || 0);

  // First, move mouse to position
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseMoved', x: viewportX, y: viewportY, button: 'none'
  });

  // Send double-click with clickCount: 2
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mousePressed', x: viewportX, y: viewportY, button: 'left', clickCount: 2
  });
  await chrome.debugger.sendCommand({ tabId }, 'Input.dispatchMouseEvent', {
    type: 'mouseReleased', x: viewportX, y: viewportY, button: 'left', clickCount: 2
  });

  // Also dispatch dblclick event via JS since Zoon may use it
  // AND manage hover state tracking (same as cdpHoverAt) to ensure proper mouseleave later
  await cdpEvaluate(tabId, `
    (function() {
      const viewportX = ${x} - window.scrollX;
      const viewportY = ${y} - window.scrollY;
      const el = document.elementFromPoint(viewportX, viewportY);
      if (!el) {
        console.warn('[Boon] No element at viewport coords for dblclick:', viewportX, viewportY);
        return;
      }

      // Dispatch dblclick event
      el.dispatchEvent(new MouseEvent('dblclick', {
        bubbles: true, cancelable: true, view: window,
        clientX: viewportX, clientY: viewportY,
        detail: 2
      }));

      // === HOVER STATE TRACKING ===
      // This ensures that when we later hover elsewhere, proper mouseleave events are dispatched.
      // Without this, double-click leaves hover state corrupted.

      // Get previous hovered elements (all ancestors that received mouseenter)
      const prevElements = window.__boonHoveredElements || [];

      // Collect new hover path (from target up to document)
      const newElements = [];
      let current = el;
      while (current && current !== document) {
        newElements.push(current);
        current = current.parentElement;
      }

      // Find elements to leave (in prevElements but not in newElements)
      const toLeave = prevElements.filter(el => !newElements.includes(el));

      // Find elements to enter (in newElements but not in prevElements)
      const toEnter = newElements.filter(el => !prevElements.includes(el));

      // Dispatch mouseleave on elements we're leaving (from innermost to outermost)
      for (const target of toLeave) {
        target.dispatchEvent(new MouseEvent('mouseleave', {
          bubbles: false, cancelable: false, view: window,
          clientX: viewportX, clientY: viewportY,
          relatedTarget: el
        }));
      }

      // Dispatch mouseenter on elements we're entering (from outermost to innermost)
      for (let i = toEnter.length - 1; i >= 0; i--) {
        toEnter[i].dispatchEvent(new MouseEvent('mouseenter', {
          bubbles: false, cancelable: false, view: window,
          clientX: viewportX, clientY: viewportY,
          relatedTarget: null
        }));
      }

      // Update tracked elements
      window.__boonHoveredElements = newElements;
    })()
  `);

  console.log(`[Boon] CDP: Double-clicked at (${x}, ${y}) with hover state tracking`);
}

// Hover at coordinates (move mouse without clicking)
// Uses CDP mouse movement + JavaScript mouseenter/mouseleave dispatch.
// CDP mouseMoved positions the pointer but doesn't fire JS mouseenter/mouseleave events.
// Zoon/dominator uses MouseEnter/MouseLeave events and doesn't check isTrusted,
// so we dispatch synthetic JS events to trigger on_hovered_change callbacks.
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

  // Dispatch JavaScript mouseenter/mouseleave events to trigger Zoon's on_hovered_change
  // Zoon/dominator doesn't check isTrusted, so synthetic events work
  await cdpEvaluate(tabId, `
    (function() {
      const viewportX = ${viewportX};
      const viewportY = ${viewportY};
      const newTarget = document.elementFromPoint(viewportX, viewportY);

      // Get previous hovered elements (all ancestors that received mouseenter)
      // Filter out any elements that are no longer in the document (e.g., after page navigation)
      const prevElements = (window.__boonHoveredElements || []).filter(el => document.contains(el));

      // Collect all ancestors of newTarget including itself
      // If newTarget is null (hovering empty area), newElements stays empty
      // which will cause mouseleave on all previously hovered elements
      const newElements = [];
      if (newTarget) {
        let el = newTarget;
        while (el && el !== document.documentElement) {
          newElements.push(el);
          el = el.parentElement;
        }
      }

      // Find elements to leave (in prevElements but not in newElements)
      const toLeave = prevElements.filter(el => !newElements.includes(el));

      // Find elements to enter (in newElements but not in prevElements)
      const toEnter = newElements.filter(el => !prevElements.includes(el));

      // Dispatch leave events on elements we're leaving (from innermost to outermost)
      // Dispatch both pointer and mouse events for broader compatibility
      for (const el of toLeave) {
        el.dispatchEvent(new PointerEvent('pointerleave', {
          bubbles: false, cancelable: false, view: window,
          clientX: viewportX, clientY: viewportY,
          relatedTarget: newTarget, pointerType: 'mouse', isPrimary: true
        }));
        el.dispatchEvent(new MouseEvent('mouseleave', {
          bubbles: false, cancelable: false, view: window,
          clientX: viewportX, clientY: viewportY,
          relatedTarget: newTarget
        }));
      }

      // Dispatch enter events on elements we're entering (from outermost to innermost)
      // Dispatch both pointer and mouse events for broader compatibility
      for (let i = toEnter.length - 1; i >= 0; i--) {
        toEnter[i].dispatchEvent(new PointerEvent('pointerenter', {
          bubbles: false, cancelable: false, view: window,
          clientX: viewportX, clientY: viewportY,
          relatedTarget: prevElements[0] || null, pointerType: 'mouse', isPrimary: true
        }));
        toEnter[i].dispatchEvent(new MouseEvent('mouseenter', {
          bubbles: false, cancelable: false, view: window,
          clientX: viewportX, clientY: viewportY,
          relatedTarget: prevElements[0] || null
        }));
      }

      // Update tracked elements
      window.__boonHoveredElements = newElements;

      return {
        success: true,
        entered: toEnter.length,
        left: toLeave.length,
        newTarget: newTarget ? newTarget.tagName : null
      };
    })()
  `);

  // Small delay for event handlers to process
  await new Promise(r => setTimeout(r, 50));

  console.log(`[Boon] CDP: Hovered at (${x}, ${y}) with mouseenter/mouseleave dispatch`);
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
  // Also try to find and dispatch on any input element in the preview pane
  const script = `
    (function() {
      // First try document.activeElement
      let target = document.activeElement;

      // If activeElement is body or not an input, try to find the input in preview
      if (!target || target === document.body || target.tagName !== 'INPUT') {
        const previewPane = document.querySelector('.preview-pane') || document.querySelector('[class*="preview"]');
        if (previewPane) {
          const input = previewPane.querySelector('input');
          if (input) {
            target = input;
            input.focus();
          }
        }
      }

      // Fallback to any input on the page
      if (!target || target === document.body) {
        target = document.querySelector('input') || document.body;
      }

      console.log('[boon-tools] Dispatching keydown on:', target.tagName, target.className);

      const event = new KeyboardEvent('keydown', {
        key: '${keyInfo.key}',
        code: '${keyInfo.code}',
        keyCode: ${keyInfo.keyCode},
        which: ${keyInfo.which},
        bubbles: true,
        cancelable: true,
        composed: true,
        view: window
      });

      target.dispatchEvent(event);
      return { target: target.tagName, className: target.className, key: '${keyInfo.key}' };
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
    // Get the active tab with localhost:8083
    const tabs = await chrome.tabs.query({ url: 'http://localhost:8083/*' });

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

      case 'hoverAt':
        // Use CDP to move mouse without clicking (trigger hover)
        await cdpHoverAt(tab.id, command.x, command.y);
        return { type: 'success', data: { x: command.x, y: command.y, method: 'cdp' } };

      case 'doubleClickAt':
        // Use CDP for trusted double-click at coordinates
        await cdpDoubleClickAt(tab.id, command.x, command.y);
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
        // Clear ALL localStorage to ensure a clean slate for tests
        // This fixes quota exceeded errors from accumulated debug logs
        try {
          const result = await cdpEvaluate(tab.id, `
            (function() {
              // Step 1: Invalidate all running timers (engine-v2)
              // This prevents old timers from saving state while we're clearing it
              let invalidated = false;
              if (window.boonPlayground && window.boonPlayground.invalidateTimers) {
                window.boonPlayground.invalidateTimers();
                invalidated = true;
                console.log('[Boon clearStates] invalidateTimers() called');
              } else {
                console.warn('[Boon clearStates] invalidateTimers not available:', {
                  hasBoonPlayground: !!window.boonPlayground,
                  hasInvalidateTimers: !!(window.boonPlayground && window.boonPlayground.invalidateTimers)
                });
              }

              // Step 2: Clear ALL localStorage for a completely fresh state
              // This clears everything including:
              // - Playground states and project files
              // - Debug logs that can fill up quota
              // - Any other accumulated data
              const keyCount = localStorage.length;
              localStorage.clear();

              return { cleared: keyCount, invalidated };
            })()
          `);
          return { type: 'success', data: { method: 'cdp-evaluate', text: 'clear saved states' } };
        } catch (e) {
          return { type: 'error', message: e.message };
        }

      case 'selectExample':
        // Select an example by name (e.g., "todo_mvc.bn", "counter.bn")
        // Uses CDP trusted clicks (Input.dispatchMouseEvent) for proper Zoon event handling
        try {
          const exampleName = command.name;
          // First, find the example tab and get its coordinates
          const result = await cdpEvaluate(tab.id, `
            (function() {
              // Find all example tabs in the header
              const tabs = document.querySelectorAll('[role="button"], button');
              for (const tab of tabs) {
                const text = (tab.textContent || '').trim();
                if (text === '${exampleName}' || text === '${exampleName.replace('.bn', '')}') {
                  // Found the tab - return its center coordinates (page coords)
                  const rect = tab.getBoundingClientRect();
                  return {
                    found: true,
                    text: text,
                    x: rect.left + rect.width / 2 + window.scrollX,
                    y: rect.top + rect.height / 2 + window.scrollY
                  };
                }
              }
              return { found: false, available: Array.from(tabs).map(t => (t.textContent || '').trim()).filter(t => t.endsWith('.bn')) };
            })()
          `);

          if (result && result.found) {
            // Use CDP trusted click (same as cdpClickAt) for proper event handling
            await cdpClickAt(tab.id, result.x, result.y);
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

              // Sort by vertical position (top to bottom) for consistent ordering
              allCheckboxes.sort((a, b) => {
                const rectA = a.getBoundingClientRect();
                const rectB = b.getBoundingClientRect();
                return rectA.top - rectB.top;
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

          // Click directly on the checkbox element using its ID or position
          const clickResult = await cdpEvaluate(tab.id, `
            (function() {
              const preview = document.querySelector('[data-boon-panel="preview"]');
              if (!preview) return { error: 'Preview panel not found' };

              // Find checkbox by ID if available, otherwise by index in sorted list
              let checkbox = null;
              const checkboxId = ${JSON.stringify(checkbox.id)};
              if (checkboxId) {
                checkbox = document.getElementById(checkboxId);
              }

              if (!checkbox) {
                // Fallback: find by position in sorted list
                const roleCheckboxes = Array.from(preview.querySelectorAll('[role="checkbox"]'));
                const idCheckboxes = Array.from(preview.querySelectorAll('[id^="cb-"]'));
                const seen = new Set();
                const allCheckboxes = [];
                roleCheckboxes.forEach(el => { seen.add(el); allCheckboxes.push(el); });
                idCheckboxes.forEach(el => { if (!seen.has(el)) { seen.add(el); allCheckboxes.push(el); } });
                allCheckboxes.sort((a, b) => a.getBoundingClientRect().top - b.getBoundingClientRect().top);
                checkbox = allCheckboxes[${checkboxIndex}];
              }

              if (!checkbox) return { error: 'Checkbox not found' };

              // Get info about what we're clicking
              const rect = checkbox.getBoundingClientRect();
              const elementAtCenter = document.elementFromPoint(rect.x + rect.width/2, rect.y + rect.height/2);

              // Dispatch click event directly on the checkbox element
              const clickEvent = new MouseEvent('click', {
                bubbles: true,
                cancelable: true,
                view: window,
                clientX: rect.x + rect.width/2,
                clientY: rect.y + rect.height/2
              });
              checkbox.dispatchEvent(clickEvent);

              return {
                success: true,
                id: checkbox.id,
                rect: { x: rect.x, y: rect.y, w: rect.width, h: rect.height },
                elementAtCenter: elementAtCenter?.tagName,
                elementAtCenterRole: elementAtCenter?.getAttribute('role')
              };
            })()
          `);

          if (clickResult.error) {
            return { type: 'error', message: clickResult.error };
          }

          return { type: 'success', data: {
            index: checkboxIndex,
            id: checkbox.id,
            text: checkbox.text,
            x: checkbox.centerX,
            y: checkbox.centerY,
            width: checkbox.width,
            height: checkbox.height,
            clickResult
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

              // Debug: check what element is at the click coordinates
              const elementAtPoint = document.elementFromPoint(centerX, centerY);
              const elementAtPointInfo = elementAtPoint ? {
                tag: elementAtPoint.tagName,
                role: elementAtPoint.getAttribute('role'),
                text: (elementAtPoint.textContent || '').substring(0, 20),
                isSameAsButton: elementAtPoint === button
              } : null;

              // Dispatch click directly on button element (same as checkbox approach)
              const clickEvent = new MouseEvent('click', {
                bubbles: true,
                cancelable: true,
                view: window,
                clientX: centerX,
                clientY: centerY
              });
              button.dispatchEvent(clickEvent);

              // Also log that we dispatched the event
              console.log('[Boon Debug] Dispatched click on button:', {
                index: buttonIndex,
                text: text,
                role: button.getAttribute('role'),
                tagName: button.tagName,
                className: button.className
              });

              return {
                success: true,
                index: buttonIndex,
                text: text,
                rect: { x: rect.x, y: rect.y, w: rect.width, h: rect.height },
                role: button.getAttribute('role'),
                centerX: centerX,
                centerY: centerY,
                elementAtPoint: elementAtPointInfo
              };
            })()
          `);

          if (clickResult.error) {
            return { type: 'error', message: clickResult.error };
          }

          return { type: 'success', data: clickResult };
        } catch (e) {
          return { type: 'error', message: `Click button failed: ${e.message}` };
        }

      case 'clickByText':
        // Click any element by its text content (more flexible than clickButton)
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

              if (bestMatch) {
                return { found: true, element: bestMatch };
              }
              return { found: false, error: 'No element found with text: ' + searchText };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Element not found' };
          }

          const element = result.element;
          await cdpClickAt(tab.id, element.centerX, element.centerY);
          return { type: 'success', data: { text: element.text, x: element.centerX, y: element.centerY } };
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

          // Step 3: Now find and click the button
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
                    if (btnRect.width > 0 && btnRect.height > 0) {
                      const btnStyle = window.getComputedStyle(btn);
                      if (btnStyle.display !== 'none' && btnStyle.visibility !== 'hidden') {
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

              const btnRect = foundButton.getBoundingClientRect();
              return {
                found: true,
                text: searchText,
                buttonText: buttonText,
                centerX: btnRect.x + btnRect.width / 2,
                centerY: btnRect.y + btnRect.height / 2,
                buttonRect: { x: btnRect.x, y: btnRect.y, w: btnRect.width, h: btnRect.height }
              };
            })()
          `);

          if (!clickResult.found) {
            return { type: 'error', message: clickResult.error || 'Button not found after hover' };
          }

          // Step 4: Click the button
          await cdpClickAt(tab.id, clickResult.centerX, clickResult.centerY);
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
              input.focus();
              input.click();

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

          // Also click via CDP to ensure focus
          await cdpClickAt(tab.id, result.x, result.y);
          return { type: 'success', data: result };
        } catch (e) {
          return { type: 'error', message: `Focus input failed: ${e.message}` };
        }

      case 'typeText':
        // Type text into the currently focused element using CDP
        try {
          await cdpTypeText(tab.id, command.text);
          return { type: 'success', data: { text: command.text } };
        } catch (e) {
          return { type: 'error', message: `Type text failed: ${e.message}` };
        }

      case 'pressKey':
        // Press a special key using JavaScript dispatchEvent (triggers web_sys listeners)
        try {
          const result = await jsDispatchKeyEvent(tab.id, command.key);
          return { type: 'success', data: result };
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

              // If activeElement is body, try fallback detection methods
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
                  // Try autofocus attribute as last resort
                  if (!focused) {
                    focused = preview.querySelector('[autofocus]');
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
                    bestMatch = {
                      text: directText,
                      centerX: Math.round(rect.x + rect.width / 2),
                      centerY: Math.round(rect.y + rect.height / 2)
                    };
                  }
                }
              });

              if (bestMatch) {
                return { found: true, element: bestMatch };
              }
              return { found: false, error: 'No element found with text: ' + searchText };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Element not found' };
          }

          const element = result.element;
          await cdpDoubleClickAt(tab.id, element.centerX, element.centerY);
          return { type: 'success', data: { text: element.text, x: element.centerX, y: element.centerY } };
        } catch (e) {
          return { type: 'error', message: `Double-click by text failed: ${e.message}` };
        }

      case 'hoverByText':
        // Hover over an element by its text content
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
                    bestMatch = {
                      text: directText,
                      centerX: Math.round(rect.x + rect.width / 2),
                      centerY: Math.round(rect.y + rect.height / 2)
                    };
                  }
                }
              });

              if (bestMatch) {
                return { found: true, element: bestMatch };
              }
              return { found: false, error: 'No element found with text: ' + searchText };
            })()
          `);

          if (!result.found) {
            return { type: 'error', message: result.error || 'Element not found' };
          }

          const element = result.element;
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
      matches: ['http://localhost:8083/*'],
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
