# CDC (Clock Domain Crossing) Pattern in Boon

**Date:** 2025-11-22
**Pattern:** 2-FF Synchronizer for metastability protection
**Status:** ✅ Naturally emergent from LATEST + PASSED.clk

---

## The Problem: Metastability

When an **asynchronous signal** (like a button press) enters a **synchronous clock domain**, it can violate setup/hold time constraints and cause **metastability**:

```
Async Signal:  ─────┐        ┌─────
               ─────┘        └─────
Clock:         ──┐  ┐  ┐  ┐  ┐  ┐──
                 └──┘  └──┘  └──┘
                     ↑
                Sampling here violates
                setup/hold time!
                Register may enter
                metastable state
```

**Metastability** = Register output is neither 0 nor 1, but stuck at intermediate voltage
- Can last for unpredictable time
- Can propagate to downstream logic as invalid value
- **Violates timing** and causes system failure

---

## The Solution: 2-FF Synchronizer

**Industry standard:** Use **two cascaded flip-flops** to safely bring async signals into clock domain.

### How It Works

```
Async Signal ──┐
               │
               ▼
          ┌────────┐     ┌────────┐
  clk ───▶│  FF 1  │────▶│  FF 2  │────▶ Safe Signal
          └────────┘     └────────┘
          sync_0         sync_1
          (may be         (stable)
           metastable)
```

1. **First FF (sync_0):** May go metastable when sampling async signal
2. **Second FF (sync_1):** Gives sync_0 a full clock cycle to settle
   - Even if sync_0 is metastable, it has time to resolve before sync_1 samples it
   - Probability of sync_1 also going metastable is **extremely low** (MTBF = billions of years)

### Mathematical Basis

**MTBF (Mean Time Between Failures):**
```
MTBF = e^(t_r / τ) / (f_clk × f_data × T_w)

Where:
  t_r   = Resolution time (clock period)
  τ     = Metastability time constant (~200ps for modern FFs)
  f_clk = Clock frequency
  f_data = Async signal frequency
  T_w   = Metastable window (~50ps)
```

**One FF:**  MTBF ≈ seconds to minutes ❌
**Two FFs:** MTBF ≈ millions of years ✅

---

## Boon Pattern: Explicit 2-FF Chain

### Pattern 1: Explicit (Clear, Verbose)

```boon
FUNCTION synchronizer(async_signal) {
    BLOCK {
        -- Stage 1: First register (may go metastable)
        sync_0: async_signal |> LATEST s0 {
            PASSED.clk |> THEN { async_signal }
        }

        -- Stage 2: Second register (metastability resolved)
        sync_1: sync_0 |> LATEST s1 {
            PASSED.clk |> THEN { sync_0 }
        }

        -- Safe synchronized signal (use this!)
        [synchronized: sync_1]
    }
}
```

**Why this works:**
- Each `LATEST` creates a flip-flop
- `PASSED.clk |> THEN` triggers on clock edge
- Data flows: `async_signal → sync_0 → sync_1`
- Two clock cycles of delay (latency = 2 cycles)

### Pattern 2: Library Function (Proposed)

From REMAINING_FEATURES_EMERGENCE.md:104-117:

```boon
-- Future standard library function
FUNCTION CDC/synchronize(signal, stages: 2) {
    -- Recursively build chain of LATEST blocks
    stages |> WHEN {
        1 => signal |> LATEST s { PASSED.clk |> THEN { signal } }
        __ => CDC/synchronize(
            signal: signal |> LATEST s { PASSED.clk |> THEN { signal } }
            stages: stages - 1
        )
    }
}

-- Usage
safe_signal: async_button
    |> CDC/synchronize(stages: 2)
```

---

## Example: Button Debouncer

The `debouncer.bn` module demonstrates CDC synchronizer in real hardware:

```boon
FUNCTION debouncer(btn_n, cntr_width) {
    BLOCK {
        -- CDC Synchronizer (2-FF chain)
        -- btn_n is async (external button), needs synchronization

        -- Stage 1: First register (may go metastable!)
        sync_0: btn_n |> LATEST s0 {
            PASSED.clk |> THEN { btn_n }
        }

        -- Stage 2: Second register (metastability resolved)
        sync_1: sync_0 |> LATEST s1 {
            PASSED.clk |> THEN { sync_0 }
        }

        -- Safe synchronized signal
        btn: sync_1 |> Bool/not()  -- Active-high after inversion

        -- Now safe to use 'btn' in rest of logic
        -- (debounce counter, state machine, etc.)
        ...
    }
}
```

---

## When to Use CDC Synchronizer

### ✅ Use CDC Synchronizer For:
1. **External async signals:**
   - Button presses
   - Sensor inputs
   - Serial data (UART RX)
   - External interrupts

2. **Signals crossing clock domains:**
   - Multi-clock designs (e.g., PCIe, DDR, video)
   - Async FIFOs (Gray code pointers)
   - Clock domain crossings in SoCs

### ❌ DON'T Use For:
1. **Signals already in same clock domain** (waste of resources)
2. **Multi-bit buses** (use async FIFO or Gray code instead!)
   - 2-FF only works for single-bit signals
   - Multi-bit buses can have different bits synchronized at different times → **invalid data!**

---

## Common Mistakes

### ❌ Mistake 1: Using async signal directly
```verilog
// WRONG: No synchronizer!
always_ff @(posedge clk) begin
    if (async_button) begin  // May see metastable value!
        counter <= counter + 1;
    end
end
```

