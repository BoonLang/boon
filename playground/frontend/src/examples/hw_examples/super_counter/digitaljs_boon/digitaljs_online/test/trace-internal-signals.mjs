/**
 * Trace Internal Signals - Debug debouncer and btn_message
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Trace Internal Signals\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop and analyze circuit
        console.log('2. Analyzing synthesized circuit...\n');
        const analysis = await page.evaluate(() => {
            const circuit = window.djCircuit;
            circuit.stop();

            const cells = circuit._graph.getCells();
            const result = {
                subcircuits: [],
                wires: [],
                inputs: [],
                outputs: []
            };

            for (const cell of cells) {
                const type = cell.get('type');
                const id = cell.id;
                const net = cell.get('net');
                const label = cell.get('label');

                if (type === 'Subcircuit') {
                    const inputSignals = cell.get('inputSignals') || {};
                    const outputSignals = cell.get('outputSignals') || {};
                    result.subcircuits.push({
                        id,
                        label,
                        inputSignals: Object.keys(inputSignals),
                        outputSignals: Object.keys(outputSignals)
                    });
                } else if (cell.isInput) {
                    result.inputs.push({ id, net, type });
                } else if (cell.isOutput) {
                    result.outputs.push({ id, net, type });
                }
            }

            return result;
        });

        console.log('Inputs:', analysis.inputs.map(i => `${i.net}(${i.id})`).join(', '));
        console.log('Outputs:', analysis.outputs.map(o => `${o.net}(${o.id})`).join(', '));
        console.log('\nSubcircuits:');
        for (const sc of analysis.subcircuits) {
            console.log(`  ${sc.label || sc.id}: inputs=[${sc.inputSignals.join(',')}] outputs=[${sc.outputSignals.join(',')}]`);
        }

        // Find debouncer and trace signals
        console.log('\n3. Looking for debouncer signals...');
        const debouncerInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            // Find subcircuit that looks like debouncer
            for (const cell of cells) {
                const label = cell.get('label') || '';
                if (label.includes('debouncer') || cell.id.includes('debouncer')) {
                    return {
                        id: cell.id,
                        label,
                        inputSignals: cell.get('inputSignals'),
                        outputSignals: cell.get('outputSignals')
                    };
                }
            }

            // Try to find by port names
            for (const cell of cells) {
                const outputSignals = cell.get('outputSignals') || {};
                if ('pressed' in outputSignals) {
                    return {
                        id: cell.id,
                        label: cell.get('label'),
                        inputSignals: cell.get('inputSignals'),
                        outputSignals
                    };
                }
            }

            return null;
        });

        if (debouncerInfo) {
            console.log('   Found debouncer:', debouncerInfo.id);
            console.log('   Input signals:', JSON.stringify(debouncerInfo.inputSignals));
            console.log('   Output signals:', JSON.stringify(debouncerInfo.outputSignals));
        } else {
            console.log('   Debouncer not found!');
        }

        // Go to I/O and set up proper reset
        console.log('\n4. Setting up I/O...');
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Proper reset: rst_n LOW first
        if (await checkboxes[0].isChecked()) {
            await checkboxes[0].click();  // Set rst_n LOW
        }
        // btn_n HIGH
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();  // Set btn_n HIGH
        }

        // Run with reset active
        await page.evaluate(() => {
            for (let i = 0; i < 10; i++) window.djCircuit.updateGates();
        });

        // Release reset
        await checkboxes[0].click();  // rst_n HIGH
        await sleep(100);

        // Run a few ticks
        await page.evaluate(() => {
            for (let i = 0; i < 20; i++) window.djCircuit.updateGates();
        });

        console.log('   Circuit properly reset');

        // Now trace signal path as we press button
        console.log('\n5. Tracing signal path during button press...\n');

        // Get current states before press
        const beforePress = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            const result = { tick: circuit.tick };

            // Find key cells and their states
            for (const cell of cells) {
                const net = cell.get('net');
                const outputSignals = cell.get('outputSignals') || {};
                const inputSignals = cell.get('inputSignals') || {};

                // btn_n input
                if (net === 'btn_n') {
                    result.btn_n = outputSignals.out?.isHigh;
                }

                // uart_tx_o output
                if (net === 'uart_tx_o') {
                    result.uart_tx_o = inputSignals.in?.isHigh;
                }

                // Check for debouncer (has 'pressed' output)
                if ('pressed' in outputSignals) {
                    result.debouncer_pressed = outputSignals.pressed?.isHigh;
                    result.debouncer_btn_n = inputSignals.btn_n?.isHigh;
                }

                // Check for btn_message (has 'tx_start' output)
                if ('tx_start' in outputSignals) {
                    result.btn_message_tx_start = outputSignals.tx_start?.isHigh;
                    result.btn_message_btn_pressed = inputSignals.btn_pressed?.isHigh;
                }

                // Check for uart_tx (has 'tx' output)
                if ('tx' in outputSignals && 'start' in inputSignals) {
                    result.uart_tx_tx = outputSignals.tx?.isHigh;
                    result.uart_tx_start = inputSignals.start?.isHigh;
                    result.uart_tx_busy = outputSignals.busy?.isHigh;
                }
            }

            return result;
        });

        console.log('Before button press:');
        console.log('  tick:', beforePress.tick);
        console.log('  btn_n (input):', beforePress.btn_n);
        console.log('  debouncer.btn_n:', beforePress.debouncer_btn_n);
        console.log('  debouncer.pressed:', beforePress.debouncer_pressed);
        console.log('  btn_message.btn_pressed:', beforePress.btn_message_btn_pressed);
        console.log('  btn_message.tx_start:', beforePress.btn_message_tx_start);
        console.log('  uart_tx.start:', beforePress.uart_tx_start);
        console.log('  uart_tx.busy:', beforePress.uart_tx_busy);
        console.log('  uart_tx.tx:', beforePress.uart_tx_tx);
        console.log('  uart_tx_o (output):', beforePress.uart_tx_o);

        // Press button
        console.log('\n--- Pressing button (btn_n LOW) ---\n');
        await checkboxes[1].click();  // btn_n LOW
        await sleep(50);

        // Run ticks one at a time and check for changes
        console.log('Tracing tick by tick:');
        for (let i = 0; i < 30; i++) {
            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                circuit.updateGates();

                const cells = circuit._graph.getCells();
                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    const net = cell.get('net');
                    const outputSignals = cell.get('outputSignals') || {};
                    const inputSignals = cell.get('inputSignals') || {};

                    if (net === 'btn_n') result.btn_n = outputSignals.out?.isHigh ? 1 : 0;
                    if (net === 'uart_tx_o') result.uart_tx_o = inputSignals.in?.isHigh ? 1 : 0;
                    if ('pressed' in outputSignals) {
                        result.pressed = outputSignals.pressed?.isHigh ? 1 : 0;
                    }
                    if ('tx_start' in outputSignals) {
                        result.tx_start = outputSignals.tx_start?.isHigh ? 1 : 0;
                    }
                    if ('tx' in outputSignals && 'start' in inputSignals) {
                        result.uart_tx = outputSignals.tx?.isHigh ? 1 : 0;
                        result.uart_busy = outputSignals.busy?.isHigh ? 1 : 0;
                    }
                }

                return result;
            });

            // Only print if something changed or at key points
            if (i < 10 || state.pressed || state.tx_start || !state.uart_tx) {
                console.log(`  tick ${state.tick}: btn_n=${state.btn_n} pressed=${state.pressed} tx_start=${state.tx_start} busy=${state.uart_busy} tx=${state.uart_tx}`);
            }
        }

        // Take screenshot
        await page.screenshot({ path: 'test/trace-internal-result.png', fullPage: true });
        console.log('\n6. Screenshot saved');

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
