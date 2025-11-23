// Simple UART receiver (8N1)
module uart_rx #(
    parameter integer CLOCK_HZ = 25_000_000,
    parameter integer BAUD = 115_200
) (
    input  wire clk,
    input  wire rst,
    input  wire serial_in,
    output reg  [7:0] data,
    output reg  valid
);
    localparam integer DIVISOR = CLOCK_HZ / BAUD;
    localparam integer CTR_WIDTH = $clog2(DIVISOR);
    localparam [CTR_WIDTH-1:0] DIVISOR_COUNT = DIVISOR[CTR_WIDTH-1:0];

    reg [CTR_WIDTH-1:0] baud_cnt = {CTR_WIDTH{1'b0}};
    reg [3:0] bit_idx = 4'd0;
    reg [7:0] shift = 8'h00;
    reg        busy = 1'b0;
    reg        serial_sync0 = 1'b1;
    reg        serial_sync1 = 1'b1;

    wire serial = serial_sync1;

    always @(posedge clk) begin
        if (rst) begin
            serial_sync0 <= 1'b1;
            serial_sync1 <= 1'b1;
        end else begin
            serial_sync0 <= serial_in;
            serial_sync1 <= serial_sync0;
        end
    end

    always @(posedge clk) begin
        if (rst) begin
            busy <= 1'b0;
            baud_cnt <= {CTR_WIDTH{1'b0}};
            bit_idx <= 4'd0;
            shift <= 8'h00;
            data <= 8'h00;
            valid <= 1'b0;
        end else begin
            valid <= 1'b0;
            if (!busy) begin
                if (!serial) begin
                    busy <= 1'b1;
                    baud_cnt <= (DIVISOR_COUNT >> 1);
                    bit_idx <= 4'd0;
                end
            end else begin
                if (baud_cnt == 0) begin
                    baud_cnt <= DIVISOR_COUNT - 1'b1;
                    if (bit_idx < 8) begin
                        shift[bit_idx[2:0]] <= serial;
                        bit_idx <= bit_idx + 1'b1;
                    end else begin
                        // stop bit check
                        if (serial) begin
                            data <= shift;
                            valid <= 1'b1;
                        end
                        busy <= 1'b0;
                    end
                end else begin
                    baud_cnt <= baud_cnt - 1'b1;
                end
            end
        end
    end
endmodule
