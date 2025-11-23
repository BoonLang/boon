// Super Counter - Test/Debug Version
// Optimized for DigitalJS testing with visible internal signals
//
// Key changes from original:
// - clk_12m (FPGA name) - tests our yosys2digitaljs modification!
// - Ultra-fast debounce (2 bits instead of 18)
// - Debug outputs to see internal counter values
// - Active-high signals for easier testing

module super_counter_test (
    // Clock - FPGA-realistic name!
    input  logic clk_12m,

    // Control (active-high for easier testing)
    input  logic rst,
    input  logic btn_press,

    // UART (for completeness, but won't fully test)
    input  logic uart_rx,
    output logic uart_tx,

    // LED output
    output logic led,

    // DEBUG outputs - show internal state!
    output logic btn_debounced,      // Shows debounced button state
    output logic [7:0] press_count   // Shows number of presses (low 8 bits)
);
    // Debouncer with ultra-fast timing
    logic btn_press_n = ~btn_press;
    logic btn_pressed;

    debouncer_simple #(
        .CNTR_WIDTH(8)  // 256 cycles debounce - more stable
    ) deb (
        .clk(clk_12m),
        .rst(rst),
        .btn_n(btn_press_n),
        .pressed(btn_pressed),
        .stable(btn_debounced)  // Debug output
    );

    // Press counter
    logic [15:0] counter;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            counter <= 16'd0;
        end else if (btn_pressed) begin
            counter <= counter + 16'd1;
        end
    end

    assign press_count = counter[7:0];  // Show low 8 bits

    // LED blinks on button press (hold for a few cycles)
    // With clock propagation=1 (very fast), use more cycles for visibility
    // Try different values: 50=fast, 100=medium, 200=slow
    localparam LED_HOLD_CYCLES = 100;  // Adjust this to tune LED timing!

    logic [7:0] led_timer;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            led_timer <= 8'd0;
            led <= 1'b0;
        end else begin
            if (btn_pressed) begin
                led_timer <= LED_HOLD_CYCLES;  // Hold for configured cycles
                led <= 1'b1;
            end else if (led_timer != 8'd0) begin
                led_timer <= led_timer - 8'd1;
                led <= 1'b1;
            end else begin
                led <= 1'b0;
            end
        end
    end

    // UART TX idle (always high)
    assign uart_tx = 1'b1;

endmodule

// Simple debouncer with debug output
module debouncer_simple #(
    parameter int CNTR_WIDTH = 2
) (
    input  logic clk,
    input  logic rst,
    input  logic btn_n,
    output logic pressed,
    output logic stable
);
    // CDC synchronizer
    logic sync_0, sync_1;

    always_ff @(posedge clk) begin
        if (rst) begin
            sync_0 <= 1'b1;
            sync_1 <= 1'b1;
        end else begin
            sync_0 <= btn_n;
            sync_1 <= sync_0;
        end
    end

    logic btn = ~sync_1;

    // Debounce counter
    logic [CNTR_WIDTH-1:0] counter;
    logic stable_reg;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= '0;
            stable_reg <= 1'b0;
        end else begin
            if (btn != stable_reg) begin
                if (counter == {CNTR_WIDTH{1'b1}}) begin
                    stable_reg <= btn;
                    counter <= '0;
                end else begin
                    counter <= counter + 1'b1;
                end
            end else begin
                counter <= '0;
            end
        end
    end

    assign stable = stable_reg;

    // Edge detection for pulse
    logic stable_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            stable_prev <= 1'b0;
            pressed <= 1'b0;
        end else begin
            stable_prev <= stable_reg;
            pressed <= stable_reg && !stable_prev;
        end
    end

endmodule
