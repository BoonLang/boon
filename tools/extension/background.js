// Boon Browser Control - Background Service Worker
// Connects to WebSocket server and routes commands to content script

const WS_URL = 'ws://127.0.0.1:9222';
let ws = null;
let reconnectTimer = null;
let contentPort = null;
let pendingRequests = new Map();

// Connect to WebSocket server
function connect() {
  if (ws && ws.readyState === WebSocket.OPEN) {
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
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    // Identify as extension to the server
    ws.send(JSON.stringify({ clientType: 'extension' }));
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

      const responseMsg = JSON.stringify({
        id: request.id,
        response: response
      });

      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(responseMsg);
        console.log('[Boon] Sent response:', response);
      }
    } catch (e) {
      console.error('[Boon] Error handling message:', e);
    }
  };
}

function scheduleReconnect() {
  if (reconnectTimer) return;

  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, 3000);
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

      case 'click':
        return await executeInTab(tab.id, clickElement, command.selector);

      case 'type':
        return await executeInTab(tab.id, typeInElement, command.selector, command.text);

      case 'injectCode':
        return await executeInTab(tab.id, injectCode, command.code);

      case 'triggerRun':
        return await executeInTab(tab.id, triggerRun);

      case 'screenshot':
        return await captureScreenshot(tab.id);

      case 'getConsole':
        // First ensure console capture is set up
        await executeInTab(tab.id, setupConsoleCapture);
        return await executeInTab(tab.id, getConsoleMessages);

      case 'setupConsole':
        return await executeInTab(tab.id, setupConsoleCapture);

      case 'getPreviewText':
        return await executeInTab(tab.id, getPreviewText);

      case 'reload':
        // Hot reload: reload the extension and refresh the playground tab
        console.log('[Boon] Reloading extension...');
        // Refresh the playground tab first
        try {
          await chrome.tabs.reload(tab.id);
        } catch (e) {
          console.log('[Boon] Could not refresh tab:', e);
        }
        // Small delay to allow tab refresh to start
        await new Promise(resolve => setTimeout(resolve, 100));
        // Now reload the extension - this will terminate the service worker
        chrome.runtime.reload();
        // Code after this won't run - extension terminates
        return { type: 'success', data: null };

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
  const el = document.querySelector(selector);
  if (!el) {
    return { type: 'error', message: `Element not found: ${selector}` };
  }
  el.click();
  return { type: 'success', data: null };
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
  if (typeof window.boonPlayground !== 'undefined' && window.boonPlayground.getPreview) {
    const text = window.boonPlayground.getPreview();
    return { type: 'previewText', text };
  }

  // Fallback: try to get preview panel content
  const preview = document.querySelector('.preview-panel') ||
                  document.querySelector('[data-panel="preview"]') ||
                  document.querySelector('#preview');
  if (preview) {
    return { type: 'previewText', text: preview.textContent || '' };
  }

  return { type: 'error', message: 'Could not get preview text' };
}

// Initialize connection when service worker starts
connect();

// Keep service worker alive by reconnecting periodically
setInterval(() => {
  if (!ws || ws.readyState !== WebSocket.OPEN) {
    connect();
  }
}, 10000);

// Send keep-alive ping every 20 seconds to prevent MV3 service worker from going inactive
// Chrome MV3 service workers can become inactive after 30s of inactivity
setInterval(() => {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: 'keepAlive' }));
    console.log('[Boon] Sent keep-alive ping');
  }
}, 20000);
