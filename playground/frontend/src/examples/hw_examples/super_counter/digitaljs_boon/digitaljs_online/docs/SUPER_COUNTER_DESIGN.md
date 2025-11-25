# super_counter.sv Design Considerations

## Overview

This document captures the design constraints and solutions for making `super_counter.sv` compatible with both:
1. **DigitalJS browser simulation** (slow, JavaScript-based)
2. **Real FPGA deployment** (fast, hardware-based)

## Key Issues with Original Design

### Issue 1: 32-bit Signal Width Limitation

**Problem:** DigitalJS uses the `3vl` (three-value logic) library. The `Vector3vl.toNumber()` method throws an assertion error when called on vectors >= 32 bits:

```javascript
// From 3vl/dist/index.mjs:942
assert(this._bits < 32, "Vector3vl.toNumber() called on a too wide vector");
```

This method is called internally by DigitalJS for:
- Shift operations (getting shift amount)
- Memory addressing
- Arithmetic operations in some contexts

**Original code using 32-bit signals:**
```verilog
reg [31:0] pulse_cycles;   // FAILS - exactly 32 bits
reg [31:0] duration_ms;    // FAILS - exactly 32 bits
```

**Solution:** Reduce all wide signals to maximum 24 bits:
```verilog
reg [23:0] pulse_cycles;   // OK - 24 bits
reg [23:0] duration;       // OK - 24 bits
```

### Issue 2: UART Timing Too Slow for Simulation

**Problem:** Original parameters:
- Clock: 12 MHz (12,000,000 Hz)
- Baud rate: 115,200 bps
- Cycles per bit: 12,000,000 / 115,200 ≈ 104 cycles

A single UART byte (10 bits including start/stop) requires ~1,040 cycles.
A 6-character message "BTN 1\n" requires ~6,240 cycles.

In DigitalJS, running at ~100 ticks/second, this takes ~62 seconds!

**Solution:** Use simulation-friendly parameters:
```verilog
parameter CLOCK_HZ = 1000;  // 1 kHz (not 12 MHz)
parameter BAUD = 100;       // 100 baud (not 115,200)
// Cycles per bit: 1000 / 100 = 10 cycles
```

Now a 6-character message requires only ~600 ticks = 6 seconds at 100 ticks/sec.

### Issue 3: Debounce Time Too Long

**Problem:** Original debounce timing:
- 20ms debounce at 12 MHz = 240,000 cycles
- At 100 ticks/second, this is 40 minutes of simulation time!

**Solution:** Minimal debounce for simulation:
```verilog
parameter DEBOUNCE_CYC = 2;  // 2 cycles (effectively instant)
```

## Recommended Approach: Parameterized Design

Create a single `super_counter.sv` that works for both targets using parameters:

```verilog
module super_counter #(
    // SIMULATION defaults (DigitalJS-friendly)
    parameter CLOCK_HZ     = 1000,      // 1kHz for simulation
    parameter BAUD         = 100,       // 100 baud
    parameter DEBOUNCE_CYC = 2,         // minimal debounce

    // Derived parameters
    parameter CYCLES_PER_BIT = CLOCK_HZ / BAUD
) (
    input  wire clk,
    input  wire rst_n,
    input  wire btn_n,
    input  wire uart_rx_i,
    output wire uart_tx_o,
    output wire led_o
);
```

### For Real FPGA Deployment

Override parameters in the instantiation or during synthesis:
```verilog
super_counter #(
    .CLOCK_HZ(12_000_000),
    .BAUD(115_200),
    .DEBOUNCE_CYC(240_000)
) u_counter (
    .clk(clk),
    .rst_n(rst_n),
    ...
);
```

Or use synthesis defines:
```verilog
`ifdef FPGA_TARGET
    localparam CLOCK_HZ = 12_000_000;
    localparam BAUD = 115_200;
`else
    localparam CLOCK_HZ = 1000;
    localparam BAUD = 100;
`endif
```

## Signal Width Summary

| Signal | Original | DigitalJS-Safe | Notes |
|--------|----------|----------------|-------|
| pulse_cycles | 32-bit | 24-bit | Max 16.7M cycles |
| duration | 32-bit | 24-bit | Max 16.7M value |
| counter | 32-bit | 24-bit | Overflow is OK |
| UART data | 8-bit | 8-bit | No change needed |

## Timing Summary

| Parameter | Real FPGA | DigitalJS Sim | Ratio |
|-----------|-----------|---------------|-------|
| Clock | 12 MHz | 1 kHz | 12000:1 |
| Baud | 115200 | 100 | 1152:1 |
| Cycles/bit | 104 | 10 | 10:1 |
| Debounce | 240000 | 2 | 120000:1 |

## UART Terminal Configuration

The JavaScript UART terminal must match the simulation parameters:

```javascript
// uart-terminal.js
this.clockHz = options.clockHz || 1000;    // Match CLOCK_HZ
this.baudRate = options.baudRate || 100;   // Match BAUD
this.cyclesPerBit = Math.round(this.clockHz / this.baudRate);  // = 10
```

## Testing Checklist

- [ ] Synthesis succeeds without errors
- [ ] Simulation runs without Vector3vl.toNumber() errors
- [ ] Button press (btn_n toggle) triggers "BTN x" message within seconds
- [ ] ACK command is received and LED flashes
- [ ] No JavaScript console errors during simulation

## Future Improvements

1. **Automatic parameter detection**: Read parameters from synthesized JSON
2. **Clock divider**: For FPGA, divide high clock internally
3. **Testbench parity**: Ensure same parameters work in Verilator/Icarus
4. **Configuration UI**: Let users adjust timing parameters in browser
