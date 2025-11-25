/**
 * Debug DFF clock edge detection
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

const code = `
module counter_test (
    input  wire clk,
    input  wire rst,
    output wire [3:0] count
);
    reg [3:0] counter;
    assign count = counter;

    always @(posedge clk) begin
        if (rst)
            counter <= 4'd0;
        else
            counter <= counter + 4'd1;
    end
endmodule
`;

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug DFF clock edge detection\\n');

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

        // Get device info
        console.log('\\n2. Analyzing circuit structure...');
        const devices = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const result = {};

            for (const cell of cells) {
                const type = cell.get('type');
                const id = cell.id;

                if (type === 'Clock') {
                    result.clock = {
                        id,
                        propagation: cell.get('propagation'),
                        outputSignals: JSON.stringify(cell.get('outputSignals'))
                    };
                }
                if (type === 'Dff') {
                    result.dff = {
                        id,
                        polarity: JSON.stringify(cell.get('polarity')),
                        last_clk: cell.last_clk,
                        propagation: cell.get('propagation'),
                        inputSignals: JSON.stringify(cell.get('inputSignals')),
                        outputSignals: JSON.stringify(cell.get('outputSignals'))
                    };
                }
            }

            result.tick = circuit.tick;
            result.hasPending = circuit._engine.hasPendingEvents;
            result.queueSize = circuit._engine._queue?.size || 0;
            result.pqSize = circuit._engine._pq?.size || 0;

            return result;
        });

        console.log('Tick:', devices.tick);
        console.log('hasPending:', devices.hasPending);
        console.log('queueSize:', devices.queueSize, 'pqSize:', devices.pqSize);
        console.log('\\nClock:', JSON.stringify(devices.clock, null, 2));
        console.log('\\nDFF:', JSON.stringify(devices.dff, null, 2));

        // Run ticks and track clock & DFF changes
        console.log('\\n3. Running simulation tick by tick...');

        for (let i = 0; i < 20; i++) {
            await page.evaluate(() => window.djCircuit.updateGates());

            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    const type = cell.get('type');
                    if (type === 'Clock') {
                        const out = cell.get('outputSignals').out;
                        result.clk = out ? (out._avec[0] & out._bvec[0] & 1) : null;
                    }
                    if (type === 'Dff') {
                        const inp = cell.get('inputSignals');
                        const out = cell.get('outputSignals');
                        result.dff_last_clk = cell.last_clk;
                        result.dff_clk = inp.clk ? (inp.clk._avec[0] & inp.clk._bvec[0] & 1) : null;

                        // Get D input value
                        if (inp.in) {
                            let val = 0;
                            for (let b = 0; b < inp.in._bits; b++) {
                                if ((inp.in._avec[b >> 5] >> (b & 31)) & (inp.in._bvec[b >> 5] >> (b & 31)) & 1) {
                                    val |= (1 << b);
                                }
                            }
                            result.dff_in = val;
                        }

                        // Get Q output value
                        if (out.out) {
                            let val = 0;
                            for (let b = 0; b < out.out._bits; b++) {
                                if ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1) {
                                    val |= (1 << b);
                                }
                            }
                            result.dff_out = val;
                        }
                    }
                }
                return result;
            });

            console.log(`Tick ${state.tick}: clk=${state.clk} last_clk=${state.dff_last_clk} dff_clk=${state.dff_clk} D=${state.dff_in} Q=${state.dff_out}`);
        }

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
