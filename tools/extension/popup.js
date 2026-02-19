// Boon Browser Control - Popup Script

async function updateStatus() {
  const wsDot = document.getElementById('ws-dot');
  const wsStatus = document.getElementById('ws-status');
  const apiDot = document.getElementById('api-dot');
  const apiStatus = document.getElementById('api-status');
  const pageUrl = document.getElementById('page-url');
  const engineInfo = document.getElementById('engine-info');
  const portInfo = document.getElementById('port-info');

  // Check WebSocket connection by trying to send a message
  try {
    // Find playground tabs on any localhost port
    const tabs = await chrome.tabs.query({ url: 'http://localhost/*' });

    if (tabs.length === 0) {
      wsDot.className = 'status-dot disconnected';
      wsStatus.textContent = 'No Playground tab';
      apiDot.className = 'status-dot disconnected';
      apiStatus.textContent = 'No Playground tab';
      pageUrl.textContent = '-';
      if (portInfo) portInfo.textContent = 'No tab detected';
      return;
    }

    // Prefer active tab, then most recently accessed
    const tab = tabs.find(t => t.active) || tabs.sort((a, b) => (b.lastAccessed || 0) - (a.lastAccessed || 0))[0];
    pageUrl.textContent = tab.url;

    // Show detected ports
    if (portInfo) {
      const portMatch = tab.url && tab.url.match(/localhost:(\d+)/);
      if (portMatch) {
        const pgPort = parseInt(portMatch[1], 10);
        const wsPort = pgPort + 1141;
        portInfo.textContent = `Playground: ${pgPort}, WS: ${wsPort}`;
      }
    }

    // Check if boonPlayground API is available
    try {
      const results = await chrome.scripting.executeScript({
        target: { tabId: tab.id },
        world: 'MAIN',  // Run in page's main world to access window.boonPlayground
        func: () => {
          return {
            apiReady: typeof window.boonPlayground !== 'undefined' &&
                      typeof window.boonPlayground.isReady === 'function' &&
                      window.boonPlayground.isReady()
          };
        }
      });

      if (results && results[0] && results[0].result) {
        const { apiReady } = results[0].result;

        if (apiReady) {
          apiDot.className = 'status-dot connected';
          apiStatus.textContent = 'API Ready';

          // If API is ready, show current engine.
          try {
            const engineResults = await chrome.scripting.executeScript({
              target: { tabId: tab.id },
              world: 'MAIN',
              func: () => {
                if (typeof window.boonPlayground !== 'undefined' &&
                    typeof window.boonPlayground.getEngine === 'function') {
                  return window.boonPlayground.getEngine();
                }
                return null;
              }
            });
            const info = engineResults && engineResults[0] && engineResults[0].result;
            if (info && info.engine) {
              engineInfo.textContent = info.switchable
                ? `${info.engine} (switchable)`
                : info.engine;
            } else {
              engineInfo.textContent = 'Unknown';
            }
          } catch (_) {
            engineInfo.textContent = 'Unknown';
          }
        } else {
          apiDot.className = 'status-dot pending';
          apiStatus.textContent = 'API Not Ready';
          engineInfo.textContent = 'Unknown';
        }
      }
    } catch (e) {
      apiDot.className = 'status-dot disconnected';
      apiStatus.textContent = 'Cannot access page';
      engineInfo.textContent = 'Unknown';
    }

    // We can't directly check WebSocket status from popup
    // Just show that extension is loaded
    wsDot.className = 'status-dot connected';
    wsStatus.textContent = 'Extension Active';

  } catch (e) {
    console.error('Status check error:', e);
    wsDot.className = 'status-dot disconnected';
    wsStatus.textContent = 'Error: ' + e.message;
    engineInfo.textContent = 'Unknown';
  }
}

// WS port override management
function setupWsPortOverride() {
  const wsPortInput = document.getElementById('ws-port-override');
  const wsPortSave = document.getElementById('ws-port-save');
  const wsPortInfo = document.getElementById('ws-port-info');

  if (!wsPortInput || !wsPortSave || !wsPortInfo) return;

  // Load current override
  chrome.storage.local.get('wsPortOverride', (result) => {
    if (result.wsPortOverride) {
      wsPortInput.value = result.wsPortOverride;
      wsPortInfo.textContent = `Override active: port ${result.wsPortOverride}`;
    }
  });

  wsPortSave.addEventListener('click', () => {
    const value = wsPortInput.value.trim();
    if (value === '' || value === '0') {
      chrome.storage.local.remove('wsPortOverride');
      wsPortInfo.textContent = 'Auto: playground_port + 1141';
    } else {
      chrome.storage.local.set({ wsPortOverride: parseInt(value, 10) });
      wsPortInfo.textContent = `Override active: port ${value}`;
    }
  });
}

// Update status on load
document.addEventListener('DOMContentLoaded', () => {
  updateStatus();
  setupWsPortOverride();
});

// Refresh button
document.getElementById('refresh-btn').addEventListener('click', updateStatus);
