# DigitalJS Testing Guide

**Purpose:** Test the super_counter hardware modules in the DigitalJS web-based simulator
**Tool:** https://digitaljs.tilk.eu/

---

## Quick Start

### Option 1: Test Simple Modules First (Recommended)

Start with the simplest module to verify DigitalJS works:

1. **Go to:** https://digitaljs.tilk.eu/
2. **Copy-paste** the code from `packed_simple_test.sv` (see below)
3. **Click** "Simulate" button
4. **Interact** with inputs (click buttons, toggle switches)
5. **Observe** outputs (LEDs, waveforms)

### Option 2: Test Full System

Use `packed_super_counter.sv` for the complete system (more complex, harder to debug)

---

## Simple Test Modules

### Test 1: LED Pulse (Easiest)

**File to test:** LED pulse generator

**Copy-paste this to DigitalJS:**

```systemverilog
// Simple LED pulse test
module led_pulse_test;
    logic clk;
    logic rst;
    logic trigger;
    logic [31:0] pulse_cycles;
    logic led;

    // Clock generator (DigitalJS uses this)
    initial begin
        clk = 0;
        forever #5 clk = ~clk;  // 100 MHz clock (10ns period)
    end

    // LED pulse module
    led_pulse #(
        .CLOCK_HZ(100_000_000)  // 100 MHz for simulation
    ) dut (
        .clk(clk),
        .rst(rst),
        .trigger(trigger),
        .pulse_cycles(pulse_cycles),
        .led(led)
    );

    // Test stimulus
    initial begin
        rst = 1;
        trigger = 0;
        pulse_cycles = 32'd100;  // 100 cycle pulse

        #20 rst = 0;           // Release reset
        #50 trigger = 1;       // Start pulse
        #10 trigger = 0;
        #1000;                 // Wait for pulse to finish

        // Try another pulse
        pulse_cycles = 32'd50;
        #50 trigger = 1;
        #10 trigger = 0;
        #500;

        $finish;
    end
endmodule

// LED Pulse module (inline)
module led_pulse #(
    parameter int CLOCK_HZ = 100_000_000
) (
    input  logic        clk,
    input  logic        rst,
    input  logic        trigger,
    input  logic [31:0] pulse_cycles,
    output logic        led
);
    logic [31:0] counter;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= 32'd0;
            led     <= 1'b0;
        end else begin
            if (trigger) begin
                counter <= pulse_cycles;
                led     <= 1'b1;
            end else if (counter != 32'd0) begin
                counter <= counter - 1'b1;
                led     <= 1'b1;
            end else begin
                led <= 1'b0;
            end
        end
    end
endmodule
```

**Expected behavior:**
- LED turns on when trigger pulses
- LED stays on for pulse_cycles clock cycles
- LED turns off automatically

**How to observe:**
- Click on waveform viewer to see LED signal timing
- Verify LED duration matches pulse_cycles

---

### Test 2: Debouncer (Medium Difficulty)

**Copy-paste this to DigitalJS:**

```systemverilog
// Debouncer test
module debouncer_test;
    logic clk;
    logic rst;
    logic btn_n;
    logic pressed;

    // Clock generator
    initial begin
        clk = 0;
        forever #5 clk = ~clk;  // 100 MHz
    end

    // Debouncer (use small counter for faster sim)
    debouncer #(
        .CNTR_WIDTH(4)  // Only 16 cycles for fast simulation
    ) dut (
        .clk(clk),
        .rst(rst),
        .btn_n(btn_n),
        .pressed(pressed)
    );

    // Test stimulus
    initial begin
        rst = 1;
        btn_n = 1;  // Released

        #20 rst = 0;

        // Simulate button press with bounce
        #50  btn_n = 0;  // Press
        #10  btn_n = 1;  // Bounce
        #5   btn_n = 0;  // Bounce
        #100 btn_n = 0;  // Stable press

        // Release with bounce
        #200 btn_n = 1;  // Release
        #10  btn_n = 0;  // Bounce
        #5   btn_n = 1;  // Stable release

        #500 $finish;
    end
endmodule

// Debouncer module (inline)
module debouncer #(
    parameter int CNTR_WIDTH = 4
) (
    input  logic clk,
    input  logic rst,
    input  logic btn_n,
    output logic pressed
);
    logic sync_0, sync_1;

    always_ff @(posedge clk) begin
        if (rst) begin
            sync_0 <= 1'b1;
            sync_1 <= 1'b1;
        end else begin
            sync_0 <= btn_n;
            sync_1 <= sync_0;
        end
    end

    logic btn = ~sync_1;

    logic [CNTR_WIDTH-1:0] counter;
    logic stable;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= '0;
            stable  <= 1'b0;
        end else begin
            if (btn != stable) begin
                if (counter == {CNTR_WIDTH{1'b1}}) begin
                    stable  <= btn;
                    counter <= '0;
                end else begin
                    counter <= counter + 1'b1;
                end
            end else begin
                counter <= '0;
            end
        end
    end

    logic stable_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            stable_prev <= 1'b0;
            pressed     <= 1'b0;
        end else begin
            stable_prev <= stable;
            pressed     <= stable && !stable_prev;
        end
    end
endmodule
```

