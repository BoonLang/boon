// Super Counter - UART-based button counter with LED acknowledgment
//
// Protocol:
//   TX: "BTN <seq>\n"  - Button press with sequence number (1-99999)
//   RX: "ACK <ms>\n"   - Flash LED for <ms> milliseconds
//
// Architecture:
//   btn → debouncer → btn_message → uart_tx → TX
//   RX → uart_rx → ack_parser → led_pulse → LED

// ============================================================================
// Top-Level Module
// ============================================================================

module super_counter #(
    parameter int CLOCK_HZ     = 12_000_000,
    parameter int BAUD         = 115_200,
    parameter int DEBOUNCE_MS  = 20
) (
    input  logic clk,
    input  logic rst_n,
    input  logic btn_n,
    input  logic uart_rx_i,
    output logic uart_tx_o,
    output logic led_o
);
    logic rst;
    assign rst = ~rst_n;

    // Debouncer
    logic btn_pressed;
    debouncer #(
        .CLOCK_HZ(CLOCK_HZ),
        .DEBOUNCE_MS(DEBOUNCE_MS)
    ) u_debouncer (
        .clk(clk),
        .rst(rst),
        .btn_n(btn_n),
        .pressed(btn_pressed)
    );

    // Button message generator
    logic [7:0] tx_data;
    logic       tx_start;
    logic       tx_busy;

    btn_message u_btn_message (
        .clk(clk),
        .rst(rst),
        .btn_pressed(btn_pressed),
        .tx_data(tx_data),
        .tx_start(tx_start),
        .tx_busy(tx_busy)
    );

    // UART transmitter
    uart_tx #(
        .CLOCK_HZ(CLOCK_HZ),
        .BAUD(BAUD)
    ) u_uart_tx (
        .clk(clk),
        .rst(rst),
        .data(tx_data),
        .start(tx_start),
        .busy(tx_busy),
        .tx(uart_tx_o)
    );

    // UART receiver
    logic [7:0] rx_data;
    logic       rx_valid;

    uart_rx #(
        .CLOCK_HZ(CLOCK_HZ),
        .BAUD(BAUD)
    ) u_uart_rx (
        .clk(clk),
        .rst(rst),
        .rx(uart_rx_i),
        .data(rx_data),
        .valid(rx_valid)
    );

    // ACK parser
    logic        ack_trigger;
    logic [31:0] ack_cycles;

    ack_parser #(
        .CLOCK_HZ(CLOCK_HZ)
    ) u_ack_parser (
        .clk(clk),
        .rst(rst),
        .rx_data(rx_data),
        .rx_valid(rx_valid),
        .trigger(ack_trigger),
        .pulse_cycles(ack_cycles)
    );

    // LED pulse generator
    led_pulse u_led_pulse (
        .clk(clk),
        .rst(rst),
        .trigger(ack_trigger),
        .cycles(ack_cycles),
        .led(led_o)
    );

endmodule

// ============================================================================
// Debouncer - CDC synchronizer + counter-based debounce filter
// ============================================================================

