/**
 * Long UART test - run for longer to see full transmission
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Long UART test\\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe').forEach(el => el.remove());
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        await page.evaluate(() => window.djCircuit.stop());

        // Go to I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Initial state - btn_n should be HIGH (not pressed)
        console.log('\\n2. Setting initial state: rst_n=HIGH, btn_n=HIGH...');
        // rst_n = checkbox[0], btn_n = checkbox[1]
        if (!await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = HIGH
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();  // btn_n = HIGH

        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        // Check UART terminal before
        let uartOutput = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('UART before:', JSON.stringify(uartOutput));

        // Press button (btn_n LOW)
        console.log('\\n3. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();  // btn_n = LOW
        await sleep(50);

        // Run for 15 seconds
        console.log('4. Running simulation for 15 seconds...');
        await page.evaluate(() => window.djCircuit.start());

        // Check every 3 seconds
        for (let i = 0; i < 5; i++) {
            await sleep(3000);

            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                let txCell = null;
                let txSubcircuit = null;

                for (const cell of cells) {
                    const label = cell.get('label');
                    if (label === 'u_uart_tx') {
                        txSubcircuit = cell;
                    }
                    const net = cell.get('net');
                    if (net === 'uart_tx_o') {
                        txCell = cell;
                    }
                }

                const txSub = txSubcircuit?.get('outputSignals') || {};
                const txOutput = txCell?.get('inputSignals')?.in;

                return {
                    tick: circuit.tick,
                    busy: txSub.busy ? ((txSub.busy._avec?.[0] ?? 0) & (txSub.busy._bvec?.[0] ?? 0) & 1) : null,
                    tx: txOutput ? ((txOutput._avec?.[0] ?? 0) & (txOutput._bvec?.[0] ?? 0) & 1) : null,
                    uartOutput: document.querySelector('.uart-output')?.textContent || ''
                };
            });

            console.log(`[${(i+1)*3}s] tick=${state.tick} busy=${state.busy} tx=${state.tx} uart="${state.uartOutput}"`);
        }

        await page.evaluate(() => window.djCircuit.stop());

        // Final check
        uartOutput = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('\\n5. Final UART output:', JSON.stringify(uartOutput));

        if (uartOutput.length > 0) {
            console.log('\\n✓ SUCCESS: UART terminal received data!');
        } else {
            console.log('\\n✗ FAILED: No UART output');
        }

        // Keep browser open
        console.log('\\nBrowser stays open for 30 seconds...');
        await sleep(30000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
