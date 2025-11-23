# Super Counter - Boon Conversion Analysis

**Date:** 2025-11-22
**Source:** `/home/martinkavik/repos/super_counter_rust/hardware/src/`
**Goal:** Convert real-world UART counter system to Boon, identify missing features

---

## Project Overview

The **super_counter** is a complete UART-based button counter system for FPGA:

### System Architecture
```
┌─────────────┐     ┌──────────────┐     ┌─────────┐
│  btn_press  │────▶│  debouncer   │────▶│   btn   │
└─────────────┘     └──────────────┘     │ message │
                                          └────┬────┘
                                               │
┌─────────────┐     ┌──────────────┐          ▼
│  uart_rx    │────▶│ ack_parser   │     ┌─────────┐
└─────────────┘     └──────┬───────┘     │ uart_tx │──▶ TX
                           │             └─────────┘
                           ▼
                    ┌──────────────┐
                    │  led_pulse   │──▶ LED
                    └──────────────┘
```

### Module Summary

| Module | Lines | Complexity | Features |
|--------|-------|------------|----------|
| **super_counter.v** | 91 | ⭐ Simple | Hierarchy, port wiring |
| **led_pulse.v** | 32 | ⭐ Simple | Counter, conditional logic |
| **debouncer.v** | 49 | ⭐⭐ Moderate | CDC (2-FF sync), FSM-like |
| **uart_tx.v** | 62 | ⭐⭐ Moderate | FSM, baud timing, shifter |
| **uart_rx.v** | 72 | ⭐⭐⭐ Complex | FSM, CDC, baud timing |
| **ack_parser.v** | 87 | ⭐⭐⭐ Complex | FSM, ASCII parsing, function |
| **btn_message.v** | 153 | ⭐⭐⭐⭐ Very Complex | FSM, BCD arithmetic, arrays, functions |

---

## Feature Analysis

### ✅ Features Boon Already Supports

1. **Basic FSM** (state machines)
   - Used in: All modules except led_pulse
   - Boon pattern: `state |> LATEST state { PASSED.clk |> THEN { ... } }`
   - Example: `fsm.bn`

2. **Counters & Timers**
   - Used in: led_pulse (down counter), uart_tx/rx (baud divider)
   - Boon pattern: `Bits/sum(delta: ...)` or `Bits/set(...)`
   - Example: `counter.bn`

3. **Generic Parameters**
   - Used in: All modules (CLOCK_HZ, BAUD, widths)
   - Boon pattern: Function parameters with compile-time constants
   - Example: `FUNCTION alu(width) { ... }`

4. **Pattern Matching**
   - Used in: FSM state transitions
   - Boon pattern: `state |> WHEN { ... }`
   - Example: `fsm.bn`, `alu.bn`

5. **Boolean Logic**
   - Used in: Control signals, conditions
   - Boon pattern: `Bool/and()`, `Bool/or()`, etc.
   - Example: `sr_gate.bn`

6. **Bit Manipulation**
   - Used in: Shifters in UART
   - Boon pattern: `Bits/shift_right()`, `Bits/set()`
   - Example: `lfsr.bn`

### ⚠️ Features Needing New Patterns

1. **CDC (Clock Domain Crossing) - 2-FF Synchronizer**
   - **Used in:** `debouncer.v:11-25`, `uart_rx.v:20-33`
   - **Pattern:** Two cascaded registers for metastability
   ```verilog
   reg sync_0 = 1'b1;
   reg sync_1 = 1'b1;
   always @(posedge clk) begin
       sync_0 <= async_input;
       sync_1 <= sync_0;  // Use sync_1 (safe)
   end
   ```
   - **Boon Status:** 🟡 85% emergent (see REMAINING_FEATURES_EMERGENCE.md:28-173)
   - **Proposed Boon:**
   ```boon
   -- Pattern 1: Explicit (verbose but clear)
   sync0: async_input |> LATEST s0 {
       PASSED.clk |> THEN { async_input }
   }
   sync1: sync0 |> LATEST s1 {
       PASSED.clk |> THEN { sync0 }
   }
   safe_signal: sync1  -- Use this

   -- Pattern 2: Library function (preferred)
   safe_signal: async_input
       |> CDC/synchronize(stages: 2)
   ```

2. **Compile-Time Width Calculation ($clog2)**
   - **Used in:** `uart_tx.v:14`, `uart_rx.v:13`
   ```verilog
   localparam integer CTR_WIDTH = $clog2(DIVISOR);
   ```
   - **Boon Status:** ❌ Missing
   - **Proposed Boon:**
   ```boon
   divisor: clock_hz / baud_rate
   ctr_width: divisor |> Math/log2() |> Math/ceil()
   -- OR: Add to Bits module
   ctr_width: divisor |> Bits/clog2()
   ```