module debouncer #(
    parameter int CLOCK_HZ    = 12_000_000,
    parameter int DEBOUNCE_MS = 20
) (
    input  logic clk,
    input  logic rst,
    input  logic btn_n,
    output logic pressed
);
    localparam int DEBOUNCE_CYCLES = CLOCK_HZ / 1000 * DEBOUNCE_MS;
    localparam int CTR_WIDTH = $clog2(DEBOUNCE_CYCLES + 1);

    // CDC synchronizer (2-FF)
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

    logic btn;
    assign btn = ~sync_1;  // Active high

    // Debounce counter
    logic [CTR_WIDTH-1:0] counter;
    logic stable;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= '0;
            stable  <= 1'b0;
            pressed <= 1'b0;
        end else begin
            pressed <= 1'b0;
            if (btn != stable) begin
                if (counter == CTR_WIDTH'(DEBOUNCE_CYCLES - 1)) begin
                    stable  <= btn;
                    counter <= '0;
                    if (btn)
                        pressed <= 1'b1;
                end else begin
                    counter <= counter + 1'b1;
                end
            end else begin
                counter <= '0;
            end
        end
    end

endmodule

// ============================================================================
// UART Transmitter - 8N1
// ============================================================================

module uart_tx #(
    parameter int CLOCK_HZ = 12_000_000,
    parameter int BAUD     = 115_200
) (
    input  logic       clk,
    input  logic       rst,
    input  logic [7:0] data,
    input  logic       start,
    output logic       busy,
    output logic       tx
);
    localparam int DIVISOR   = CLOCK_HZ / BAUD;
    localparam int CTR_WIDTH = $clog2(DIVISOR);

    logic [CTR_WIDTH-1:0] baud_cnt;
    logic [3:0]           bit_idx;
    logic [9:0]           shifter;

    logic baud_tick;
    assign baud_tick = (baud_cnt == '0);

    // Baud rate generator
    always_ff @(posedge clk) begin
        if (rst) begin
            baud_cnt <= CTR_WIDTH'(DIVISOR - 1);
        end else if (busy) begin
            if (baud_cnt == '0)
                baud_cnt <= CTR_WIDTH'(DIVISOR - 1);
            else
                baud_cnt <= baud_cnt - 1'b1;
        end else begin
            baud_cnt <= CTR_WIDTH'(DIVISOR - 1);
        end
    end

    // Transmit logic
    always_ff @(posedge clk) begin
        if (rst) begin
            busy    <= 1'b0;
            tx      <= 1'b1;
            shifter <= 10'h3FF;
            bit_idx <= 4'd0;
        end else begin
            if (!busy) begin
                tx <= 1'b1;
                if (start) begin
                    busy    <= 1'b1;
                    shifter <= {1'b1, data, 1'b0};  // stop + data + start
                    bit_idx <= 4'd0;
                end
            end else if (baud_tick) begin
                tx      <= shifter[0];
                shifter <= {1'b1, shifter[9:1]};
                bit_idx <= bit_idx + 1'b1;
                if (bit_idx == 4'd9)
                    busy <= 1'b0;
            end
        end
    end

endmodule

// ============================================================================
// UART Receiver - 8N1 with CDC and mid-bit sampling
// ============================================================================

module uart_rx #(
    parameter int CLOCK_HZ = 12_000_000,
    parameter int BAUD     = 115_200
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       rx,
    output logic [7:0] data,
    output logic       valid
);
    localparam int DIVISOR   = CLOCK_HZ / BAUD;
    localparam int CTR_WIDTH = $clog2(DIVISOR);

    // CDC synchronizer
    logic sync_0, sync_1;
    always_ff @(posedge clk) begin
        if (rst) begin
            sync_0 <= 1'b1;
            sync_1 <= 1'b1;
        end else begin
            sync_0 <= rx;
            sync_1 <= sync_0;
        end
    end

    logic rx_sync;
    assign rx_sync = sync_1;

    // Receiver state
    logic [CTR_WIDTH-1:0] baud_cnt;
    logic [3:0]           bit_idx;
    logic [7:0]           shifter;
    logic                 busy;

    always_ff @(posedge clk) begin
        if (rst) begin
            busy     <= 1'b0;
            baud_cnt <= '0;
            bit_idx  <= 4'd0;
            shifter  <= 8'h00;
            data     <= 8'h00;
            valid    <= 1'b0;
        end else begin
            valid <= 1'b0;
            if (!busy) begin
                // Wait for start bit (falling edge)
                if (!rx_sync) begin
                    busy     <= 1'b1;
                    baud_cnt <= CTR_WIDTH'(DIVISOR / 2);  // Sample at mid-bit
                    bit_idx  <= 4'd0;
                end
            end else begin
                if (baud_cnt == '0) begin
                    baud_cnt <= CTR_WIDTH'(DIVISOR - 1);
                    if (bit_idx < 4'd8) begin
                        shifter[bit_idx[2:0]] <= rx_sync;
                        bit_idx <= bit_idx + 1'b1;
                    end else begin
                        // Stop bit
                        if (rx_sync) begin
                            data  <= shifter;
                            valid <= 1'b1;
                        end
                        busy <= 1'b0;
                    end
                end else begin
                    baud_cnt <= baud_cnt - 1'b1;
                end
            end
        end
    end

endmodule

// ============================================================================
// Button Message Generator - Sends "BTN <seq>\n" over UART
// ============================================================================

module btn_message (
    input  logic       clk,
    input  logic       rst,
    input  logic       btn_pressed,
    output logic [7:0] tx_data,
    output logic       tx_start,
    input  logic       tx_busy
);
    typedef enum logic [1:0] {
        IDLE,
        SEND,
        WAIT
    } state_t;

    state_t state;

    // Sequence counter (1-99999)
    logic [16:0] seq_value;

    // BCD digits (5 digits, little-endian: [0]=ones)
    logic [3:0] bcd [0:4];

    // Message buffer (max 10 bytes: "BTN 99999\n")
    logic [7:0] msg [0:9];
    logic [3:0] msg_len;
    logic [3:0] msg_idx;

    // ASCII conversion
    function automatic logic [7:0] digit_to_ascii(input logic [3:0] d);
        return 8'h30 + {4'b0, d};
    endfunction

    // Count significant digits
    function automatic logic [2:0] count_digits(
        input logic [3:0] d4, d3, d2, d1, d0
    );
        if (d4 != 0) return 3'd5;
        if (d3 != 0) return 3'd4;
        if (d2 != 0) return 3'd3;
        if (d1 != 0) return 3'd2;
        return 3'd1;
    endfunction

    always_ff @(posedge clk) begin
        if (rst) begin
            state     <= IDLE;
            seq_value <= 17'd0;
            tx_start  <= 1'b0;
            msg_idx   <= 4'd0;
            msg_len   <= 4'd6;
            for (int i = 0; i < 5; i++)
                bcd[i] <= 4'd0;
        end else begin
            tx_start <= 1'b0;

            case (state)
                IDLE: begin
                    if (btn_pressed) begin
                        // Increment sequence
                        seq_value <= seq_value + 1'b1;

                        // Increment BCD with ripple carry
                        automatic logic carry = 1'b1;
                        for (int i = 0; i < 5; i++) begin
                            if (carry) begin
                                if (bcd[i] == 4'd9) begin
                                    bcd[i] <= 4'd0;
                                end else begin
                                    bcd[i] <= bcd[i] + 1'b1;
                                    carry = 1'b0;
                                end
                            end
                        end

                        // Build message: "BTN "
                        msg[0] <= "B";
                        msg[1] <= "T";
                        msg[2] <= "N";
                        msg[3] <= " ";

                        // Add digits (big-endian) and newline
                        automatic logic [2:0] n_digits = count_digits(
                            bcd[4], bcd[3], bcd[2], bcd[1], bcd[0]
                        );
                        case (n_digits)
                            3'd5: begin
                                msg[4] <= digit_to_ascii(bcd[4]);
                                msg[5] <= digit_to_ascii(bcd[3]);
                                msg[6] <= digit_to_ascii(bcd[2]);
                                msg[7] <= digit_to_ascii(bcd[1]);
                                msg[8] <= digit_to_ascii(bcd[0]);
                                msg[9] <= 8'h0A;
                                msg_len <= 4'd10;
                            end
                            3'd4: begin
                                msg[4] <= digit_to_ascii(bcd[3]);
                                msg[5] <= digit_to_ascii(bcd[2]);
                                msg[6] <= digit_to_ascii(bcd[1]);
                                msg[7] <= digit_to_ascii(bcd[0]);
                                msg[8] <= 8'h0A;
                                msg_len <= 4'd9;
                            end
                            3'd3: begin
                                msg[4] <= digit_to_ascii(bcd[2]);
                                msg[5] <= digit_to_ascii(bcd[1]);
                                msg[6] <= digit_to_ascii(bcd[0]);
                                msg[7] <= 8'h0A;
                                msg_len <= 4'd8;
                            end
                            3'd2: begin
                                msg[4] <= digit_to_ascii(bcd[1]);
                                msg[5] <= digit_to_ascii(bcd[0]);
                                msg[6] <= 8'h0A;
                                msg_len <= 4'd7;
                            end
                            default: begin
                                msg[4] <= digit_to_ascii(bcd[0]);
                                msg[5] <= 8'h0A;
                                msg_len <= 4'd6;
                            end
                        endcase

                        msg_idx <= 4'd0;
                        state   <= SEND;
                    end
                end

                SEND: begin
                    if (!tx_busy) begin
                        tx_data  <= msg[msg_idx];
                        tx_start <= 1'b1;
                        state    <= WAIT;
                    end
                end

                WAIT: begin
                    if (tx_busy) begin
                        // Byte accepted, advance
                        if (msg_idx == msg_len - 1)
                            state <= IDLE;
                        else begin
                            msg_idx <= msg_idx + 1'b1;
                            state   <= SEND;
                        end
                    end
                end

                default: state <= IDLE;
            endcase
        end
    end

endmodule

// ============================================================================
// ACK Parser - Parses "ACK <ms>\n" and triggers LED pulse
// ============================================================================

module ack_parser #(
    parameter int CLOCK_HZ = 12_000_000
) (
    input  logic        clk,
    input  logic        rst,
    input  logic [7:0]  rx_data,
    input  logic        rx_valid,
    output logic        trigger,
    output logic [31:0] pulse_cycles
);
    typedef enum logic [2:0] {
        IDLE,
        GOT_A,
        GOT_C,
        GOT_K,
        GOT_SPACE,
        GOT_NUM
    } state_t;

    state_t state;
    logic [31:0] duration_ms;

    // Convert ms to clock cycles
    function automatic logic [31:0] ms_to_cycles(input logic [31:0] ms);
        return ms * (CLOCK_HZ / 1000);
    endfunction

    // Check if character is digit
    function automatic logic is_digit(input logic [7:0] ch);
        return (ch >= "0") && (ch <= "9");
    endfunction

    // Convert ASCII digit to value
    function automatic logic [3:0] to_digit(input logic [7:0] ch);
        return ch[3:0];  // '0'-'9' = 0x30-0x39, lower nibble is value
    endfunction

    always_ff @(posedge clk) begin
        if (rst) begin
            state        <= IDLE;
            duration_ms  <= 32'd0;
            trigger      <= 1'b0;
            pulse_cycles <= 32'd0;
        end else begin
            trigger <= 1'b0;

            if (rx_valid) begin
                case (state)
                    IDLE: begin
                        duration_ms <= 32'd0;
                        if (rx_data == "A")
                            state <= GOT_A;
                    end

                    GOT_A: begin
                        state <= (rx_data == "C") ? GOT_C : IDLE;
                    end

                    GOT_C: begin
                        state <= (rx_data == "K") ? GOT_K : IDLE;
                    end

                    GOT_K: begin
                        state <= (rx_data == " ") ? GOT_SPACE : IDLE;
                    end

                    GOT_SPACE: begin
                        if (is_digit(rx_data)) begin
                            duration_ms <= {28'd0, to_digit(rx_data)};
                            state <= GOT_NUM;
                        end else if (rx_data == 8'h0A) begin
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            trigger <= 1'b1;
                            state <= IDLE;
                        end else begin
                            state <= IDLE;
                        end
                    end

                    GOT_NUM: begin
                        if (is_digit(rx_data)) begin
                            duration_ms <= duration_ms * 10 + {28'd0, to_digit(rx_data)};
                        end else if (rx_data == 8'h0A) begin
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            trigger <= 1'b1;
                            state <= IDLE;
                        end else begin
                            state <= IDLE;
                        end
                    end

                    default: state <= IDLE;
                endcase
            end
        end
    end

endmodule

// ============================================================================
// LED Pulse Generator - Counts down from specified cycles
// ============================================================================

module led_pulse (
    input  logic        clk,
    input  logic        rst,
    input  logic        trigger,
    input  logic [31:0] cycles,
    output logic        led
);
    logic [31:0] counter;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= 32'd0;
            led     <= 1'b0;
        end else begin
            if (trigger) begin
                counter <= cycles;
                led     <= 1'b1;
            end else if (counter != 32'd0) begin
                counter <= counter - 1'b1;
                if (counter == 32'd1)
                    led <= 1'b0;
            end
        end
    end

endmodule
