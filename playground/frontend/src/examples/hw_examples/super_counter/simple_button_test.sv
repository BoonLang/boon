// Minimal button test - no debouncer, just direct counter
// This will help us see if the basic circuit logic works

module simple_button_test (
    input  logic clk_12m,
    input  logic rst,
    input  logic btn,           // Direct button input
    output logic [7:0] count,
    output logic led
);
    logic [7:0] counter;
    logic btn_prev;

    // Simple edge detection
    always_ff @(posedge clk_12m) begin
        if (rst) begin
            btn_prev <= 1'b0;
            counter <= 8'd0;
            led <= 1'b0;
        end else begin
            btn_prev <= btn;

            // Increment on rising edge
            if (btn && !btn_prev) begin
                counter <= counter + 8'd1;
                led <= 1'b1;
            end else begin
                led <= 1'b0;
            end
        end
    end

    assign count = counter;
endmodule
