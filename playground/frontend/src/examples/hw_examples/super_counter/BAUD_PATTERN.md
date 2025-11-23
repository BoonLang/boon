# Baud Rate Generator Pattern in Boon

**Date:** 2025-11-22
**Pattern:** Clock divider for generating precise timing ticks
**Use Case:** UART communication, SPI, I2C, custom protocols
**Status:** ✅ Demonstrated in uart_tx.bn

---

## The Problem: Generating Precise Timing

**UART communication** requires precise bit timing:
- At **115200 baud**: Send 1 bit every 8.68 μs
- At **9600 baud**: Send 1 bit every 104.17 μs

**If your system clock is 25 MHz** (40 ns period):
- 115200 baud: 8.68 μs / 40 ns = **217 clock cycles per bit**
- 9600 baud: 104.17 μs / 40 ns = **2604 clock cycles per bit**

**Need:** A **clock enable signal** that pulses once every N clock cycles

---

## The Solution: Baud Rate Divider

**Divide system clock to generate baud rate tick:**

```
System Clock (25 MHz):
  ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐
──┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └──
  │←────── 217 cycles ────────→│

Baud Tick (115200 baud):
  ┌──────────────────────────────┐
──┘                              └────
  ^                              ^
  Tick                           Tick
  (enable UART operation)
```

### How It Works

**Counter counts from (DIVISOR-1) down to 0:**
1. Counter = 216 → 215 → 214 → ... → 1 → 0
2. When counter reaches 0: **Generate tick** (baud_tick = True)
3. Reload counter to (DIVISOR-1) and repeat

**Why count down instead of up?**
- Checking `counter == 0` is simpler than `counter == DIVISOR-1`
- Common hardware pattern

---

## Boon Pattern: Down-Counter with Reload

### Basic Pattern (Always Running)

```boon
FUNCTION baud_generator(clock_hz, baud_rate, divisor_width) {
    BLOCK {
        divisor: clock_hz / baud_rate
        divisor_minus_1: divisor - 1

        -- Counter counts down from (divisor-1) to 0
        counter: BITS[divisor_width] { 10u0 } |> LATEST count {
            PASSED.clk |> THEN {
                count |> WHEN {
                    -- Reached 0: reload and generate tick
                    BITS[divisor_width] { 10u0 } => divisor_minus_1 |> Bits/from_nat()
                    -- Otherwise: count down
                    __ => count |> Bits/decrement()
                }
            }
        }

        -- Tick signal (true when counter is 0)
        tick: counter == BITS[divisor_width] { 10u0 }

        [tick: tick]
    }
}
```

**Usage:**
```boon
baud_gen: baud_generator(
    clock_hz: 25000000
    baud_rate: 115200
    divisor_width: 8  -- $clog2(217) = 8 bits
)

-- Use tick to enable UART operations
uart_shift: baud_gen.tick |> WHEN {
    True => shift_next_bit()
    False => SKIP
}
```

---

### Advanced Pattern (Enable Control)

**For UART TX:** Only count when transmitting

```boon
FUNCTION baud_generator_with_enable(divisor, divisor_width, enable) {
    BLOCK {
        divisor_minus_1: divisor - 1

        counter: BITS[divisor_width] { 10u0 } |> LATEST count {
            PASSED.clk |> THEN {
                enable |> WHILE {
                    -- Enabled: count down
                    True => count |> WHEN {
                        BITS[divisor_width] { 10u0 } => divisor_minus_1 |> Bits/from_nat()
                        __ => count |> Bits/decrement()
                    }
                    -- Disabled: reload counter (ready to start)
                    False => divisor_minus_1 |> Bits/from_nat()
                }
            }
        }

        tick: (counter == BITS[divisor_width] { 10u0 }) |> Bool/and(enable)

        [tick: tick]
    }
}
```

**From uart_tx.bn:**
```boon
-- Baud counter only runs when busy (transmitting)
baud_counter: BITS[divisor_width] { 10u0 } |> LATEST baud_cnt {
    PASSED.clk |> THEN {
        busy |> WHILE {
            True => baud_cnt |> WHEN {
                BITS[divisor_width] { 10u0 } => divisor_minus_1 |> Bits/from_nat()
                __ => baud_cnt |> Bits/decrement()
            }
            False => divisor_minus_1 |> Bits/from_nat()
        }
    }
}

baud_tick: baud_counter == BITS[divisor_width] { 10u0 }
```

**Benefit:** Saves power (counter only runs when needed)

---

## Parameter Calculation: The $clog2 Problem

### The Issue

**Need to calculate counter width:**
```
baud_rate = 115200
clock_hz = 25000000
divisor = 25000000 / 115200 = 217

width_needed = ceil(log2(217)) = ceil(7.76) = 8 bits
```

**In SystemVerilog:**
```systemverilog
localparam int DIVISOR = CLOCK_HZ / BAUD;
localparam int CTR_WIDTH = $clog2(DIVISOR);  // ← Built-in!

logic [CTR_WIDTH-1:0] baud_cnt;
```