**Expected behavior:**
- `pressed` pulses for one cycle when button stabilizes
- Ignores bounce noise (rapid transitions)
- CDC synchronizer prevents metastability

---

### Test 3: UART TX (Advanced)

**Copy-paste this to DigitalJS:**

```systemverilog
// UART TX test
module uart_tx_test;
    logic clk;
    logic rst;
    logic [7:0] data;
    logic start;
    logic busy;
    logic serial_out;

    // Fast clock for faster simulation
    initial begin
        clk = 0;
        forever #5 clk = ~clk;  // 100 MHz
    end

    uart_tx #(
        .CLOCK_HZ(100_000_000),
        .BAUD(1_000_000)  // 1 Mbaud for faster sim
    ) dut (
        .clk(clk),
        .rst(rst),
        .data(data),
        .start(start),
        .busy(busy),
        .serial_out(serial_out)
    );

    // Test stimulus
    initial begin
        rst = 1;
        start = 0;
        data = 8'h41;  // 'A'

        #20 rst = 0;

        // Send 'A' (0x41)
        #100 start = 1;
        #10  start = 0;

        // Wait for transmission
        wait(!busy);

        // Send 'B' (0x42)
        #100 data = 8'h42;
        #10  start = 1;
        #10  start = 0;

        wait(!busy);
        #500 $finish;
    end
endmodule

// UART TX module (inline)
module uart_tx #(
    parameter int CLOCK_HZ = 100_000_000,
    parameter int BAUD = 1_000_000
) (
    input  logic       clk,
    input  logic       rst,
    input  logic [7:0] data,
    input  logic       start,
    output logic       busy,
    output logic       serial_out
);
    localparam int DIVISOR = CLOCK_HZ / BAUD;
    localparam int CTR_WIDTH = $clog2(DIVISOR);

    logic [CTR_WIDTH-1:0] baud_counter;
    logic baud_tick;

    always_ff @(posedge clk) begin
        if (rst) begin
            baud_counter <= CTR_WIDTH'(DIVISOR - 1);
        end else begin
            if (busy) begin
                if (baud_counter == 0) begin
                    baud_counter <= CTR_WIDTH'(DIVISOR - 1);
                end else begin
                    baud_counter <= baud_counter - 1'b1;
                end
            end else begin
                baud_counter <= CTR_WIDTH'(DIVISOR - 1);
            end
        end
    end

    assign baud_tick = (baud_counter == 0);

    logic [9:0] shifter;
    logic [3:0] bit_idx;

    always_ff @(posedge clk) begin
        if (rst) begin
            busy       <= 1'b0;
            serial_out <= 1'b1;
            shifter    <= 10'h3FF;
            bit_idx    <= 4'd0;
        end else begin
            if (!busy) begin
                serial_out <= 1'b1;
                if (start) begin
                    busy    <= 1'b1;
                    shifter <= {1'b1, data, 1'b0};
                    bit_idx <= 4'd0;
                end
            end else if (baud_tick) begin
                serial_out <= shifter[0];
                shifter    <= {1'b1, shifter[9:1]};
                bit_idx    <= bit_idx + 1'b1;

                if (bit_idx == 4'd9) begin
                    busy <= 1'b0;
                end
            end
        end
    end
endmodule
```

**Expected behavior:**
- `serial_out` transmits: start bit (0) + 8 data bits + stop bit (1)
- `busy` goes high during transmission
- Transmission takes 10 bit periods

---

## Full System Test

**File:** `packed_super_counter.sv`

### Steps:

1. **Open DigitalJS:** https://digitaljs.tilk.eu/
2. **Copy entire contents** of `packed_super_counter.sv`
3. **Paste into DigitalJS editor**
4. **Click "Simulate"**

### Potential Issues:

⚠️ **DigitalJS Limitations:**
- May have trouble with large designs
- Parameters might not work perfectly
- Some SystemVerilog features may not be supported

**If full system doesn't work:**
1. Start with simple tests above
2. Test individual modules first
3. Report issues to DigitalJS project

### What to Observe:

**Button Path:**
- Press `btn_press_n` (active-low button)
- Watch debouncer synchronize and filter
- See counter increment in `seq_value`
- Observe UART TX send message bytes

**UART RX Path:**
- Send ASCII command via `uart_rx` input
- Watch parser FSM states
- See LED pulse when "ACK \<ms\>\n" received

