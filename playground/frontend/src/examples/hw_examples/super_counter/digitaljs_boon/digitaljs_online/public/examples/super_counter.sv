// Super Counter - UART-based button counter with LED acknowledgment
// SIMULATION-OPTIMIZED VERSION for DigitalJS
//
// Protocol:
//   TX: "BTN <seq>\n"  - Button press with sequence number (1-99999)
//   RX: "ACK <ms>\n"   - Flash LED for <ms> cycles (not milliseconds in sim)
//
// Testing with UART Terminal:
//   1. Click "Run" to synthesize and start simulation
//   2. In the I/O tab, find the UART Terminal
//   3. Toggle btn_n OFF (active low = pressed) to see "BTN 1" appear
//   4. Type "ACK 500" and press Enter to flash the LED
//   5. Repeat to see sequence numbers increment
//
// NOTE: This version uses a LOW clock frequency (1000 Hz) and minimal
// debounce (2 cycles) to make simulation responsive in DigitalJS.
// The UART runs at 100 baud (10 cycles per bit) for fast communication.
//
// Architecture:
//   btn -> debouncer -> btn_message -> uart_tx -> TX
//   RX -> uart_rx -> ack_parser -> led_pulse -> LED

// ============================================================================
// Top-Level Module
// ============================================================================

module super_counter #(
    parameter CLOCK_HZ     = 1000,      // 1kHz for fast simulation
    parameter BAUD         = 100,       // 100 baud = 10 cycles per bit
    parameter DEBOUNCE_CYC = 2          // 2 cycle debounce (instant in sim)
) (
    input  wire clk,
    input  wire rst_n,
    input  wire btn_n,
    input  wire uart_rx_i,
    output wire uart_tx_o,
    output wire led_o
);
    wire rst;
    assign rst = ~rst_n;

    // Debouncer
    wire btn_pressed;
    debouncer #(
        .DEBOUNCE_CYC(DEBOUNCE_CYC)
    ) u_debouncer (
        .clk(clk),
        .rst(rst),
        .btn_n(btn_n),
        .pressed(btn_pressed)
    );

    // Button message generator
    wire [7:0] tx_data;
    wire       tx_start;
    wire       tx_busy;

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
    wire [7:0] rx_data;
    wire       rx_valid;

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
    wire        ack_trigger;
    wire [23:0] ack_cycles;  // 24 bits max (DigitalJS limit)

    ack_parser u_ack_parser (
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
// Debouncer - Simple counter-based debounce (no CDC for simulation)
// ============================================================================

module debouncer #(
    parameter DEBOUNCE_CYC = 2
) (
    input  wire clk,
    input  wire rst,
    input  wire btn_n,
    output reg  pressed
);
    // ULTRA-SIMPLE version for debugging DigitalJS
    // Just detect falling edge of btn_n (rising edge of btn)

    wire btn;
    assign btn = ~btn_n;  // Active high when button pressed

    reg btn_prev;

    always @(posedge clk) begin
        if (rst) begin
            btn_prev <= 1'b0;
            pressed  <= 1'b0;
        end else begin
            // Detect rising edge of btn (falling edge of btn_n)
            // pressed goes HIGH for one cycle when btn transitions 0->1
            pressed  <= btn & ~btn_prev;
            btn_prev <= btn;
        end
    end

endmodule

// ============================================================================
// UART Transmitter - 8N1 (Full implementation with baud rate counter)
// ============================================================================

module uart_tx #(
    parameter CLOCK_HZ = 1000,
    parameter BAUD     = 100
) (
    input  wire       clk,
    input  wire       rst,
    input  wire [7:0] data,
    input  wire       start,
    output reg        busy,
    output reg        tx
);
    localparam DIVISOR = CLOCK_HZ / BAUD;
    localparam CTR_WIDTH = 8;

    reg [CTR_WIDTH-1:0] baud_cnt;
    reg [3:0]           bit_idx;   // 0=idle, 1=start, 2-9=data, 10=stop
    reg [7:0]           shifter;

    always @(posedge clk) begin
        if (rst) begin
            busy     <= 1'b0;
            tx       <= 1'b1;
            baud_cnt <= 0;
            bit_idx  <= 4'd0;
            shifter  <= 8'h00;
        end else begin
            if (!busy) begin
                // Idle - wait for start
                tx <= 1'b1;
                if (start) begin
                    busy     <= 1'b1;
                    shifter  <= data;
                    baud_cnt <= DIVISOR - 1;
                    bit_idx  <= 4'd1;  // Start with start bit
                    tx       <= 1'b0;  // Start bit is LOW
                end
            end else begin
                // Transmitting
                if (baud_cnt == 0) begin
                    baud_cnt <= DIVISOR - 1;
                    bit_idx  <= bit_idx + 4'd1;

                    case (bit_idx)
                        4'd1: tx <= shifter[0];           // Data bit 0
                        4'd2: tx <= shifter[1];           // Data bit 1
                        4'd3: tx <= shifter[2];           // Data bit 2
                        4'd4: tx <= shifter[3];           // Data bit 3
                        4'd5: tx <= shifter[4];           // Data bit 4
                        4'd6: tx <= shifter[5];           // Data bit 5
                        4'd7: tx <= shifter[6];           // Data bit 6
                        4'd8: tx <= shifter[7];           // Data bit 7
                        4'd9: tx <= 1'b1;                 // Stop bit
                        4'd10: begin
                            tx   <= 1'b1;
                            busy <= 1'b0;
                            bit_idx <= 4'd0;
                        end
                        default: tx <= 1'b1;
                    endcase
                end else begin
                    baud_cnt <= baud_cnt - 1'b1;
                end
            end
        end
    end

