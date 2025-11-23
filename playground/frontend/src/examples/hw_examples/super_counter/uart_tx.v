// Simple UART transmitter (8N1)
module uart_tx #(
    parameter integer CLOCK_HZ = 25_000_000,
    parameter integer BAUD = 115_200
) (
    input  wire clk,
    input  wire rst,
    input  wire [7:0] data,
    input  wire start,
    output reg  busy,
    output reg  serial_out
);
    localparam integer DIVISOR = CLOCK_HZ / BAUD;
    localparam integer CTR_WIDTH = $clog2(DIVISOR);
    localparam [CTR_WIDTH-1:0] DIVISOR_COUNT = DIVISOR[CTR_WIDTH-1:0];

    reg [CTR_WIDTH-1:0] baud_cnt = {CTR_WIDTH{1'b0}};
    reg [3:0] bit_idx = 4'd0;
    reg [9:0] shifter = 10'h3FF;

    wire baud_tick = (baud_cnt == 0);

    always @(posedge clk) begin
        if (rst) begin
            baud_cnt <= DIVISOR_COUNT - 1'b1;
        end else if (busy) begin
            if (baud_cnt == 0) begin
                baud_cnt <= DIVISOR_COUNT - 1'b1;
            end else begin
                baud_cnt <= baud_cnt - 1'b1;
            end
        end else begin
            baud_cnt <= DIVISOR_COUNT - 1'b1;
        end
    end

    always @(posedge clk) begin
        if (rst) begin
            busy <= 1'b0;
            bit_idx <= 4'd0;
            shifter <= 10'h3FF;
            serial_out <= 1'b1;
        end else begin
            if (!busy) begin
                serial_out <= 1'b1;
                if (start) begin
                    busy <= 1'b1;
                    bit_idx <= 4'd0;
                    shifter <= {1'b1, data, 1'b0}; // stop, data, start
                end
            end else if (baud_tick) begin
                serial_out <= shifter[0];
                shifter <= {1'b1, shifter[9:1]};
                bit_idx <= bit_idx + 1'b1;
                if (bit_idx == 4'd9) begin
                    busy <= 1'b0;
                end
            end
        end
    end
endmodule