**In Boon (current):**
- ❌ No `$clog2()` equivalent yet
- ⚠️ Must pass width as parameter or hard-code

### Workarounds (Until Boon Gets $clog2)

#### Workaround 1: Pass Width as Parameter

```boon
FUNCTION uart_tx(clock_hz, baud_rate, divisor_width, data, start) {
    -- User must calculate: divisor_width = ceil(log2(clock_hz/baud_rate))
    -- For 115200 @ 25MHz: width = 8
    -- For 9600 @ 25MHz: width = 12
}
```

**Pros:** Flexible
**Cons:** User must do math manually

#### Workaround 2: Hard-Code Common Baud Rates

```boon
FUNCTION uart_tx_115200(data, start) {
    -- Hard-coded for 115200 baud @ 25MHz
    divisor: 217
    divisor_width: 8

    -- ... rest of implementation
}

FUNCTION uart_tx_9600(data, start) {
    -- Hard-coded for 9600 baud @ 25MHz
    divisor: 2604
    divisor_width: 12

    -- ... rest of implementation
}
```

**Pros:** Simple, no manual calculation
**Cons:** Not flexible, many functions needed

#### Workaround 3: Overprovision Bits

```boon
-- Use 16 bits (enough for any reasonable baud rate)
divisor_width: 16

-- Works for all baud rates, slightly wasteful
```

**Pros:** Simple, works everywhere
**Cons:** Wastes a few flip-flops (usually acceptable)

### Future: Add Math/clog2() to Boon

**Proposed:**
```boon
FUNCTION uart_tx(clock_hz, baud_rate, data, start) {
    divisor: clock_hz / baud_rate
    divisor_width: divisor |> Math/clog2()  -- ← Future feature!

    counter: BITS[divisor_width] { 10u0 } |> LATEST count {
        -- ... implementation
    }
}
```

**Transpiles to:**
```systemverilog
localparam int DIVISOR = CLOCK_HZ / BAUD;
localparam int CTR_WIDTH = $clog2(DIVISOR);
```

---

## Common Baud Rates (Reference Table)

**At 25 MHz system clock:**

| Baud Rate | Divisor | Width | Error |
|-----------|---------|-------|-------|
| 9600 | 2604 | 12 bits | 0.16% |
| 19200 | 1302 | 11 bits | 0.16% |
| 38400 | 651 | 10 bits | 0.16% |
| 57600 | 434 | 9 bits | 0.79% |
| 115200 | 217 | 8 bits | 0.16% |
| 230400 | 108 | 7 bits | 1.73% |
| 460800 | 54 | 6 bits | 1.73% |
| 921600 | 27 | 5 bits | 1.73% |

**At 12 MHz system clock (iCE40 default):**

| Baud Rate | Divisor | Width | Error |
|-----------|---------|-------|-------|
| 9600 | 1250 | 11 bits | 0% |
| 115200 | 104 | 7 bits | 1.73% |

**Rule of thumb:** Error < 2% is acceptable for UART

---

## SystemVerilog Output (Ideal Transpiler)

**Boon:**
```boon
baud_counter: BITS[divisor_width] { 10u0 } |> LATEST count {
    PASSED.clk |> THEN {
        count |> WHEN {
            BITS[divisor_width] { 10u0 } => divisor_minus_1 |> Bits/from_nat()
            __ => count |> Bits/decrement()
        }
    }
}

tick: counter == BITS[divisor_width] { 10u0 }
```

**Transpiles to:**
```systemverilog
logic [DIVISOR_WIDTH-1:0] baud_counter;
logic baud_tick;

always_ff @(posedge clk) begin
    if (rst) begin
        baud_counter <= DIVISOR[DIVISOR_WIDTH-1:0] - 1'b1;
    end else begin
        if (baud_counter == '0) begin
            baud_counter <= DIVISOR[DIVISOR_WIDTH-1:0] - 1'b1;
        end else begin
            baud_counter <= baud_counter - 1'b1;
        end
    end
end

assign baud_tick = (baud_counter == '0);
```

**Clean, readable output** that Yosys/DigitalJS accepts ✅

---

## Timing Analysis

### Waveform Example (115200 baud @ 25MHz)

```
Clock (25 MHz):
  ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐ ┌─┐     ┌─┐ ┌─┐
──┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─┘ └─...─┘ └─┘ └──

Counter:
  216  215  214  213  212  211 ... 2   1   0   216

Baud Tick:
  ────────────────────────────...──────────┐   ┌────
                                           └───┘
                                           ^
                                        8.68 μs period
```

### Verification

**Period = DIVISOR × Clock Period**

```
At 115200 baud, 25 MHz clock:
Period = 217 × 40ns = 8.68 μs
Frequency = 1 / 8.68μs = 115207 Hz

Error = (115207 - 115200) / 115200 = 0.006% ✅
```