3. **Register Arrays (Fixed-Size)**
   - **Used in:** `btn_message.v:18-19`
   ```verilog
   reg [7:0] msg [0:9];           // 10 bytes
   reg [3:0] bcd_digits [0:4];    // 5 BCD digits
   ```
   - **Boon Status:** ⚠️ Could use LIST or MEMORY
   - **Proposed Boon:**
   ```boon
   -- Option 1: LIST (for small arrays)
   msg: LIST[10] { BITS[8] { 10u0 } } |> LATEST msg {
       PASSED.clk |> THEN {
           msg |> List/set(index: 0, value: BITS[8] { 10u66 })  -- 'B'
       }
   }

   -- Option 2: MEMORY (for larger arrays, or consistency with RAM)
   msg: MEMORY[10] { BITS[8] { 10u0 } }
       |> Memory/write_entry(entry: [address: 0, data: BITS[8] { 10u66 }])
   ```

4. **Functions within Modules**
   - **Used in:** `btn_message.v:21-36` (ascii_digit), `ack_parser.v:23-28` (ms_to_cycles)
   ```verilog
   function automatic [7:0] ascii_digit(input [3:0] val);
       case (val)
           4'd0: ascii_digit = 8'h30;
           4'd1: ascii_digit = 8'h31;
           // ...
       endcase
   endfunction
   ```
   - **Boon Status:** ✅ FUNCTION exists
   - **Proposed Boon:**
   ```boon
   FUNCTION ascii_digit(val) {
       val |> WHEN {
           BITS[4] { 10u0 } => BITS[8] { 16u30 }  -- '0'
           BITS[4] { 10u1 } => BITS[8] { 16u31 }  -- '1'
           -- ...
           BITS[4] { 10u9 } => BITS[8] { 16u39 }  -- '9'
       }
   }
   ```

5. **Module Hierarchy & Port Connections**
   - **Used in:** `super_counter.v` (top-level instantiation)
   ```verilog
   debouncer #(.CNTR_WIDTH(DEBOUNCE_BITS)) debouncer_inst (
       .clk(clk_12m),
       .rst(rst),
       .btn_n(btn_press_n),
       .pressed(btn_pressed)
   );
   ```
   - **Boon Status:** 🟡 80% emergent (see REMAINING_FEATURES_EMERGENCE.md:773-857)
   - **Proposed Boon:**
   ```boon
   FUNCTION super_counter(clk_12m, rst_n, btn_press_n, uart_rx_in) {
       BLOCK {
           -- Establish PASSED context for all children
           PASSED: [clk: clk_12m]

           rst: rst_n |> Bool/not()

           -- Instantiate debouncer (accesses PASSED.clk)
           debouncer_out: debouncer(
               btn_n: btn_press_n
               cntr_width: 18
           )

           -- Wire outputs to inputs
           btn_msg_out: btn_message(
               btn_pressed: debouncer_out.pressed
               uart_busy: uart_tx_out.busy
           )

           uart_tx_out: uart_tx(
               data: btn_msg_out.uart_data
               start: btn_msg_out.uart_start
           )

           [uart_tx: uart_tx_out.serial_out]
       }
   }
   ```

### ⚠️ Features That Are Very Complex

1. **BCD Arithmetic with Ripple Carry**
   - **Used in:** `btn_message.v:62-74`
   ```verilog
   carry = 1'b1;
   for (i = 0; i < 5; i = i + 1) begin
       if (carry) begin
           if (bcd_digits[i] == 4'd9) begin
               bcd_digits[i] <= 4'd0;
               carry = 1'b1;
           end else begin
               bcd_digits[i] <= bcd_digits[i] + 4'd1;
               carry = 1'b0;
           end
       end
   end
   ```
   - **Challenge:** Verilog uses blocking assignments in combinational block with loop
   - **Proposed Boon:** Use List/fold with accumulator pattern
   ```boon
   -- Increment BCD digit array (5 digits, little-endian)
   new_bcd: bcd_digits
       |> List/fold(
           init: [digits: LIST[5] { BITS[4] { 10u0 } }, carry: True]
           digit, acc: BLOCK {
               result: acc.carry |> WHEN {
                   True => digit |> WHEN {
                       BITS[4] { 10u9 } => [
                           new_digit: BITS[4] { 10u0 }
                           carry: True
                       ]
                       __ => [
                           new_digit: digit |> Bits/increment()
                           carry: False
                       ]
                   }
                   False => [new_digit: digit, carry: False]
               }
               [
                   digits: acc.digits |> List/append(result.new_digit)
                   carry: result.carry
               ]
           }
       )
   ```

