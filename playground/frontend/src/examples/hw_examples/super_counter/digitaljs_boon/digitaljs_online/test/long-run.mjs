/**
 * Long run test - press button and run for many ticks
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Long run test\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe').forEach(el => el.remove());
        });
        await sleep(500);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        await page.evaluate(() => window.djCircuit.stop());

        // Go to I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Reset sequence
        console.log('2. Reset sequence...');
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());
        await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        console.log('3. Press button and run for 5000 ticks...');
        await checkboxes[1].click();  // btn_n LOW
        await sleep(50);

        // Get starting tick
        let startTick = await page.evaluate(() => window.djCircuit.tick);
        console.log('   Starting tick:', startTick);

        // Run simulation continuously
        await page.evaluate(() => window.djCircuit.start());

        // Check periodically for busy or tx changes
        let busySeenHigh = false;
        let txSeenLow = false;

        for (let i = 0; i < 50; i++) {
            await sleep(100);

            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        const output = cell.get('outputSignals') || {};
                        const busySig = output.busy;
                        const txSig = output.tx;

                        return {
                            tick: circuit.tick,
                            busy: (busySig?._avec?.[0] ?? 0) === 1 && (busySig?._bvec?.[0] ?? 0) === 1 ? 1 : 0,
                            tx: (txSig?._avec?.[0] ?? 0) === 1 && (txSig?._bvec?.[0] ?? 0) === 1 ? 1 : 0
                        };
                    }
                }
                return null;
            });

            if (state.busy === 1) busySeenHigh = true;
            if (state.tx === 0) txSeenLow = true;

            // Log every 10 iterations or on state change
            if (i % 10 === 0 || state.busy === 1 || state.tx === 0) {
                console.log(`   [${i * 100}ms] tick=${state.tick} busy=${state.busy} tx=${state.tx}`);
            }
        }

        await page.evaluate(() => window.djCircuit.stop());

        let endTick = await page.evaluate(() => window.djCircuit.tick);
        console.log('   Ending tick:', endTick, '(ran', endTick - startTick, 'ticks)');

        console.log('\n4. Results:');
        console.log('   busy seen HIGH:', busySeenHigh);
        console.log('   tx seen LOW:', txSeenLow);

        // Check UART terminal
        const output = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent?.trim() || '';
        });
        console.log('   UART output:', output || '(empty)');

        await sleep(2000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
