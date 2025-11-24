# DigitalJS Testing Guide - Super Counter

**Quick Start:** Copy `packed_super_counter.sv` to [DigitalJS](https://digitaljs.tilk.eu/) to test the complete system!

---

## 🎯 What You Can Test

### 1️⃣ **Button Counter**
- **Press button** → Counter increments → UART sends "BTN N\n"
- Watch `seq_value` count up: 1, 2, 3...
- See `btn_pulse` pulse on each press
- Observe `uart_tx` transmit ASCII bytes

### 2️⃣ **UART Reception & LED Control**
- **Send "ACK 100\n"** → LED turns on for 100ms
- Type via `uart_rx` input (see below)
- Watch `led_counter` output

---

## 📋 Step-by-Step Instructions

### **Method 1: Quick Test (Recommended)**

1. **Open DigitalJS:** https://digitaljs.tilk.eu/

2. **Load the design:**
   - Copy entire contents of `packed_super_counter.sv`
   - Paste into DigitalJS editor
   - Click **"Simulate"** button

3. **Initial state:**
   - Leave `rst` **unchecked** (normal operation)
   - Leave `btn_press` **unchecked** (button not pressed)
   - Leave `uart_rx` **unchecked**

4. **Test the counter:**
   - **Check** `btn_press` checkbox → Counter increments to 1
   - **Uncheck** `btn_press` → Ready for next press
   - **Check** again → Counter increments to 2
   - Repeat!

5. **Watch outputs:**
   - `seq_value` = Current count (in hex, e.g., 0x0001, 0x0002...)
   - `btn_pulse` = Pulses high for 1 clock when pressed
   - `btn_debounced` = Stable button state
   - `uart_tx` = Serial output (you'll see it toggle)

---

## 📊 Understanding UART Output

### What's Being Sent?

Each button press sends an ASCII message via `uart_tx`:

```
Press 1: "BTN 1\n"   (6 bytes)
Press 2: "BTN 2\n"   (6 bytes)
Press 3: "BTN 3\n"   (6 bytes)
...
Press 10: "BTN 10\n" (7 bytes)
```

### How to View UART Data in DigitalJS:

**Option A: Waveform Viewer** (See the bits)
1. Right-click on `uart_tx` signal
2. Select "Add to waveform"
3. Watch the serial bits in timing diagram
4. You'll see the start bit (0), 8 data bits, stop bit (1)

**Option B: Monitor Module** (Decode ASCII)

Unfortunately, DigitalJS doesn't have built-in UART ASCII display. To see actual text, you would need to:
- Manually decode the waveform (tedious)
- Or use the test wrapper below

---

## 🔧 Advanced: Test Wrapper with UART Monitor

For easier testing, I created `super_counter_digitaljs_test.sv` with:
- **TX Monitor:** Displays transmitted ASCII bytes
- **ACK Generator:** Sends "ACK 100\n" command automatically

### Using the Test Wrapper:

1. Copy `super_counter_digitaljs_test.sv` to DigitalJS
2. Click "Simulate"
3. Inputs:
   - `btn_press` - Press button
   - `send_ack` - Send ACK command (turns on LED)
   - `rst` - Reset system
4. Outputs:
   - `tx_ascii` - Last transmitted byte (in hex)
   - `tx_byte_valid` - Pulses when new byte sent
   - `led_counter` - LED state
   - `ack_busy` - Sending ACK command

**Note:** The test wrapper requires both files. For simplicity, use the main `packed_super_counter.sv` first.

---

## 💡 Testing the LED (ACK Command)

The system responds to: **"ACK <duration_ms>\n"**

Example: `ACK 100\n` turns LED on for 100 milliseconds

### How to Send ACK in DigitalJS:

**This is the tricky part!** You need to send serial data bit-by-bit.

#### Manual Method (Very Tedious):

For "ACK 100\n", you'd need to manually toggle `uart_rx` to send:
1. Start bit (0)
2. 8 data bits for 'A' (0x41 = 01000001)
3. Stop bit (1)
4. Repeat for C, K, space, 1, 0, 0, newline...

**This is NOT practical** for manual testing!

#### Recommended Method:

**Use a loopback test:**
1. In `packed_super_counter.sv`, temporarily connect:
   ```systemverilog
   assign uart_rx = uart_tx;  // Loopback
   ```
2. Now when you press the button, it sends "BTN N\n"
3. The receiver will see it (won't trigger LED, but you can verify RX works)

**Or modify ACK parser** to trigger on button press:
```systemverilog
// Temporary test: trigger LED on any RX byte
assign ack_trigger = rx_valid;
assign led_cycles = 32'd1200;  // ~100ms at 12MHz
```

---

## 🎮 What You Should See

### Normal Operation:

```
Initial State:
  seq_value = 0x0000
  btn_pulse = 0
  led_counter = 0
  uart_tx = 1 (idle high)

Button Press #1:
  seq_value = 0x0001
  btn_pulse = 1 (for 1 clock)
  uart_tx = toggles (sending "BTN 1\n")

After ~280 clocks:
  UART transmission complete
  Ready for next press

Button Press #2:
  seq_value = 0x0002
  btn_pulse = 1 (for 1 clock)
  uart_tx = toggles (sending "BTN 2\n")
```

### With ACK Command (if you can send it):

```
Receive "ACK 100\n":
  led_counter = 1 (turns on)

After 1,200,000 clocks (100ms @ 12MHz, or ~300 clocks in FAST_SIM):
  led_counter = 0 (turns off)
```

---

## 🔍 Key Signals to Monitor

Add these to the waveform viewer:

**Button Path:**
- `btn_press` - Your input
- `btn_debounced` - After debounce filter
- `btn_pulse` - Single-cycle pulse
- `seq_value` - Counter value

**UART TX Path:**
- `uart_tx` - Serial output
- `tx_busy` - Transmission active
- `tx_start` - Trigger pulse

**UART RX Path:**
- `uart_rx` - Serial input
- `rx_valid` - Byte received pulse
- `rx_data` - Received byte

**LED:**
- `led_counter` - LED output
- `ack_trigger` - ACK command detected

---

## ⚙️ Simulation Parameters

The design uses **FAST_SIM mode** by default:

```systemverilog
parameter bit FAST_SIM = 1  // DigitalJS friendly
```

This means:
- ✅ UART divisor = 4 (instead of 104)
- ✅ Byte transmission = ~40 clocks (instead of 1,040)
- ✅ Full message = ~280 clocks (instead of 7,280)
- ✅ LED pulse = ~300 clocks (instead of 1,200,000)

**For real hardware,** change to `FAST_SIM = 0`

---

## 🐛 Troubleshooting

### Issue: Counter doesn't increment

**Check:**
1. Is `rst` unchecked? (Should be 0 for normal operation)
2. Did you toggle `btn_press` on AND off? (Needs edge detection)
3. Is clock running? (Auto in DigitalJS)

**Fix:** Uncheck `rst`, toggle `btn_press` checkbox

---

### Issue: Can't see UART ASCII data

**Reason:** DigitalJS shows raw bits, not decoded ASCII

**Solutions:**
1. Use waveform viewer to see serial bits
2. Manually decode (0x42='B', 0x54='T', 0x4E='N', etc.)
3. Use the test wrapper with TX monitor (advanced)

---

### Issue: LED never turns on

**Reason:** Sending ACK command manually is very difficult

**Solutions:**
1. Use loopback test (connect uart_tx to uart_rx)
2. Modify design to trigger LED on button press
3. Use the test wrapper with ACK generator

**Temporary LED test:**
```systemverilog
// In super_counter module, add:
assign led_counter = btn_pulse;  // LED blinks on button press
```

---

### Issue: Simulation is slow

**Check:**
- Is `FAST_SIM = 1`? (Should be for DigitalJS)
- Are you running too many clock cycles?

**Speed it up:**
- Reduce `DEBOUNCE_CYCLES` to 1
- Use faster clock speed in DigitalJS settings
- Step through manually instead of continuous run

---

## 📈 Expected Performance

With `FAST_SIM = 1`:

| Operation | Clock Cycles | Real Time @ 12MHz |
|-----------|-------------|-------------------|
| Button press | 1 | 83 ns |
| Debounce delay | 2-4 | 166-333 ns |
| TX one byte | ~40 | 3.3 μs |
| TX full message | ~280 | 23 μs |
| LED pulse (100ms) | ~300* | 25 μs* |

*In FAST_SIM mode, timings are scaled down

---

## 🎓 Learning Exercises

### Exercise 1: Count to 10
1. Press button 10 times
2. Verify `seq_value` reaches 0x000A
3. Watch UART send "BTN 10\n" (2-digit number!)

### Exercise 2: Counter Overflow
1. Press button 65,536 times (just kidding!)
2. Or modify initial value: `seq_value_reg <= 16'hFFFF;`
3. Press once, verify it wraps to 0x0000

### Exercise 3: UART Timing
1. Add `uart_tx` to waveform
2. Press button
3. Count clock cycles for one byte transmission
4. Should be ~40 clocks in FAST_SIM mode

### Exercise 4: LED Blink
1. Add this temporary code:
   ```systemverilog
   assign led_counter = btn_debounced;
   ```
2. Press and hold button
3. LED should stay on while pressed

---

## 🚀 Next Steps

After testing in DigitalJS:

1. ✅ **Verify logic** - Button counting works
2. ✅ **Check UART timing** - Transmission completes
3. ✅ **Test on real hardware** - Change `FAST_SIM=0`
4. ✅ **Add features** - Modify and experiment!

---

## 📚 Additional Resources

- **DigitalJS:** https://digitaljs.tilk.eu/
- **Yosys Docs:** https://yosyshq.net/yosys/
- **SystemVerilog Tutorial:** https://www.chipverify.com/systemverilog/systemverilog-tutorial

**Files in this directory:**
- `packed_super_counter.sv` - Main design (use this!)
- `super_counter_digitaljs_test.sv` - Advanced test wrapper
- `DIGITALJS_TESTING.md` - Detailed testing guide
- `BAUD_PATTERN.md` - UART timing explanation

---

## 💬 Quick Reference

**To increment counter:**
```
1. Check btn_press
2. Uncheck btn_press
3. Repeat
```

**To reset:**
```
1. Check rst
2. Uncheck rst
```

**To see UART bits:**
```
1. Right-click uart_tx
2. Add to waveform
3. Zoom in on timing
```

**To test LED (hacky way):**
```systemverilog
// Replace in super_counter:
assign led_counter = btn_pulse;  // Blink on press
```

---

**Happy Testing!** 🎉

If you get stuck, start with the simplest test: just increment the counter a few times and verify `seq_value` increases.
