/**
 * Debug UART activity - press button and verify internal DFFs are working
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug UART Activity\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    page.on('console', msg => {
        if (msg.text().includes('[UART') || msg.text().includes('DEBUG')) {
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

        // Set up inputs - rst_n=HIGH, btn_n=HIGH
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        console.log('2. Reset sequence...');
        // Set rst_n = LOW first
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = LOW
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click(); // btn_n = HIGH

        // Run a few ticks with reset LOW
        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        console.log('   Reset held LOW for 50 ticks');

        // Release reset
        await checkboxes[0].click();  // rst_n = HIGH
        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        console.log('   Reset released, ran 50 ticks');

        // Check state after reset
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    return {
                        tick: circuit.tick,
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
        console.log('3. State after reset:', JSON.stringify(state));

        // Press button (btn_n = LOW)
        console.log('\n4. Pressing button...');
        await checkboxes[1].click();  // btn_n = LOW

        // Run several batches and check state changes
        for (let batch = 0; batch < 10; batch++) {
            for (let i = 0; i < 100; i++) {
                await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            }

            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        return {
                            tick: circuit.tick,
                            output: Object.fromEntries(
                                Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                    [k, v ? (v._avec?.[0] & v._bvec?.[0]) : null])
                            )
                        };
                    }
                }
                return null;
            });
            console.log(`   Batch ${batch + 1}: tick=${state.tick} tx=${state.output.tx} busy=${state.output.busy}`);

            // If busy goes high, we have activity!
            if (state.output.busy === 1) {
                console.log('   *** UART TX is BUSY - transmission started! ***');
            }
        }

        // Check UART terminal output
        const uartOutput = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('\n5. UART terminal output:', JSON.stringify(uartOutput));

        // Check internal DFF states in uart_tx subcircuit (observe first)
        console.log('\n6. Observing uart_tx internal state...');
        await page.evaluate(() => {
            const circuit = window.djCircuit;
            circuit.observeGraph(['u_uart_tx']);
        });

        // Run more ticks after observation to sync
        for (let i = 0; i < 100; i++) {
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
                        dffs: dffs.slice(0, 5)  // Just first 5 DFFs
                    };
                }
            }
            return null;
        });
        console.log('Internal state:', JSON.stringify(internalState, null, 2));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
