/**
 * Test counter with continuous simulation
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

const code = `
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
    console.log('Test counter with continuous simulation\\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        await page.evaluate((code) => {
            const cmElement = document.querySelector('.CodeMirror');
            if (cmElement && cmElement.CodeMirror) {
                cmElement.CodeMirror.setValue(code);
            }
        }, code);
        await sleep(500);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 30000 });
        await sleep(2000);

        await page.evaluate(() => window.djCircuit.stop());

        // Set clock to fast (propagation=10)
        console.log('2. Setting clock propagation=10...');
        await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    cell.set('propagation', 10);
                }
            }
        });

        // Get initial count
        let initial = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('type') === 'Dff') {
                    const out = cell.get('outputSignals');
                    if (out.out) {
                        let val = 0;
                        for (let b = 0; b < Math.min(8, out.out._bits); b++) {
                            if ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1) {
                                val |= (1 << b);
                            }
                        }
                        return val;
                    }
                }
            }
            return null;
        });
        console.log('Initial count:', initial);

        // Run simulation continuously for 2 seconds
        console.log('\\n3. Running continuous simulation for 2 seconds...');
        await page.evaluate(() => window.djCircuit.start());
        await sleep(2000);
        await page.evaluate(() => window.djCircuit.stop());

        // Get final count
        let final = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('type') === 'Dff') {
                    const out = cell.get('outputSignals');
                    if (out.out) {
                        let val = 0;
                        for (let b = 0; b < Math.min(8, out.out._bits); b++) {
                            if ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1) {
                                val |= (1 << b);
                            }
                        }
                        return { val, tick: circuit.tick };
                    }
                }
            }
            return null;
        });
        console.log('Final count:', final.val, 'at tick', final.tick);

        console.log('\\n4. Result:');
        if (final.val > initial + 5) {
            console.log('✓ SUCCESS: Counter incremented from', initial, 'to', final.val);
            console.log('  Counter feedback loop is WORKING!');
        } else {
            console.log('✗ FAILED: Counter only went from', initial, 'to', final.val);
        }

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
