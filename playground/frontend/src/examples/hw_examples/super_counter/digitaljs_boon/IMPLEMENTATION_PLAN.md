# DigitalJS UART Terminal Implementation Plan

## Overview

This plan details how to run DigitalJS locally with UART terminal support for the super_counter.sv example.

## Architecture Summary

```
┌─────────────────────────────────────────────────────────────────┐
│                        Browser (port 3001)                       │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │  digitaljs_online (Webpack dev server)                      ││
│  │  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐ ││
│  │  │ Code Editor  │  │   Circuit    │  │    I/O Panel      │ ││
│  │  │  (CodeMirror)│  │   (DigitalJS)│  │  ┌─────────────┐  │ ││
│  │  │              │  │              │  │  │ UART        │  │ ││
│  │  │              │  │    ┌────┐    │  │  │ Terminal    │  │ ││
│  │  │              │  │    │FPGA│    │  │  │ (NEW)       │  │ ││
│  │  │              │  │    │SIM │    │  │  └─────────────┘  │ ││
│  │  └──────────────┘  │    └────┘    │  │  Standard I/O     │ ││
│  │                    └──────────────┘  └───────────────────┘ ││
│  └─────────────────────────────────────────────────────────────┘│
│                              │                                   │
│                              │ POST /api/yosys2digitaljs         │
│                              ▼                                   │
└─────────────────────────────────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│                      Backend (port 8081)                         │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │  Express.js + yosys2digitaljs                               ││
│  │                    │                                        ││
│  │                    ▼                                        ││
│  │           Local Yosys installation                          ││
│  │           (SystemVerilog → DigitalJS JSON)                  ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

## Phase 1: Get DigitalJS Running Locally (Ports 3001/8081)

### 1.1 Install Dependencies

```bash
cd digitaljs_online
npm install

cd ../digitaljs
npm install

cd ../yosys2digitaljs
npm install
```

### 1.2 Install System Yosys

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install yosys

# Or build from source for latest version
git clone https://github.com/YosysHQ/yosys.git
cd yosys
make -j$(nproc)
sudo make install
```

### 1.3 Configure Ports

**File: `digitaljs_online/webpack.config.js`**
Change:
- `devServer.port: 3000` → `devServer.port: 3001`
- `proxy: "/api": "http://localhost:8080"` → `proxy: "/api": "http://localhost:8081"`

**File: `digitaljs_online/src/server/index.js`**
Change:
- `app.listen(8080, 'localhost')` → `app.listen(8081, 'localhost')`

### 1.4 Test Basic Setup

```bash
cd digitaljs_online
npm run dev
# Should start server on 8081 and frontend on 3001
```

## Phase 2: Load super_counter.sv by Default

### 2.1 Copy super_counter.sv to Examples

Copy the file to `digitaljs_online/public/examples/super_counter.sv`

### 2.2 Update Testing Instructions in super_counter.sv Header

Add these lines at the top of super_counter.sv:
```verilog
// Super Counter - UART-based button counter with LED acknowledgment
//
// Protocol:
//   TX: "BTN <seq>\n"  - Button press with sequence number (1-99999)
//   RX: "ACK <ms>\n"   - Flash LED for <ms> milliseconds
//
// Testing with UART Terminal:
//   1. Click "Run" to synthesize and start simulation
//   2. In the I/O tab, find the UART Terminal
//   3. Press the btn_n button (toggle off = pressed) to see "BTN 1" appear
//   4. Type "ACK 500" and press Enter to flash the LED for 500ms
//   5. Repeat to see sequence numbers increment
```

### 2.3 Auto-load and Auto-synthesize

**Option A: URL hash loading**
Modify `index.js` to check for a URL parameter:
```javascript
// Add to window.onload handler
const params = new URLSearchParams(window.location.search);
if (params.get('example')) {
    $.get('/examples/' + params.get('example') + '.sv', (data, status) => {
        make_tab(params.get('example'), 'sv', data);
        if (params.get('autorun')) {
            // Trigger synthesis after tab is created
            setTimeout(() => $('button[type=submit]').click(), 100);
        }
    });
}
```

Access via: `http://localhost:3001/?example=super_counter&autorun=true`

**Option B: Make it the startup default**
Modify `index.js` to auto-load super_counter.sv on page load.

## Phase 3: Implement UART Terminal

### 3.1 Terminal UI Component

Create new file: `digitaljs_online/src/client/uart-terminal.js`

