/**
 * Test counter with fast clock (propagation=1)
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
    console.log('Test counter with fast clock\\n');

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

        // Set clock to fast (propagation=1)
        console.log('2. Setting clock to fast (propagation=1)...');
        await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    cell.set('propagation', 1);
                    console.log('Clock propagation set to 1');
                }
            }
        });

        // Run simulation and collect counter values
        console.log('\\n3. Running simulation...');
        const values = [];

        for (let i = 0; i < 50; i++) {
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
                                if ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1) {
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
            if (i < 20 || i % 10 === 0) {
                console.log(`Tick ${state.tick}: count=${state.count}`);
            }
        }

        // Analyze results
        const uniqueValues = [...new Set(values)];
        const maxValue = Math.max(...values);
        const minValue = Math.min(...values);

        console.log('\\n4. Summary:');
        console.log('Unique values:', uniqueValues.sort((a,b) => a-b).join(', '));
        console.log('Min:', minValue, 'Max:', maxValue);

        if (uniqueValues.length >= 5 && maxValue > minValue + 3) {
            console.log('\\n✓ SUCCESS: Counter is properly incrementing!');
        } else {
            console.log('\\n✗ FAILED: Counter not incrementing properly');
        }

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
