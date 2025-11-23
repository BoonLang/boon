// ============================================================================
// PACKED COUNTER TEST - Testing clock auto-conversion with clk_12m
// ============================================================================
//
// This version tests our yosys2digitaljs clock auto-conversion with:
// - FPGA-realistic clock name: clk_12m ✅
// - Simple button counter with debouncing
// - LED pulse on button press
// - Minimal module instantiation to avoid synthesis issues
//
// Based on lessons learned:
// - Single always_ff block per module (avoid state conflicts)
// - Fast timing for interactive simulation (propagation=1)
// - Active-high signals for clarity in testing
//
// ============================================================================

// ----------------------------------------------------------------------------
// Simple Debouncer - Standalone module with clean interface
// ----------------------------------------------------------------------------
module simple_debouncer #(
    parameter int DEBOUNCE_CYCLES = 16
) (
    input  logic clk,
    input  logic rst,
    input  logic btn,           // Active-high button
    output logic btn_stable,    // Debounced output
    output logic btn_pulse      // Single-cycle pulse on press
);
    localparam CNTR_WIDTH = $clog2(DEBOUNCE_CYCLES);
    logic [CNTR_WIDTH-1:0] counter;
    logic btn_debounced;
    logic btn_debounced_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= '0;
            btn_debounced <= 1'b0;
            btn_debounced_prev <= 1'b0;
            btn_pulse <= 1'b0;
        end else begin
            // Debounce logic
            if (btn == btn_debounced) begin
                counter <= '0;
            end else begin
                if (counter == DEBOUNCE_CYCLES - 1) begin
                    btn_debounced <= btn;
                    counter <= '0;
                end else begin
                    counter <= counter + 1'b1;
                end
            end

            // Edge detection
            btn_debounced_prev <= btn_debounced;
            btn_pulse <= btn_debounced && !btn_debounced_prev;
        end
    end

    assign btn_stable = btn_debounced;
endmodule

// ----------------------------------------------------------------------------
// LED Pulse Generator - Standalone module
// ----------------------------------------------------------------------------
module simple_led_pulse #(
    parameter int PULSE_CYCLES = 100
) (
    input  logic clk,
    input  logic rst,
    input  logic trigger,       // Start pulse
    output logic led
);
    logic [7:0] counter;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= 8'd0;
            led <= 1'b0;
        end else begin
            if (trigger) begin
                counter <= PULSE_CYCLES;
                led <= 1'b1;
            end else if (counter != 8'd0) begin
                counter <= counter - 8'd1;
                led <= 1'b1;
            end else begin
                led <= 1'b0;
            end
        end
    end
endmodule

// ============================================================================
// TOP LEVEL - Packed Counter Test
// ============================================================================
module packed_counter_test (
    // FPGA-realistic clock name - testing yosys2digitaljs auto-conversion!
    input  logic clk_12m,

    // Control signals (active-high for easier testing)
    input  logic rst,
    input  logic btn_press,

    // Outputs
    output logic [7:0] count,
    output logic btn_stable,    // DEBUG: shows debounced button state
    output logic led
);
    // Debouncer instance
    logic btn_pulse;

    simple_debouncer #(
        .DEBOUNCE_CYCLES(16)    // Fast debounce for interactive simulation
    ) debouncer_inst (
        .clk(clk_12m),
        .rst(rst),
        .btn(btn_press),
        .btn_stable(btn_stable),
        .btn_pulse(btn_pulse)
    );

    // Press counter - single always_ff block
    logic [7:0] press_counter;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            press_counter <= 8'd0;
        end else if (btn_pulse) begin
            press_counter <= press_counter + 8'd1;
        end
    end

    assign count = press_counter;

    // LED pulse instance
    simple_led_pulse #(
        .PULSE_CYCLES(100)      // Visible LED pulse
    ) led_pulse_inst (
        .clk(clk_12m),
        .rst(rst),
        .trigger(btn_pulse),
        .led(led)
    );

endmodule
