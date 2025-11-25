/**
 * Test UART terminal with super_counter
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test UART Terminal with super_counter\\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    page.on('console', msg => {
        if (msg.type() === 'log') {
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

        console.log('1. Synthesizing super_counter...');
        await page.click('#synthesize-btn');

        try {
            await page.waitForSelector('#paper svg', { timeout: 60000 });
        } catch (e) {
            const errorText = await page.evaluate(() => {
                return document.querySelector('.alert-danger')?.textContent || null;
            });
            if (errorText) {
                console.log('Synthesis error:', errorText);
                await browser.close();
                return;
            }
            throw e;
        }
        await sleep(3000);

        // Stop simulation initially
        await page.evaluate(() => window.djCircuit.stop());

        // Go to I/O panel
        console.log('\\n2. Setting up I/O...');
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        // Find checkboxes for rst_n and btn_n
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('Found', checkboxes.length, 'checkboxes');

        // Reset sequence: rst_n LOW, btn_n HIGH (not pressed)
        console.log('3. Reset sequence...');

        // Assuming: checkbox[0] = rst_n, checkbox[1] = btn_n
        // Initial: rst_n=0 (reset active), btn_n=1 (not pressed)
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();

        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset: rst_n=1
        await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        console.log('   Reset complete');

        // Check UART terminal
        console.log('\\n4. Checking UART terminal...');
        const uartOutput = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('   UART output before button:', JSON.stringify(uartOutput));

        // Press button (btn_n LOW = active)
        console.log('\\n5. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(50);

        // Run simulation for a while to transmit
        console.log('6. Running simulation...');
        await page.evaluate(() => window.djCircuit.start());
        await sleep(5000);  // 5 seconds should be enough for UART TX
        await page.evaluate(() => window.djCircuit.stop());

        // Release button
        await checkboxes[1].click();

        // Check UART terminal again
        const uartOutputAfter = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent || '';
        });
        console.log('\\n7. UART output after button:', JSON.stringify(uartOutputAfter));

        // Check for "BTN" in output
        if (uartOutputAfter.includes('BTN') || uartOutputAfter.includes('B')) {
            console.log('\\n✓ SUCCESS: UART terminal received button press message!');
        } else {
            console.log('\\n✗ FAILED: No UART output detected');

            // Debug: check if TX is working
            const txState = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                for (const cell of cells) {
                    const net = cell.get('net');
                    if (net === 'uart_tx_o') {
                        const inp = cell.get('inputSignals')?.in;
                        return {
                            tick: circuit.tick,
                            tx: inp ? ((inp._avec?.[0] ?? 0) & (inp._bvec?.[0] ?? 0) & 1) : null
                        };
                    }
                }
                return null;
            });
            console.log('TX state:', txState);
        }

        // Keep browser open to inspect
        console.log('\\nKeeping browser open for 10 seconds...');
        await sleep(10000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
