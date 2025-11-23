// UART Transmitter (8N1) - SystemVerilog
// Ideal transpiler output from uart_tx.bn
//
// Demonstrates:
// - Baud rate generator (divider counter)
// - FSM (idle/busy states)
// - Shift register (10 bits)
//
// Target: Yosys-compatible SystemVerilog

module uart_tx #(
    parameter int CLOCK_HZ = 25_000_000,
    parameter int BAUD = 115_200
) (
    input  logic       clk,
    input  logic       rst,
    input  logic [7:0] data,
    input  logic       start,
    output logic       busy,
    output logic       serial_out
);
    // Baud rate calculations
    localparam int DIVISOR = CLOCK_HZ / BAUD;
    localparam int CTR_WIDTH = $clog2(DIVISOR);

    // Baud rate divider (counts down from DIVISOR-1 to 0)
    logic [CTR_WIDTH-1:0] baud_cnt;
    logic baud_tick;

    always_ff @(posedge clk) begin
        if (rst) begin
            baud_cnt <= DIVISOR[CTR_WIDTH-1:0] - 1'b1;
        end else if (busy) begin
            if (baud_cnt == '0) begin
                baud_cnt <= DIVISOR[CTR_WIDTH-1:0] - 1'b1;
            end else begin
                baud_cnt <= baud_cnt - 1'b1;
            end
        end else begin
            baud_cnt <= DIVISOR[CTR_WIDTH-1:0] - 1'b1;
        end
    end

    assign baud_tick = (baud_cnt == '0);

    // Bit index (0-9 for 10 bits)
    logic [3:0] bit_idx;

    // Shift register (10 bits: start + 8 data + stop)
    logic [9:0] shifter;

    // FSM and datapath
    always_ff @(posedge clk) begin
        if (rst) begin
            busy       <= 1'b0;
            bit_idx    <= 4'd0;
            shifter    <= 10'h3FF;  // All 1's (idle)
            serial_out <= 1'b1;     // Idle high
        end else begin
            if (!busy) begin
                // Idle state
                serial_out <= 1'b1;
                if (start) begin
                    // Start transmission
                    busy    <= 1'b1;
                    bit_idx <= 4'd0;
                    shifter <= {1'b1, data, 1'b0};  // {stop, data, start}
                end
            end else if (baud_tick) begin
                // Transmitting: shift on baud tick
                serial_out <= shifter[0];           // Output LSB
                shifter    <= {1'b1, shifter[9:1]}; // Shift right, fill with 1
                bit_idx    <= bit_idx + 1'b1;

                // Check if done (sent all 10 bits)
                if (bit_idx == 4'd9) begin
                    busy <= 1'b0;
                end
            end
        end
    end
endmodule
