/**
 * Verify counter feedback loop fix
 * Test that counters actually count through multiple values
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

const code = `
// Counter test
module counter_test (
    input  wire clk,
    input  wire rst,
    output wire [7:0] count
);
    reg [7:0] counter;
    assign count = counter;

    always @(posedge clk) begin
        if (rst)
            counter <= 8'd0;
        else
            counter <= counter + 8'd1;
    end
endmodule
`;

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Verify counter feedback loop fix\\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        // Load our counter code
        await page.evaluate((code) => {
            const cmElement = document.querySelector('.CodeMirror');
            if (cmElement && cmElement.CodeMirror) {
                cmElement.CodeMirror.setValue(code);
            }
        }, code);
        await sleep(500);

        console.log('1. Synthesizing counter...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 30000 });
        await sleep(2000);

        // Stop and get initial state
        await page.evaluate(() => window.djCircuit.stop());

        // Run simulation and collect counter values
        console.log('\\n2. Running simulation...');
        const values = [];

        for (let i = 0; i < 30; i++) {
            await page.evaluate(() => window.djCircuit.updateGates());

            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                for (const cell of cells) {
                    if (cell.get('type') === 'Dff') {
                        const out = cell.get('outputSignals');
                        if (out.out) {
                            let val = 0;
                            for (let b = 0; b < Math.min(8, out.out._bits); b++) {
                                if ((out.out._avec[b >> 5] >> (b & 31)) & 1) {
                                    val |= (1 << b);
                                }
                            }
                            return { tick: circuit.tick, count: val };
                        }
                    }
                }
                return { tick: circuit.tick, count: null };
            });

            values.push(state.count);
        }

        // Analyze results
        const uniqueValues = [...new Set(values)];
        const maxValue = Math.max(...values);
        const minValue = Math.min(...values);

        console.log('Counter values over 30 ticks:', values.join(', '));
        console.log('Unique values:', uniqueValues.length);
        console.log('Min value:', minValue);
        console.log('Max value:', maxValue);

        console.log('\\n3. Result:');
        if (uniqueValues.length > 3 && maxValue > minValue) {
            console.log('✓ SUCCESS: Counter is properly incrementing!');
            console.log('  Counter went from', minValue, 'to', maxValue);
        } else {
            console.log('✗ FAILED: Counter not incrementing properly');
        }

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