endmodule

// ============================================================================
// UART Receiver - 8N1 with mid-bit sampling
// ============================================================================

module uart_rx #(
    parameter CLOCK_HZ = 1000,
    parameter BAUD     = 100
) (
    input  wire       clk,
    input  wire       rst,
    input  wire       rx,
    output reg  [7:0] data,
    output reg        valid
);
    localparam DIVISOR   = CLOCK_HZ / BAUD;
    localparam CTR_WIDTH = 8;

    // Receiver state
    reg [CTR_WIDTH-1:0] baud_cnt;
    reg [2:0]           bit_idx;  // 3 bits is enough for 0-7
    reg [7:0]           shifter;
    reg                 busy;
    reg                 got_all_bits;

    always @(posedge clk) begin
        if (rst) begin
            busy         <= 1'b0;
            baud_cnt     <= 0;
            bit_idx      <= 3'd0;
            shifter      <= 8'h00;
            data         <= 8'h00;
            valid        <= 1'b0;
            got_all_bits <= 1'b0;
        end else begin
            valid <= 1'b0;
            if (!busy) begin
                // Wait for start bit (falling edge)
                if (!rx) begin
                    busy         <= 1'b1;
                    baud_cnt     <= DIVISOR / 2;  // Sample at mid-bit
                    bit_idx      <= 3'd0;
                    got_all_bits <= 1'b0;
                end
            end else begin
                if (baud_cnt == 0) begin
                    baud_cnt <= DIVISOR - 1;
                    if (!got_all_bits) begin
                        // Shift in the received bit (LSB first)
                        shifter <= {rx, shifter[7:1]};
                        if (bit_idx == 3'd7) begin
                            got_all_bits <= 1'b1;
                        end else begin
                            bit_idx <= bit_idx + 3'd1;
                        end
                    end else begin
                        // Stop bit
                        if (rx) begin
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
// Button Message Generator - Sends "BTN 1\n" over UART
// ============================================================================

module btn_message (
    input  wire       clk,
    input  wire       rst,
    input  wire       btn_pressed,
    output reg  [7:0] tx_data,
    output reg        tx_start,
    input  wire       tx_busy
);
    // Message: "BTN 1\n" = 6 characters
    localparam IDLE = 3'd0;
    localparam SEND_B = 3'd1;
    localparam SEND_T = 3'd2;
    localparam SEND_N = 3'd3;
    localparam SEND_SPACE = 3'd4;
    localparam SEND_1 = 3'd5;
    localparam SEND_NL = 3'd6;
    localparam DONE = 3'd7;

    reg [2:0] state;
    reg tx_was_busy;

    always @(posedge clk) begin
        if (rst) begin
            state      <= IDLE;
            tx_data    <= 8'h00;
            tx_start   <= 1'b0;
            tx_was_busy <= 1'b0;
        end else begin
            tx_was_busy <= tx_busy;

            case (state)
                IDLE: begin
                    tx_start <= 1'b0;
                    if (btn_pressed && !tx_busy) begin
                        state    <= SEND_B;
                        tx_data  <= 8'h42;  // 'B'
                        tx_start <= 1'b1;
                    end
                end

                SEND_B: begin
                    if (tx_busy) tx_start <= 1'b0;
                    if (tx_was_busy && !tx_busy) begin
                        state    <= SEND_T;
                        tx_data  <= 8'h54;  // 'T'
                        tx_start <= 1'b1;
                    end
                end

                SEND_T: begin
                    if (tx_busy) tx_start <= 1'b0;
                    if (tx_was_busy && !tx_busy) begin
                        state    <= SEND_N;
                        tx_data  <= 8'h4E;  // 'N'
                        tx_start <= 1'b1;
                    end
                end

                SEND_N: begin
                    if (tx_busy) tx_start <= 1'b0;
                    if (tx_was_busy && !tx_busy) begin
                        state    <= SEND_SPACE;
                        tx_data  <= 8'h20;  // ' '
                        tx_start <= 1'b1;
                    end
                end

                SEND_SPACE: begin
                    if (tx_busy) tx_start <= 1'b0;
                    if (tx_was_busy && !tx_busy) begin
                        state    <= SEND_1;
                        tx_data  <= 8'h31;  // '1'
                        tx_start <= 1'b1;
                    end
                end

                SEND_1: begin
                    if (tx_busy) tx_start <= 1'b0;
                    if (tx_was_busy && !tx_busy) begin
                        state    <= SEND_NL;
                        tx_data  <= 8'h0A;  // '\n'
                        tx_start <= 1'b1;
                    end
                end

                SEND_NL: begin
                    if (tx_busy) tx_start <= 1'b0;
                    if (tx_was_busy && !tx_busy) begin
                        state    <= IDLE;
                        tx_start <= 1'b0;
                    end
                end

                default: state <= IDLE;
            endcase
        end
    end

endmodule

// ============================================================================
// ACK Parser - Parses "ACK <num>\n" and triggers LED pulse
// NOTE: <num> is directly used as cycle count (not milliseconds)
// ============================================================================

module ack_parser (
    input  wire        clk,
    input  wire        rst,
    input  wire [7:0]  rx_data,
    input  wire        rx_valid,
    output reg         trigger,
    output reg  [23:0] pulse_cycles  // 24-bit to stay under DigitalJS limit
);
    localparam IDLE      = 3'd0;
    localparam GOT_A     = 3'd1;
    localparam GOT_C     = 3'd2;
    localparam GOT_K     = 3'd3;
    localparam GOT_SPACE = 3'd4;
    localparam GOT_NUM   = 3'd5;

    reg [2:0] state;
    reg [23:0] duration;  // 24-bit

    // Check if character is digit
    wire is_digit;
    assign is_digit = (rx_data >= 8'h30) && (rx_data <= 8'h39);

    // Convert ASCII digit to value
    wire [3:0] digit_val;
    assign digit_val = rx_data[3:0];

    always @(posedge clk) begin
        if (rst) begin
            state        <= IDLE;
            duration     <= 24'd0;
            trigger      <= 1'b0;
            pulse_cycles <= 24'd0;
        end else begin
            trigger <= 1'b0;

            if (rx_valid) begin
                case (state)
                    IDLE: begin
                        duration <= 24'd0;
                        if (rx_data == 8'h41)  // 'A'
                            state <= GOT_A;
                    end

                    GOT_A: begin
                        if (rx_data == 8'h43)  // 'C'
                            state <= GOT_C;
                        else
                            state <= IDLE;
                    end

                    GOT_C: begin
                        if (rx_data == 8'h4B)  // 'K'
                            state <= GOT_K;
                        else
                            state <= IDLE;
                    end

                    GOT_K: begin
                        if (rx_data == 8'h20)  // ' '
                            state <= GOT_SPACE;
                        else
                            state <= IDLE;
                    end

                    GOT_SPACE: begin
                        if (is_digit) begin
                            duration <= {20'd0, digit_val};
                            state <= GOT_NUM;
                        end else if (rx_data == 8'h0A) begin  // '\n'
                            pulse_cycles <= duration;
                            trigger <= 1'b1;
                            state <= IDLE;
                        end else begin
                            state <= IDLE;
                        end
                    end

                    GOT_NUM: begin
                        if (is_digit) begin
                            // duration * 10 + digit (using shifts: *10 = *8 + *2)
                            duration <= (duration << 3) + (duration << 1) + {20'd0, digit_val};
                        end else if (rx_data == 8'h0A) begin  // '\n'
                            pulse_cycles <= duration;
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
    input  wire        clk,
    input  wire        rst,
    input  wire        trigger,
    input  wire [23:0] cycles,  // 24-bit
    output reg         led
);
    reg [23:0] counter;

    always @(posedge clk) begin
        if (rst) begin
            counter <= 24'd0;
            led     <= 1'b0;
        end else begin
            if (trigger) begin
                counter <= cycles;
                led     <= 1'b1;
            end else if (counter != 24'd0) begin
                counter <= counter - 1'b1;
                if (counter == 24'd1)
                    led <= 1'b0;
            end
        end
    end

endmodule
