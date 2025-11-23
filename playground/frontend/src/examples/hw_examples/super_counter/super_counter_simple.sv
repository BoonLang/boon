// ============================================================================
// SUPER COUNTER - Simplified Version
// ============================================================================
//
// Simplified version without complex UART message formatting
// Focus on getting debouncer + counter + LED working reliably
// Then we can add UART complexity later
//
// ============================================================================

// ----------------------------------------------------------------------------
// Button Debouncer
// ----------------------------------------------------------------------------
module debouncer #(
    parameter int DEBOUNCE_CYCLES = 16
) (
    input  logic clk,
    input  logic rst,
    input  logic btn,
    output logic pressed,
    output logic stable_out
);
    localparam CNTR_WIDTH = $clog2(DEBOUNCE_CYCLES);
    logic [CNTR_WIDTH-1:0] counter;
    logic btn_sync;
    logic btn_debounced;
    logic btn_debounced_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            btn_sync <= 1'b0;
            btn_debounced <= 1'b0;
            btn_debounced_prev <= 1'b0;
            counter <= '0;
            pressed <= 1'b0;
        end else begin
            // Step 1: Synchronize button input
            btn_sync <= btn;

            // Step 2: Debounce logic
            if (btn_sync == btn_debounced) begin
                counter <= '0;
            end else begin
                if (counter == DEBOUNCE_CYCLES - 1) begin
                    btn_debounced <= btn_sync;
                    counter <= '0;
                end else begin
                    counter <= counter + 1'b1;
                end
            end

            // Step 3: Edge detection
            btn_debounced_prev <= btn_debounced;
            pressed <= btn_debounced && !btn_debounced_prev;
        end
    end

    assign stable_out = btn_debounced;
endmodule

// ============================================================================
// TOP LEVEL - Simple Super Counter
// ============================================================================
module super_counter_simple (
    // Clock - FPGA-realistic name for yosys2digitaljs auto-conversion
    input  logic clk_12m,

    // Control (active-high for easier testing)
    input  logic rst,
    input  logic btn_press,

    // Outputs
    output logic [15:0] btn_count,      // Button press counter
    output logic        btn_debounced,  // Debounced button state
    output logic        led             // LED pulse on press
);
    // Debouncer
    logic btn_pressed;
    logic btn_stable;

    debouncer #(
        .DEBOUNCE_CYCLES(16)
    ) debouncer_inst (
        .clk(clk_12m),
        .rst(rst),
        .btn(btn_press),
        .pressed(btn_pressed),
        .stable_out(btn_stable)
    );

    assign btn_debounced = btn_stable;

    // Simple counter - increments on button press
    logic [15:0] counter;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            counter <= 16'd0;
        end else if (btn_pressed) begin
            counter <= counter + 16'd1;
        end
    end

    assign btn_count = counter;

    // LED pulse - stays on for 100 cycles after button press
    logic [7:0] led_counter;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            led_counter <= 8'd0;
            led <= 1'b0;
        end else begin
            if (btn_pressed) begin
                led_counter <= 8'd100;
                led <= 1'b1;
            end else if (led_counter != 8'd0) begin
                led_counter <= led_counter - 8'd1;
                led <= 1'b1;
            end else begin
                led <= 1'b0;
            end
        end
    end

endmodule
