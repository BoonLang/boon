// Console capture script - runs in MAIN world at document_start
// This intercepts console messages before WASM loads

(function() {
  if (typeof window.__boonCapturedConsole !== 'undefined') {
    return; // Already set up
  }

  window.__boonCapturedConsole = [];
  const maxMessages = 200;

  const originalConsole = {
    log: console.log.bind(console),
    warn: console.warn.bind(console),
    error: console.error.bind(console),
    info: console.info.bind(console)
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

    originalConsole[level](...args);
  }

  console.log = (...args) => captureConsole('log', ...args);
  console.warn = (...args) => captureConsole('warn', ...args);
  console.error = (...args) => captureConsole('error', ...args);
  console.info = (...args) => captureConsole('info', ...args);
})();
