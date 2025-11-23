// Parses ASCII "ACK <duration>" commands terminated by newline.
module ack_parser #(
    parameter integer CLOCK_HZ = 25_000_000
) (
    input  wire clk,
    input  wire rst,
    input  wire [7:0] data,
    input  wire       valid,
    output reg        trigger,
    output reg [31:0] pulse_cycles
);
    localparam STATE_IDLE  = 3'd0;
    localparam STATE_A     = 3'd1;
    localparam STATE_C1    = 3'd2;
    localparam STATE_C2    = 3'd3;
    localparam STATE_SPACE = 3'd4;
    localparam STATE_NUM   = 3'd5;

    reg [2:0] state = STATE_IDLE;
    reg [31:0] duration_ms = 32'd0;
    integer digit_value;

    function [31:0] ms_to_cycles;
        input [31:0] ms;
        begin
            ms_to_cycles = ms * (CLOCK_HZ / 1000);
        end
    endfunction

    always @(posedge clk) begin
        if (rst) begin
            state <= STATE_IDLE;
            duration_ms <= 32'd0;
            trigger <= 1'b0;
            pulse_cycles <= 32'd0;
        end else begin
            trigger <= 1'b0;
            if (valid) begin
                case (state)
                    STATE_IDLE: begin
                        duration_ms <= 0;
                        if (data == "A") state <= STATE_A;
                    end
                    STATE_A: begin
                        if (data == "C") state <= STATE_C1;
                        else state <= STATE_IDLE;
                    end
                    STATE_C1: begin
                        if (data == "K") state <= STATE_C2;
                        else state <= STATE_IDLE;
                    end
                    STATE_C2: begin
                        if (data == " ") state <= STATE_SPACE;
                        else state <= STATE_IDLE;
                    end
                    STATE_SPACE: begin
                        if (data >= "0" && data <= "9") begin
                            digit_value = {24'd0, data} - 32'd48;
                            duration_ms <= digit_value[31:0];
                            state <= STATE_NUM;
                        end else if (data == "\n") begin
                            trigger <= 1'b1;
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            state <= STATE_IDLE;
                        end else begin
                            state <= STATE_IDLE;
                        end
                    end
                    STATE_NUM: begin
                        if (data >= "0" && data <= "9") begin
                            digit_value = {24'd0, data} - 32'd48;
                            duration_ms <= (duration_ms * 10) + digit_value[31:0];
                        end else if (data == "\n") begin
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            trigger <= 1'b1;
                            state <= STATE_IDLE;
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