2. **ASCII Character Comparison & Parsing**
   - **Used in:** `ack_parser.v:38-83`
   ```verilog
   if (data == "A") state <= STATE_A;
   if (data >= "0" && data <= "9") begin
       digit_value = {24'd0, data} - 32'd48;
   ```
   - **Challenge:** Character literals, range checks
   - **Proposed Boon:**
   ```boon
   data |> WHEN {
       BITS[8] { 16u41 } => state_a  -- 'A' = 0x41
       __ => state_idle
   }

   -- Check if ASCII digit ('0' to '9')
   is_digit: (data >= BITS[8] { 16u30 })
       |> Bool/and(data <= BITS[8] { 16u39 })

   digit_value: data |> Bits/sub(BITS[8] { 16u30 })  -- data - '0'
   ```

3. **Dynamic Array Indexing Based on Count**
   - **Used in:** `btn_message.v:92-128` (variable-length message)
   ```verilog
   case (digits_count)
       5: begin
           msg[4] <= ascii_digit(bcd_digits[4]);
           msg[5] <= ascii_digit(bcd_digits[3]);
           // ...
       end
       4: begin
           msg[4] <= ascii_digit(bcd_digits[3]);
           // ...
   ```
   - **Challenge:** Same array index (msg[4]) gets different values based on count
   - **Proposed Boon:** Use pattern matching to generate different arrays
   ```boon
   msg: digits_count |> WHEN {
       5 => LIST {
           BITS[8] { 16u42 }  -- 'B'
           BITS[8] { 16u54 }  -- 'T'
           BITS[8] { 16u4E }  -- 'N'
           BITS[8] { 16u20 }  -- ' '
           bcd_digits |> List/get(index: 4) |> ascii_digit()
           bcd_digits |> List/get(index: 3) |> ascii_digit()
           bcd_digits |> List/get(index: 2) |> ascii_digit()
           bcd_digits |> List/get(index: 1) |> ascii_digit()
           bcd_digits |> List/get(index: 0) |> ascii_digit()
           BITS[8] { 16u0A }  -- '\n'
       }
       4 => LIST {
           -- Similar but only 4 digits
       }
       -- ...
   }
   ```

---

## Conversion Strategy

### Phase 1: Simple Modules (Start Here) ✅

These modules use only existing Boon features:

1. **✅ led_pulse.v** (32 lines)
   - Features: Counter, conditional logic
   - Boon patterns: `Bits/sum`, `WHEN`, `LATEST`
   - Difficulty: ⭐ Easy
   - **Status: Start with this**

2. **⚠️ debouncer.v** (49 lines)
   - Features: CDC synchronizer, counter, FSM-like
   - Boon patterns: Explicit 2-FF pattern (not library yet)
   - Difficulty: ⭐⭐ Moderate (introduces CDC pattern)
   - **Status: Second module**

### Phase 2: UART Modules (Moderate Complexity) ⚠️

3. **⚠️ uart_tx.v** (62 lines)
   - Features: FSM, baud timing, shifter
   - Missing: $clog2() for width calculation
   - Workaround: Hard-code width or calculate manually
   - Difficulty: ⭐⭐ Moderate
   - **Status: Third module**

4. **⚠️ uart_rx.v** (72 lines)
   - Features: FSM, CDC, baud timing, bit sampling
   - Missing: $clog2()
   - Difficulty: ⭐⭐⭐ Complex (FSM + CDC + timing)
   - **Status: Fourth module**

### Phase 3: Complex FSMs (Deferred) ❌

These modules need features not yet in Boon or are very complex:

5. **❌ ack_parser.v** (87 lines) - **DEFER**
   - Features: FSM, ASCII parsing, character comparison, function
   - Missing: Clean ASCII literal syntax
   - Difficulty: ⭐⭐⭐ Complex
   - **Reason:** ASCII pattern matching is verbose in current Boon

6. **❌ btn_message.v** (153 lines) - **DEFER**
   - Features: Complex FSM, BCD arithmetic, register arrays, functions, for-loops
   - Missing: Clean array update patterns
   - Difficulty: ⭐⭐⭐⭐ Very Complex
   - **Reason:** BCD increment loop and dynamic message formatting are very complex

7. **❌ super_counter.v** (91 lines) - **DEFER**
   - Features: Module instantiation, port wiring
   - Missing: Complete set of sub-modules in Boon
   - Difficulty: ⭐ Simple (once sub-modules done)
   - **Reason:** Depends on all other modules

---

## Verilog vs SystemVerilog Decision

**Recommendation: Keep as Verilog, add SystemVerilog version**

