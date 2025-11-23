// Mechanical button debouncer with one-cycle pulse output on press.
module debouncer #(
    parameter integer CNTR_WIDTH = 20  // covers ~1 ms @ 1 MHz (adjust in top level)
) (
    input  wire clk,
    input  wire rst,
    input  wire btn_n,
    output reg  pressed
);
    reg [CNTR_WIDTH-1:0] counter = {CNTR_WIDTH{1'b0}};
    reg sync_0 = 1'b1;
    reg sync_1 = 1'b1;
    reg stable = 1'b1;

    wire btn = ~sync_1; // active high after sync

    always @(posedge clk) begin
        if (rst) begin
            sync_0 <= 1'b1;
            sync_1 <= 1'b1;
        end else begin
            sync_0 <= btn_n;
            sync_1 <= sync_0;
        end
    end

    always @(posedge clk) begin
        if (rst) begin
            counter <= {CNTR_WIDTH{1'b0}};
            stable  <= 1'b0;
            pressed <= 1'b0;
        end else begin
            pressed <= 1'b0;
            if (btn != stable) begin
                counter <= counter + 1'b1;
                if (&counter) begin
                    stable <= btn;
                    counter <= {CNTR_WIDTH{1'b0}};
                    if (btn) begin
                        pressed <= 1'b1;
                    end
                end
            end else begin
                counter <= {CNTR_WIDTH{1'b0}};
            end
        end
    end
endmodule