```javascript
// UART Terminal Component
// Provides a terminal-like interface for UART communication

export class UartTerminal {
    constructor(options) {
        this.el = options.el;
        this.circuit = options.circuit;
        this.baudRate = options.baudRate || 115200;
        this.clockHz = options.clockHz || 12000000;

        // UART state
        this.txBuffer = [];
        this.rxBuffer = [];
        this.txBitIndex = -1;
        this.rxBitIndex = -1;
        this.txShiftReg = 0;
        this.rxShiftReg = 0;
        this.txCycleCount = 0;
        this.rxCycleCount = 0;

        this.cyclesPerBit = Math.round(this.clockHz / this.baudRate);

        // History
        this.history = [];
        this.historyIndex = -1;

        this.render();
        this.bindEvents();
    }

    render() {
        this.el.innerHTML = `
            <div class="uart-terminal">
                <div class="uart-output" readonly></div>
                <div class="uart-input-wrapper">
                    <span class="uart-prompt">&gt;</span>
                    <input type="text" class="uart-input" placeholder="Type command (e.g., ACK 100)">
                </div>
            </div>
        `;
        this.outputEl = this.el.querySelector('.uart-output');
        this.inputEl = this.el.querySelector('.uart-input');
    }

    bindEvents() {
        this.inputEl.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                const command = this.inputEl.value.trim();
                if (command) {
                    this.sendCommand(command);
                    this.history.push(command);
                    this.historyIndex = this.history.length;
                    this.inputEl.value = '';
                }
            } else if (e.key === 'ArrowUp') {
                if (this.historyIndex > 0) {
                    this.historyIndex--;
                    this.inputEl.value = this.history[this.historyIndex];
                }
                e.preventDefault();
            } else if (e.key === 'ArrowDown') {
                if (this.historyIndex < this.history.length - 1) {
                    this.historyIndex++;
                    this.inputEl.value = this.history[this.historyIndex];
                } else {
                    this.historyIndex = this.history.length;
                    this.inputEl.value = '';
                }
                e.preventDefault();
            }
        });
    }

    appendOutput(text, className = '') {
        const line = document.createElement('div');
        line.className = 'uart-line ' + className;
        line.textContent = text;
        this.outputEl.appendChild(line);
        this.outputEl.scrollTop = this.outputEl.scrollHeight;
    }

    sendCommand(command) {
        this.appendOutput('> ' + command, 'uart-sent');
        // Queue bytes for transmission
        for (const char of command + '\n') {
            this.rxBuffer.push(char.charCodeAt(0));
        }
    }

    // Called every simulation tick
    tick() {
        // Handle TX (from FPGA) - receive bytes
        this.processTx();
        // Handle RX (to FPGA) - transmit bytes
        this.processRx();
    }

    processTx() {
        // Monitor uart_tx_o signal
        // Implement UART receive logic
    }

    processRx() {
        // Drive uart_rx_i signal
        // Implement UART transmit logic
    }

    shutdown() {
        this.el.innerHTML = '';
    }
}
```

### 3.2 UART Signal Detection

In `index.js`, modify `mkcircuit()` to detect UART signals:

```javascript
function mkcircuit(data, opts) {
    // ... existing code ...

    // Detect UART signals
    const uartSignals = detectUartSignals(circuit);
    if (uartSignals.tx && uartSignals.rx) {
        const uartTerminalEl = document.getElementById('uart-terminal');
        if (!uartTerminalEl) {
            // Create UART terminal container in I/O panel
            const iopanel = document.getElementById('iopanel');
            const terminalContainer = document.createElement('div');
            terminalContainer.id = 'uart-terminal';
            iopanel.appendChild(terminalContainer);
        }

        uartTerminal = new UartTerminal({
            el: document.getElementById('uart-terminal'),
            circuit: circuit,
            txSignal: uartSignals.tx,
            rxSignal: uartSignals.rx
        });
    }
}

function detectUartSignals(circuit) {
    // Look for signals named uart_tx, uart_rx, uart_tx_o, uart_rx_i
    const cells = circuit._graph.getCells();
    let tx = null, rx = null;

    for (const cell of cells) {
        const net = cell.get('net');
        if (net && (net.includes('uart_tx') || net === 'tx')) {
            tx = cell;
        }
        if (net && (net.includes('uart_rx') || net === 'rx')) {
            rx = cell;
        }
    }

    return { tx, rx };
}
```

### 3.3 UART Protocol Implementation

The UART terminal needs to:
1. **Receive (from FPGA via uart_tx_o):**
   - Monitor the tx output signal
   - Detect start bit (falling edge)
   - Sample 8 data bits at mid-bit timing
   - Verify stop bit
   - Accumulate characters until newline

2. **Transmit (to FPGA via uart_rx_i):**
   - When user types command, queue bytes
   - Generate start bit (drive low)
   - Shift out 8 data bits LSB first
   - Generate stop bit (drive high)

### 3.4 Integration with Simulation Engine

Hook into the circuit's tick events:

