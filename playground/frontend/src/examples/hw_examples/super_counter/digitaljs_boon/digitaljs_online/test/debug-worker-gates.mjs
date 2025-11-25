/**
 * Debug worker gate internal state - check link targets and subgraph setup
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug Worker Gate Internal State\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    page.on('console', msg => {
        if (msg.text().includes('DEBUG')) {
            console.log('BROWSER:', msg.text());
        }
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        await page.evaluate(() => window.djCircuit.stop());

        // Get main graph cid and gate info
        console.log('\n2. Getting graph and gate info...');
        const info = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const mainGraph = circuit._graph;
            const cells = mainGraph.getCells();

            let clockId = null;
            let uartTxId = null;
            let uartTxCelltype = null;

            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    clockId = cell.id;
                }
                if (cell.get('label') === 'u_uart_tx') {
                    uartTxId = cell.id;
                    uartTxCelltype = cell.get('celltype');
                }
            }

            return {
                mainGraphCid: mainGraph.cid,
                clockId,
                uartTxId,
                uartTxCelltype
            };
        });
        console.log(JSON.stringify(info, null, 2));

        // Debug Clock gate in worker
        console.log('\n3. Debugging Clock gate in worker...');
        const clockDebug = await page.evaluate(async ({ graphCid, gateId }) => {
            const circuit = window.djCircuit;
            const engine = circuit._engine;
            return await engine.debugGate(graphCid, gateId);
        }, { graphCid: info.mainGraphCid, gateId: info.clockId });
        console.log('Clock gate:', JSON.stringify(clockDebug, null, 2));

        // Debug uart_tx gate in worker
        console.log('\n4. Debugging uart_tx gate in worker...');
        const uartDebug = await page.evaluate(async ({ graphCid, gateId }) => {
            const circuit = window.djCircuit;
            const engine = circuit._engine;
            return await engine.debugGate(graphCid, gateId);
        }, { graphCid: info.mainGraphCid, gateId: info.uartTxId });
        console.log('uart_tx gate:', JSON.stringify(uartDebug, null, 2));

        // Check links info in JointJS
        console.log('\n5. Links from Clock in JointJS...');
        const linksInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const mainGraph = circuit._graph;
            const links = mainGraph.getLinks();
            const cells = mainGraph.getCells();

            let clockId = null;
            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    clockId = cell.id;
                    break;
                }
            }

            const clockLinks = links.filter(l => l.get('source')?.id === clockId);
            return clockLinks.map(l => ({
                id: l.id,
                source: l.get('source'),
                target: l.get('target'),
                warning: l.get('warning')
            }));
        });
        console.log(JSON.stringify(linksInfo, null, 2));

        // Run simulation and check if targets update
        console.log('\n6. Running 100 ticks...');
        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Check Clock output signal after simulation
        const clockAfter = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    const out = cell.get('outputSignals')?.out;
                    return {
                        tick: circuit.tick,
                        output: out ? (out._avec?.[0] & out._bvec?.[0]) : null,
                        propagation: cell.get('propagation')
                    };
                }
            }
            return null;
        });
        console.log('Clock state after 100 ticks:', JSON.stringify(clockAfter));

        // Check uart_tx input signal
        const uartAfter = await page.evaluate(() => {
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
        console.log('uart_tx state after 100 ticks:', JSON.stringify(uartAfter));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
