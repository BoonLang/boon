/**
 * Test UART terminal receiving full "BTN 1\n" message
 * Each character needs ~20000 ticks (10 bits * 2000 ticks/bit)
 * "BTN 1\n" = 6 characters = ~120000 ticks
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test Full UART Message\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    // Capture browser console for BYTE RECEIVED messages
    page.on('console', msg => {
        const text = msg.text();
        if (text.includes('BYTE RECEIVED')) {
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
        console.log('2. Reset sequence...');
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
        console.log('3. Pressing button...');
        await checkboxes[1].click();

        // Run 150000 ticks (enough for "BTN 1\n" = 6 chars)
        console.log('4. Running 150000 ticks (enough for 6+ characters)...');

        const startTime = Date.now();
        for (let batch = 0; batch < 1500; batch++) {
            for (let i = 0; i < 100; i++) {
                await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            }

            if (batch % 200 === 0) {
                const state = await page.evaluate(() => {
                    const ut = window.uartTerminal;
                    const output = document.querySelector('#uart-terminal-container .uart-output');
                    return {
                        tick: window.djCircuit.tick,
                        txLineBuffer: ut?.txLineBuffer || '',
                        outputText: output?.innerText || ''
                    };
                });

                const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                console.log(`   tick=${state.tick} buffer="${state.txLineBuffer}" output="${state.outputText.replace(/\n/g, '\\n')}" (${elapsed}s)`);

                // Check for success
                if (state.outputText && state.outputText.includes('BTN')) {
                    console.log('\n*** SUCCESS: Full message received! ***');
                    break;
                }
            }
        }

        // Final state
        const finalState = await page.evaluate(() => {
            const ut = window.uartTerminal;
            const output = document.querySelector('#uart-terminal-container .uart-output');
            return {
                tick: window.djCircuit.tick,
                txLineBuffer: ut?.txLineBuffer || '',
                outputText: output?.innerText || ''
            };
        });

        console.log('\n5. Final state:');
        console.log(`   tick: ${finalState.tick}`);
        console.log(`   txLineBuffer: "${finalState.txLineBuffer}"`);
        console.log(`   outputText: "${finalState.outputText}"`);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
