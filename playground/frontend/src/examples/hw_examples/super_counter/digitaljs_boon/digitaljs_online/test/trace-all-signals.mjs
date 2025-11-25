/**
 * Trace ALL intermediate signals in debouncer
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

// Helper to read Vector3vl value
function v3vlValue(sig) {
    if (!sig) return 'null';
    // Vector3vl format: {_bits, _avec, _bvec}
    // avec=0,bvec=0 → LOW; avec=1,bvec=1 → HIGH; avec=0,bvec=1 → X
    if (sig._bits === 1) {
        const a = sig._avec?.[0] || sig._avec?.['0'] || 0;
        const b = sig._bvec?.[0] || sig._bvec?.['0'] || 0;
        if (a === 0 && b === 0) return '0';
        if (a === 1 && b === 1) return '1';
        return 'X';
    }
    // Multi-bit: return as binary string or value
    const bits = sig._bits;
    let val = 0;
    for (let i = 0; i < bits; i++) {
        const a = sig._avec?.[i] || sig._avec?.[String(i)] || 0;
        const b = sig._bvec?.[i] || sig._bvec?.[String(i)] || 0;
        if (a === 1 && b === 1) val |= (1 << i);
    }
    return val.toString();
}

async function test() {
    console.log('Trace All Debouncer Signals\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        // Wait for overlay and remove it
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe[src="about:blank"]').forEach(el => el.remove());
        });
        await sleep(500);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Find all debouncer internal cells
        console.log('2. Finding debouncer signals...\n');
        const cells = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const allCells = circuit._graph.getCells();

            // Build map of cells
            const cellMap = {};
            for (const cell of allCells) {
                cellMap[cell.id] = {
                    id: cell.id,
                    type: cell.get('type'),
                    label: cell.get('label'),
                    net: cell.get('net'),
                    inputSignals: cell.get('inputSignals'),
                    outputSignals: cell.get('outputSignals')
                };
            }
            return cellMap;
        });

        // Print cell summary
        console.log('Top-level cells:');
        for (const [id, cell] of Object.entries(cells)) {
            if (cell.net || cell.label?.startsWith('u_')) {
                console.log(`  ${id}: ${cell.type} - ${cell.net || cell.label}`);
            }
        }

        // Set up I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Proper reset sequence
        console.log('\n3. Reset sequence...');

        // btn_n HIGH first
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(50);
        }

        // rst_n LOW (active reset)
        if (await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(50);
        }

        // Run with reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(200);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset (rst_n HIGH)
        await checkboxes[0].click();
        await sleep(50);

        // Run after reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Check debouncer internal state
        console.log('\n4. Debouncer state after reset:');
        const afterReset = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            // Find debouncer cell
            for (const cell of cells) {
                if (cell.get('label') === 'u_debouncer') {
                    return {
                        tick: circuit.tick,
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
            }
            return null;
        });

        console.log('   tick:', afterReset?.tick);
        console.log('   inputSignals:', JSON.stringify(afterReset?.inputSignals));
        console.log('   outputSignals:', JSON.stringify(afterReset?.outputSignals));

        // Press button
        console.log('\n5. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(50);

        // Verify
        const btnState = await checkboxes[1].isChecked();
        console.log('   btn_n checkbox:', btnState ? 'checked (HIGH)' : 'unchecked (LOW)');

        // Run simulation and trace
        console.log('\n6. Running and tracing debouncer every 50ms for 2 seconds...\n');
        await page.evaluate(() => window.djCircuit.start());

        for (let i = 0; i < 40; i++) {
            await sleep(50);
            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                let debouncer = null;
                let btnInput = null;

                for (const cell of cells) {
                    if (cell.get('label') === 'u_debouncer') {
                        debouncer = cell;
                    }
                    if (cell.get('net') === 'btn_n') {
                        btnInput = cell;
                    }
                }

                return {
                    tick: circuit.tick,
                    btn_n_out: btnInput?.get('outputSignals')?.out,
                    deb_clk: debouncer?.get('inputSignals')?.clk,
                    deb_rst: debouncer?.get('inputSignals')?.rst,
                    deb_btn_n: debouncer?.get('inputSignals')?.btn_n,
                    deb_pressed: debouncer?.get('outputSignals')?.pressed
                };
            });

            // Parse signals
            const btn_n = v3vlValue(state.btn_n_out);
            const clk = v3vlValue(state.deb_clk);
            const rst = v3vlValue(state.deb_rst);
            const deb_btn_n = v3vlValue(state.deb_btn_n);
            const pressed = v3vlValue(state.deb_pressed);

            // Log every 10 iterations or if pressed changes
            if (i % 10 === 0 || pressed !== '0') {
                console.log(`[${i*50}ms] tick=${state.tick} btn_n_input=${btn_n} deb.btn_n=${deb_btn_n} deb.rst=${rst} deb.pressed=${pressed}`);
            }
        }

        // Stop and check terminal
        await page.evaluate(() => window.djCircuit.stop());

        const output = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent?.trim() || '';
        });
        console.log('\n7. UART output:', output || '(empty)');

        // Final state
        const finalState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_debouncer') {
                    return {
                        tick: circuit.tick,
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
            }
            return null;
        });

        console.log('\n8. Final debouncer state:');
        console.log('   inputSignals:', JSON.stringify(finalState?.inputSignals));
        console.log('   outputSignals:', JSON.stringify(finalState?.outputSignals));

        await page.screenshot({ path: 'test/trace-all-result.png', fullPage: true });
        console.log('\n9. Screenshot saved');

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
