// Testbench for super_counter_fast
// Simulates button press and release to debug the counter behavior

`timescale 1ns/1ps

module tb_super_counter_fast;
    // Clock and reset
    logic clk_12m;
    logic rst;

    // Inputs
    logic btn_press;
    logic uart_rx;

    // Outputs
    logic uart_tx;
    logic led;
    logic [15:0] btn_count;
    logic btn_debounced;
    logic tx_busy;
    logic rx_valid;

    // Instantiate DUT
    super_counter_fast #(
        .CLOCK_HZ(12_000_000),
        .BAUD(115_200),
        .DEBOUNCE_CYCLES(16)
    ) dut (
        .*
    );

    // Clock generation - 12 MHz = 83.33ns period
    initial begin
        clk_12m = 0;
        forever #41.67 clk_12m = ~clk_12m;  // 83.33ns period
    end

    // Test stimulus
    initial begin
        $dumpfile("super_counter_fast.vcd");
        $dumpvars(0, tb_super_counter_fast);

        // Initialize
        rst = 1;
        btn_press = 0;
        uart_rx = 1;  // UART idle high

        // Reset
        repeat(5) @(posedge clk_12m);
        rst = 0;

        $display("=== Test Start ===");
        $display("Time: %0t - Reset released", $time);

        // Wait a bit
        repeat(10) @(posedge clk_12m);

        // Press button
        $display("Time: %0t - Button pressed", $time);
        btn_press = 1;

        // Hold button for longer than debounce time (16 cycles + margin)
        repeat(50) @(posedge clk_12m);

        $display("Time: %0t - btn_count=%0d, btn_debounced=%b, tx_busy=%b",
                 $time, btn_count, btn_debounced, tx_busy);

        // Release button
        $display("Time: %0t - Button released", $time);
        btn_press = 0;

        // Hold released for debounce time
        repeat(50) @(posedge clk_12m);

        $display("Time: %0t - btn_count=%0d, btn_debounced=%b, tx_busy=%b",
                 $time, btn_count, btn_debounced, tx_busy);

        // Press again
        $display("Time: %0t - Button pressed again", $time);
        btn_press = 1;
        repeat(50) @(posedge clk_12m);

        $display("Time: %0t - btn_count=%0d, btn_debounced=%b, tx_busy=%b",
                 $time, btn_count, btn_debounced, tx_busy);

        // Release again
        $display("Time: %0t - Button released again", $time);
        btn_press = 0;
        repeat(50) @(posedge clk_12m);

        $display("Time: %0t - btn_count=%0d, btn_debounced=%b, tx_busy=%b",
                 $time, btn_count, btn_debounced, tx_busy);

        // Let UART finish transmitting
        $display("Time: %0t - Waiting for UART TX to complete...", $time);
        repeat(20000) @(posedge clk_12m);  // Wait for UART

        $display("Time: %0t - Final: btn_count=%0d, btn_debounced=%b, tx_busy=%b",
                 $time, btn_count, btn_debounced, tx_busy);

        $display("=== Test Complete ===");
        $finish;
    end

    // Monitor for debugging
    initial begin
        $monitor("Time=%0t clk=%b rst=%b btn_press=%b btn_debounced=%b btn_count=%0d tx_busy=%b uart_tx=%b",
                 $time, clk_12m, rst, btn_press, btn_debounced, btn_count, tx_busy, uart_tx);
    end

    // Watchdog timeout
    initial begin
        #100000000;  // 100ms
        $display("ERROR: Simulation timeout!");
        $finish;
    end

endmodule
