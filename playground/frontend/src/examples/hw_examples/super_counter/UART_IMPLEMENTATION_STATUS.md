# UART Debugging - Implementation Status

## ✅ Phase 2: Proper Integration - IN PROGRESS

**Goal:** Add native UART support to DigitalJS library for clean hardware debugging

---

## 🎯 Completed Steps

### 1. ✅ Repository Setup
- **DigitalJS fork:** `~/repos/digitaljs`
- **yosys2digitaljs fork:** `~/repos/yosys2digitaljs`
- **digitaljs_online:** `~/repos/digitaljs_online`
- **Linked:** digitaljs_online now uses local digitaljs (`file:../digitaljs`)

### 2. ✅ UART Cell Implementation

**Created:** `~/repos/digitaljs/src/cells/uart.mjs`

**Features:**
- `UartMonitor` cell - Decodes TX serial data to ASCII
- `UartInput` cell - Generates RX serial data from user input
- State machine-based UART decoder
- Configurable divisor for baud rate
- Event-driven architecture for UI updates

**Key Classes:**
```javascript
export const UartMonitor = Gate.define('UartMonitor', {
    divisor: 104,  // CLOCK_HZ / BAUD
    // Decodes serial → ASCII
});

export const UartInput = Gate.define('UartInput', {
    divisor: 104,
    // User input → serial
});
```

### 3. ✅ DigitalJS Library Updated

**Modified Files:**
- `src/cells/uart.mjs` - NEW: UART cell implementations
- `src/cells.mjs` - Added: `export * from "./cells/uart.mjs"`

**Built:** Successfully compiled 28 files with Babel

### 4. ✅ Server Restarted

- Killed old server on port 8080
- Started new server with UART support
- **URL:** http://localhost:8080

---

## 🚧 Next Steps

### Step 5: Add UI Terminal Widget

**File:** `~/repos/digitaljs_online/src/client/index.js`

Add UART terminal panel to I/O tab:
```javascript
// Listen for UART events
circuit.on('uart:monitor:byte', (cid, byte) => {
    appendToTerminal(cid, String.fromCharCode(byte));
});

// Create terminal UI in I/O panel
function createUARTTerminal(monitor_cid) {
    const terminal = $('<div class="uart-terminal">').appendTo('#iopanel');
    // ... terminal UI code
}
```

### Step 6: yosys2digitaljs Integration

**Goal:** Auto-detect UART modules and create monitor cells

**File:** `~/repos/yosys2digitaljs/src/process.js` (or similar)

```javascript
// Detect uart_tx/uart_rx modules
if (module_type === 'uart_tx') {
    // Create UartMonitor cell connected to serial_out
    cells.push({
        type: 'UartMonitor',
        divisor: params.FAST_SIM ? 4 : (params.CLOCK_HZ / params.BAUD),
        connections: {
            in: module.connections.serial_out
        }
    });
}
```

### Step 7: Test with Clean HDL

**Test File:** `packed_super_counter.sv`

**Expected Workflow:**
1. Load clean SV code (no debug outputs)
2. DigitalJS synthesizes
3. Detects `uart_tx` module
4. Auto-creates `UartMonitor` cell
5. I/O panel shows UART terminal
6. User presses button
7. Terminal displays: "BTN 1\n" ✨

---

## 📁 File Structure

```
~/repos/
├── digitaljs/
│   ├── src/
│   │   ├── cells/
│   │   │   ├── uart.mjs          ← NEW: UART cells
│   │   │   └── ...
│   │   ├── cells.mjs             ← UPDATED: export UART
│   │   └── ...
│   ├── lib/                      ← Built files (28 files)
│   └── package.json
│
├── digitaljs_online/
│   ├── src/
│   │   ├── client/
│   │   │   └── index.js          ← TODO: Add UI terminal
│   │   └── server/
│   ├── public/
│   │   └── index.html            ← Has I/O panel tab
│   └── package.json              ← Links to ../digitaljs
│
└── yosys2digitaljs/
    └── src/
        └── ...                   ← TODO: Auto-detect UART
```

