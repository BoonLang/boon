// ============================================================================
// PACKED SUPER COUNTER - Fast Simulation Version
// ============================================================================
//
// Full-featured super counter optimized for DigitalJS simulation:
// - Fast timing parameters for interactive testing
// - UART TX/RX fully implemented (ready for future DigitalJS UART support)
// - Debug outputs to monitor internal state
// - Uses clk_12m for yosys2digitaljs auto-conversion
//
// Architecture:
//   btn_press → debouncer → btn_message → uart_tx → TX serial output
//   uart_rx → ack_parser → led_pulse → LED output
//
// Message format:
//   Button press sends: "BTN <count>\n"
//   UART RX expects: "ACK <duration_ms>\n" (triggers LED pulse)
//
// ============================================================================

// ----------------------------------------------------------------------------
// LED Pulse Generator
// ----------------------------------------------------------------------------
module led_pulse #(
    parameter int CLOCK_HZ = 12_000_000
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

// ----------------------------------------------------------------------------
// Button Debouncer - Simple working pattern from button_counter_fixed
// ----------------------------------------------------------------------------
module debouncer #(
    parameter int DEBOUNCE_CYCLES = 16
) (
    input  logic clk,
    input  logic rst,
    input  logic btn,           // Active-high button input
    output logic pressed,       // Single-cycle pulse on press
    output logic stable_out     // Debounced stable state
);
    localparam CNTR_WIDTH = $clog2(DEBOUNCE_CYCLES);
    logic [CNTR_WIDTH-1:0] counter;
    logic btn_sync;
    logic btn_debounced;
    logic btn_debounced_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            btn_sync <= 1'b0;
            btn_debounced <= 1'b0;
            btn_debounced_prev <= 1'b0;
            counter <= '0;
            pressed <= 1'b0;
        end else begin
            // Step 1: Synchronize button input
            btn_sync <= btn;

            // Step 2: Debounce logic
            if (btn_sync == btn_debounced) begin
                counter <= '0;
            end else begin
                if (counter == DEBOUNCE_CYCLES - 1) begin
                    btn_debounced <= btn_sync;
                    counter <= '0;
                end else begin
                    counter <= counter + 1'b1;
                end
            end

            // Step 3: Edge detection
            btn_debounced_prev <= btn_debounced;
            pressed <= btn_debounced && !btn_debounced_prev;
        end
    end

    assign stable_out = btn_debounced;
endmodule

// ----------------------------------------------------------------------------
// UART Transmitter
// ----------------------------------------------------------------------------
module uart_tx #(
    parameter int CLOCK_HZ = 12_000_000,
    parameter int BAUD = 115_200
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

    // Baud rate generator
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

    // Shift register (start + 8 data + stop)
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

