/**
 * Trace subcircuit simulation step by step
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Trace subcircuit simulation\n');

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

        // Set initial state: rst_n=HIGH, btn_n=HIGH
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        if (!await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = HIGH
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();  // btn_n = HIGH

        // Get initial subcircuit state
        console.log('\n2. Initial subcircuit internal state:');
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    const subGraph = cell.get('graph');
                    if (!subGraph) return { error: 'No graph' };

                    const subCells = subGraph.getCells();
                    const result = {
                        subcircuitInput: Object.fromEntries(
                            Object.entries(cell.get('inputSignals') || {}).map(([k, v]) =>
                                [k, v ? (v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0) : null])
                        ),
                        subcircuitOutput: Object.fromEntries(
                            Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                [k, v ? (v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0) : null])
                        ),
                        internalInputs: [],
                        dffs: []
                    };

                    for (const sc of subCells) {
                        const type = sc.get('type');
                        if (type === 'Input') {
                            const out = sc.get('outputSignals')?.out;
                            result.internalInputs.push({
                                net: sc.get('net'),
                                value: out ? (out._avec?.[0] ?? 0) & (out._bvec?.[0] ?? 0) : null
                            });
                        }
                        if (type === 'Dff') {
                            const out = sc.get('outputSignals')?.out;
                            result.dffs.push({
                                label: sc.get('label') || sc.id,
                                bits: out?._bits,
                                value: out ? (out._avec?.[0] ?? 0) & (out._bvec?.[0] ?? 0) : null,
                                lastClk: sc.last_clk
                            });
                        }
                    }
                    return result;
                }
            }
            return { error: 'uart_tx not found' };
        });
        console.log(JSON.stringify(state, null, 2));

        // Step simulation manually and watch state changes
        console.log('\n3. Running 10 simulation ticks manually...');
        for (let i = 0; i < 10; i++) {
            await page.evaluate(() => window.djCircuit.updateGates());

            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        const subGraph = cell.get('graph');
                        const subCells = subGraph.getCells();

                        let clockInputValue = null;
                        let startInputValue = null;
                        let dffStates = [];

                        for (const sc of subCells) {
                            const type = sc.get('type');
                            if (type === 'Input') {
                                const net = sc.get('net');
                                const out = sc.get('outputSignals')?.out;
                                const val = out ? (out._avec?.[0] ?? 0) & (out._bvec?.[0] ?? 0) : null;
                                if (net === 'clk') clockInputValue = val;
                                if (net === 'start') startInputValue = val;
                            }
                            if (type === 'Dff') {
                                const out = sc.get('outputSignals')?.out;
                                dffStates.push({
                                    id: sc.id.substring(0, 12),
                                    val: out ? (out._avec?.[0] ?? 0) & (out._bvec?.[0] ?? 0) : null,
                                    lastClk: sc.last_clk
                                });
                            }
                        }

                        return {
                            tick: circuit.tick,
                            clk: clockInputValue,
                            start: startInputValue,
                            dffs: dffStates
                        };
                    }
                }
                return null;
            });

            console.log(`Tick ${state.tick}: clk=${state.clk} start=${state.start} dffs=${JSON.stringify(state.dffs.map(d => d.val))}`);
        }

        // Now press button and trace again
        console.log('\n4. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(50);

        console.log('\n5. Running 50 more ticks...');
        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates());

            if (i % 5 === 0) {
                state = await page.evaluate(() => {
                    const circuit = window.djCircuit;
                    const cells = circuit._graph.getCells();

                    for (const cell of cells) {
                        if (cell.get('label') === 'u_uart_tx') {
                            const subGraph = cell.get('graph');
                            const subCells = subGraph.getCells();

                            let clockInputValue = null;
                            let startInputValue = null;
                            let busyVal = null;
                            let txVal = null;
                            let dffStates = [];

                            for (const sc of subCells) {
                                const type = sc.get('type');
                                if (type === 'Input') {
                                    const net = sc.get('net');
                                    const out = sc.get('outputSignals')?.out;
                                    const val = out ? (out._avec?.[0] ?? 0) & (out._bvec?.[0] ?? 0) : null;
                                    if (net === 'clk') clockInputValue = val;
                                    if (net === 'start') startInputValue = val;
                                }
                                if (type === 'Output') {
                                    const net = sc.get('net');
                                    const inp = sc.get('inputSignals')?.in;
                                    const val = inp ? (inp._avec?.[0] ?? 0) & (inp._bvec?.[0] ?? 0) : null;
                                    if (net === 'busy') busyVal = val;
                                    if (net === 'tx') txVal = val;
                                }
                                if (type === 'Dff') {
                                    const out = sc.get('outputSignals')?.out;
                                    dffStates.push({
                                        id: sc.id.substring(0, 12),
                                        val: out ? (out._avec?.[0] ?? 0) & (out._bvec?.[0] ?? 0) : null,
                                        lastClk: sc.last_clk
                                    });
                                }
                            }

                            return {
                                tick: circuit.tick,
                                clk: clockInputValue,
                                start: startInputValue,
                                busy: busyVal,
                                tx: txVal,
                                dffs: dffStates
                            };
                        }
                    }
                    return null;
                });

                console.log(`Tick ${state.tick}: clk=${state.clk} start=${state.start} busy=${state.busy} tx=${state.tx}`);
                console.log(`  DFFs: ${JSON.stringify(state.dffs.map(d => ({val: d.val, lc: d.lastClk})))}`);
            }
        }

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
