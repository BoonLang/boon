/**
 * Deep trace of uart_tx internal signals
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

function v3vlValue(sig) {
    if (!sig) return 'null';

    // Vector3vl stores values as packed integers in element 0 (or '0')
    const a = sig._avec?.[0] ?? sig._avec?.['0'] ?? 0;
    const b = sig._bvec?.[0] ?? sig._bvec?.['0'] ?? 0;

    if (sig._bits === 1) {
        if (a === 0 && b === 0) return '0';
        if (a === 1 && b === 1) return '1';
        return 'X';
    }

    // Multi-bit: check if any bits are X (a != b means undefined bit)
    if (a !== b) return 'X';

    // Return decimal value (a and b are equal, so just use a)
    return a.toString();
}

async function test() {
    console.log('Deep trace uart_tx signals\n');

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

        // Go to I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Reset sequence
        console.log('\n2. Reset sequence...');
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(100);
        }
        if (await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(100);
        }
        console.log('   rst_n=LOW (active)');

        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        await checkboxes[0].click();
        console.log('   rst_n=HIGH (released)');

        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Get subcircuit info
        console.log('\n3. Subcircuit structures:');
        const info = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const result = {};

            for (const cell of cells) {
                const label = cell.get('label');
                if (label === 'u_uart_tx' || label === 'u_btn_message') {
                    result[label] = {
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
            }
            return result;
        });

        for (const [label, data] of Object.entries(info)) {
            console.log(`\n   ${label}:`);
            console.log('     Inputs:', Object.keys(data.inputSignals || {}));
            console.log('     Outputs:', Object.keys(data.outputSignals || {}));
            if (data.outputSignals?.tx_data) {
                console.log('     tx_data raw:', JSON.stringify(data.outputSignals.tx_data));
            }
        }

        // Press button
        console.log('\n4. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(100);

        // Run one tick at a time and watch signals
        console.log('\n5. Stepping simulation tick by tick...\n');

        for (let i = 0; i < 30; i++) {
            // Run for a short time
            await page.evaluate(() => window.djCircuit.start());
            await sleep(50);
            await page.evaluate(() => window.djCircuit.stop());

            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    const label = cell.get('label');

                    if (label === 'u_uart_tx') {
                        result.uart_tx = {
                            input: cell.get('inputSignals'),
                            output: cell.get('outputSignals')
                        };
                    }
                    if (label === 'u_btn_message') {
                        result.btn_message = {
                            input: cell.get('inputSignals'),
                            output: cell.get('outputSignals')
                        };
                    }
                }

                return result;
            });

            const start = v3vlValue(state.uart_tx?.input?.start);
            const busy = v3vlValue(state.uart_tx?.output?.busy);
            const tx = v3vlValue(state.uart_tx?.output?.tx);
            const data = v3vlValue(state.uart_tx?.input?.data);
            const rst = v3vlValue(state.uart_tx?.input?.rst);
            const clk = v3vlValue(state.uart_tx?.input?.clk);
            const bm_start = v3vlValue(state.btn_message?.output?.tx_start);
            const bm_data = v3vlValue(state.btn_message?.output?.tx_data);

            // Log if something interesting happens
            if (i < 5 || start === '1' || busy === '1' || tx === '0') {
                console.log(`[tick ${state.tick}] bm.out: start=${bm_start} data=${bm_data} | uart.in: start=${start} data=${data} rst=${rst} clk=${clk} | uart.out: busy=${busy} tx=${tx}`);
            }
        }

        // Check terminal
        const output = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent?.trim() || '';
        });
        console.log('\n6. UART Terminal:', output || '(empty)');

        await page.screenshot({ path: 'test/trace-uart-tx-result.png', fullPage: true });
        await sleep(3000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