---

## 🔧 Technical Details

### UART Decoder State Machine

```
State Flow:
IDLE → (detect start bit) → START → DATA (8 bits) → STOP → IDLE
                                      ↓
                                Decoded byte → Event → UI
```

### Event System

```javascript
// In UART cell:
this.trigger('uart:byte', decoded_byte);

// In circuit graph:
circuit.trigger('uart:monitor:byte', cell_cid, byte);

// In UI:
circuit.on('uart:monitor:byte', (cid, byte) => {
    // Update terminal display
});
```

---

## 🎨 Planned UI Design

```
DigitalJS I/O Panel:
┌─────────────────────────────────┐
│ [Setup] [I/O] ← Tabs            │
├─────────────────────────────────┤
│ UART Monitors                   │
│ ┌─────────────────────────────┐ │
│ │ uart_tx (115200 baud)       │ │
│ │ ┌─────────────────────────┐ │ │
│ │ │ BTN 1                   │ │ │
│ │ │ BTN 2                   │ │ │
│ │ │ BTN 3                   │ │ │
│ │ │                         │ │ │
│ │ │                         │ │ │
│ │ └─────────────────────────┘ │ │
│ │ [Clear] [Export] [Pause]   │ │
│ └─────────────────────────────┘ │
│                                 │
│ UART Inputs                     │
│ ┌─────────────────────────────┐ │
│ │ uart_rx (115200 baud)       │ │
│ │ [Send: ACK 100]             │ │
│ │ <input type="text">         │ │
│ │ [Send]                      │ │
│ └─────────────────────────────┘ │
└─────────────────────────────────┘
```

---

## 📊 Success Metrics

### Completed:
- [x] UART cells implemented
- [x] DigitalJS library built
- [x] Server running with new code

### In Progress:
- [ ] UI terminal widget
- [ ] yosys2digitaljs auto-detection
- [ ] End-to-end testing

### Target:
- [ ] Load `packed_super_counter.sv` (clean code)
- [ ] See UART terminal in I/O panel
- [ ] Press button → See "BTN 1\n" in terminal
- [ ] No manual hex decoding needed
- [ ] Perfect debugging experience

---

## 🚀 Next Action Items

**Immediate (Today):**
1. Add UART terminal UI to `client/index.js`
2. Style terminal CSS in `client/scss/app.scss`
3. Test with manual `UartMonitor` cell creation

**This Week:**
1. Integrate with yosys2digitaljs
2. Auto-detect UART modules
3. Test with `packed_super_counter.sv`
4. Document usage

**Future:**
1. Submit PR to upstream DigitalJS
2. Support multiple baud rates
3. Add SPI/I2C decoders
4. Protocol analyzer view

---

## 🎯 Alignment with Boon Goals

✅ **Clean HDL Code**
```systemverilog
// packed_super_counter.sv - NO debug outputs!
uart_tx #(.BAUD(115_200), .FAST_SIM(1)) tx_inst (
    .serial_out(uart_tx),
    // Pure, production-ready code
);
```

✅ **DigitalJS Handles Debugging**
- Auto-detects UART modules
- Creates monitor cells automatically
- Displays ASCII terminal in I/O panel
- Zero code pollution

✅ **Boon Transpiler Stays Simple**
- Outputs clean SystemVerilog
- No special debug hooks
- Works in DigitalJS AND real FPGA
- Perfect separation of concerns

---

## 📝 Notes

- Server is running on http://localhost:8080
- DigitalJS library uses local fork
- Ready for UI development
- UART cells are compiled and available

---

**Status:** 🟡 In Progress - UI implementation next
**Estimated Completion:** Today (2-3 hours for UI)
**Blockers:** None

**Last Updated:** 2025-11-24 02:53 UTC
