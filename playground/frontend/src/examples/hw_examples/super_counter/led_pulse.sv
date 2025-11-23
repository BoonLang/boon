// LED Pulse Generator (SystemVerilog version)
// Generates an LED pulse of configurable duration (in clock cycles)
//
// When trigger is asserted, the LED turns on and stays on for
// pulse_cycles clock cycles, then turns off.

module led_pulse #(
    parameter int CLOCK_HZ = 25_000_000  // Not used in this module
) (
    input  logic        clk,
    input  logic        rst,
    input  logic        trigger,
    input  logic [31:0] pulse_cycles,
    output logic        led
);
    logic [31:0] counter;

    always_ff @(posedge clk) begin
        if (rst) begin
            led <= 1'b0;
            counter <= 32'd0;
        end else begin
            if (trigger) begin
                // Load counter and turn on LED
                counter <= pulse_cycles;
                led <= 1'b1;
            end else if (counter != 32'd0) begin
                // Count down
                counter <= counter - 32'd1;
                // Turn off LED when counter reaches 1
                if (counter == 32'd1) begin
                    led <= 1'b0;
                end
            end else begin
                // Counter is 0: LED stays off
                led <= 1'b0;
            end
        end
    end
endmodule
