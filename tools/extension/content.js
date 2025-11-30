// Boon Browser Control - Content Script
// Runs in the context of localhost:8081 pages

(function() {
  'use strict';

  console.log('[Boon Content] Content script loaded');

  // Store console messages
  const consoleMessages = [];
  const maxMessages = 100;

  // Intercept console methods to capture messages
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

    consoleMessages.push({
      level,
      text,
      timestamp: Date.now()
    });

    // Keep only last N messages
    if (consoleMessages.length > maxMessages) {
      consoleMessages.shift();
    }

    // Call original
    originalConsole[level].apply(console, args);
  }

  console.log = (...args) => captureConsole('log', ...args);
  console.warn = (...args) => captureConsole('warn', ...args);
  console.error = (...args) => captureConsole('error', ...args);
  console.info = (...args) => captureConsole('info', ...args);

  // Expose console messages getter
  window.__boonConsoleMessages = () => [...consoleMessages];
  window.__boonClearConsole = () => { consoleMessages.length = 0; };

  // Wait for boonPlayground API to be available (WASM can take a while to load)
  function waitForApi(callback, maxWait = 30000) {
    const start = Date.now();

    function check() {
      if (typeof window.boonPlayground !== 'undefined') {
        callback(true);
      } else if (Date.now() - start < maxWait) {
        setTimeout(check, 100);
      } else {
        callback(false);
      }
    }

    check();
  }

  // Signal that content script is ready
  waitForApi((ready) => {
    if (ready) {
      console.log('[Boon Content] boonPlayground API detected');
    } else {
      console.log('[Boon Content] boonPlayground API not available, using fallbacks');
    }
  });

})();
