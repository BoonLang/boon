// Flat test - no subcircuits, everything inline
// Tests if counters work at the top level

module flat_test (
    input  wire clk,
    input  wire rst,
    input  wire btn_n,
    output wire tx
);
    // Button edge detector
    wire btn = ~btn_n;
    reg btn_prev;
    wire btn_edge = btn & ~btn_prev;

    always @(posedge clk) begin
        if (rst)
            btn_prev <= 1'b0;
        else
            btn_prev <= btn;
    end

    // Main counter - increments when triggered
    reg [6:0] counter;
    reg busy;
    reg triggered;

    // TX output based on counter bit 3
    assign tx = busy ? ~counter[3] : 1'b1;

    always @(posedge clk) begin
        if (rst) begin
            counter   <= 7'd0;
            busy      <= 1'b0;
            triggered <= 1'b0;
        end else begin
            if (!busy) begin
                if (btn_edge && !triggered) begin
                    busy      <= 1'b1;
                    triggered <= 1'b1;
                    counter   <= 7'd0;
                end
            end else begin
                counter <= counter + 7'd1;
                if (counter >= 7'd100) begin
                    busy <= 1'b0;
                end
            end
        end
    end

endmodule
