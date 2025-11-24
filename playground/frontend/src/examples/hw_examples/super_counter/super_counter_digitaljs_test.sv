// ============================================================================
// DigitalJS Test Wrapper for Super Counter
// ============================================================================
// This wrapper makes testing easier in DigitalJS by adding:
//   * UART TX monitor: Decodes and displays ASCII bytes sent
//   * ACK command generator: Simulates receiving "ACK 100\n" command
//   * Visual LED indicators
//
// How to use in DigitalJS:
//   1. Copy this entire file to https://digitaljs.tilk.eu/
//   2. Click "Simulate"
//   3. Use inputs:
//      - btn_press: Toggle to increment counter (watch tx_ascii display bytes)
//      - send_ack: Toggle to send "ACK 100\n" command (watch led_counter turn on)
//      - rst: Toggle to reset everything
// ============================================================================

`include "packed_super_counter.sv"

// ----------------------------------------------------------------------------
// UART TX Monitor - Captures and displays transmitted bytes
// ----------------------------------------------------------------------------
module uart_tx_monitor (
    input  logic       clk,
    input  logic       rst,
    input  logic       serial_in,    // Connect to uart_tx
    output logic [7:0] last_byte,    // Last received ASCII byte
    output logic       byte_valid    // Pulses when new byte received
);
    parameter bit FAST_SIM = 1;

    // Use same UART RX to decode the TX output
    uart_rx #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .FAST_SIM(FAST_SIM)
    ) monitor (
        .clk(clk),
        .rst(rst),
        .serial_in(serial_in),
        .data(last_byte),
        .valid(byte_valid)
    );
endmodule

// ----------------------------------------------------------------------------
// ACK Command Generator - Sends "ACK 100\n" when triggered
// ----------------------------------------------------------------------------
module ack_generator (
    input  logic       clk,
    input  logic       rst,
    input  logic       trigger,      // Pulse to start sending
    output logic       serial_out,   // Connect to uart_rx input
    output logic       busy          // High while sending
);
    parameter bit FAST_SIM = 1;

    // Message: "ACK 100\n" = 8 bytes
    logic [7:0] message [0:7];
    initial begin
        message[0] = 8'h41;  // 'A'
        message[1] = 8'h43;  // 'C'
        message[2] = 8'h4B;  // 'K'
        message[3] = 8'h20;  // ' '
        message[4] = 8'h31;  // '1'
        message[5] = 8'h30;  // '0'
        message[6] = 8'h30;  // '0'
        message[7] = 8'h0A;  // '\n'
    end

    typedef enum logic [1:0] {
        IDLE,
        SEND,
        WAIT
    } state_t;

    state_t state;
    logic [2:0] byte_idx;
    logic [7:0] tx_data;
    logic       tx_start;
    logic       tx_busy;

    // UART transmitter
    uart_tx #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .FAST_SIM(FAST_SIM)
    ) tx (
        .clk(clk),
        .rst(rst),
        .data(tx_data),
        .start(tx_start),
        .busy(tx_busy),
        .serial_out(serial_out)
    );

    always_ff @(posedge clk) begin
        if (rst) begin
            state    <= IDLE;
            byte_idx <= 3'd0;
            tx_start <= 1'b0;
            busy     <= 1'b0;
        end else begin
            tx_start <= 1'b0;

            case (state)
                IDLE: begin
                    busy <= 1'b0;
                    if (trigger) begin
                        byte_idx <= 3'd0;
                        state    <= SEND;
                        busy     <= 1'b1;
                    end
                end

                SEND: begin
                    if (!tx_busy) begin
                        tx_data  <= message[byte_idx];
                        tx_start <= 1'b1;
                        state    <= WAIT;
                    end
                end

                WAIT: begin
                    if (!tx_busy && !tx_start) begin
                        if (byte_idx == 3'd7) begin
                            state <= IDLE;
                        end else begin
                            byte_idx <= byte_idx + 3'd1;
                            state    <= SEND;
                        end
                    end
                end
            endcase
        end
    end
endmodule

// ----------------------------------------------------------------------------
// Test Wrapper Top Module
// ----------------------------------------------------------------------------
module super_counter_test (
    input  logic clk,
    input  logic rst,
    input  logic btn_press,
    input  logic send_ack,       // NEW: Trigger ACK command

    // Status outputs for DigitalJS display
    output logic led_counter,
    output logic [15:0] seq_value,
    output logic btn_pulse,

    // UART monitoring outputs
    output logic [7:0] tx_ascii,      // Last transmitted ASCII byte
    output logic       tx_byte_valid, // Pulses when byte sent
    output logic       ack_busy       // Sending ACK command
);
    // Internal UART signals
    logic uart_tx_wire;
    logic uart_rx_wire;

    // Super counter instance
    super_counter #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .DEBOUNCE_CYCLES(2),
        .FAST_SIM(1)
    ) dut (
        .clk_12m(clk),
        .rst(rst),
        .btn_press(btn_press),
        .uart_rx(uart_rx_wire),
        .uart_tx(uart_tx_wire),
        .led_counter(led_counter),
        .btn_debounced(),
        .btn_pulse(btn_pulse),
        .seq_value(seq_value)
    );

    // TX Monitor - Shows what's being transmitted
    uart_tx_monitor #(
        .FAST_SIM(1)
    ) tx_mon (
        .clk(clk),
        .rst(rst),
        .serial_in(uart_tx_wire),
        .last_byte(tx_ascii),
        .byte_valid(tx_byte_valid)
    );

    // ACK Generator - Sends "ACK 100\n" when send_ack is pulsed
    logic send_ack_prev;
    logic send_ack_pulse;

    always_ff @(posedge clk) begin
        if (rst) begin
            send_ack_prev <= 1'b0;
        end else begin
            send_ack_prev <= send_ack;
        end
    end

    assign send_ack_pulse = send_ack && !send_ack_prev;

    ack_generator #(
        .FAST_SIM(1)
    ) ack_gen (
        .clk(clk),
        .rst(rst),
        .trigger(send_ack_pulse),
        .serial_out(uart_rx_wire),
        .busy(ack_busy)
    );
endmodule
