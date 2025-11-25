/**
 * Debug worker console output
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug Worker Console Output\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    // Capture ALL console messages
    page.on('console', msg => {
        console.log('BROWSER:', msg.text());
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        await page.evaluate(() => window.djCircuit.stop());

        // Set up inputs
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        console.log('2. Reset sequence...');
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = LOW
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click(); // btn_n = HIGH

        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        await checkboxes[0].click();  // rst_n = HIGH
        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Press button
        console.log('3. Pressing button...');
        await checkboxes[1].click();  // btn_n = LOW

        // Run for 500 ticks and watch console
        console.log('4. Running 500 ticks...');
        for (let i = 0; i < 500; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        const state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    return {
                        tick: circuit.tick,
                        tx: cell.get('outputSignals')?.tx?._avec?.[0] & cell.get('outputSignals')?.tx?._bvec?.[0],
                        busy: cell.get('outputSignals')?.busy?._avec?.[0] & cell.get('outputSignals')?.busy?._bvec?.[0]
                    };
                }
            }
            return null;
        });
        console.log('\nFinal state:', JSON.stringify(state));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
