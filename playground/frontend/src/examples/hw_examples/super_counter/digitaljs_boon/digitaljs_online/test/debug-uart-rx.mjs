/**
 * Debug UART RX state machine - capture browser console output
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug UART RX State Machine\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    // Capture browser console
    page.on('console', msg => {
        const text = msg.text();
        if (text.includes('UART RX')) {
            console.log('BROWSER:', text);
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

        // Set up inputs
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Reset sequence
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();

        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        await checkboxes[0].click();
        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Press button
        console.log('2. Pressing button and running simulation...\n');
        await checkboxes[1].click();

        // Run 25000 ticks (enough for 1 character)
        for (let i = 0; i < 25000; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Check final state
        const finalState = await page.evaluate(() => {
            const ut = window.uartTerminal;
            return {
                tick: window.djCircuit.tick,
                txShiftReg: ut?.txShiftReg,
                txLineBuffer: ut?.txLineBuffer
            };
        });

        console.log('\n3. Final state:');
        console.log(`   tick: ${finalState.tick}`);
        console.log(`   txShiftReg: 0x${finalState.txShiftReg?.toString(16)} = ${finalState.txShiftReg}`);
        console.log(`   txLineBuffer: "${finalState.txLineBuffer}"`);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
