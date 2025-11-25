/**
 * Trace btn_message and uart_tx signals
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Trace btn_message and uart_tx\n');

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

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Go to I/O and set up
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('Found', checkboxes.length, 'checkboxes');

        // Proper reset: btn_n HIGH first, then rst_n cycling
        console.log('\n2. Reset sequence...');

        // btn_n HIGH (released)
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(100);
        }
        console.log('   btn_n = HIGH');

        // rst_n LOW (reset active)
        if (await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(100);
        }
        console.log('   rst_n = LOW (reset active)');

        // Run with reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Release reset
        await checkboxes[0].click();
        console.log('   rst_n = HIGH (reset released)');

        // Run after reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Check all subcircuit states
        console.log('\n3. Checking subcircuit states...');
        const states = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            const result = { tick: circuit.tick };

            for (const cell of cells) {
                const label = cell.get('label');
                const net = cell.get('net');

                if (label === 'u_debouncer') {
                    result.debouncer = {
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
                if (label === 'u_btn_message') {
                    result.btn_message = {
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
                if (label === 'u_uart_tx') {
                    result.uart_tx = {
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
                if (net === 'uart_tx_o') {
                    result.uart_tx_o = cell.get('inputSignals');
                }
            }

            return result;
        });

        console.log('   tick:', states.tick);
        console.log('   debouncer.pressed:', JSON.stringify(states.debouncer?.outputSignals?.pressed));
        console.log('   btn_message.btn_pressed:', JSON.stringify(states.btn_message?.inputSignals?.btn_pressed));
        console.log('   btn_message.tx_start:', JSON.stringify(states.btn_message?.outputSignals?.tx_start));
        console.log('   btn_message.tx_data:', JSON.stringify(states.btn_message?.outputSignals?.tx_data));
        console.log('   uart_tx.start:', JSON.stringify(states.uart_tx?.inputSignals?.start));
        console.log('   uart_tx.busy:', JSON.stringify(states.uart_tx?.outputSignals?.busy));
        console.log('   uart_tx.tx:', JSON.stringify(states.uart_tx?.outputSignals?.tx));
        console.log('   uart_tx_o:', JSON.stringify(states.uart_tx_o));

        // Press button
        console.log('\n4. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(100);

        // Run and monitor
        console.log('5. Running and monitoring signals...');
        await page.evaluate(() => window.djCircuit.start());

        for (let i = 0; i < 20; i++) {
            await sleep(100);
            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    const label = cell.get('label');
                    const net = cell.get('net');

                    if (label === 'u_debouncer') {
                        const p = cell.get('outputSignals')?.pressed;
                        result.pressed = p?._avec?.[0] === 1 && p?._bvec?.[0] === 1 ? 1 : 0;
                    }
                    if (label === 'u_btn_message') {
                        const bp = cell.get('inputSignals')?.btn_pressed;
                        const ts = cell.get('outputSignals')?.tx_start;
                        result.btn_pressed_in = bp?._avec?.[0] === 1 && bp?._bvec?.[0] === 1 ? 1 : 0;
                        result.tx_start = ts?._avec?.[0] === 1 && ts?._bvec?.[0] === 1 ? 1 : 0;
                    }
                    if (label === 'u_uart_tx') {
                        const busy = cell.get('outputSignals')?.busy;
                        const tx = cell.get('outputSignals')?.tx;
                        result.busy = busy?._avec?.[0] === 1 && busy?._bvec?.[0] === 1 ? 1 : 0;
                        result.tx = tx?._avec?.[0] === 1 && tx?._bvec?.[0] === 1 ? 1 : 0;
                    }
                    if (net === 'uart_tx_o') {
                        const inp = cell.get('inputSignals')?.in;
                        result.tx_o = inp?._avec?.[0] === 1 && inp?._bvec?.[0] === 1 ? 1 : 0;
                    }
                }

                return result;
            });

            if (i % 5 === 0 || state.pressed || state.tx_start || state.busy || !state.tx_o) {
                console.log(`   [${i*100}ms] tick=${state.tick} pressed=${state.pressed} btn_pressed_in=${state.btn_pressed_in} tx_start=${state.tx_start} busy=${state.busy} uart_tx=${state.tx} uart_tx_o=${state.tx_o}`);
            }
        }

        await page.evaluate(() => window.djCircuit.stop());

        // Check terminal output
        const output = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent?.trim() || '';
        });
        console.log('\n6. UART Terminal output:', output || '(empty)');

        await page.screenshot({ path: 'test/trace-btn-message-result.png', fullPage: true });
        console.log('7. Screenshot saved');

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
