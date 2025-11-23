// UART Receiver (8N1) - SystemVerilog
// Ideal transpiler output from uart_rx.bn
//
// Demonstrates:
// - CDC synchronizer (2-FF for async serial_in)
// - Baud rate generator with half-period offset
// - FSM (idle/receiving states)
// - Shift register with bit indexing
// - Valid pulse generation
//
// Target: Yosys-compatible SystemVerilog

module uart_rx #(
    parameter int CLOCK_HZ = 25_000_000,
    parameter int BAUD = 115_200
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       serial_in,
    output logic [7:0] data,
    output logic       valid
);
    // Baud rate calculations
    localparam int DIVISOR = CLOCK_HZ / BAUD;
    localparam int CTR_WIDTH = $clog2(DIVISOR);

    // CDC Synchronizer (2-FF chain for async serial_in)
    logic serial_sync0, serial_sync1;
    logic serial;

    always_ff @(posedge clk) begin
        if (rst) begin
            serial_sync0 <= 1'b1;  // Idle high
            serial_sync1 <= 1'b1;
        end else begin
            serial_sync0 <= serial_in;      // First FF (may be metastable)
            serial_sync1 <= serial_sync0;   // Second FF (stable)
        end
    end

    assign serial = serial_sync1;  // Safe synchronized signal

    // FSM state
    logic busy;  // 0=idle, 1=receiving

    // Baud rate counter
    logic [CTR_WIDTH-1:0] baud_cnt;

    // Bit index (0-8: 0-7 for data, 8 for stop bit)
    logic [3:0] bit_idx;

    // Shift register (accumulates received bits)
    logic [7:0] shift;

    // FSM and datapath
    always_ff @(posedge clk) begin
        if (rst) begin
            busy     <= 1'b0;
            baud_cnt <= '0;
            bit_idx  <= 4'd0;
            shift    <= 8'h00;
            data     <= 8'h00;
            valid    <= 1'b0;
        end else begin
            valid <= 1'b0;  // Default: no valid pulse

            if (!busy) begin
                // Idle state: wait for start bit (line goes low)
                if (!serial) begin
                    // Start bit detected!
                    busy     <= 1'b1;
                    baud_cnt <= DIVISOR[CTR_WIDTH-1:0] >> 1;  // Half period (sample at middle)
                    bit_idx  <= 4'd0;
                end
            end else begin
                // Receiving state: count down and sample
                if (baud_cnt == '0) begin
                    // Baud tick: reload counter
                    baud_cnt <= DIVISOR[CTR_WIDTH-1:0] - 1'b1;

                    if (bit_idx < 8) begin
                        // Sample data bit
                        shift[bit_idx[2:0]] <= serial;
                        bit_idx <= bit_idx + 1'b1;
                    end else begin
                        // Stop bit check (bit_idx == 8)
                        if (serial) begin  // Valid stop bit (should be 1)
                            data  <= shift;
                            valid <= 1'b1;  // One-cycle pulse
                        end
                        busy <= 1'b0;  // Back to idle
                    end
                end else begin
                    // Count down
                    baud_cnt <= baud_cnt - 1'b1;
                end
            end
        end
    end
endmodule