### ✅ Correct: Synchronize first
```boon
sync_button: async_button
    |> LATEST s0 { PASSED.clk |> THEN { async_button } }
    |> LATEST s1 { PASSED.clk |> THEN { s0 } }

counter: counter |> LATEST count {
    PASSED.clk |> THEN {
        sync_button |> WHEN {
            True => count |> Bits/increment()
            False => count
        }
    }
}
```

### ❌ Mistake 2: Single FF (not enough)
```verilog
// WRONG: Only one FF!
always_ff @(posedge clk) begin
    sync <= async_signal;  // MTBF too short!
end
```

### ❌ Mistake 3: Synchronizing multi-bit bus
```verilog
// WRONG: Bits can sync at different cycles!
always_ff @(posedge clk) begin
    sync_bus[0] <= async_bus[0];  // May sync on cycle N
    sync_bus[1] <= async_bus[1];  // May sync on cycle N+1 !
end
// Result: Invalid data (bus value is half old, half new)
```

### ✅ Correct: Use async FIFO or Gray code
```boon
-- For multi-bit buses, use:
-- 1. Async FIFO (write in one domain, read in another)
-- 2. Gray code (only 1 bit changes at a time)
-- 3. Handshake protocol (req/ack)
```

---

## Hardware Synthesis

### Verilog Output

The Boon 2-FF pattern transpiles to:

```verilog
// Boon: sync_0 |> LATEST s0 { PASSED.clk |> THEN { async_signal } }
reg sync_0;
always_ff @(posedge clk) begin
    if (rst) begin
        sync_0 <= 1'b0;
    end else begin
        sync_0 <= async_signal;
    end
end

// Boon: sync_1 |> LATEST s1 { PASSED.clk |> THEN { sync_0 } }
reg sync_1;
always_ff @(posedge clk) begin
    if (rst) begin
        sync_1 <= 1'b0;
    end else begin
        sync_1 <= sync_0;
    end
end
```

### Synthesis Tool Recognition

Modern synthesis tools (Vivado, Quartus, etc.) **recognize** 2-FF patterns:
- Mark as synchronizer (don't optimize away!)
- Apply special timing constraints
- Use high-reliability FFs
- Report MTBF statistics

**Important:** Don't add combinational logic between FFs!
```verilog
// ❌ WRONG: Logic between FFs breaks synchronizer
sync_1 <= sync_0 & some_signal;  // NO!

// ✅ CORRECT: Pure register chain
sync_1 <= sync_0;  // YES!
```

---

## Timing Constraints (SDC/XDC)

### False Path Constraint

The async signal has **no timing relationship** to the clock:

```tcl
# Vivado XDC
set_false_path -from [get_ports async_button] -to [get_registers sync_0]
set_max_delay -from [get_ports async_button] -to [get_registers sync_0] [get_property PERIOD [get_clocks clk]]

# Quartus SDC
set_false_path -from [get_ports async_button] -to [get_registers sync_0]
```

### ASYNC_REG Property

Mark synchronizer FFs for tool recognition:

```verilog
(* ASYNC_REG = "TRUE" *) reg sync_0;
(* ASYNC_REG = "TRUE" *) reg sync_1;
```

**Boon could auto-generate these!**

---

## Advanced: Multi-Stage Synchronizers

For **very high-speed clocks** or **ultra-low failure rates**, use **3 or more FFs**:

```boon
FUNCTION synchronizer_3ff(async_signal) {
    BLOCK {
        sync_0: async_signal |> LATEST s0 {
            PASSED.clk |> THEN { async_signal }
        }
        sync_1: sync_0 |> LATEST s1 {
            PASSED.clk |> THEN { sync_0 }
        }
        sync_2: sync_1 |> LATEST s2 {
            PASSED.clk |> THEN { sync_1 }
        }
        [synchronized: sync_2]  -- 3 cycles latency
    }
}
```

**MTBF grows exponentially with stages:**
- 2 FFs: MTBF ≈ 10^6 years
- 3 FFs: MTBF ≈ 10^12 years
- 4 FFs: MTBF ≈ 10^18 years (universe lifetime!)

---

## References

1. **REMAINING_FEATURES_EMERGENCE.md:28-173** - CDC primitives in Boon
2. **Clifford Cummings - "Clock Domain Crossing (CDC) Design & Verification Techniques"**
3. **FPGA vendor app notes:**
   - Xilinx WP272: "Metastability in Xilinx FPGAs"
   - Intel AN 42: "Guidelines for Reliable Clock Domain Crossing"

---

## Conclusion

**CDC synchronizers are naturally emergent in Boon!**

- ✅ `LATEST` creates flip-flops
- ✅ `PASSED.clk |> THEN` creates clock-triggered behavior
- ✅ Two cascaded `LATEST` blocks = 2-FF synchronizer
- ✅ **No new primitives needed!**

**Boon makes CDC explicit and safe:**
- Clear dataflow (`async → sync_0 → sync_1`)
- Type-safe (can't accidentally bypass synchronizer)
- Self-documenting (pattern is obvious in code)

**Future enhancement:** `CDC/synchronize()` library function for convenience.

---

**Related Examples:**
- `debouncer.bn` - 2-FF CDC + debounce counter
- `uart_rx.bn` - 2-FF CDC for serial input
- See also: REMAINING_FEATURES_EMERGENCE.md (CDC analysis)
