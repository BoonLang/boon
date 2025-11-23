// Simple counter test - no debouncer, direct button increment
// For testing DigitalJS auto-clock

module simple_counter_test (
    input  logic clk,
    input  logic rst,
    input  logic btn_press,
    output logic [7:0] count,
    output logic led
);
    logic [7:0] counter;
    logic btn_prev;
    logic btn_edge;

    // Detect button rising edge
    always_ff @(posedge clk) begin
        if (rst) begin
            btn_prev <= 1'b0;
        end else begin
            btn_prev <= btn_press;
        end
    end

    assign btn_edge = btn_press && !btn_prev;

    // Counter
    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= 8'd0;
        end else if (btn_edge) begin
            counter <= counter + 8'd1;
        end
    end

    assign count = counter;

    // LED blinks when counter changes
    always_ff @(posedge clk) begin
        if (rst) begin
            led <= 1'b0;
        end else if (btn_edge) begin
            led <= 1'b1;
        end else begin
            led <= 1'b0;
        end
    end

endmodule
