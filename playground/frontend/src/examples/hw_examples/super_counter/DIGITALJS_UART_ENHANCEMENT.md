# DigitalJS UART Terminal Enhancement Proposal

## 🎯 Goal

Add native UART debugging to DigitalJS so **clean hardware code** can be debugged without modifications.

**Key Principle:** Debugging is DigitalJS's responsibility, not the HDL's responsibility.

---

## 📋 Architecture Analysis

### Current DigitalJS Structure:

```
digitaljs_online/
├── src/
│   ├── client/
│   │   ├── index.js          ← Main app, imports digitaljs
│   │   └── scss/
│   └── server/
│       └── index.js           ← Yosys synthesis server
├── public/
│   └── index.html             ← Has I/O panel tab (line 67)
└── package.json
```

### DigitalJS Library (External):
- Repository: https://github.com/tilk/digitaljs
- Provides: Circuit simulation, rendering, I/O widgets
- Used by: `import * as digitaljs from 'digitaljs'`

---

## 🏗️ Implementation Plan

### **Option A: Enhance digitaljs_online (Frontend Only)**

**Add to I/O Panel:**
- UART Terminal widget in the I/O tab
- Monitors wire signals from the circuit
- Decodes serial data in JavaScript
- Displays ASCII text

**Files to Modify:**
1. `src/client/index.js` - Add UART terminal widget
2. `public/index.html` - Add terminal UI in I/O panel
3. `src/client/scss/app.scss` - Style the terminal

**Implementation:**
```javascript
// In index.js:
class UARTTerminal {
    constructor(circuit, wire_name, baud_rate, fast_sim) {
        this.circuit = circuit;
        this.wire = circuit.getWire(wire_name);
        this.baud_rate = baud_rate;
        this.divisor = fast_sim ? 4 : (12_000_000 / baud_rate);
        this.buffer = [];
        this.decoder = new UARTDecoder(this.divisor);

        // Listen to wire changes
        this.wire.on('change', (value) => {
            const byte = this.decoder.push(value);
            if (byte !== null) {
                this.displayByte(byte);
            }
        });
    }

    displayByte(byte) {
        const char = String.fromCharCode(byte);
        $('#uart-terminal').append(char);
        this.buffer.push(byte);
    }
}

// Usage:
const uart_tx_monitor = new UARTTerminal(
    circuit,
    'uart_tx',   // Wire name from synthesis
    115200,      // Baud rate
    true         // FAST_SIM mode
);
```

**Pros:**
- ✅ Quick to implement
- ✅ No changes to DigitalJS library
- ✅ Works immediately

**Cons:**
- ⚠️ Only in digitaljs_online, not portable
- ⚠️ Need to manually configure for each design

**Effort:** 4-8 hours

---

### **Option B: Enhance DigitalJS Library (Proper Way)**

**Add Native UART Support:**
- New cell type: `UartMonitor`, `UartInput`
- Yosys2digitaljs recognizes UART modules
- Automatic terminal widget creation

**Files to Modify (in digitaljs repo):**
1. Add `cells/uart.mjs` - UART cell definitions
2. Update `cells/index.mjs` - Register UART cells
3. Add terminal widget to I/O components

**Yosys Integration:**
```systemverilog
// Hardware code stays clean:
uart_tx #(
    .CLOCK_HZ(12_000_000),
    .BAUD(115_200)
) tx_inst (
    .serial_out(uart_tx)
);

// yosys2digitaljs automatically creates:
// { "type": "UartMonitor", "wire": "uart_tx", "baud": 115200 }
```

**DigitalJS Renders:**
```
+-------------------+
| UART TX Monitor   |
| BTN 1            |
| BTN 2            |
| BTN 3            |
|                   |
| [Clear] [Export]  |
+-------------------+
```

**Pros:**
- ✅ Portable (works anywhere DigitalJS is used)
- ✅ Automatic detection
- ✅ Clean separation
- ✅ Can contribute back to project

**Cons:**
- ⚠️ Requires forking DigitalJS
- ⚠️ More complex implementation
- ⚠️ Longer development time

**Effort:** 2-3 days

---

### **Option C: Protocol Decoder Plugin System**

**Generic Approach:**
- Add plugin system to DigitalJS
- UART is first plugin
- Future: SPI, I2C, CAN decoders

