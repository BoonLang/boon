/**
 * Debug worker subcircuit handling
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug Worker Subcircuit Handling\n');

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

        // Check if subcircuit graphs were added
        console.log('\n2. Checking subcircuit graphs in JointJS model...');
        const graphInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const mainGraph = circuit._graph;
            const cells = mainGraph.getCells();
            const result = { mainGraphCid: mainGraph.cid, subcircuits: [] };

            for (const cell of cells) {
                if (cell.get('type') === 'Subcircuit') {
                    const subGraph = cell.get('graph');
                    result.subcircuits.push({
                        label: cell.get('label'),
                        celltype: cell.get('celltype'),
                        hasGraph: !!subGraph,
                        graphCid: subGraph?.cid,
                        iomap: cell.get('circuitIOmap'),
                        subCellCount: subGraph ? subGraph.getCells().length : 0
                    });
                }
            }
            return result;
        });
        console.log(JSON.stringify(graphInfo, null, 2));

        // Check if worker has the subcircuit graphs
        console.log('\n3. Checking if WorkerEngine has subcircuit graphs...');
        const workerInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const engine = circuit._engine;
            return {
                engineType: engine.constructor.name,
                graphsCount: Object.keys(engine._graphs || {}).length,
                graphCids: Object.keys(engine._graphs || {})
            };
        });
        console.log(JSON.stringify(workerInfo, null, 2));

        // Try to manually propagate clock and check subcircuit response
        console.log('\n4. Manual propagation test...');

        // First, get uart_tx subcircuit output state
        const beforeState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    return {
                        input: Object.fromEntries(
                            Object.entries(cell.get('inputSignals') || {}).map(([k, v]) =>
                                [k, v?._avec?.[0] & v?._bvec?.[0]])
                        ),
                        output: Object.fromEntries(
                            Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                [k, v?._avec?.[0] & v?._bvec?.[0]])
                        )
                    };
                }
            }
            return null;
        });
        console.log('Before sync:', JSON.stringify(beforeState));

        // Synchronize to ensure worker state is consistent
        await page.evaluate(() => window.djCircuit.synchronize());

        const afterSync = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    return {
                        input: Object.fromEntries(
                            Object.entries(cell.get('inputSignals') || {}).map(([k, v]) =>
                                [k, v?._avec?.[0] & v?._bvec?.[0]])
                        ),
                        output: Object.fromEntries(
                            Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                [k, v?._avec?.[0] & v?._bvec?.[0]])
                        )
                    };
                }
            }
            return null;
        });
        console.log('After sync:', JSON.stringify(afterSync));

        // Run a few ticks and check
        console.log('\n5. Running 200 ticks and checking...');
        for (let i = 0; i < 200; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        const afterTicks = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    return {
                        tick: circuit.tick,
                        input: Object.fromEntries(
                            Object.entries(cell.get('inputSignals') || {}).map(([k, v]) =>
                                [k, v?._avec?.[0] & v?._bvec?.[0]])
                        ),
                        output: Object.fromEntries(
                            Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                [k, v?._avec?.[0] & v?._bvec?.[0]])
                        )
                    };
                }
            }
            return null;
        });
        console.log('After 200 ticks:', JSON.stringify(afterTicks));

        // Check if clock is toggling at top level
        console.log('\n6. Clock cell output...');
        const clockState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    return {
                        id: cell.id,
                        propagation: cell.get('propagation'),
                        output: cell.get('outputSignals')?.out?._avec?.[0] & cell.get('outputSignals')?.out?._bvec?.[0]
                    };
                }
            }
            return null;
        });
        console.log(JSON.stringify(clockState));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
