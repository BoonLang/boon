# DigitalJS Debugging Notes

## Bug: DFF Clock Edge Detection Fails in WorkerEngine (2025-11-25)

### Symptom
UART TX module stays stuck in start bit (tx=0, busy=1) forever. The internal DFFs don't advance their counters even though simulation appears to run.

### Root Cause
The `WorkerEngine` runs simulation in a Web Worker using the `Gate` class in `worker-worker.mjs`. This class wraps cell operations but doesn't properly initialize DFF state.

**The problem:** DFF cells require `last_clk` to be initialized for clock edge detection. In the normal JointJS-based simulation, `Dff.initialize()` sets `this.last_clk = 0`. But the worker's `Gate` constructor only calls `cell.prepare.call(this)`, and `prepare()` is empty by default.

**Location:** `src/engines/worker-worker.mjs`, `Gate` constructor

**DFF edge detection code** (`src/cells/dff.mjs` lines 88-107):
```javascript
if ('clock' in polarity) {
    last_clk = this.last_clk;           // Read previous clock value
    this.last_clk = data.clk.get(0);    // Save current clock value
}
...
if (!('clock' in polarity) || data.clk.get(0) == pol('clock') && last_clk == -pol('clock')) {
    // Clock edge detected - update output
}
```

Without `last_clk` initialized, it starts as `undefined`, and edge detection fails because:
- `undefined == -1` is always false (for positive edge clock with pol('clock')=1)

### Fix
Added DFF-specific initialization in the Gate constructor:
```javascript
// Initialize last_clk for DFF gates - required for clock edge detection
// IMPORTANT: Must be -1 (Vector3vl LOW) not 0, so first rising edge is detected
// Vector3vl.get() returns: -1=LOW, 0=X, 1=HIGH
if (gateParams.type === 'Dff') {
    this.last_clk = -1;
}
```

### Debugging Journey

1. **Initial observation:** UART terminal receives no characters despite simulation running

2. **First hypothesis (wrong):** SynchEngine wasn't simulating subcircuits
   - Created debug tests showing subcircuit gates were being skipped
   - Later discovered browser uses WorkerEngine, not SynchEngine

3. **Second hypothesis:** Signals not propagating into subcircuits
   - Added debug capability to worker (`debugGate` method)
   - Found signals WERE reaching subcircuit inputs (clk=1 on uart_tx)

4. **Third observation:** Output cell receiving signal, uart_tx shows tx=0, busy=1
   - UART TX started transmitting (start bit sent)
   - But stuck - never progressed to data bits

5. **Key insight:** Internal DFFs have `lastClk=0` permanently
   - Clock was toggling at top level
   - Clock was reaching DFF inputs
   - But DFFs never detected edges

6. **Root cause found:** Gate class doesn't call DFF's `initialize()` method
   - Only calls `prepare()` which is empty
   - `last_clk` never set, edge detection broken

### Test Files Created During Debug
- `test/debug-subcircuit.mjs` - Examine DFF values inside uart_tx
- `test/debug-worker-subcircuit.mjs` - Check worker graph registration
- `test/debug-observed-graphs.mjs` - Test graph observation pattern
- `test/debug-links.mjs` - Verify link connections
- `test/debug-worker-gates.mjs` - Debug worker gate internal state
- `test/debug-uart-activity.mjs` - Test UART with button press
- `test/debug-output-cell.mjs` - Check Output cell signal propagation
- `test/debug-iomap.mjs` - Examine circuitIOmap structure

### Related Files Modified
- `src/engines/worker-worker.mjs` - Added `last_clk` initialization for DFFs
- `src/engines/worker.mjs` - Added `debugGate` method for debugging
- `src/cells/dff.mjs` - Changed default initial value from 'x' to '0' (earlier fix)

### Lessons Learned
1. Worker-based simulation has different initialization paths than JointJS-based
2. Cell state that's set in `initialize()` needs explicit handling in worker Gates
3. Edge-triggered components (DFFs) are particularly sensitive to state initialization
4. When signals propagate but outputs don't change, check state initialization

---

## Bug: UART Terminal Timing Drift (2025-11-25)

### Symptom
After fixing the DFF bug above, the UART TX was transmitting correctly (tx line toggling), but the UART terminal was decoding wrong characters. For example, 'B' (0x42) was being decoded as 0x82 or 0x86.

### Root Cause
The UartTerminal receiver was counting `tick()` method calls instead of using actual simulation tick values. When simulation ticks were skipped or batched, the call count diverged from actual elapsed ticks.

**The problem:** The receiver used `this.txCycleCount++` to track timing, but this incremented once per `tick()` call, not once per simulation tick. The `postUpdateGates` event might skip ticks, causing timing drift.

**Location:** `src/client/uart-terminal.js`, `processTx()` method

**Observed behavior:**
- Expected: 2000 ticks per bit (cyclesPerBit=2000)
- Actual: ~1920-1940 ticks per bit (call-count based)
- Cumulative error: ~500-600 ticks over 8 bits
- Result: Sampling shifted into wrong bit slots

### Fix
Changed from call-count timing to tick-based timing:

**Before (buggy):**
```javascript
this.txCycleCount++;
if (this.txCycleCount >= this.cyclesPerBit) {
    // Sample bit
    this.txCycleCount = 0;
}
```

**After (fixed):**
```javascript
const elapsed = this.lastTick - this.txStateStartTick;
if (elapsed >= this.cyclesPerBit) {
    // Sample bit
    this.txStateStartTick = this.lastTick;
}
```

### Debugging Journey

1. **Initial observation:** 'B' was being received as 0x82 (bits 1,7 set) instead of 0x42 (bits 1,6 set)

2. **Verified signal correctness:** Created `debug-signal-value.mjs` test that manually sampled at exact tick positions - confirmed the TX signal from FPGA was correct (0x42)

3. **Added receiver debug logging:** Traced each bit sample:
   - Expected: 2000 ticks between samples
   - Actual: ~1920-1940 ticks between samples
   - Cumulative drift caused wrong bits to be sampled

4. **Root cause:** `tick()` calls don't equal simulation ticks
   - The `postUpdateGates` event fires once per `updateGates()` call
   - But the tick value can advance more than 1 between calls
   - Call-count timing was inherently unreliable

5. **Fix:** Changed to tick-based timing using `this.lastTick - this.txStateStartTick`

### Files Modified
- `src/client/uart-terminal.js` - Changed TX and RX processing to use tick-based timing
- `src/client/index.js` - Added `clockPropagation` option to UartTerminal constructor

### Test Files Created
- `test/debug-uart-rx.mjs` - Trace UART receiver state machine
- `test/debug-signal-value.mjs` - Verify signal values at sample points
- `test/debug-bit-timing.mjs` - Track exact TX bit transitions
- `test/test-uart-full-message.mjs` - End-to-end test for "BTN 1" message

### Lessons Learned
1. Never assume callback counts equal time ticks in event-driven systems
2. Use actual timestamps/tick values for timing-critical operations
3. Cumulative timing errors are insidious - small per-bit errors compound
4. End-to-end testing with known patterns helps identify subtle timing bugs