**Architecture:**
```javascript
// Protocol decoder interface:
class ProtocolDecoder {
    constructor(config) { }
    decode(wire_value, timestamp) { }
    render() { }
}

// UART plugin:
class UARTDecoder extends ProtocolDecoder {
    // ... implementation
}

// Register:
digitaljs.registerDecoder('uart', UARTDecoder);
```

**Pros:**
- ✅ Extensible architecture
- ✅ Future-proof
- ✅ Clean design

**Cons:**
- ⚠️ Most complex
- ⚠️ Requires DigitalJS architecture changes

**Effort:** 1-2 weeks

---

## 🎖️ Recommended Approach: **Option A + Option B**

### Phase 1: Quick Prototype (Option A)
**Timeline:** This weekend

1. Add UART terminal to `digitaljs_online`
2. Manually configure for `super_counter`
3. Test with clean `packed_super_counter.sv`
4. Validate the concept works

**Deliverables:**
- Working UART terminal in I/O panel
- Can debug `super_counter` UART messages
- Proof of concept for Option B

---

### Phase 2: Proper Integration (Option B)
**Timeline:** Next 1-2 weeks

1. Fork DigitalJS library
2. Add UART cell types
3. Integrate with yosys2digitaljs
4. Submit PR to DigitalJS project

**Deliverables:**
- Native UART support in DigitalJS
- Automatic terminal widget creation
- Clean, production-ready solution

---

## 🔧 Technical Details

### UART Decoder State Machine:

```javascript
class UARTDecoder {
    constructor(divisor) {
        this.divisor = divisor;
        this.state = 'IDLE';
        this.bit_counter = 0;
        this.sample_counter = 0;
        this.shift_reg = 0;
    }

    push(wire_value) {
        switch(this.state) {
            case 'IDLE':
                if (wire_value === 0) {  // Start bit
                    this.state = 'START';
                    this.sample_counter = this.divisor / 2;
                }
                break;

            case 'START':
                if (--this.sample_counter === 0) {
                    this.state = 'DATA';
                    this.bit_counter = 0;
                    this.sample_counter = this.divisor;
                }
                break;

            case 'DATA':
                if (--this.sample_counter === 0) {
                    this.shift_reg = (this.shift_reg >> 1) | (wire_value << 7);
                    this.bit_counter++;

                    if (this.bit_counter === 8) {
                        this.state = 'STOP';
                    }

                    this.sample_counter = this.divisor;
                }
                break;

            case 'STOP':
                if (--this.sample_counter === 0) {
                    this.state = 'IDLE';
                    return this.shift_reg;  // Return decoded byte
                }
                break;
        }

        return null;  // No byte yet
    }
}
```

---

### UI Component (Bootstrap + jQuery):

```html
<!-- In public/index.html, I/O panel tab -->
<div id="uart-panel" class="mt-3">
    <div class="card">
        <div class="card-header">
            <strong>UART Monitor</strong>
            <div class="float-right">
                <button class="btn btn-sm btn-secondary" id="uart-clear">Clear</button>
                <button class="btn btn-sm btn-primary" id="uart-export">Export</button>
            </div>
        </div>
        <div class="card-body">
            <div id="uart-terminal" class="uart-terminal">
                <!-- ASCII text appears here -->
            </div>
        </div>
        <div class="card-footer text-muted">
            <span id="uart-stats">0 bytes received</span>
        </div>
    </div>
</div>
```

**CSS:**
```scss
.uart-terminal {
    font-family: 'Courier New', monospace;
    background-color: #1e1e1e;
    color: #00ff00;
    padding: 10px;
    height: 300px;
    overflow-y: scroll;
    white-space: pre-wrap;
    word-break: break-all;
}
```

---

## 📊 Configuration

### Auto-detect from Module Name:

