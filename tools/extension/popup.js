// Boon Browser Control - Popup Script

async function updateStatus() {
  const wsDot = document.getElementById('ws-dot');
  const wsStatus = document.getElementById('ws-status');
  const apiDot = document.getElementById('api-dot');
  const apiStatus = document.getElementById('api-status');
  const pageUrl = document.getElementById('page-url');

  // Check WebSocket connection by trying to send a message
  try {
    // Get tabs with localhost:8083
    const tabs = await chrome.tabs.query({ url: 'http://localhost:8083/*' });

    if (tabs.length === 0) {
      wsDot.className = 'status-dot disconnected';
      wsStatus.textContent = 'No Playground tab';
      apiDot.className = 'status-dot disconnected';
      apiStatus.textContent = 'No Playground tab';
      pageUrl.textContent = '-';
      return;
    }

    const tab = tabs[0];
    pageUrl.textContent = tab.url;

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
        } else {
          apiDot.className = 'status-dot pending';
          apiStatus.textContent = 'API Not Ready';
        }
      }
    } catch (e) {
      apiDot.className = 'status-dot disconnected';
      apiStatus.textContent = 'Cannot access page';
    }

    // We can't directly check WebSocket status from popup
    // Just show that extension is loaded
    wsDot.className = 'status-dot connected';
    wsStatus.textContent = 'Extension Active';

  } catch (e) {
    console.error('Status check error:', e);
    wsDot.className = 'status-dot disconnected';
    wsStatus.textContent = 'Error: ' + e.message;
  }
}

// Update status on load
document.addEventListener('DOMContentLoaded', updateStatus);

// Refresh button
document.getElementById('refresh-btn').addEventListener('click', updateStatus);
