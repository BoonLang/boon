/**
 * Debug subcircuit internal signals
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug subcircuit internals\\n');

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

        // Get the uart_tx subcircuit internal graph
        console.log('\\n2. Examining uart_tx subcircuit internals...');
        const internals = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    const subGraph = cell.get('graph');
                    if (!subGraph) return { error: 'No graph' };

                    const subCells = subGraph.getCells();
                    const devices = [];

                    for (const sc of subCells) {
                        const type = sc.get('type');
                        const label = sc.get('label');
                        const id = sc.id;

                        if (type === 'Dff') {
                            const out = sc.get('outputSignals');
                            let outVal = null;
                            if (out?.out) {
                                outVal = [];
                                for (let b = 0; b < out.out._bits; b++) {
                                    const bit = ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1);
                                    outVal.push(bit);
                                }
                            }
                            devices.push({
                                id, type, label,
                                bits: out?.out?._bits,
                                value: outVal?.reverse().join('') || 'null'
                            });
                        }
                    }

                    return { deviceCount: subCells.length, dffs: devices };
                }
            }
            return { error: 'uart_tx not found' };
        });

        console.log('Device count:', internals.deviceCount);
        console.log('\\nDFFs in uart_tx:');
        for (const dff of internals.dffs || []) {
            console.log(`  ${dff.label || dff.id}: ${dff.bits}b value=${dff.value}`);
        }

        // Set up and run
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // rst_n=HIGH, btn_n=HIGH initially
        if (!await checkboxes[0].isChecked()) await checkboxes[0].click();
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();

        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        // Press button
        console.log('\\n3. Pressing button...');
        await checkboxes[1].click();
        await sleep(50);

        await page.evaluate(() => window.djCircuit.start());
        await sleep(2000);

        // Check internal DFFs
        console.log('\\n4. DFF values after 2 seconds:');
        const afterState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    const subGraph = cell.get('graph');
                    const subCells = subGraph.getCells();
                    const devices = [];

                    for (const sc of subCells) {
                        const type = sc.get('type');
                        const label = sc.get('label');

                        if (type === 'Dff') {
                            const out = sc.get('outputSignals');
                            let outVal = null;
                            if (out?.out) {
                                outVal = [];
                                for (let b = 0; b < out.out._bits; b++) {
                                    const bit = ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1);
                                    outVal.push(bit);
                                }
                            }
                            devices.push({
                                label: label || sc.id,
                                bits: out?.out?._bits,
                                value: outVal?.reverse().join('') || 'null'
                            });
                        }
                    }

                    return { tick: circuit.tick, dffs: devices };
                }
            }
            return null;
        });

        console.log('Tick:', afterState?.tick);
        for (const dff of afterState?.dffs || []) {
            console.log(`  ${dff.label}: ${dff.bits}b value=${dff.value}`);
        }

        await page.evaluate(() => window.djCircuit.stop());

        // Run more and check again
        await page.evaluate(() => window.djCircuit.start());
        await sleep(3000);

        const finalState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    const subGraph = cell.get('graph');
                    const subCells = subGraph.getCells();
                    const devices = [];

                    for (const sc of subCells) {
                        const type = sc.get('type');
                        const label = sc.get('label');

                        if (type === 'Dff') {
                            const out = sc.get('outputSignals');
                            let outVal = null;
                            if (out?.out) {
                                outVal = [];
                                for (let b = 0; b < out.out._bits; b++) {
                                    const bit = ((out.out._avec[b >> 5] >> (b & 31)) & (out.out._bvec[b >> 5] >> (b & 31)) & 1);
                                    outVal.push(bit);
                                }
                            }
                            devices.push({
                                label: label || sc.id,
                                bits: out?.out?._bits,
                                value: outVal?.reverse().join('') || 'null'
                            });
                        }
                    }

                    return { tick: circuit.tick, dffs: devices };
                }
            }
            return null;
        });

        console.log('\\n5. DFF values after 5 more seconds:');
        console.log('Tick:', finalState?.tick);
        for (const dff of finalState?.dffs || []) {
            console.log(`  ${dff.label}: ${dff.bits}b value=${dff.value}`);
        }

        await page.evaluate(() => window.djCircuit.stop());

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
