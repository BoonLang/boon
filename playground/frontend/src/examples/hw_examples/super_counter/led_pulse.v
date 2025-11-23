// LED pulse generator
module led_pulse #(
    parameter integer CLOCK_HZ = 25_000_000
) (
    input  wire clk,
    input  wire rst,
    input  wire trigger,
    input  wire [31:0] pulse_cycles,
    output reg  led
);
    reg [31:0] counter = 32'd0;

    always @(posedge clk) begin
        if (rst) begin
            led <= 1'b0;
            counter <= 32'd0;
        end else begin
            if (trigger) begin
                counter <= pulse_cycles;
                led <= 1'b1;
            end else if (counter != 32'd0) begin
                counter <= counter - 1'b1;
                if (counter == 32'd1) begin
                    led <= 1'b0;
                end
            end else begin
                led <= 1'b0;
            end
        end
    end
endmodule
