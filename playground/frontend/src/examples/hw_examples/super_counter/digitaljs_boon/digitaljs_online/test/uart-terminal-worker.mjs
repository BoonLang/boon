/**
 * Test UART terminal with WorkerEngine - run via start() for proper simulation
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test UART Terminal with WorkerEngine\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    page.on('console', msg => {
        if (msg.text().includes('[UART') || msg.text().includes('SynchEngine')) {
            console.log('BROWSER:', msg.text());
        }
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        // Remove overlays
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe').forEach(el => el.remove());
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        // Stop simulation initially
        await page.evaluate(() => window.djCircuit.stop());

        // Set initial state: rst_n=HIGH, btn_n=HIGH
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type=\"checkbox\"]');

        console.log('2. Reset sequence...');
        // Start with reset LOW
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = LOW
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click(); // btn_n = HIGH

        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset
        await checkboxes[0].click();  // rst_n = HIGH
        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        console.log('   Reset complete');

        // Check UART terminal before
        let uartOutput = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('3. UART output before button:', JSON.stringify(uartOutput));

        // Press button
        console.log('4. Pressing button...');
        await checkboxes[1].click();  // btn_n = LOW
        await sleep(50);

        // Run simulation for 10 seconds with start()
        console.log('5. Running simulation for 10 seconds...');
        await page.evaluate(() => window.djCircuit.start());

        // Check every 2 seconds
        for (let i = 0; i < 5; i++) {
            await sleep(2000);
            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                return {
                    tick: circuit.tick,
                    running: circuit.running,
                    uartOutput: document.querySelector('.uart-output')?.textContent || ''
                };
            });
            console.log(`   [${(i+1)*2}s] tick=${state.tick} running=${state.running} uart="${state.uartOutput}"`);
        }

        await page.evaluate(() => window.djCircuit.stop());

        // Final check
        uartOutput = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('\n6. Final UART output:', JSON.stringify(uartOutput));

        if (uartOutput.length > 0) {
            console.log('\n✓ SUCCESS: UART terminal received data!');
        } else {
            console.log('\n✗ FAILED: No UART output');

            // Debug: trace uart_tx output signals
            const debug = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        return {
                            inputSignals: Object.fromEntries(
                                Object.entries(cell.get('inputSignals') || {}).map(([k, v]) =>
                                    [k, v ? `bits=${v._bits} val=${(v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0)}` : null])
                            ),
                            outputSignals: Object.fromEntries(
                                Object.entries(cell.get('outputSignals') || {}).map(([k, v]) =>
                                    [k, v ? `bits=${v._bits} val=${(v._avec?.[0] ?? 0) & (v._bvec?.[0] ?? 0)}` : null])
                            )
                        };
                    }
                }
                return null;
            });
            console.log('uart_tx signals:', JSON.stringify(debug, null, 2));
        }

        // Keep browser open for inspection
        console.log('\nKeeping browser open for 20 seconds...');
        await sleep(20000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
