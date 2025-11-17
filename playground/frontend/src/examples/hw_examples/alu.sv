// Arithmetic Logic Unit (ALU)
module alu(
    input [7:0] a,
    input [7:0] b,
    input [2:0] op,
    output logic [7:0] result
);

  // ALU operation codes
  localparam ADD = 3'd0;
  localparam SUB = 3'd1;
  localparam AND = 3'd2;
  localparam OR  = 3'd3;
  localparam XOR = 3'd4;
  localparam SHL = 3'd5;
  localparam SHR = 3'd6;
  localparam EQ  = 3'd7;

  always_comb begin
    case (op)
      ADD: result = a + b;
      SUB: result = a - b;
      AND: result = a & b;
      OR:  result = a | b;
      XOR: result = a ^ b;
      SHL: result = a << b[2:0];
      SHR: result = a >> b[2:0];
      EQ:  result = (a == b) ? 8'hFF : 8'h00;
      default: result = 8'h00;
    endcase
  end

endmodule
