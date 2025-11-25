/**
 * Trace subcircuit simulation - capture browser console logs
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Trace subcircuit simulation\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    // Capture all browser console logs
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

        // Set initial state: rst_n=HIGH, btn_n=HIGH
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type=\"checkbox\"]');
        if (!await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = HIGH
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();  // btn_n = HIGH

        console.log('\n2. Running 10 manual ticks...');
        for (let i = 0; i < 10; i++) {
            await page.evaluate(() => window.djCircuit.updateGates());
        }

        console.log('\n3. Check if engine is using SynchEngine:');
        const engineInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            return {
                engineConstructor: circuit._engine.constructor.name,
                hasUpdateSubcircuit: typeof circuit._engine._updateSubcircuit === 'function',
                tick: circuit._engine._tick
            };
        });
        console.log(JSON.stringify(engineInfo, null, 2));

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
