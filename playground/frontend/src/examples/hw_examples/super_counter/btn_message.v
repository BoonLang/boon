module btn_message (
    input  wire clk,
    input  wire rst,
    input  wire btn_pressed,
    output reg  [15:0] seq_value,
    output reg  [7:0]  uart_data,
    output reg         uart_start,
    input  wire        uart_busy
);
    localparam STATE_IDLE = 1'b0;
    localparam STATE_SEND = 1'b1;

    reg        state = STATE_IDLE;
    reg [3:0]  idx   = 4'd0;
    reg [3:0]  last_idx = 4'd5;
    reg        waiting_busy = 1'b0;

    reg [7:0] msg [0:9];
    reg [3:0] bcd_digits [0:4]; // little-endian: digit 0 = ones place

    function automatic [7:0] ascii_digit(input [3:0] val);
        begin
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
                default: ascii_digit = 8'h39;
            endcase
        end
    endfunction

    integer digits_count;
    integer i;
    reg carry;

    always @(posedge clk) begin
        if (rst) begin
            state      <= STATE_IDLE;
            seq_value  <= 16'd0;
            uart_start <= 1'b0;
            idx        <= 4'd0;
            last_idx   <= 4'd5;
            waiting_busy <= 1'b0;
            msg[0] <= "B"; msg[1] <= "T"; msg[2] <= "N"; msg[3] <= " ";
            msg[4] <= "0"; msg[5] <= "\n";
            for (i = 0; i < 5; i = i + 1) begin
                bcd_digits[i] <= 4'd0;
            end
        end else begin
            uart_start <= 1'b0;
            case (state)
                STATE_IDLE: begin
                    if (btn_pressed) begin
                        seq_value <= seq_value + 16'd1;

                        // Increment BCD digits with ripple carry.
                        carry = 1'b1;
                        for (i = 0; i < 5; i = i + 1) begin
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

                        if (bcd_digits[4] != 0)
                            digits_count = 5;
                        else if (bcd_digits[3] != 0)
                            digits_count = 4;
                        else if (bcd_digits[2] != 0)
                            digits_count = 3;
                        else if (bcd_digits[1] != 0)
                            digits_count = 2;
                        else
                            digits_count = 1;

                        msg[0] <= "B";
                        msg[1] <= "T";
                        msg[2] <= "N";
                        msg[3] <= " ";

                        case (digits_count)
                            5: begin
                                msg[4] <= ascii_digit(bcd_digits[4]);
                                msg[5] <= ascii_digit(bcd_digits[3]);
                                msg[6] <= ascii_digit(bcd_digits[2]);
                                msg[7] <= ascii_digit(bcd_digits[1]);
                                msg[8] <= ascii_digit(bcd_digits[0]);
                                msg[9] <= 8'h0A;
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
                            default: begin
                                msg[4] <= ascii_digit(bcd_digits[0]);
                                msg[5] <= 8'h0A;
                                last_idx <= 4'd5;
                            end
                        endcase

                        idx   <= 4'd0;
                        waiting_busy <= 1'b0;
                        state <= STATE_SEND;
                    end
                end
                STATE_SEND: begin
                    if (!uart_busy && !waiting_busy) begin
                        uart_data  <= msg[idx];
                        uart_start <= 1'b1;
                        waiting_busy <= 1'b1;
                    end else if (waiting_busy && uart_busy) begin
                        waiting_busy <= 1'b0;
                        if (idx == last_idx) begin
                            state <= STATE_IDLE;
                        end else begin
                            idx <= idx + 1'b1;
                        end
                    end
                end
            endcase
        end
    end
endmodule