**Extremely accurate!**

---

## Usage in UART TX

**Complete example from uart_tx.bn:**

```boon
FUNCTION uart_tx(rst, data, start, clock_hz, baud_rate, divisor_width) {
    BLOCK {
        divisor: clock_hz / baud_rate
        divisor_minus_1: divisor - 1

        -- Baud rate generator
        baud_counter: BITS[divisor_width] { 10u0 } |> LATEST baud_cnt {
            PASSED.clk |> THEN {
                busy |> WHILE {
                    True => baud_cnt |> WHEN {
                        BITS[divisor_width] { 10u0 } => divisor_minus_1 |> Bits/from_nat()
                        __ => baud_cnt |> Bits/decrement()
                    }
                    False => divisor_minus_1 |> Bits/from_nat()
                }
            }
        }

        baud_tick: baud_counter == BITS[divisor_width] { 10u0 }

        -- Use tick to control shift register
        shifter: BITS[10] { 2u1023 } |> LATEST shift {
            PASSED.clk |> THEN {
                baud_tick |> WHILE {
                    True => shift |> Bits/shift_right(by: 1) |> Bits/or(BITS[10] { 2u512 })
                    False => shift
                }
            }
        }

        -- Output LSB of shifter on each baud tick
        serial_out: shifter |> Bits/and(BITS[10] { 2u1 }) == BITS[10] { 2u1 }

        [serial_out: serial_out]
    }
}
```

---

## Other Uses

**This pattern works for any precise timing:**

### SPI Clock Generation
```boon
-- Generate SPI clock (e.g., 1 MHz from 25 MHz)
spi_divider: 25  -- 25 MHz / 25 = 1 MHz
spi_tick: baud_generator(25000000, 1000000, 5)
```

### I2C Clock Generation
```boon
-- Generate I2C SCL (e.g., 100 kHz from 25 MHz)
i2c_divider: 250  -- 25 MHz / 250 = 100 kHz
i2c_tick: baud_generator(25000000, 100000, 8)
```

### Custom Timing
```boon
-- Blink LED every 1 second @ 25 MHz
blink_divider: 25000000  -- 1 Hz
blink_tick: baud_generator(25000000, 1, 25)
```

---

## Testing Strategy

### Simulation Check

**Verify tick period:**
```systemverilog
// Testbench
int tick_count = 0;
realtime last_tick_time = 0;

always @(posedge clk) begin
    if (baud_tick) begin
        if (tick_count > 0) begin
            realtime period = $realtime - last_tick_time;
            $display("Baud period: %0t (expected: 8.68us)", period);
        end
        last_tick_time = $realtime;
        tick_count++;
    end
end
```

### Logic Analyzer

**On real hardware:**
- Probe `serial_out` pin
- Measure bit period
- Compare to expected (1/baud_rate)

**Example (115200 baud):**
- Expected: 8.68 μs per bit
- Measured: Should be within ±2%

---

## Common Mistakes

### ❌ Mistake 1: Counting Up (Off-by-One)

```verilog
// WRONG: Harder to detect reload condition
if (counter == DIVISOR - 1) begin
    counter <= 0;
    tick <= 1;
end else begin
    counter <= counter + 1;
    tick <= 0;
end
```

**Problem:** `counter == DIVISOR - 1` is more complex than `counter == 0`

### ❌ Mistake 2: Continuous Tick

```verilog
// WRONG: Tick is high for entire cycle!
assign tick = (counter == 0);

// RIGHT: Tick is pulse (one cycle only)
always_ff @(posedge clk) begin
    tick <= (counter == 0);  // Registered, one cycle
end
```

### ❌ Mistake 3: Wrong Width

```boon
-- WRONG: Counter too narrow, overflows!
divisor: 2604  -- Needs 12 bits
counter: BITS[8] { 10u0 }  -- Only 8 bits, max=255

-- RIGHT: Match width to divisor
counter: BITS[12] { 10u0 }  -- 12 bits, max=4095
```

---

## Summary

**Baud rate generator = Down-counter with reload**

**Pattern:**
1. Counter counts from (DIVISOR-1) to 0
2. Generate tick when counter reaches 0
3. Reload counter and repeat

**Benefits:**
- ✅ Precise timing (error < 0.2% typical)
- ✅ Simple logic (just decrement + compare)
- ✅ Flexible (any baud rate)
- ✅ Power-efficient (can enable/disable)

**Current limitation:**
- ⚠️ Need to manually calculate width (or overprovision bits)

**Future enhancement:**
- ✅ Add `Math/clog2()` to Boon for automatic width calculation

---

**Related Examples:**
- `uart_tx.bn` - Complete UART transmitter with baud generator
- `uart_rx.bn` - UART receiver (similar pattern)

**See Also:**
- TRANSPILER_TARGET.md - Why SystemVerilog
- CDC_PATTERN.md - Clock domain crossing
