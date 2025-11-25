/**
 * Test flat design with no subcircuits
 */

import { chromium } from 'playwright';
import fs from 'fs';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test flat design (no subcircuits)\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        // Load with an example first to get CodeMirror initialized
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe').forEach(el => el.remove());
        });
        await sleep(500);

        // Load flat_test.sv
        const code = fs.readFileSync('public/examples/flat_test.sv', 'utf8');
        console.log('   Loading flat_test.sv (' + code.length + ' bytes)');

        await page.evaluate((code) => {
            const cmElement = document.querySelector('.CodeMirror');
            if (cmElement && cmElement.CodeMirror) {
                cmElement.CodeMirror.setValue(code);
                console.log('Code set, first line:', code.split('\n')[0]);
            }
        }, code);
        await sleep(500);

        // Verify code was set
        const loadedCode = await page.evaluate(() => {
            const cmElement = document.querySelector('.CodeMirror');
            return cmElement?.CodeMirror?.getValue().substring(0, 100);
        });
        console.log('   Loaded code starts with:', loadedCode?.substring(0, 50));

        console.log('1. Synthesizing flat design...');
        await page.click('#synthesize-btn');

        try {
            await page.waitForSelector('#paper svg', { timeout: 30000 });
        } catch (e) {
            const errorText = await page.evaluate(() => {
                return document.querySelector('.alert-danger')?.textContent || null;
            });
            if (errorText) {
                console.log('Synthesis error:', errorText);
                return;
            }
            throw e;
        }
        await sleep(2000);

        await page.evaluate(() => window.djCircuit.stop());

        // Go to I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('Found', checkboxes.length, 'checkboxes');

        // Reset: rst HIGH, btn_n HIGH
        console.log('\n2. Reset sequence...');
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();
        if (!await checkboxes[0].isChecked()) await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset
        await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Check all output signals
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const result = { tick: circuit.tick };

            for (const cell of cells) {
                const net = cell.get('net');
                if (net === 'tx') {
                    const inp = cell.get('inputSignals')?.in;
                    result.tx = (inp?._avec?.[0] ?? 0) === 1 && (inp?._bvec?.[0] ?? 0) === 1 ? 1 : 0;
                }
                // Check all Dff outputs
                if (cell.get('type') === 'Dff') {
                    const out = cell.get('outputSignals')?.out;
                    const bits = out?._bits || 1;
                    const a = out?._avec?.[0] ?? 0;
                    result[cell.get('label') || cell.id] = a;
                }
            }
            return result;
        });
        console.log('   After reset:', JSON.stringify(state));

        // Press button
        console.log('\n3. Press button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(50);

        // Run and monitor
        console.log('4. Running and monitoring tx...');
        await page.evaluate(() => window.djCircuit.start());

        let txChanges = 0;
        let lastTx = 1;
        for (let i = 0; i < 30; i++) {
            await sleep(100);
            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('net') === 'tx') {
                        const inp = cell.get('inputSignals')?.in;
                        return {
                            tick: circuit.tick,
                            tx: (inp?._avec?.[0] ?? 0) === 1 && (inp?._bvec?.[0] ?? 0) === 1 ? 1 : 0
                        };
                    }
                }
                return null;
            });

            if (state?.tx !== lastTx) {
                console.log(`   [${i*100}ms] tick=${state.tick} tx CHANGED to ${state.tx}`);
                txChanges++;
                lastTx = state.tx;
            } else if (i % 10 === 0) {
                console.log(`   [${i*100}ms] tick=${state.tick} tx=${state.tx}`);
            }
        }

        await page.evaluate(() => window.djCircuit.stop());

        console.log('\n5. Results:');
        console.log('   tx changes:', txChanges);
        if (txChanges > 0) {
            console.log('   SUCCESS: Counter is incrementing!');
        } else {
            console.log('   FAILED: Counter not incrementing');
        }

        await sleep(3000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
