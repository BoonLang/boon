/**
 * Test UART terminal end-to-end with correct timing
 *
 * With clockPropagation=100:
 * - 1 clock cycle = 200 simulation ticks
 * - 10 clock cycles per bit (100 baud / 1000Hz clock)
 * - 1 bit = 2000 simulation ticks
 * - 1 char (10 bits) = 20000 simulation ticks
 *
 * So we need 25000+ ticks for one complete character.
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('UART Terminal End-to-End Test\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        // Check UartTerminal initialization
        const terminalInfo = await page.evaluate(() => {
            if (!window.uartTerminal) return { error: 'uartTerminal not initialized' };
            return {
                cyclesPerBit: window.uartTerminal.cyclesPerBit,
                clockPropagation: window.uartTerminal.clockPropagation,
                ticksPerClockCycle: window.uartTerminal.ticksPerClockCycle,
                txCell: !!window.uartTerminal.txCell,
                rxCell: !!window.uartTerminal.rxCell
            };
        });
        console.log('2. UartTerminal config:', JSON.stringify(terminalInfo));

        if (terminalInfo.cyclesPerBit !== 2000) {
            console.log('WARNING: cyclesPerBit should be 2000, got', terminalInfo.cyclesPerBit);
        }

        await page.evaluate(() => window.djCircuit.stop());

        // Set up inputs
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        console.log('3. Reset sequence...');
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();

        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        await checkboxes[0].click();
        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        console.log('4. Pressing button...');
        await checkboxes[1].click();

        // Run 30000 ticks (enough for 1.5 characters)
        console.log('5. Running 30000 ticks (enough for ~1.5 characters)...');

        const startTime = Date.now();
        for (let batch = 0; batch < 300; batch++) {
            for (let i = 0; i < 100; i++) {
                await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            }

            if (batch % 30 === 0) {
                const state = await page.evaluate(() => {
                    const ut = window.uartTerminal;
                    const output = document.querySelector('#uart-terminal-container .uart-output');

                    return {
                        tick: window.djCircuit.tick,
                        txState: ut?.txState,
                        txBitIndex: ut?.txBitIndex,
                        txCycleCount: ut?.txCycleCount,
                        txShiftReg: ut?.txShiftReg,
                        txLineBuffer: ut?.txLineBuffer,
                        outputText: output?.innerText || '',
                        outputChildCount: output?.children?.length || 0
                    };
                });

                const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                console.log(`   tick=${state.tick} txState=${state.txState} bit=${state.txBitIndex} cycle=${state.txCycleCount} shift=0x${state.txShiftReg?.toString(16)} buffer="${state.txLineBuffer}" output="${state.outputText.replace(/\n/g, '\\n')}" (${elapsed}s)`);

                if (state.outputText && state.outputText.trim()) {
                    console.log('\n*** SUCCESS: Terminal received data! ***');
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
                txState: ut?.txState,
                txLineBuffer: ut?.txLineBuffer,
                outputText: output?.innerText || ''
            };
        });

        console.log('\n6. Final state:');
        console.log('   tick:', finalState.tick);
        console.log('   txState:', finalState.txState);
        console.log('   txLineBuffer:', JSON.stringify(finalState.txLineBuffer));
        console.log('   outputText:', JSON.stringify(finalState.outputText));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
