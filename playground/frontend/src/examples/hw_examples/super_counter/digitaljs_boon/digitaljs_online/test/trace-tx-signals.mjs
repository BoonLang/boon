/**
 * Trace TX Signal Path - Detailed Debug
 * Traces: btn_n -> debouncer -> btn_pressed -> btn_message -> tx_start -> uart_tx -> uart_tx_o
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function debug() {
    console.log('TX Signal Path Tracer\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    // Capture ALL console logs
    page.on('console', msg => {
        console.log('BROWSER:', msg.text());
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        // Remove overlay
        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop simulation first
        console.log('2. Pausing simulation...');
        const isPaused = await page.evaluate(() => {
            if (window.digitaljs && window.digitaljs._circuit) {
                window.digitaljs._circuit.stop();
                return true;
            }
            return false;
        });
        console.log('   Paused:', isPaused);

        // Inject signal tracer
        console.log('3. Injecting signal tracer...');
        await page.evaluate(() => {
            const circuit = window.digitaljs._circuit;
            if (!circuit) {
                console.log('No circuit found!');
                return;
            }

            // Find all cells and their types
            const cells = circuit._graph.getCells();
            console.log('Total cells:', cells.length);

            // Find key cells
            let txLamp = null;
            let btnInput = null;
            let rstInput = null;

            for (const cell of cells) {
                const net = cell.get('net');
                const cellType = cell.get('type');
                const id = cell.id;

                if (net) {
                    console.log(`Cell ${id}: type=${cellType}, net=${net}`);
                    if (net === 'uart_tx_o') txLamp = cell;
                    if (net === 'btn_n') btnInput = cell;
                    if (net === 'rst_n') rstInput = cell;
                }
            }

            // Check initial states
            console.log('\n--- Initial Signal States ---');
            if (txLamp) {
                const sig = txLamp.get('inputSignals');
                console.log('uart_tx_o input signals:', JSON.stringify(sig));
            }
            if (btnInput) {
                const sig = btnInput.get('outputSignals');
                console.log('btn_n output signals:', JSON.stringify(sig));
            }
            if (rstInput) {
                const sig = rstInput.get('outputSignals');
                console.log('rst_n output signals:', JSON.stringify(sig));
            }

            // Store references for later
            window.txLamp = txLamp;
            window.btnInput = btnInput;
            window.rstInput = rstInput;
        });

        // Go to I/O tab and set up signals
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('4. Checkboxes found:', checkboxes.length);

        // Get checkbox labels
        for (let i = 0; i < checkboxes.length; i++) {
            const label = await checkboxes[i].evaluate(el => {
                const row = el.closest('tr');
                return row?.querySelector('label')?.textContent || `cb${i}`;
            });
            const checked = await checkboxes[i].isChecked();
            console.log(`   [${i}] ${label}: ${checked ? 'HIGH' : 'LOW'}`);
        }

        // Set btn_n HIGH first
        console.log('\n5. Setting btn_n HIGH (button released)...');
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(200);
        }

        // Set rst_n HIGH (release reset)
        console.log('6. Setting rst_n HIGH (releasing reset)...');
        if (!await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(200);
        }

        // Check signals after reset release
        console.log('\n7. Signals after reset release:');
        await page.evaluate(() => {
            if (window.txLamp) {
                const sig = window.txLamp.get('inputSignals');
                console.log('uart_tx_o:', JSON.stringify(sig));
            }
        });

        // Run a few ticks manually
        console.log('\n8. Running 50 ticks manually and checking TX...');
        for (let i = 0; i < 5; i++) {
            await page.evaluate((n) => {
                const circuit = window.digitaljs._circuit;
                for (let j = 0; j < 10; j++) {
                    circuit.updateGates();
                }
                if (window.txLamp) {
                    const sig = window.txLamp.get('inputSignals');
                    const isHigh = sig?.in?.isHigh;
                    console.log(`After ${(n+1)*10} ticks: uart_tx_o = ${isHigh ? 'HIGH' : 'LOW'}`);
                }
            }, i);
            await sleep(100);
        }

        // Now press button and trace
        console.log('\n9. Pressing button (btn_n LOW) and tracing...');
        await checkboxes[1].click();  // Set btn_n LOW

        // Run more ticks and check
        for (let i = 0; i < 20; i++) {
            await page.evaluate((n) => {
                const circuit = window.digitaljs._circuit;
                for (let j = 0; j < 10; j++) {
                    circuit.updateGates();
                }
                if (window.txLamp) {
                    const sig = window.txLamp.get('inputSignals');
                    const isHigh = sig?.in?.isHigh;
                    // Only log changes or periodically
                    if (n % 5 === 0 || !isHigh) {
                        console.log(`After button press + ${(n+1)*10} ticks: uart_tx_o = ${isHigh ? 'HIGH' : 'LOW'}`);
                    }
                }
            }, i);
            await sleep(50);
        }

        // Resume normal simulation
        console.log('\n10. Resuming normal simulation for 5 seconds...');
        await page.evaluate(() => {
            window.digitaljs._circuit.start();
        });

        for (let i = 0; i < 5; i++) {
            await sleep(1000);
            const result = await page.evaluate(() => {
                const tick = window.digitaljs._circuit.tick;
                const output = document.querySelector('.uart-output')?.textContent?.trim() || '';
                const txSig = window.txLamp?.get('inputSignals');
                return {
                    tick,
                    output,
                    txHigh: txSig?.in?.isHigh
                };
            });
            console.log(`   [${i+1}s] tick=${result.tick} tx=${result.txHigh ? 'HIGH' : 'LOW'} output="${result.output}"`);
        }

        // Take screenshot
        await page.screenshot({ path: 'test/trace-tx-result.png', fullPage: true });
        console.log('\n11. Screenshot saved to test/trace-tx-result.png');

        // Keep browser open
        console.log('\nBrowser open for inspection. Ctrl+C to close.');
        await sleep(60000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

debug().catch(console.error);
