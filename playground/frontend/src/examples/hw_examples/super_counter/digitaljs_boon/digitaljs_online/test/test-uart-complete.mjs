/**
 * Test complete UART transmission and terminal display
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test Complete UART Transmission\n');

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

        // Run until UART completes (need ~6000 ticks for "BTN 1\n" = 6 chars × ~1000 ticks each)
        console.log('4. Running simulation (10000 ticks)...');

        let lastTx = 1;
        let transitions = 0;

        for (let batch = 0; batch < 100; batch++) {
            for (let i = 0; i < 100; i++) {
                await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            }

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

                // Check terminal
                const terminal = document.querySelector('#uart-terminal');
                let terminalText = '';
                if (terminal) {
                    const serialElem = terminal.querySelector('.xterm-screen');
                    terminalText = serialElem?.innerText || '';
                }

                return {
                    tick: circuit.tick,
                    uartTx,
                    terminalText
                };
            });

            // Track TX transitions (to see data being sent)
            if (state.uartTx?.tx !== lastTx) {
                transitions++;
                lastTx = state.uartTx?.tx;
            }

            if (batch % 10 === 0 || state.terminalText) {
                console.log(`   tick=${state.tick} tx=${state.uartTx?.tx} busy=${state.uartTx?.busy} transitions=${transitions} terminal="${state.terminalText.replace(/\n/g, '\\n')}"`);
            }

            // If terminal has content, we're done!
            if (state.terminalText && state.terminalText.trim()) {
                console.log('\n*** SUCCESS: Terminal received data! ***');
                break;
            }
        }

        // Final check
        const finalState = await page.evaluate(() => {
            const terminal = document.querySelector('#uart-terminal');
            const serialElem = terminal?.querySelector('.xterm-screen');
            return {
                terminalText: serialElem?.innerText || '',
                terminalHTML: terminal?.innerHTML?.slice(0, 500)
            };
        });

        console.log('\n5. Final terminal content:', JSON.stringify(finalState.terminalText));

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
