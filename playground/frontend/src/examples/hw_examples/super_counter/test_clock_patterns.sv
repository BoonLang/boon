// Test file to verify flexible clock pattern recognition in yosys2digitaljs
// This demonstrates that FPGA-realistic clock names now work in DigitalJS!
//
// The key test: clk_12m should auto-convert to a Clock cell (not Input)
// and the circuit should work without needing to rename it to "clk"

module test_clock_patterns (
    // FPGA-realistic clock name - should auto-convert to Clock cell!
    input  logic clk_12m,       // ✅ FPGA frequency pattern

    // Reset and control
    input  logic rst,           // Reset the counter
    input  logic enable,        // Enable counting

    // Outputs
    output logic [7:0] count,
    output logic [7:0] count2
);
    logic [7:0] counter1;
    logic [7:0] counter2;

    // Counter 1: Simple free-running counter with reset
    always_ff @(posedge clk_12m) begin
        if (rst) begin
            counter1 <= 8'd0;
        end else begin
            counter1 <= counter1 + 8'd1;
        end
    end

    // Counter 2: Enabled counter (only counts when enable is high)
    // Remove the explicit hold - let the register naturally retain its value
    always_ff @(posedge clk_12m) begin
        if (rst) begin
            counter2 <= 8'd0;
        end else if (enable) begin
            counter2 <= counter2 + 8'd1;
        end
        // No else clause - register holds its value by default
    end

    assign count = counter1;
    assign count2 = counter2;
endmodule
