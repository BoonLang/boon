// ============================================================================
// Super Counter with UART Debug Display for DigitalJS
// ============================================================================
// This version adds visual UART debugging:
//   - TX_MSG[0-15]: Last 16 bytes transmitted (circular buffer)
//   - TX_PTR: Points to most recent byte
//   - RX_MSG[0-15]: Pre-loaded message to send on trigger
//   - RX_SEND: Toggle to send RX message
//
// How to view UART data in DigitalJS:
//   1. Right-click TX_MSG signals → "Display as" → "ASCII" or "Hex"
//   2. TX_PTR shows which byte was written last
//   3. Messages appear character by character as UART transmits
// ============================================================================

`include "packed_super_counter.sv"

// ----------------------------------------------------------------------------
// UART TX Circular Buffer Monitor
// ----------------------------------------------------------------------------
// Captures transmitted bytes and stores in a visible circular buffer
module uart_tx_monitor #(
    parameter FAST_SIM = 1,
    parameter BUFFER_SIZE = 16
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       uart_tx,

    // Debug outputs - visible in DigitalJS
    output logic [7:0] msg_buf [0:BUFFER_SIZE-1],  // Last N transmitted bytes
    output logic [3:0] write_ptr,                  // Points to newest byte
    output logic [7:0] last_byte,                  // Most recent byte
    output logic       byte_valid                  // Pulses when new byte received
);
    // UART RX to decode TX output
    logic [7:0] rx_data;
    logic       rx_valid;

    uart_rx #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .FAST_SIM(FAST_SIM)
    ) decoder (
        .clk(clk),
        .rst(rst),
        .serial_in(uart_tx),
        .data(rx_data),
        .valid(rx_valid)
    );

    assign last_byte = rx_data;
    assign byte_valid = rx_valid;

    // Circular buffer
    integer i;
    always_ff @(posedge clk) begin
        if (rst) begin
            write_ptr <= 4'd0;
            for (i = 0; i < BUFFER_SIZE; i = i + 1) begin
                msg_buf[i] <= 8'h20;  // Initialize with spaces
            end
        end else begin
            if (rx_valid) begin
                msg_buf[write_ptr] <= rx_data;
                write_ptr <= write_ptr + 4'd1;
            end
        end
    end
endmodule

// ----------------------------------------------------------------------------
// UART RX Message Injector
// ----------------------------------------------------------------------------
// Sends pre-programmed message when triggered
module uart_rx_injector #(
    parameter FAST_SIM = 1
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       send_trigger,    // Pulse to start sending
    input  logic [7:0] msg_buf [0:15],  // Message to send (16 bytes max)
    input  logic [3:0] msg_len,         // Number of bytes to send

    output logic       uart_rx,         // Serial output
    output logic       busy,            // Sending in progress
    output logic [3:0] byte_idx         // Current byte being sent
);
    typedef enum logic [1:0] {
        IDLE,
        SEND_BYTE,
        WAIT_DONE
    } state_t;

    state_t state;
    logic [7:0] tx_data;
    logic       tx_start;
    logic       tx_busy;

    uart_tx #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .FAST_SIM(FAST_SIM)
    ) transmitter (
        .clk(clk),
        .rst(rst),
        .data(tx_data),
        .start(tx_start),
        .busy(tx_busy),
        .serial_out(uart_rx)
    );

    logic send_prev;
    logic send_pulse;

    always_ff @(posedge clk) begin
        if (rst) begin
            send_prev <= 1'b0;
        end else begin
            send_prev <= send_trigger;
        end
    end

    assign send_pulse = send_trigger && !send_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            state     <= IDLE;
            byte_idx  <= 4'd0;
            tx_start  <= 1'b0;
            busy      <= 1'b0;
        end else begin
            tx_start <= 1'b0;

            case (state)
                IDLE: begin
                    busy <= 1'b0;
                    if (send_pulse) begin
                        byte_idx <= 4'd0;
                        state    <= SEND_BYTE;
                        busy     <= 1'b1;
                    end
                end

                SEND_BYTE: begin
                    if (!tx_busy) begin
                        tx_data  <= msg_buf[byte_idx];
                        tx_start <= 1'b1;
                        state    <= WAIT_DONE;
                    end
                end

                WAIT_DONE: begin
                    if (!tx_busy && !tx_start) begin
                        if (byte_idx >= msg_len - 4'd1) begin
                            state <= IDLE;
                        end else begin
                            byte_idx <= byte_idx + 4'd1;
                            state    <= SEND_BYTE;
                        end
                    end
                end
            endcase
        end
    end
endmodule

// ----------------------------------------------------------------------------
// TOP LEVEL - Debug Wrapper
// ----------------------------------------------------------------------------
module super_counter_debug (
    input  logic clk,
    input  logic rst,
    input  logic btn_press,
    input  logic rx_send,           // Toggle to send pre-loaded RX message

    // Standard outputs
    output logic       led_counter,
    output logic [15:0] seq_value,
    output logic       btn_pulse,

    // TX Debug Display (visible in DigitalJS)
    output logic [7:0] TX_MSG_0,
    output logic [7:0] TX_MSG_1,
    output logic [7:0] TX_MSG_2,
    output logic [7:0] TX_MSG_3,
    output logic [7:0] TX_MSG_4,
    output logic [7:0] TX_MSG_5,
    output logic [7:0] TX_MSG_6,
    output logic [7:0] TX_MSG_7,
    output logic [7:0] TX_MSG_8,
    output logic [7:0] TX_MSG_9,
    output logic [7:0] TX_MSG_10,
    output logic [7:0] TX_MSG_11,
    output logic [7:0] TX_MSG_12,
    output logic [7:0] TX_MSG_13,
    output logic [7:0] TX_MSG_14,
    output logic [7:0] TX_MSG_15,
    output logic [3:0] TX_PTR,
    output logic [7:0] TX_LAST,
    output logic       TX_VALID,

    // RX Debug Display
    output logic [7:0] RX_MSG_0,
    output logic [7:0] RX_MSG_1,
    output logic [7:0] RX_MSG_2,
    output logic [7:0] RX_MSG_3,
    output logic [7:0] RX_MSG_4,
    output logic [7:0] RX_MSG_5,
    output logic [7:0] RX_MSG_6,
    output logic [7:0] RX_MSG_7,
    output logic [3:0] RX_IDX,
    output logic       RX_BUSY
);
    // Internal UART wires
    logic uart_tx_wire;
    logic uart_rx_wire;

    // Super counter instance
    super_counter #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .DEBOUNCE_CYCLES(2),
        .FAST_SIM(1)
    ) counter (
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

    // TX Monitor
    logic [7:0] tx_msg_buf [0:15];

    uart_tx_monitor #(
        .FAST_SIM(1),
        .BUFFER_SIZE(16)
    ) tx_mon (
        .clk(clk),
        .rst(rst),
        .uart_tx(uart_tx_wire),
        .msg_buf(tx_msg_buf),
        .write_ptr(TX_PTR),
        .last_byte(TX_LAST),
        .byte_valid(TX_VALID)
    );

    // Expose TX buffer as individual outputs for DigitalJS
    assign TX_MSG_0  = tx_msg_buf[0];
    assign TX_MSG_1  = tx_msg_buf[1];
    assign TX_MSG_2  = tx_msg_buf[2];
    assign TX_MSG_3  = tx_msg_buf[3];
    assign TX_MSG_4  = tx_msg_buf[4];
    assign TX_MSG_5  = tx_msg_buf[5];
    assign TX_MSG_6  = tx_msg_buf[6];
    assign TX_MSG_7  = tx_msg_buf[7];
    assign TX_MSG_8  = tx_msg_buf[8];
    assign TX_MSG_9  = tx_msg_buf[9];
    assign TX_MSG_10 = tx_msg_buf[10];
    assign TX_MSG_11 = tx_msg_buf[11];
    assign TX_MSG_12 = tx_msg_buf[12];
    assign TX_MSG_13 = tx_msg_buf[13];
    assign TX_MSG_14 = tx_msg_buf[14];
    assign TX_MSG_15 = tx_msg_buf[15];

    // RX Injector - pre-loaded with "ACK 100\n"
    logic [7:0] rx_msg_buf [0:15];
    initial begin
        rx_msg_buf[0]  = 8'h41;  // 'A'
        rx_msg_buf[1]  = 8'h43;  // 'C'
        rx_msg_buf[2]  = 8'h4B;  // 'K'
        rx_msg_buf[3]  = 8'h20;  // ' '
        rx_msg_buf[4]  = 8'h31;  // '1'
        rx_msg_buf[5]  = 8'h30;  // '0'
        rx_msg_buf[6]  = 8'h30;  // '0'
        rx_msg_buf[7]  = 8'h0A;  // '\n'
        rx_msg_buf[8]  = 8'h20;  // (spaces)
        rx_msg_buf[9]  = 8'h20;
        rx_msg_buf[10] = 8'h20;
        rx_msg_buf[11] = 8'h20;
        rx_msg_buf[12] = 8'h20;
        rx_msg_buf[13] = 8'h20;
        rx_msg_buf[14] = 8'h20;
        rx_msg_buf[15] = 8'h20;
    end

    uart_rx_injector #(
        .FAST_SIM(1)
    ) rx_inj (
        .clk(clk),
        .rst(rst),
        .send_trigger(rx_send),
        .msg_buf(rx_msg_buf),
        .msg_len(4'd8),
        .uart_rx(uart_rx_wire),
        .busy(RX_BUSY),
        .byte_idx(RX_IDX)
    );

    // Expose RX message for display
    assign RX_MSG_0 = rx_msg_buf[0];
    assign RX_MSG_1 = rx_msg_buf[1];
    assign RX_MSG_2 = rx_msg_buf[2];
    assign RX_MSG_3 = rx_msg_buf[3];
    assign RX_MSG_4 = rx_msg_buf[4];
    assign RX_MSG_5 = rx_msg_buf[5];
    assign RX_MSG_6 = rx_msg_buf[6];
    assign RX_MSG_7 = rx_msg_buf[7];
endmodule