```javascript
// After synthesis, scan for UART modules:
function detectUARTModules(circuit_data) {
    const uart_modules = [];

    for (const [name, cell] of Object.entries(circuit_data.cells)) {
        if (cell.type === 'uart_tx' || cell.type === 'uart_rx') {
            uart_modules.push({
                name: name,
                type: cell.type,
                wire: cell.connections.serial_out || cell.connections.serial_in,
                baud: cell.parameters.BAUD || 115200,
                fast_sim: cell.parameters.FAST_SIM || 0
            });
        }
    }

    return uart_modules;
}

// Create terminals automatically:
const uart_modules = detectUARTModules(circuit_data);
uart_modules.forEach(config => {
    if (config.type === 'uart_tx') {
        new UARTTXMonitor(circuit, config);
    } else if (config.type === 'uart_rx') {
        new UARTRXInput(circuit, config);
    }
});
```

---

## 🎮 User Experience

### Workflow:

1. **User writes clean HDL:**
   ```systemverilog
   uart_tx #(.BAUD(115_200), .FAST_SIM(1))
       tx_inst (.serial_out(uart_tx), ...);
   ```

2. **DigitalJS synthesizes** → Detects UART module

3. **I/O Panel shows:**
   ```
   [Setup] [I/O] ← Tabs

   I/O Panel:
   +----------------------+
   | UART TX (uart_tx)    |
   | Baud: 115200        |
   | Fast Sim: ON        |
   +----------------------+
   | BTN 1               |
   | BTN 2               |
   | BTN 3               |
   |                     |
   +----------------------+
   | 3 bytes received    |
   +----------------------+
   ```

4. **User interacts:**
   - Press button → See "BTN 1\n" instantly
   - Clear terminal, export log
   - Perfect debugging experience

---

## 🚀 Implementation Steps (Phase 1)

### Weekend Project:

**Day 1 (Saturday):**
- [ ] Create `UARTDecoder` class
- [ ] Add UI to I/O panel
- [ ] Test decoder with static data
- [ ] Wire up to circuit.getWire()

**Day 2 (Sunday):**
- [ ] Test with `packed_super_counter.sv`
- [ ] Polish UI/UX
- [ ] Add clear/export functions
- [ ] Document usage

**Monday:**
- [ ] Test with Boon-generated output
- [ ] Verify works with real synthesis
- [ ] Share prototype!

---

## 📦 Deliverables

### Phase 1 (Prototype):
- ✅ `digitaljs_online` with UART terminal
- ✅ Works with `packed_super_counter.sv`
- ✅ Proof of concept

### Phase 2 (Production):
- ✅ Fork of DigitalJS with UART cells
- ✅ PR to upstream DigitalJS project
- ✅ Boon transpiler outputs clean SV
- ✅ DigitalJS handles debugging automatically

---

## 🎯 Success Criteria

1. ✅ `packed_super_counter.sv` has NO debug outputs
2. ✅ DigitalJS I/O panel shows UART messages as ASCII
3. ✅ User can read "BTN 1\n", "BTN 2\n", etc. instantly
4. ✅ No manual hex→ASCII translation needed
5. ✅ Boon transpiler stays simple (no debug hooks)
6. ✅ Same HDL works in DigitalJS AND real FPGA

---

## 🌟 Future Enhancements

### Phase 3+:
- SPI decoder
- I2C decoder
- Logic analyzer view
- Protocol timing diagrams
- Export to VCD/FST
- Replay captured data

---

## 🤝 Contributing Back

Once Phase 2 is complete:

1. **Fork DigitalJS:** github.com/tilk/digitaljs
2. **Implement UART cells** + terminal widget
3. **Test thoroughly**
4. **Write documentation**
5. **Submit PR** with:
   - Feature description
   - Usage examples
   - Test cases
6. **Benefit community** - everyone gets UART debugging!

---

## 💡 Alignment with Boon Goals

This approach perfectly aligns with Boon's philosophy:

1. **Clean Code:** HDL stays pure, no debug pollution
2. **Separation:** Debugging is tooling's job, not language's job
3. **Transpiler Simplicity:** Boon → clean SV, nothing special
4. **Tooling Enhancement:** DigitalJS handles visualization
5. **Production Ready:** Same code works everywhere

**Boon generates clean HDL → DigitalJS provides rich debugging → Win!**

---

## 🎬 Next Steps

**What do you want to do?**

1. **Quick Win:** Implement Phase 1 prototype this weekend?
2. **Full Solution:** Plan Phase 2 DigitalJS enhancement?
3. **Explore:** Look at DigitalJS source code structure?
4. **Alternative:** Different approach entirely?

I'm ready to start implementing whenever you are! 🚀
