module debouncer(
    input wire clk,
    input wire rst,
    input wire btn_n,
    output reg pressed
);
    wire btn;
    assign btn = ~btn_n;

    reg [3:0] counter;
    reg stable;

    always @(posedge clk) begin
        if (rst) begin
            counter <= 0;
            stable <= 0;
            pressed <= 0;
        end else begin
            pressed <= 0;
            if (btn != stable) begin
                if (counter == 1) begin
                    stable <= btn;
                    counter <= 0;
                    if (btn)
                        pressed <= 1;
                end else begin
                    counter <= counter + 1;
                end
            end else begin
                counter <= 0;
            end
        end
    end
endmodule
