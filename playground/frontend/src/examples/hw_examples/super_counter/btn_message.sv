// Button Message Formatter - SystemVerilog
// Ideal transpiler output from btn_message.bn
//
// Demonstrates:
// - BCD arithmetic (5-digit decimal counter)
// - Dynamic message formatting
// - Array management
// - UART transmission FSM with handshake
//
// Target: Yosys-compatible SystemVerilog

module btn_message (
    input  logic        clk,
    input  logic        rst,
    input  logic        btn_pressed,
    input  logic        uart_busy,
    output logic [15:0] seq_value,
    output logic [7:0]  uart_data,
    output logic        uart_start
);
    // FSM states
    typedef enum logic {
        IDLE,
        SEND
    } state_t;

    state_t state;

    // Counters
    logic [15:0] seq_value_reg;        // Binary counter
    logic [3:0]  bcd_digits [0:4];     // BCD counter (little-endian)

    // Message buffer and transmission
    logic [7:0]  msg [0:9];            // Message buffer (max 10 bytes)
    logic [3:0]  idx;                  // Current byte index
    logic [3:0]  last_idx;             // Last valid byte index
    logic        waiting_busy;         // Handshake state

    // Helper function: BCD digit to ASCII
    function automatic logic [7:0] ascii_digit(input logic [3:0] val);
        case (val)
            4'd0: ascii_digit = 8'h30;  // '0'
            4'd1: ascii_digit = 8'h31;
            4'd2: ascii_digit = 8'h32;
            4'd3: ascii_digit = 8'h33;
            4'd4: ascii_digit = 8'h34;
            4'd5: ascii_digit = 8'h35;
            4'd6: ascii_digit = 8'h36;
            4'd7: ascii_digit = 8'h37;
            4'd8: ascii_digit = 8'h38;
            4'd9: ascii_digit = 8'h39;  // '9'
        endcase
    endfunction

    // Main FSM
    always_ff @(posedge clk) begin
        if (rst) begin
            state        <= IDLE;
            seq_value_reg <= 16'd0;
            uart_start   <= 1'b0;
            idx          <= 4'd0;
            last_idx     <= 4'd5;
            waiting_busy <= 1'b0;

            // Initialize message: "BTN 0\n"
            msg[0] <= 8'h42;  // 'B'
            msg[1] <= 8'h54;  // 'T'
            msg[2] <= 8'h4E;  // 'N'
            msg[3] <= 8'h20;  // ' '
            msg[4] <= 8'h30;  // '0'
            msg[5] <= 8'h0A;  // '\n'

            // Initialize BCD digits
            for (int i = 0; i < 5; i++) begin
                bcd_digits[i] <= 4'd0;
            end
        end else begin
            uart_start <= 1'b0;  // Default: no start pulse

            unique case (state)
                IDLE: begin
                    if (btn_pressed) begin
                        // Increment binary counter
                        seq_value_reg <= seq_value_reg + 16'd1;

                        // Increment BCD with carry propagation
                        automatic logic carry = 1'b1;
                        for (int i = 0; i < 5; i++) begin
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

                        // Count significant digits
                        automatic int digits_count;
                        if (bcd_digits[4] != 0)      digits_count = 5;
                        else if (bcd_digits[3] != 0) digits_count = 4;
                        else if (bcd_digits[2] != 0) digits_count = 3;
                        else if (bcd_digits[1] != 0) digits_count = 2;
                        else                         digits_count = 1;

                        // Format message
                        msg[0] <= 8'h42;  // 'B'
                        msg[1] <= 8'h54;  // 'T'
                        msg[2] <= 8'h4E;  // 'N'
                        msg[3] <= 8'h20;  // ' '

                        unique case (digits_count)
                            5: begin
                                msg[4] <= ascii_digit(bcd_digits[4]);
                                msg[5] <= ascii_digit(bcd_digits[3]);
                                msg[6] <= ascii_digit(bcd_digits[2]);
                                msg[7] <= ascii_digit(bcd_digits[1]);
                                msg[8] <= ascii_digit(bcd_digits[0]);
                                msg[9] <= 8'h0A;  // '\n'
                                last_idx <= 4'd9;
                            end
                            4: begin
                                msg[4] <= ascii_digit(bcd_digits[3]);
                                msg[5] <= ascii_digit(bcd_digits[2]);
                                msg[6] <= ascii_digit(bcd_digits[1]);
                                msg[7] <= ascii_digit(bcd_digits[0]);
                                msg[8] <= 8'h0A;
                                last_idx <= 4'd8;
                            end
                            3: begin
                                msg[4] <= ascii_digit(bcd_digits[2]);
                                msg[5] <= ascii_digit(bcd_digits[1]);
                                msg[6] <= ascii_digit(bcd_digits[0]);
                                msg[7] <= 8'h0A;
                                last_idx <= 4'd7;
                            end
                            2: begin
                                msg[4] <= ascii_digit(bcd_digits[1]);
                                msg[5] <= ascii_digit(bcd_digits[0]);
                                msg[6] <= 8'h0A;
                                last_idx <= 4'd6;
                            end
                            default: begin  // 1 digit
                                msg[4] <= ascii_digit(bcd_digits[0]);
                                msg[5] <= 8'h0A;
                                last_idx <= 4'd5;
                            end
                        endcase

                        // Start transmission
                        idx          <= 4'd0;
                        waiting_busy <= 1'b0;
                        state        <= SEND;
                    end
                end

                SEND: begin
                    if (!uart_busy && !waiting_busy) begin
                        // Start transmitting next byte
                        uart_data    <= msg[idx];
                        uart_start   <= 1'b1;
                        waiting_busy <= 1'b1;
                    end else if (waiting_busy && uart_busy) begin
                        // UART accepted byte
                        waiting_busy <= 1'b0;

                        if (idx == last_idx) begin
                            // Done transmitting
                            state <= IDLE;
                        end else begin
                            // Move to next byte
                            idx <= idx + 1'b1;
                        end
                    end
                end
            endcase
        end
    end

    assign seq_value = seq_value_reg;
endmodule
