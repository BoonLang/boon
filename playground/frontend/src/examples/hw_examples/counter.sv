// Loadable up/down counter
module counter(
    input clk,
    input rst,
    input load,
    input [7:0] load_value,
    input up,
    input en,
    output logic [7:0] count
);

  always_ff @(posedge clk or posedge rst)
    if (rst)
      count <= 8'd0;
    else if (en)
      if (load)
        count <= load_value;
      else
        if (up)
          count <= count + 1;
        else
          count <= count - 1;

endmodule