```javascript
circuit.on('postUpdateGates', (tick) => {
    if (uartTerminal) {
        uartTerminal.tick();
    }
});
```

## Phase 4: Browser Automation for Testing

### 4.1 Recommended Approach: Playwright

Playwright is the most reliable and modern browser automation tool:

```bash
npm install playwright
npx playwright install chromium
```

### 4.2 Test Script Structure

```javascript
// test/uart-simulation.test.js
const { chromium } = require('playwright');

describe('UART Simulation', () => {
    let browser, page;

    beforeAll(async () => {
        browser = await chromium.launch({ headless: false });
        page = await browser.newPage();
    });

    afterAll(async () => {
        await browser.close();
    });

    test('should synthesize super_counter.sv', async () => {
        await page.goto('http://localhost:3001/?example=super_counter');
        await page.waitForSelector('#synthesize-btn');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 30000 });
    });

    test('should receive BTN message on button press', async () => {
        // Toggle btn_n input
        await page.click('[data-net="btn_n"]');
        // Wait for UART terminal to show BTN message
        await page.waitForSelector('.uart-output:has-text("BTN")');
    });

    test('should send ACK command and flash LED', async () => {
        await page.fill('.uart-input', 'ACK 500');
        await page.press('.uart-input', 'Enter');
        // Verify LED flashes
        await page.waitForSelector('[data-net="led_o"].high');
    });
});
```

## Phase 5: Style Preservation

**Critical:** Minimize changes to avoid breaking styles.

### 5.1 Files to Modify (Minimal Changes)

1. `webpack.config.js` - Only change port numbers
2. `src/server/index.js` - Only change port number
3. `src/client/index.js` - Add UART detection and terminal initialization
4. `public/index.html` - Add UART terminal container (optional)
5. `src/client/scss/app.scss` - Add UART terminal styles (append only)

### 5.2 CSS for UART Terminal (Append to app.scss)

```scss
/* UART Terminal Styles - Append to end of app.scss */
.uart-terminal {
    font-family: monospace;
    background: #1e1e1e;
    color: #d4d4d4;
    padding: 10px;
    border-radius: 4px;
    margin-top: 15px;
}

.uart-output {
    height: 200px;
    overflow-y: auto;
    white-space: pre-wrap;
    word-wrap: break-word;
    margin-bottom: 10px;
    padding: 5px;
    background: #0d0d0d;
    border: 1px solid #333;
}

.uart-line.uart-sent {
    color: #569cd6;
}

.uart-line.uart-received {
    color: #4ec9b0;
}

.uart-input-wrapper {
    display: flex;
    align-items: center;
    background: #252526;
    border: 1px solid #333;
}

.uart-prompt {
    padding: 5px 10px;
    color: #569cd6;
}

.uart-input {
    flex: 1;
    background: transparent;
    border: none;
    color: #d4d4d4;
    padding: 5px;
    font-family: inherit;
    outline: none;
}
```

## Implementation Order

1. **Day 1: Basic Setup**
   - Install dependencies
   - Change ports to 3001/8081
   - Verify basic synthesis works
   - Copy super_counter.sv

2. **Day 2: Auto-load Feature**
   - Implement URL parameter loading
   - Add auto-synthesis option
   - Update super_counter.sv header with testing instructions

3. **Day 3: UART Terminal UI**
   - Create terminal component
   - Add styles
   - Implement history navigation

4. **Day 4: UART Protocol**
   - Implement bit-banging receive logic
   - Implement bit-banging transmit logic
   - Integrate with simulation tick

5. **Day 5: Testing & Polish**
   - Set up Playwright
   - Write automated tests
   - Fix any issues found

## Risk Mitigation

### Style Breakage
- Make a backup before any CSS changes
- Use browser dev tools to compare before/after
- Only append new styles, never modify existing

### UART Timing
- The simulation runs at discrete ticks, not real-time
- Calculate cycles per bit based on clock/baud parameters
- May need to adjust sampling points for reliability

### Browser Compatibility
- Test in Chrome, Firefox, Safari
- Use standard Web APIs
- Avoid browser-specific features

## Files Created/Modified Summary

### New Files
- `digitaljs_boon/IMPLEMENTATION_PLAN.md` (this file)
- `digitaljs_online/src/client/uart-terminal.js`
- `digitaljs_online/test/uart-simulation.test.js`

### Modified Files (Minimal Changes)
- `digitaljs_online/webpack.config.js` (ports only)
- `digitaljs_online/src/server/index.js` (port only)
- `digitaljs_online/src/client/index.js` (add UART detection/init)
- `digitaljs_online/src/client/scss/app.scss` (append terminal styles)
- `super_counter.sv` (add testing instructions to header)
