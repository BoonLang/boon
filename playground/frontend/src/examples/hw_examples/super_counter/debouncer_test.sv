// Minimal debouncer test for DigitalJS
// 2‑FF sync, 2‑cycle debounce, single edge counter

module debouncer_test (
    input  logic clk_12m,
    input  logic rst,
    input  logic btn_press,
    output logic btn_debounced,
    output logic btn_pulse,
    output logic [7:0] pulse_count
);
    // small POR (4 cycles) so startup is defined
    logic [1:0] por = 2'b00;
    logic por_active = 1'b1;
    always_ff @(posedge clk_12m) begin
        if (por_active) begin
            por <= por + 2'd1;
            if (&por) por_active <= 1'b0;
        end
    end
    wire rst_i = rst | por_active;

    // debounce + one-cycle pulse on a clean rising edge
    logic pulse;
    debouncer #(.DEBOUNCE_CYCLES(2)) db (
        .clk(clk_12m), .rst(rst_i), .btn(btn_press),
        .pressed(pulse), .stable_out(btn_debounced)
    );

    // count pulses directly (already single-cycle)
    always_ff @(posedge clk_12m or posedge rst_i) begin
        if (rst_i)
            pulse_count <= 8'd0;
        else if (pulse)
            pulse_count <= pulse_count + 8'd1;
    end

    assign btn_pulse = pulse;
endmodule

// 2‑FF sync, debounce counter, rising-edge pulse
module debouncer #(
    parameter int DEBOUNCE_CYCLES = 2
) (
    input  logic clk,
    input  logic rst,
    input  logic btn,
    output logic pressed,
    output logic stable_out
);
    localparam int W = (DEBOUNCE_CYCLES <= 1) ? 1 : $clog2(DEBOUNCE_CYCLES);
    localparam [W-1:0] MAX = W'(DEBOUNCE_CYCLES-1);

    logic s0, s1;
    logic [W-1:0] cnt;
    logic stable;

    always_ff @(posedge clk or posedge rst) begin
        if (rst) begin
            s0 <= 1'b0;
            s1 <= 1'b0;
            cnt <= '0;
            stable <= 1'b0;
            pressed <= 1'b0;
        end else begin
            s0 <= btn;
            s1 <= s0;
            pressed <= 1'b0;  // default

            if (s1 == stable) begin
                cnt <= '0;
            end else if (cnt == MAX) begin
                stable <= s1;
                cnt <= '0;
                if (s1)         // pulse only on rising edge
                    pressed <= 1'b1;
            end else begin
                cnt <= cnt + 1'b1;
            end
        end
    end

    assign stable_out = stable;
endmodule
