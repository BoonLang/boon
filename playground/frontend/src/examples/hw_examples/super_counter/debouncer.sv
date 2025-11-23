// Mechanical Button Debouncer with CDC Synchronizer (SystemVerilog)
// Outputs a single-cycle pulse when a button press is detected
//
// Features:
// - 2-FF synchronizer for metastability protection
// - Counter-based debouncing
// - Single-cycle pulse output

module debouncer #(
    parameter int CNTR_WIDTH = 20  // Counter width (~1ms @ 1MHz)
) (
    input  logic clk,
    input  logic rst,
    input  logic btn_n,       // Active-low button (async)
    output logic pressed      // Single-cycle pulse
);
    // CDC Synchronizer (2-FF chain)
    logic sync_0, sync_1;
    always_ff @(posedge clk) begin
        if (rst) begin
            sync_0 <= 1'b1;
            sync_1 <= 1'b1;
        end else begin
            sync_0 <= btn_n;      // May go metastable
            sync_1 <= sync_0;      // Metastability resolved
        end
    end

    // Active-high button signal (safe to use)
    logic btn;
    assign btn = ~sync_1;

    // Debounce logic
    logic [CNTR_WIDTH-1:0] counter;
    logic stable;

    always_ff @(posedge clk) begin
        if (rst) begin
            counter <= '0;
            stable  <= 1'b0;
            pressed <= 1'b0;
        end else begin
            pressed <= 1'b0;  // Default: no pulse

            if (btn != stable) begin
                // Button state changed: increment counter
                counter <= counter + 1'b1;
                if (&counter) begin  // Counter maxed out
                    stable <= btn;
                    counter <= '0;
                    // Pulse only on transition to pressed
                    if (btn) begin
                        pressed <= 1'b1;
                    end
                end
            end else begin
                // Button state stable: reset counter
                counter <= '0;
            end
        end
    end
endmodule
