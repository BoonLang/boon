/**
 * Debug with observed graphs - to see actual worker state
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug With Observed Graphs\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        await page.evaluate(() => window.djCircuit.stop());

        // Observe main graph to get worker state updates
        console.log('\n2. Observing main graph...');
        await page.evaluate(() => {
            const circuit = window.djCircuit;
            circuit.observeGraph([]);  // Observe main graph
        });

        // Run simulation and check clock state
        console.log('\n3. Running 500 ticks with observation...');
        for (let i = 0; i < 500; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        const clockState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    const out = cell.get('outputSignals')?.out;
                    return {
                        tick: circuit.tick,
                        output: out ? (out._avec?.[0] & out._bvec?.[0]) : null
                    };
                }
            }
            return null;
        });
        console.log('Clock after 500 ticks:', JSON.stringify(clockState));

        // Run more ticks
        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        const clockState2 = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    const out = cell.get('outputSignals')?.out;
                    return {
                        tick: circuit.tick,
                        output: out ? (out._avec?.[0] & out._bvec?.[0]) : null
                    };
                }
            }
            return null;
        });
        console.log('Clock after 600 ticks:', JSON.stringify(clockState2));

        // Check uart_tx state
        const uartState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    return {
                        input: Object.fromEntries(
                            Object.entries(cell.get('inputSignals') || {}).map(([k, v]) =>
                                [k, v ? (v._avec?.[0] & v._bvec?.[0]) : null])
                        ),
                        output: Object.fromEntries(
                            Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                [k, v ? (v._avec?.[0] & v._bvec?.[0]) : null])
                        )
                    };
                }
            }
            return null;
        });
        console.log('uart_tx state:', JSON.stringify(uartState));

        // Also observe uart_tx subcircuit graph and check internal state
        console.log('\n4. Observing uart_tx subcircuit graph...');
        await page.evaluate(() => {
            const circuit = window.djCircuit;
            circuit.observeGraph(['u_uart_tx']);  // Observe uart_tx's internal graph
        });

        // Run more ticks
        for (let i = 0; i < 200; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        const internalState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    const subGraph = cell.get('graph');
                    const subCells = subGraph.getCells();

                    const inputs = [];
                    const dffs = [];

                    for (const sc of subCells) {
                        const type = sc.get('type');
                        if (type === 'Input') {
                            const out = sc.get('outputSignals')?.out;
                            inputs.push({
                                net: sc.get('net'),
                                value: out ? (out._avec?.[0] & out._bvec?.[0]) : null
                            });
                        }
                        if (type === 'Dff') {
                            const out = sc.get('outputSignals')?.out;
                            dffs.push({
                                id: sc.id,
                                bits: out?._bits,
                                value: out ? (out._avec?.[0] & out._bvec?.[0]) : null,
                                lastClk: sc.last_clk
                            });
                        }
                    }

                    return {
                        tick: circuit.tick,
                        inputs,
                        dffs
                    };
                }
            }
            return null;
        });
        console.log('\nInternal state after observation:');
        console.log(JSON.stringify(internalState, null, 2));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
