/**
 * Test UART transmission with enough ticks for a full character
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test UART (Long Run - 50000 ticks)\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

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

        console.log('3. Pressing button...');
        await checkboxes[1].click();  // btn_n = LOW

        // Run for 50000 ticks (enough for 2+ characters)
        console.log('4. Running 50000 ticks (2+ characters worth)...');

        const startTime = Date.now();
        for (let batch = 0; batch < 500; batch++) {
            for (let i = 0; i < 100; i++) {
                await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            }

            if (batch % 50 === 0) {
                const state = await page.evaluate(() => {
                    const circuit = window.djCircuit;
                    const cells = circuit._graph.getCells();

                    let uartTx = null;
                    for (const cell of cells) {
                        if (cell.get('label') === 'u_uart_tx') {
                            const out = cell.get('outputSignals');
                            uartTx = {
                                tx: out?.tx ? (out.tx._avec?.[0] & out.tx._bvec?.[0]) : null,
                                busy: out?.busy ? (out.busy._avec?.[0] & out.busy._bvec?.[0]) : null
                            };
                            break;
                        }
                    }

                    const terminal = document.querySelector('#uart-terminal');
                    const serialElem = terminal?.querySelector('.xterm-screen');

                    return {
                        tick: circuit.tick,
                        uartTx,
                        terminalText: serialElem?.innerText || ''
                    };
                });

                const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                console.log(`   tick=${state.tick} tx=${state.uartTx?.tx} busy=${state.uartTx?.busy} (${elapsed}s) terminal="${state.terminalText.replace(/\n/g, '\\n')}"`);

                if (state.terminalText && state.terminalText.trim()) {
                    console.log('\n*** SUCCESS: Terminal received data! ***');
                    break;
                }
            }
        }

        // Final check
        const finalState = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const terminal = document.querySelector('#uart-terminal');
            const serialElem = terminal?.querySelector('.xterm-screen');
            return {
                tick: circuit.tick,
                terminalText: serialElem?.innerText || ''
            };
        });

        console.log('\n5. Final state:');
        console.log(`   tick=${finalState.tick}`);
        console.log(`   terminal="${finalState.terminalText}"`);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