// ----------------------------------------------------------------------------
// UART Receiver
// ----------------------------------------------------------------------------
module uart_rx #(
    parameter int CLOCK_HZ = 12_000_000,
    parameter int BAUD = 115_200
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       serial_in,
    output logic [7:0] data,
    output logic       valid
);
    localparam int DIVISOR = CLOCK_HZ / BAUD;
    localparam int CTR_WIDTH = $clog2(DIVISOR);

    // CDC synchronizer
    logic sync_0, sync_1, serial;

    always_ff @(posedge clk) begin
        if (rst) begin
            sync_0 <= 1'b1;
            sync_1 <= 1'b1;
            serial <= 1'b1;
        end else begin
            sync_0 <= serial_in;
            sync_1 <= sync_0;
            serial <= sync_1;
        end
    end

    // Baud rate generator (half-period offset for mid-bit sampling)
    logic [CTR_WIDTH-1:0] baud_counter;
    logic baud_tick;
    logic busy;

    always_ff @(posedge clk) begin
        if (rst) begin
            baud_counter <= CTR_WIDTH'(DIVISOR - 1);
        end else begin
            if (!busy) begin
                if (!serial) begin
                    baud_counter <= CTR_WIDTH'(DIVISOR / 2);
                end else begin
                    baud_counter <= CTR_WIDTH'(DIVISOR - 1);
                end
            end else begin
                if (baud_counter == 0) begin
                    baud_counter <= CTR_WIDTH'(DIVISOR - 1);
                end else begin
                    baud_counter <= baud_counter - 1'b1;
                end
            end
        end
    end

    assign baud_tick = (baud_counter == 0);

    // Shift register
    logic [7:0] shifter;
    logic [3:0] bit_idx;

    always_ff @(posedge clk) begin
        if (rst) begin
            busy    <= 1'b0;
            shifter <= 8'd0;
            bit_idx <= 4'd0;
            data    <= 8'd0;
            valid   <= 1'b0;
        end else begin
            valid <= 1'b0;

            if (!busy) begin
                if (!serial) begin
                    busy    <= 1'b1;
                    bit_idx <= 4'd0;
                end
            end else if (baud_tick) begin
                if (bit_idx < 4'd8) begin
                    shifter <= {serial, shifter[7:1]};
                    bit_idx <= bit_idx + 1'b1;
                end else begin
                    data  <= shifter;
                    valid <= 1'b1;
                    busy  <= 1'b0;
                end
            end
        end
    end
endmodule

// ----------------------------------------------------------------------------
// Button Message Formatter
// ----------------------------------------------------------------------------
module btn_message (
    input  logic        clk,
    input  logic        rst,
    input  logic        btn_pressed,
    output logic [15:0] seq_value,
    output logic [7:0]  uart_data,
    output logic        uart_start,
    input  logic        uart_busy
);
    typedef enum logic {
        IDLE,
        SEND
    } state_t;

    state_t state;

    logic [15:0] seq_value_reg;
    logic [3:0]  bcd_digits [0:4];

    logic [7:0]  msg [0:9];
    logic [3:0]  idx;
    logic [3:0]  last_idx;
    logic        waiting_busy;

    function automatic logic [7:0] ascii_digit(input logic [3:0] val);
        case (val)
            4'd0: ascii_digit = 8'h30;
            4'd1: ascii_digit = 8'h31;
            4'd2: ascii_digit = 8'h32;
            4'd3: ascii_digit = 8'h33;
            4'd4: ascii_digit = 8'h34;
            4'd5: ascii_digit = 8'h35;
            4'd6: ascii_digit = 8'h36;
            4'd7: ascii_digit = 8'h37;
            4'd8: ascii_digit = 8'h38;
            4'd9: ascii_digit = 8'h39;
        endcase
    endfunction

    always_ff @(posedge clk) begin
        if (rst) begin
            state         <= IDLE;
            seq_value_reg <= 16'd0;
            uart_start    <= 1'b0;
            idx           <= 4'd0;
            last_idx      <= 4'd5;
            waiting_busy  <= 1'b0;

            msg[0] <= 8'h42;  // 'B'
            msg[1] <= 8'h54;  // 'T'
            msg[2] <= 8'h4E;  // 'N'
            msg[3] <= 8'h20;  // ' '
            msg[4] <= 8'h30;  // '0'
            msg[5] <= 8'h0A;  // '\n'

            for (int i = 0; i < 5; i++) begin
                bcd_digits[i] <= 4'd0;
            end
        end else begin
            uart_start <= 1'b0;

            case (state)
                IDLE: begin
                    if (btn_pressed) begin
                        seq_value_reg <= seq_value_reg + 16'd1;

                        // BCD increment with carry (unrolled for synthesis)
                        if (bcd_digits[0] == 4'd9) begin
                            bcd_digits[0] <= 4'd0;
                            if (bcd_digits[1] == 4'd9) begin
                                bcd_digits[1] <= 4'd0;
                                if (bcd_digits[2] == 4'd9) begin
                                    bcd_digits[2] <= 4'd0;
                                    if (bcd_digits[3] == 4'd9) begin
                                        bcd_digits[3] <= 4'd0;
                                        if (bcd_digits[4] == 4'd9) begin
                                            bcd_digits[4] <= 4'd0;
                                        end else begin
                                            bcd_digits[4] <= bcd_digits[4] + 4'd1;
                                        end
                                    end else begin
                                        bcd_digits[3] <= bcd_digits[3] + 4'd1;
                                    end
                                end else begin
                                    bcd_digits[2] <= bcd_digits[2] + 4'd1;
                                end
                            end else begin
                                bcd_digits[1] <= bcd_digits[1] + 4'd1;
                            end
                        end else begin
                            bcd_digits[0] <= bcd_digits[0] + 4'd1;
                        end

                        // Format message
                        msg[0] <= 8'h42;  // 'B'
                        msg[1] <= 8'h54;  // 'T'
                        msg[2] <= 8'h4E;  // 'N'
                        msg[3] <= 8'h20;  // ' '

                        if (bcd_digits[4] != 4'd0) begin
                            msg[4] <= ascii_digit(bcd_digits[4]);
                            msg[5] <= ascii_digit(bcd_digits[3]);
                            msg[6] <= ascii_digit(bcd_digits[2]);
                            msg[7] <= ascii_digit(bcd_digits[1]);
                            msg[8] <= ascii_digit(bcd_digits[0]);
                            msg[9] <= 8'h0A;
                            last_idx <= 4'd9;
                        end else if (bcd_digits[3] != 4'd0) begin
                            msg[4] <= ascii_digit(bcd_digits[3]);
                            msg[5] <= ascii_digit(bcd_digits[2]);
                            msg[6] <= ascii_digit(bcd_digits[1]);
                            msg[7] <= ascii_digit(bcd_digits[0]);
                            msg[8] <= 8'h0A;
                            last_idx <= 4'd8;
                        end else if (bcd_digits[2] != 4'd0) begin
                            msg[4] <= ascii_digit(bcd_digits[2]);
                            msg[5] <= ascii_digit(bcd_digits[1]);
                            msg[6] <= ascii_digit(bcd_digits[0]);
                            msg[7] <= 8'h0A;
                            last_idx <= 4'd7;
                        end else if (bcd_digits[1] != 4'd0) begin
                            msg[4] <= ascii_digit(bcd_digits[1]);
                            msg[5] <= ascii_digit(bcd_digits[0]);
                            msg[6] <= 8'h0A;
                            last_idx <= 4'd6;
                        end else begin
                            msg[4] <= ascii_digit(bcd_digits[0]);
                            msg[5] <= 8'h0A;
                            last_idx <= 4'd5;
                        end

                        idx          <= 4'd0;
                        waiting_busy <= 1'b0;
                        state        <= SEND;
                    end
                end

                SEND: begin
                    if (!uart_busy && !waiting_busy) begin
                        uart_data    <= msg[idx];
                        uart_start   <= 1'b1;
                        waiting_busy <= 1'b1;
                    end else if (waiting_busy && uart_busy) begin
                        waiting_busy <= 1'b0;

                        if (idx == last_idx) begin
                            state <= IDLE;
                        end else begin
                            idx <= idx + 1'b1;
                        end
                    end
                end
            endcase
        end
    end

    assign seq_value = seq_value_reg;
endmodule

// ----------------------------------------------------------------------------
// ACK Command Parser
// ----------------------------------------------------------------------------
module ack_parser #(
    parameter int CLOCK_HZ = 12_000_000
) (
    input  logic        clk,
    input  logic        rst,
    input  logic [7:0]  data,
    input  logic        valid,
    output logic        trigger,
    output logic [31:0] pulse_cycles
);
    typedef enum logic [2:0] {
        STATE_IDLE  = 3'd0,
        STATE_A     = 3'd1,
        STATE_C1    = 3'd2,
        STATE_C2    = 3'd3,
        STATE_SPACE = 3'd4,
        STATE_NUM   = 3'd5
    } state_t;

    state_t state;
    logic [31:0] duration_ms;

    function automatic logic [31:0] ms_to_cycles(input logic [31:0] ms);
        ms_to_cycles = ms * (CLOCK_HZ / 1000);
    endfunction

    always_ff @(posedge clk) begin
        if (rst) begin
            state        <= STATE_IDLE;
            duration_ms  <= 32'd0;
            trigger      <= 1'b0;
            pulse_cycles <= 32'd0;
        end else begin
            trigger <= 1'b0;

            if (valid) begin
                case (state)
                    STATE_IDLE: begin
                        duration_ms <= 32'd0;
                        if (data == 8'h41)  // 'A'
                            state <= STATE_A;
                    end

                    STATE_A: begin
                        if (data == 8'h43)  // 'C'
                            state <= STATE_C1;
                        else
                            state <= STATE_IDLE;
                    end

                    STATE_C1: begin
                        if (data == 8'h4B)  // 'K'
                            state <= STATE_C2;
                        else
                            state <= STATE_IDLE;
                    end

                    STATE_C2: begin
                        if (data == 8'h20)  // ' '
                            state <= STATE_SPACE;
                        else
                            state <= STATE_IDLE;
                    end

                    STATE_SPACE: begin
                        if (data >= 8'h30 && data <= 8'h39) begin
                            duration_ms <= {24'd0, data} - 32'd48;
                            state <= STATE_NUM;
                        end else if (data == 8'h0A) begin
                            trigger      <= 1'b1;
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            state        <= STATE_IDLE;
                        end else begin
                            state <= STATE_IDLE;
                        end
                    end

                    STATE_NUM: begin
                        if (data >= 8'h30 && data <= 8'h39) begin
                            duration_ms <= (duration_ms * 10) + ({24'd0, data} - 32'd48);
                        end else if (data == 8'h0A) begin
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            trigger      <= 1'b1;
                            state        <= STATE_IDLE;
                        end else begin
                            state <= STATE_IDLE;
                        end
                    end

                    default: state <= STATE_IDLE;
                endcase
            end
        end
    end
endmodule

// ============================================================================
// TOP LEVEL - Super Counter (Fast Simulation)
// ============================================================================
module super_counter_fast #(
    parameter int CLOCK_HZ = 12_000_000,
    parameter int BAUD = 115_200,
    parameter int DEBOUNCE_CYCLES = 16  // Fast for simulation (was 256 cycles)
) (
    // Clock - FPGA-realistic name for yosys2digitaljs auto-conversion
    input  logic clk_12m,

    // Control (active-high for easier testing)
    input  logic rst,
    input  logic btn_press,

    // UART
    input  logic uart_rx,
    output logic uart_tx,

    // LED output
    output logic led,

    // DEBUG outputs - monitor internal state
    output logic [15:0] btn_count,      // Button press counter
    output logic        btn_debounced,  // Debounced button state
    output logic        tx_busy,        // UART TX busy flag
    output logic        rx_valid        // UART RX valid pulse
);
    // Debouncer (now uses active-high button directly)
    logic btn_pressed;
    logic btn_stable;
    debouncer #(
        .DEBOUNCE_CYCLES(DEBOUNCE_CYCLES)
    ) debouncer_inst (
        .clk(clk_12m),
        .rst(rst),
        .btn(btn_press),
        .pressed(btn_pressed),
        .stable_out(btn_stable)
    );

    // Debug: expose debounced state
    assign btn_debounced = btn_stable;

    // Button message formatter
    logic [7:0]  tx_data;
    logic        tx_start;
    logic [15:0] seq_value;

    btn_message btn_message_inst (
        .clk(clk_12m),
        .rst(rst),
        .btn_pressed(btn_pressed),
        .seq_value(seq_value),
        .uart_data(tx_data),
        .uart_start(tx_start),
        .uart_busy(tx_busy)
    );

    assign btn_count = seq_value;

    // UART transmitter
    uart_tx #(
        .CLOCK_HZ(CLOCK_HZ),
        .BAUD(BAUD)
    ) uart_tx_inst (
        .clk(clk_12m),
        .rst(rst),
        .data(tx_data),
        .start(tx_start),
        .busy(tx_busy),
        .serial_out(uart_tx)
    );

    // UART receiver
    logic [7:0] rx_data;

    uart_rx #(
        .CLOCK_HZ(CLOCK_HZ),
        .BAUD(BAUD)
    ) uart_rx_inst (
        .clk(clk_12m),
        .rst(rst),
        .serial_in(uart_rx),
        .data(rx_data),
        .valid(rx_valid)
    );

    // ACK parser
    logic        ack_trigger;
    logic [31:0] led_cycles;

    ack_parser #(
        .CLOCK_HZ(CLOCK_HZ)
    ) ack_parser_inst (
        .clk(clk_12m),
        .rst(rst),
        .data(rx_data),
        .valid(rx_valid),
        .trigger(ack_trigger),
        .pulse_cycles(led_cycles)
    );

    // LED pulse
    led_pulse #(
        .CLOCK_HZ(CLOCK_HZ)
    ) led_pulse_inst (
        .clk(clk_12m),
        .rst(rst),
        .trigger(ack_trigger),
        .pulse_cycles(led_cycles),
        .led(led)
    );

endmodule
