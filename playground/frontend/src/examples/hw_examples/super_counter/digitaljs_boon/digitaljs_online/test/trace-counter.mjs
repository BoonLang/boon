/**
 * Trace counter simulation tick by tick
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

const code = `
// Minimal counter test - 4-bit counter
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
    console.log('Trace counter simulation\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    page.on('console', msg => {
        if (msg.type() === 'log' || msg.type() === 'error') {
            console.log('BROWSER:', msg.text());
        }
    });

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

        try {
            await page.waitForSelector('#paper svg', { timeout: 30000 });
        } catch (e) {
            const errorText = await page.evaluate(() => {
                return document.querySelector('.alert-danger')?.textContent || null;
            });
            if (errorText) {
                console.log('Synthesis error:', errorText);
                await browser.close();
                return;
            }
            throw e;
        }
        await sleep(2000);

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Get initial state and trace signals
        console.log('\n2. Initial state:');
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const result = { tick: circuit.tick, devices: {} };

            for (const cell of cells) {
                const type = cell.get('type');
                const id = cell.id;

                if (type === 'Dff') {
                    const inp = cell.get('inputSignals');
                    const out = cell.get('outputSignals');
                    result.devices[id] = {
                        type: 'Dff',
                        label: cell.get('label'),
                        in: inp.in ? {
                            bits: inp.in._bits,
                            avec: inp.in._avec?.slice(0, 4),
                            bvec: inp.in._bvec?.slice(0, 4)
                        } : null,
                        out: out.out ? {
                            bits: out.out._bits,
                            avec: out.out._avec?.slice(0, 4),
                            bvec: out.out._bvec?.slice(0, 4)
                        } : null
                    };
                }
                if (type === 'Addition') {
                    const inp = cell.get('inputSignals');
                    const out = cell.get('outputSignals');
                    result.devices[id] = {
                        type: 'Addition',
                        in1: inp.in1 ? {
                            bits: inp.in1._bits,
                            avec: inp.in1._avec?.slice(0, 4),
                            bvec: inp.in1._bvec?.slice(0, 4)
                        } : null,
                        in2: inp.in2 ? {
                            bits: inp.in2._bits,
                            avec: inp.in2._avec?.slice(0, 4),
                            bvec: inp.in2._bvec?.slice(0, 4)
                        } : null,
                        out: out.out ? {
                            bits: out.out._bits,
                            avec: out.out._avec?.slice(0, 4),
                            bvec: out.out._bvec?.slice(0, 4)
                        } : null
                    };
                }
                if (type === 'Mux') {
                    const inp = cell.get('inputSignals');
                    const out = cell.get('outputSignals');
                    result.devices[id] = {
                        type: 'Mux',
                        sel: inp.sel ? {
                            bits: inp.sel._bits,
                            avec: inp.sel._avec?.slice(0, 1),
                            bvec: inp.sel._bvec?.slice(0, 1)
                        } : null,
                        in0: inp.in0 ? {
                            bits: inp.in0._bits,
                            avec: inp.in0._avec?.slice(0, 4),
                            bvec: inp.in0._bvec?.slice(0, 4)
                        } : null,
                        in1: inp.in1 ? {
                            bits: inp.in1._bits,
                            avec: inp.in1._avec?.slice(0, 4),
                            bvec: inp.in1._bvec?.slice(0, 4)
                        } : null,
                        out: out.out ? {
                            bits: out.out._bits,
                            avec: out.out._avec?.slice(0, 4),
                            bvec: out.out._bvec?.slice(0, 4)
                        } : null
                    };
                }
            }
            return result;
        });

        console.log('Tick:', state.tick);
        for (const [id, dev] of Object.entries(state.devices)) {
            console.log(`  ${id} (${dev.type}):`);
            for (const [port, sig] of Object.entries(dev)) {
                if (port !== 'type' && port !== 'label' && sig) {
                    console.log(`    ${port}: avec=${JSON.stringify(sig.avec)}, bvec=${JSON.stringify(sig.bvec)}`);
                }
            }
        }

        // Run simulation tick by tick
        console.log('\n3. Running simulation tick by tick...');

        for (let i = 0; i < 10; i++) {
            await page.evaluate(() => window.djCircuit.updateGates());

            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    if (cell.get('type') === 'Dff') {
                        const out = cell.get('outputSignals');
                        if (out.out) {
                            // Convert to number
                            let val = 0;
                            for (let b = 0; b < out.out._bits; b++) {
                                if ((out.out._avec[b >> 5] >> (b & 31)) & 1) {
                                    val |= (1 << b);
                                }
                            }
                            result.dff_out = val;
                        }
                    }
                    if (cell.get('type') === 'Addition') {
                        const out = cell.get('outputSignals');
                        if (out.out) {
                            let val = 0;
                            for (let b = 0; b < out.out._bits; b++) {
                                if ((out.out._avec[b >> 5] >> (b & 31)) & 1) {
                                    val |= (1 << b);
                                }
                            }
                            result.add_out = val;
                        }
                    }
                    if (cell.get('type') === 'Mux') {
                        const inp = cell.get('inputSignals');
                        const out = cell.get('outputSignals');
                        result.mux_sel = inp.sel ? (inp.sel._avec[0] & 1) : null;
                        if (out.out) {
                            let val = 0;
                            for (let b = 0; b < out.out._bits; b++) {
                                if ((out.out._avec[b >> 5] >> (b & 31)) & 1) {
                                    val |= (1 << b);
                                }
                            }
                            result.mux_out = val;
                        }
                    }
                }
                return result;
            });

            console.log(`Tick ${state.tick}: dff_out=${state.dff_out}, add_out=${state.add_out}, mux_sel=${state.mux_sel}, mux_out=${state.mux_out}`);
        }

        // Check if counter incremented
        console.log('\n4. Result:');
        if (state.dff_out > 0) {
            console.log('SUCCESS: Counter is incrementing!');
        } else {
            console.log('FAILED: Counter stuck at 0');

            // Debug - check queue
            const queueInfo = await page.evaluate(() => {
                const engine = window.djCircuit._engine;
                return {
                    queueSize: engine._queue?.size || 0,
                    pqSize: engine._pq?.size || 0,
                    hasPending: engine.hasPendingEvents
                };
            });
            console.log('Queue info:', queueInfo);
        }

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
