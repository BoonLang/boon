/**
 * Test UART TX directly without btn_message
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test Direct UART TX\n');

    // Create minimal test circuit: button -> uart_tx directly
    const testCode = `
module test_uart(
    input wire clk,
    input wire rst,
    input wire btn_n,
    output wire tx
);
    // Direct edge detector
    wire btn = ~btn_n;
    reg btn_prev;
    wire btn_edge = btn & ~btn_prev;

    always @(posedge clk) begin
        if (rst)
            btn_prev <= 1'b0;
        else
            btn_prev <= btn;
    end

    // UART TX
    localparam DIVISOR = 10;  // 10 cycles per bit

    reg [7:0] shifter;
    reg [3:0] bit_cnt;
    reg [3:0] baud_cnt;
    reg busy;
    reg tx_out;

    assign tx = tx_out;

    always @(posedge clk) begin
        if (rst) begin
            shifter <= 8'h41;  // 'A'
            bit_cnt <= 0;
            baud_cnt <= DIVISOR - 1;
            busy <= 0;
            tx_out <= 1;
        end else begin
            if (!busy) begin
                tx_out <= 1;
                if (btn_edge) begin
                    busy <= 1;
                    shifter <= 8'h41;  // 'A'
                    bit_cnt <= 0;
                    baud_cnt <= DIVISOR - 1;
                end
            end else begin
                if (baud_cnt == 0) begin
                    baud_cnt <= DIVISOR - 1;
                    if (bit_cnt == 0) begin
                        tx_out <= 0;  // Start bit
                        bit_cnt <= 1;
                    end else if (bit_cnt <= 8) begin
                        tx_out <= shifter[0];
                        shifter <= {1'b1, shifter[7:1]};
                        bit_cnt <= bit_cnt + 1;
                    end else if (bit_cnt == 9) begin
                        tx_out <= 1;  // Stop bit
                        bit_cnt <= 10;
                    end else begin
                        busy <= 0;
                    end
                end else begin
                    baud_cnt <= baud_cnt - 1;
                end
            end
        end
    end
endmodule
`;

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(BASE_URL, { waitUntil: 'networkidle' });
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe').forEach(el => el.remove());
        });
        await sleep(500);

        // Set the code
        await page.evaluate((code) => {
            const cmElement = document.querySelector('.CodeMirror');
            if (cmElement && cmElement.CodeMirror) {
                cmElement.CodeMirror.setValue(code);
                console.log('Set code via CodeMirror, length:', code.length);
            } else {
                const textarea = document.getElementById('code');
                if (textarea) {
                    textarea.value = code;
                    console.log('Set code via textarea');
                } else {
                    console.log('Could not find editor');
                }
            }
        }, testCode);
        await sleep(500);

        // Verify code was set
        const codeLength = await page.evaluate(() => {
            const cmElement = document.querySelector('.CodeMirror');
            if (cmElement && cmElement.CodeMirror) {
                return cmElement.CodeMirror.getValue().length;
            }
            return 0;
        });
        console.log('Code set, length:', codeLength);

        console.log('1. Synthesizing direct UART test...');
        await page.click('#synthesize-btn');

        // Wait for either success or error
        try {
            await page.waitForSelector('#paper svg', { timeout: 30000 });
        } catch (e) {
            // Check for error message
            const errorText = await page.evaluate(() => {
                const alert = document.querySelector('.alert-danger');
                return alert ? alert.textContent : null;
            });
            if (errorText) {
                console.log('Synthesis error:', errorText);
                return;
            }
            throw e;
        }
        await sleep(2000);

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Go to I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('Found', checkboxes.length, 'checkboxes');

        // Reset sequence
        console.log('\n2. Reset sequence...');

        // btn_n HIGH first
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(100);
        }
        console.log('   btn_n = HIGH');

        // rst HIGH (active)
        if (!await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(100);
        }
        console.log('   rst = HIGH (reset active)');

        // Run with reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset
        await checkboxes[0].click();
        console.log('   rst = LOW (reset released)');

        // Run after reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Check TX value
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('net') === 'tx') {
                    const inp = cell.get('inputSignals')?.in;
                    return {
                        tick: circuit.tick,
                        tx: inp?._avec?.[0] === 1 && inp?._bvec?.[0] === 1 ? 1 : 0
                    };
                }
            }
            return { tick: circuit.tick, tx: 'not found' };
        });
        console.log(`   After reset: tick=${state.tick} tx=${state.tx}`);

        // Press button
        console.log('\n3. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(100);

        // Run and monitor
        console.log('4. Running and monitoring tx output...');
        await page.evaluate(() => window.djCircuit.start());

        let txLowSeen = false;
        for (let i = 0; i < 50; i++) {
            await sleep(50);
            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                for (const cell of cells) {
                    if (cell.get('net') === 'tx') {
                        const inp = cell.get('inputSignals')?.in;
                        return {
                            tick: circuit.tick,
                            tx: inp?._avec?.[0] === 1 && inp?._bvec?.[0] === 1 ? 1 : 0
                        };
                    }
                }
                return { tick: circuit.tick, tx: -1 };
            });

            if (i % 10 === 0 || state.tx === 0) {
                console.log(`   [${i*50}ms] tick=${state.tick} tx=${state.tx}`);
            }
            if (state.tx === 0) txLowSeen = true;
        }

        await page.evaluate(() => window.djCircuit.stop());

        if (txLowSeen) {
            console.log('\nSUCCESS: TX went LOW (start bit transmitted)!');
        } else {
            console.log('\nFAILED: TX never went LOW');
        }

        await page.screenshot({ path: 'test/test-direct-uart-result.png', fullPage: true });
        await sleep(3000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
