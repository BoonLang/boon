# Super Counter

A UART-based button counter with LED acknowledgment.

**Source:** [super_counter_rust](https://github.com/MartinKavik/super_counter_rust)

## Protocol

```
TX: "BTN <seq>\n"   - Button press with sequence number (1-99999)
RX: "ACK <ms>\n"    - Flash LED for <ms> milliseconds
```

## Architecture

```
btn → debouncer → btn_message → uart_tx → TX
RX → uart_rx → ack_parser → led_pulse → LED
```

## Files

| File | Description |
|------|-------------|
| `super_counter.sv` | Clean SystemVerilog (Yosys-compatible) |
| `super_counter.bn` | Boon equivalent |

## Modules

| Module | Function |
|--------|----------|
| `debouncer` | CDC synchronizer + counter-based debounce |
| `uart_tx` | UART transmitter (8N1, 115200 baud) |
| `uart_rx` | UART receiver with mid-bit sampling |
| `btn_message` | BCD counter + "BTN N\n" message formatter |
| `ack_parser` | FSM parser for "ACK ms\n" commands |
| `led_pulse` | Down-counter LED pulse generator |

## Testing

```bash
# Synthesize with Yosys
yosys -p "read_verilog -sv super_counter.sv; synth_ice40"

# Test in DigitalJS
# Copy super_counter.sv to https://digitaljs.tilk.eu/
```

## Configuration

Default: 12 MHz clock, 115200 baud, 20ms debounce
