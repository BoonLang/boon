/**
 * Debug UART terminal structure and state
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug UART Terminal Structure\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    // Capture console
    page.on('console', msg => {
        if (msg.text().includes('UART') || msg.text().includes('cyclesPerBit')) {
            console.log('BROWSER:', msg.text());
        }
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        // Get terminal structure
        const terminalInfo = await page.evaluate(() => {
            const terminal = document.querySelector('#uart-terminal');
            if (!terminal) return { error: 'Terminal element not found' };

            // Check if UartTerminal was initialized by looking at the structure
            const output = terminal.querySelector('.uart-output');
            const input = terminal.querySelector('.uart-input');
            const status = terminal.querySelector('.uart-status');

            return {
                html: terminal.innerHTML.slice(0, 1000),
                hasOutput: !!output,
                hasInput: !!input,
                hasStatus: !!status,
                statusTitle: status?.title || null,
                outputChildCount: output?.children?.length || 0,
                outputText: output?.innerText || ''
            };
        });

        console.log('\n2. Terminal structure:');
        console.log(JSON.stringify(terminalInfo, null, 2));

        // Check if UartTerminal class is using correct cyclesPerBit
        console.log('\n3. Running simulation and checking UartTerminal state...');

        await page.evaluate(() => window.djCircuit.stop());
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
        await checkboxes[1].click();

        // Run some ticks and check terminal state
        for (let i = 0; i < 5000; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Check UartTerminal internal state (if accessible)
        const uartTerminalState = await page.evaluate(() => {
            // Try to find the UartTerminal instance
            if (window.uartTerminal) {
                return {
                    cyclesPerBit: window.uartTerminal.cyclesPerBit,
                    txState: window.uartTerminal.txState,
                    txCycleCount: window.uartTerminal.txCycleCount,
                    txBitIndex: window.uartTerminal.txBitIndex,
                    txShiftReg: window.uartTerminal.txShiftReg,
                    txLineBuffer: window.uartTerminal.txLineBuffer,
                    lastTxValue: window.uartTerminal.lastTxValue,
                    lastTick: window.uartTerminal.lastTick,
                    clockPropagation: window.uartTerminal.clockPropagation,
                    ticksPerClockCycle: window.uartTerminal.ticksPerClockCycle
                };
            }
            return { error: 'uartTerminal not exposed on window' };
        });

        console.log('\n4. UartTerminal state:');
        console.log(JSON.stringify(uartTerminalState, null, 2));

        // Check actual TX signal value
        const txSignal = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('net') === 'uart_tx_o' && cell.isOutput) {
                    const signals = cell.get('inputSignals');
                    return {
                        net: cell.get('net'),
                        type: cell.get('type'),
                        inputSignals: signals ? {
                            in_avec: signals.in?._avec?.[0],
                            in_bvec: signals.in?._bvec?.[0],
                            isHigh: signals.in?.isHigh
                        } : null
                    };
                }
            }
            return { error: 'uart_tx_o cell not found' };
        });

        console.log('\n5. TX signal cell:');
        console.log(JSON.stringify(txSignal, null, 2));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
