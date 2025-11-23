// Counter with debug debouncer - shows stable signal instead of pulse
// This lets you see the debouncer working

module debouncer_debug #(
    parameter int CNTR_WIDTH = 3
) (
    input  logic clk,
    input  logic rst,
    input  logic btn_n,
    output logic pressed,      // Original pulse output
    output logic btn_stable    // DEBUG: shows stable signal
);
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
    logic [CNTR_WIDTH-1:0] counter;
    logic stable;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= '0;
            stable  <= 1'b0;
        end else begin
            if (btn != stable) begin
                if (counter == {CNTR_WIDTH{1'b1}}) begin
                    stable  <= btn;
                    counter <= '0;
                end else begin
                    counter <= counter + 1'b1;
                end
            end else begin
                counter <= '0;
            end
        end
    end

    logic stable_prev;

    always_ff @(posedge clk) begin
        if (rst) begin
            stable_prev <= 1'b0;
            pressed     <= 1'b0;
        end else begin
            stable_prev <= stable;
            pressed     <= stable && !stable_prev;
        end
    end

    assign btn_stable = stable;  // DEBUG: expose stable signal
endmodule


module counter_with_debug_debouncer (
    input  logic clk,
    input  logic rst,
    input  logic btn_press,
    output logic [7:0] count,
    output logic btn_stable,    // DEBUG: watch this!
    output logic btn_pulse,     // DEBUG: 1-cycle pulse
    output logic led
);
    logic btn_press_n = ~btn_press;
    logic [7:0] counter;

    // Debug debouncer
    debouncer_debug #(
        .CNTR_WIDTH(3)
    ) deb (
        .clk(clk),
        .rst(rst),
        .btn_n(btn_press_n),
        .pressed(btn_pulse),
        .btn_stable(btn_stable)
    );

    // Counter increments on pulse
    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= 8'd0;
        end else if (btn_pulse) begin
            counter <= counter + 8'd1;
        end
    end

    assign count = counter;

    // LED follows pulse
    always_ff @(posedge clk) begin
        if (rst) begin
            led <= 1'b0;
        end else begin
            led <= btn_pulse;
        end
    end

endmodule
