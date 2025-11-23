// Button Counter - Fixed version based on working simple_button_test
// Combines edge detection, debouncing, LED pulse, and counter in one clean module

module button_counter_fixed (
    input  logic clk_12m,
    input  logic rst,
    input  logic btn,               // Direct button input (active high)
    output logic [7:0] count,
    output logic btn_stable,        // DEBUG: shows debounced state
    output logic led
);
    // Debounce counter
    localparam DEBOUNCE_CYCLES = 16;  // Adjust as needed
    logic [$clog2(DEBOUNCE_CYCLES)-1:0] debounce_counter;
    logic btn_sync;                   // Synchronized button
    logic btn_debounced;              // Debounced button

    // LED timer
    localparam LED_CYCLES = 100;
    logic [7:0] led_counter;

    // Press counter
    logic [7:0] press_counter;
    logic btn_debounced_prev;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            // Reset all state
            btn_sync <= 1'b0;
            btn_debounced <= 1'b0;
            btn_debounced_prev <= 1'b0;
            debounce_counter <= '0;
            press_counter <= 8'd0;
            led_counter <= 8'd0;
            led <= 1'b0;
        end else begin
            // Step 1: Synchronize button input
            btn_sync <= btn;

            // Step 2: Debounce logic
            if (btn_sync == btn_debounced) begin
                // Button stable - reset counter
                debounce_counter <= '0;
            end else begin
                // Button changed - count cycles
                if (debounce_counter == DEBOUNCE_CYCLES - 1) begin
                    // Enough cycles - accept new state
                    btn_debounced <= btn_sync;
                    debounce_counter <= '0;
                end else begin
                    debounce_counter <= debounce_counter + 1'b1;
                end
            end

            // Step 3: Edge detection and counter increment
            btn_debounced_prev <= btn_debounced;
            if (btn_debounced && !btn_debounced_prev) begin
                // Rising edge - increment counter and start LED pulse
                press_counter <= press_counter + 8'd1;
                led_counter <= LED_CYCLES;
                led <= 1'b1;
            end else if (led_counter != 8'd0) begin
                // LED timer counting down
                led_counter <= led_counter - 8'd1;
                led <= 1'b1;
            end else begin
                // LED off
                led <= 1'b0;
            end
        end
    end

    assign count = press_counter;
    assign btn_stable = btn_debounced;
endmodule
