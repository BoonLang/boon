// ============================================================================
// SUPER COUNTER - Inline Version (No Module Instantiation)
// ============================================================================
//
// Based on button_counter_fixed.sv which works!
// All logic in ONE module with ONE always_ff block
//
// Features:
// - Button debouncing (16 cycles)
// - Press counter (shows number of button presses)
// - LED pulse on button press (100 cycles)
// - FPGA-realistic clock name (clk_12m) for auto-conversion
//
// Pattern: Everything in one always_ff block avoids synthesis issues
//
// ============================================================================

module super_counter_inline (
    // Clock - FPGA-realistic name for yosys2digitaljs auto-conversion
    input  logic clk_12m,

    // Control (active-high)
    input  logic rst,
    input  logic btn_press,

    // Outputs
    output logic [15:0] btn_count,      // Button press counter
    output logic        btn_debounced,  // Debounced button state (DEBUG)
    output logic        led             // LED pulse
);
    // Debounce parameters
    localparam DEBOUNCE_CYCLES = 16;
    localparam LED_CYCLES = 100;
    localparam CNTR_WIDTH = $clog2(DEBOUNCE_CYCLES);
    localparam [CNTR_WIDTH-1:0] DEBOUNCE_MAX = CNTR_WIDTH'(DEBOUNCE_CYCLES - 1);

    // Debounce state
    logic btn_clean;
    assign btn_clean = (btn_press === 1'b1);

    logic [CNTR_WIDTH-1:0] debounce_counter;
    logic btn_sync;
    logic btn_stable;
    logic btn_stable_prev;

    // Counter state
    logic [15:0] press_counter;

    // LED state
    logic [7:0] led_counter;

    // ========================================================================
    // SINGLE always_ff BLOCK - All logic here!
    // ========================================================================
    always_ff @(posedge clk_12m) begin
        if (rst) begin
            // Reset all state
            btn_sync <= 1'b0;
            btn_stable <= 1'b0;
            btn_stable_prev <= 1'b0;
            debounce_counter <= '0;
            press_counter <= 16'd0;
            led_counter <= 8'd0;
            led <= 1'b0;
        end else begin
            // ================================================================
            // Step 1: Synchronize button input (CDC)
            // ================================================================
            btn_sync <= btn_clean;

            // ================================================================
            // Step 2: Debounce logic
            // ================================================================
            if (btn_sync == btn_stable) begin
                // Button stable - reset counter
                debounce_counter <= '0;
            end else begin
                // Button changed - count cycles
                if (debounce_counter == DEBOUNCE_MAX) begin
                    // Enough cycles - accept new state
                    btn_stable <= btn_sync;
                    debounce_counter <= '0;
                end else begin
                    debounce_counter <= debounce_counter + 1'b1;
                end
            end

            // ================================================================
            // Step 3: Edge detection and counter increment
            // ================================================================
            btn_stable_prev <= btn_stable;

            if (btn_stable && !btn_stable_prev) begin
                // Rising edge detected - button press!
                press_counter <= press_counter + 16'd1;
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

    // ========================================================================
    // Output assignments
    // ========================================================================
    assign btn_count = press_counter;
    assign btn_debounced = btn_stable;

endmodule
