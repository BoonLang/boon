// ACK Command Parser - SystemVerilog
// Ideal transpiler output from ack_parser.bn
//
// Parses ASCII command: "ACK <duration_ms>\n"
// Example: "ACK 100\n" -> 100ms LED pulse
//
// Demonstrates:
// - ASCII character matching
// - Decimal string parsing
// - FSM for protocol
//
// Target: Yosys-compatible SystemVerilog

module ack_parser #(
    parameter int CLOCK_HZ = 25_000_000
) (
    input  logic        clk,
    input  logic        rst,
    input  logic [7:0]  data,
    input  logic        valid,
    output logic        trigger,
    output logic [31:0] pulse_cycles
);
    // FSM states
    typedef enum logic [2:0] {
        STATE_IDLE  = 3'd0,
        STATE_A     = 3'd1,
        STATE_C1    = 3'd2,
        STATE_C2    = 3'd3,
        STATE_SPACE = 3'd4,
        STATE_NUM   = 3'd5
    } state_t;

    state_t state;

    // Duration accumulator
    logic [31:0] duration_ms;

    // Helper function: Convert ms to clock cycles
    function automatic logic [31:0] ms_to_cycles(input logic [31:0] ms);
        ms_to_cycles = ms * (CLOCK_HZ / 1000);
    endfunction

    // Main FSM
    always_ff @(posedge clk) begin
        if (rst) begin
            state        <= STATE_IDLE;
            duration_ms  <= 32'd0;
            trigger      <= 1'b0;
            pulse_cycles <= 32'd0;
        end else begin
            trigger <= 1'b0;  // Default: no trigger

            if (valid) begin
                unique case (state)
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
                        if (data == 8'h20)  // ' ' (space)
                            state <= STATE_SPACE;
                        else
                            state <= STATE_IDLE;
                    end

                    STATE_SPACE: begin
                        if (data >= 8'h30 && data <= 8'h39) begin
                            // First digit
                            duration_ms <= {24'd0, data} - 32'd48;  // '0' = 0x30
                            state <= STATE_NUM;
                        end else if (data == 8'h0A) begin  // '\n'
                            trigger      <= 1'b1;
                            pulse_cycles <= ms_to_cycles(duration_ms);
                            state        <= STATE_IDLE;
                        end else begin
                            state <= STATE_IDLE;
                        end
                    end

                    STATE_NUM: begin
                        if (data >= 8'h30 && data <= 8'h39) begin
                            // Accumulate digit: duration * 10 + digit
                            duration_ms <= (duration_ms * 10) + ({24'd0, data} - 32'd48);
                        end else if (data == 8'h0A) begin  // '\n'
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
