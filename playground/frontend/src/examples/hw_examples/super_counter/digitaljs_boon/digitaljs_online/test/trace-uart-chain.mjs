/**
 * Trace UART signal chain
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Trace UART signal chain\\n');

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

        // List all I/O
        console.log('\\n2. I/O Panel inputs:');
        const ioInfo = await page.evaluate(() => {
            const inputs = document.querySelectorAll('#iopanel input[type="checkbox"]');
            return Array.from(inputs).map((inp, i) => {
                const label = inp.closest('label')?.textContent || inp.closest('.form-group')?.querySelector('label')?.textContent || 'unknown';
                return { index: i, label: label.trim(), checked: inp.checked };
            });
        });
        for (const io of ioInfo) {
            console.log(`  [${io.index}] ${io.label}: ${io.checked ? 'HIGH' : 'LOW'}`);
        }

        // Get all subcircuit devices
        console.log('\\n3. Subcircuits:');
        const subcircuits = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const result = [];
            for (const cell of cells) {
                const type = cell.get('type');
                if (type === 'Subcircuit') {
                    result.push({
                        label: cell.get('label'),
                        celltype: cell.get('celltype'),
                        inputs: Object.keys(cell.get('inputSignals') || {}),
                        outputs: Object.keys(cell.get('outputSignals') || {})
                    });
                }
            }
            return result;
        });
        for (const sc of subcircuits) {
            console.log(`  ${sc.label} (${sc.celltype}): in=[${sc.inputs.join(',')}] out=[${sc.outputs.join(',')}]`);
        }

        // Go to I/O panel and set up
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Reset sequence based on labels
        console.log('\\n4. Reset sequence...');
        // Find rst_n checkbox (should be LOW for reset, then HIGH)
        // Set rst_n=LOW (active reset)
        for (const io of ioInfo) {
            if (io.label.includes('rst')) {
                if (checkboxes[io.index] && await checkboxes[io.index].isChecked()) {
                    await checkboxes[io.index].click();
                }
            }
        }

        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset (rst_n=HIGH)
        for (const io of ioInfo) {
            if (io.label.includes('rst')) {
                if (checkboxes[io.index] && !(await checkboxes[io.index].isChecked())) {
                    await checkboxes[io.index].click();
                }
            }
        }

        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Trace initial state
        console.log('\\n5. Signal chain after reset:');
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const result = { tick: circuit.tick };

            for (const cell of cells) {
                const label = cell.get('label');
                const type = cell.get('type');

                if (type === 'Subcircuit') {
                    const inp = cell.get('inputSignals');
                    const out = cell.get('outputSignals');
                    result[label] = { inputs: {}, outputs: {} };

                    for (const [k, v] of Object.entries(inp || {})) {
                        const val = v ? ((v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0)) : null;
                        result[label].inputs[k] = val;
                    }
                    for (const [k, v] of Object.entries(out || {})) {
                        const val = v ? ((v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0)) : null;
                        result[label].outputs[k] = val;
                    }
                }
            }
            return result;
        });

        console.log('Tick:', state.tick);
        for (const [key, val] of Object.entries(state)) {
            if (key !== 'tick' && typeof val === 'object') {
                console.log(`  ${key}:`);
                console.log(`    in:`, JSON.stringify(val.inputs));
                console.log(`    out:`, JSON.stringify(val.outputs));
            }
        }

        // Press button
        console.log('\\n6. Pressing button (btn_n LOW)...');
        for (const io of ioInfo) {
            if (io.label.includes('btn')) {
                if (checkboxes[io.index] && await checkboxes[io.index].isChecked()) {
                    await checkboxes[io.index].click();
                }
            }
        }
        await sleep(50);

        // Run and trace
        console.log('\\n7. Running simulation and tracing...');
        await page.evaluate(() => window.djCircuit.start());

        for (let i = 0; i < 10; i++) {
            await sleep(500);

            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    const label = cell.get('label');
                    const type = cell.get('type');

                    if (label === 'u_debouncer' || label === 'u_btn_message' || label === 'u_uart_tx') {
                        const inp = cell.get('inputSignals') || {};
                        const out = cell.get('outputSignals') || {};
                        result[label] = {};

                        for (const [k, v] of Object.entries(inp)) {
                            result[label]['in_' + k] = v ? ((v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0)) : null;
                        }
                        for (const [k, v] of Object.entries(out)) {
                            result[label]['out_' + k] = v ? ((v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0)) : null;
                        }
                    }
                }
                return result;
            });

            const debouncer = state.u_debouncer || {};
            const btn_msg = state.u_btn_message || {};
            const uart_tx = state.u_uart_tx || {};

            console.log(`[${i*500}ms] tick=${state.tick}`);
            console.log(`  debouncer: btn_n=${debouncer.in_btn_n} -> pressed=${debouncer.out_pressed}`);
            console.log(`  btn_msg: pressed=${btn_msg.in_btn_pressed} busy=${btn_msg.in_tx_busy} -> start=${btn_msg.out_tx_start} data=${btn_msg.out_tx_data}`);
            console.log(`  uart_tx: start=${uart_tx.in_start} -> busy=${uart_tx.out_busy} tx=${uart_tx.out_tx}`);
        }

        await page.evaluate(() => window.djCircuit.stop());

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
