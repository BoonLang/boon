// Minimal test: Verify clk_12m auto-converts to Clock cell
// This is the simplest possible test of the flexible clock pattern recognition

module simple_clock_test (
    input  logic clk_12m,    // Should auto-convert to Clock cell!
    input  logic rst,
    output logic [7:0] count
);
    logic [7:0] counter;

    // Simple counter with reset - that's it!
    always_ff @(posedge clk_12m) begin
        if (rst) begin
            counter <= 8'd0;
        end else begin
            counter <= counter + 8'd1;
        end
    end

    assign count = counter;
endmodule
