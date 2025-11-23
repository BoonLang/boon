// Simple debouncer test - isolate the debouncer to verify it works

module debouncer_test (
    input  logic clk_12m,
    input  logic rst,
    input  logic btn_press,
    output logic btn_debounced,
    output logic btn_pulse,
    output logic [7:0] pulse_count
);
    // Debouncer instance
    logic pressed;
    logic stable;

    debouncer #(
        .DEBOUNCE_CYCLES(16)
    ) deb (
        .clk(clk_12m),
        .rst(rst),
        .btn(btn_press),
        .pressed(pressed),
        .stable_out(stable)
    );

    // Counter for pulses
    logic [7:0] counter;

    always_ff @(posedge clk_12m) begin
        if (rst) begin
            counter <= 8'd0;
        end else if (pressed) begin
            counter <= counter + 8'd1;
        end
    end

    assign btn_debounced = stable;
    assign btn_pulse = pressed;
    assign pulse_count = counter;

endmodule

// Debouncer module (copy from packed_super_counter_fast.sv)
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