### Current Files
- All source files are **Verilog (.v)**
- Testbench is **SystemVerilog (.sv)**
- Design uses only synthesizable Verilog subset

### Proposal
For each Boon module, provide:
1. **`.bn`** - Boon source (primary)
2. **`.v`** - Verilog output from transpiler (for compatibility)
3. **`.sv`** - SystemVerilog version (cleaner syntax, for comparison)

**Rationale:**
- Verilog is more universal (works everywhere)
- SystemVerilog is cleaner (always_ff, logic type, etc.)
- Having both shows transpiler flexibility

---

## DigitalJS Compatibility

**Status:** ⚠️ Partial compatibility expected

### DigitalJS Limitations (from testing)
- ✅ Supports: Basic always blocks, case statements, simple FSMs
- ⚠️ Limited: Parameterization, complex timing
- ❌ No support: $clog2, advanced SystemVerilog features

### Modules Likely to Work
1. **led_pulse** - Simple counter ✅
2. **debouncer** - Should work ✅
3. **uart_tx** - May work if we hard-code widths ⚠️
4. **uart_rx** - Same as uart_tx ⚠️

### Recommendation
- **Test led_pulse and debouncer first** in DigitalJS
- For UART modules, provide **non-parameterized versions** for DigitalJS
- Include both parameterized (.v) and fixed-width (.digitaljs.v) versions

---

## New Features to Document

Based on this conversion, we should document these **new patterns**:

### 1. CDC Synchronizer Pattern
**File:** `super_counter/CDC_PATTERN.md`
```boon
-- 2-FF Synchronizer (metastability protection)
FUNCTION synchronizer(async_signal) {
    sync0: async_signal |> LATEST s0 {
        PASSED.clk |> THEN { async_signal }
    }
    sync1: sync0 |> LATEST s1 {
        PASSED.clk |> THEN { sync0 }
    }
    [synchronized: sync1]
}
```

### 2. Register Array Pattern
**File:** `super_counter/ARRAY_PATTERN.md`
```boon
-- Fixed-size register array
FUNCTION register_array(size, width) {
    array: LIST[size] { BITS[width] { 10u0 } }
        |> LATEST arr {
            PASSED.clk |> THEN {
                -- Update element 0
                arr |> List/set(index: 0, value: new_value)
            }
        }
    [array: array]
}
```

### 3. Baud Rate Generator Pattern
**File:** `super_counter/BAUD_PATTERN.md`
```boon
-- Baud rate divider (clock enable generation)
FUNCTION baud_generator(clock_hz, baud_rate) {
    divisor: clock_hz / baud_rate

    counter: BITS[16] { 10u0 } |> LATEST count {
        PASSED.clk |> THEN {
            count |> WHEN {
                divisor - 1 => BITS[16] { 10u0 }  -- Reset
                __ => count |> Bits/increment()    -- Count up
            }
        }
    }

    tick: counter == (divisor - 1)
    [tick: tick]
}
```

---

## Next Steps

### Immediate Actions
1. ✅ Create `super_counter/` folder structure
2. ✅ Write this analysis document
3. ➡️ **Convert led_pulse.v to led_pulse.bn** (simplest module)
4. ➡️ Convert debouncer.v to debouncer.bn (introduces CDC)
5. ➡️ Test both in DigitalJS (if possible)

### Documentation Needed
- CDC_PATTERN.md - 2-FF synchronizer explanation
- ARRAY_PATTERN.md - Register array usage
- BAUD_PATTERN.md - Clock divider pattern
- HIERARCHY_PATTERN.md - Module instantiation (once done)

### Future Work (Requires New Features)
- Implement $clog2() equivalent in Boon
- Add CDC/synchronize() to standard library
- Design better ASCII literal syntax
- Explore BCD arithmetic patterns

---

## Conclusion

**Feasibility: ✅ Partially Feasible**

- **Immediate:** 2 modules (led_pulse, debouncer) can be converted **now**
- **Near-term:** 2 modules (uart_tx, uart_rx) can be converted with **workarounds** (hard-coded widths)
- **Future:** 3 modules (btn_message, ack_parser, super_counter) need **new features** or are very complex

**Value: ⭐⭐⭐⭐⭐ Very High**
- Real-world hardware design (not toy example)
- Introduces important patterns (CDC, UART, hierarchy)
- Identifies missing features through practical use
- Will create comprehensive pattern library

**Recommendation:**
1. **Start now** with led_pulse and debouncer
2. **Document patterns** as we discover them
3. **Test DigitalJS compatibility** with simple modules
4. **Defer complex modules** until we have better patterns or language features

This conversion will **significantly advance** Boon's hardware design capability!