---

## Interactive Testing in DigitalJS

### Basic Controls:

1. **Clock:**
   - Usually auto-generated by DigitalJS
   - Can adjust speed with slider

2. **Inputs:**
   - Click on input signals to toggle
   - Can create buttons for common inputs

3. **Outputs:**
   - Watch LED indicators
   - View waveforms for timing

4. **Reset:**
   - Toggle `rst` or `rst_n` to reset system
   - Should initialize all state

### Waveform Viewing:

1. **Right-click** on a signal
2. **Select "Add to waveform"**
3. **View timing diagram** at bottom

Useful signals to watch:
- `clk` - System clock
- `btn_pressed` - Debounced button
- `uart_tx` / `uart_rx` - Serial data
- `led_counter` - LED output
- Internal state machines

---

## Common DigitalJS Issues & Solutions

### Issue 1: Simulation doesn't start

**Solution:**
- Check for syntax errors (red underlines)
- Verify all parameters are constant integers
- Try removing `parameter int` and use `parameter` only

### Issue 2: Parameters not working

**Solution:**
Replace parameterized values with constants:

```systemverilog
// Instead of:
parameter int CLOCK_HZ = 12_000_000;
localparam int DIVISOR = CLOCK_HZ / BAUD;

// Use:
localparam int DIVISOR = 104;  // Pre-calculated
```

### Issue 3: Module not found

**Solution:**
- Ensure all modules are in the same file (packed version)
- Check module names match instantiations
- Verify module is defined before it's used

### Issue 4: Simulation too slow

**Solution:**
- Use faster clock in simulation (100 MHz vs 12 MHz)
- Reduce counter widths for faster sim
- Test smaller modules individually

### Issue 5: $clog2 not working

**Solution:**
Replace `$clog2()` with hard-coded values:

```systemverilog
// Instead of:
localparam int CTR_WIDTH = $clog2(DIVISOR);

// Use:
localparam int CTR_WIDTH = 7;  // For DIVISOR = 104
```

---

## Debugging Tips

### 1. Start Simple
- Test led_pulse first (simplest module)
- Then debouncer
- Then UART modules
- Finally full system

### 2. Use Waveforms
- Add all state machine states to waveforms
- Watch counter values
- Verify timing relationships

### 3. Reduce Complexity
- Lower BAUD rate divisors for faster sim
- Use smaller counter widths
- Remove unused features temporarily

### 4. Check Yosys Support
- DigitalJS uses Yosys for synthesis
- Some SystemVerilog features may not work
- Fall back to simpler Verilog constructs if needed

---

## Expected Results

### LED Pulse Test:
- ✅ LED turns on when triggered
- ✅ LED stays on for specified cycles
- ✅ LED turns off automatically

### Debouncer Test:
- ✅ Filters button bounce noise
- ✅ Single pulse output per press
- ✅ CDC synchronizer prevents metastability

### UART TX Test:
- ✅ Correct start/stop bits
- ✅ Data bits transmitted LSB-first
- ✅ Busy flag timing correct

### Full System:
- ✅ Button press increments counter
- ✅ Counter sent as ASCII via UART
- ✅ ACK command pulses LED

---

## Alternative: Local Simulation

If DigitalJS doesn't work well, try local simulation:

### Using Icarus Verilog:

```bash
# Install
sudo apt-get install iverilog gtkwave  # Linux
brew install icarus-verilog gtkwave    # Mac

# Compile and run
iverilog -g2012 -o sim packed_super_counter.sv
vvp sim

# View waveforms
gtkwave dump.vcd
```

### Using Verilator:

```bash
# Install
sudo apt-get install verilator

# Create testbench and simulate
verilator --cc --exe --build -j \
  packed_super_counter.sv testbench.cpp
./obj_dir/Vsuper_counter
```

---

## Next Steps After Testing

1. ✅ **Verify synthesis** - Test with Yosys
2. ✅ **Check timing** - Ensure no combinational loops
3. ✅ **Test on real FPGA** - Use original super_counter_rust project
4. ✅ **Report issues** - Document any DigitalJS incompatibilities

---

## Questions or Issues?

If you encounter problems:

1. **Try simpler tests first** (led_pulse, debouncer)
2. **Check DigitalJS documentation**: https://digitaljs.tilk.eu/
3. **Report Yosys issues**: https://github.com/YosysHQ/yosys
4. **Test locally** with Icarus or Verilator

---

## Files Reference

- `packed_super_counter.sv` - Full system (all modules inline)
- `led_pulse.sv`, `debouncer.sv`, etc. - Individual modules
- `*.bn` - Boon source (for transpiler development)
- `LIBRARY_MODULES.md` - API documentation

**Happy testing!** 🚀
