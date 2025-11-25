/**
 * Async Trace Test - Properly handles DigitalJS async simulation
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Async Trace Test for UART TX\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    // Capture relevant console logs
    page.on('console', msg => {
        const text = msg.text();
        if (text.includes('TRACE') || text.includes('SIGNAL')) {
            console.log('BROWSER:', text);
        }
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });

        // Wait for simulation to start and settle
        await sleep(2000);

        // Stop simulation
        console.log('2. Stopping simulation...');
        await page.evaluate(() => {
            window.djCircuit.stop();
        });
        await sleep(200);

        // Set up tracing hooks
        console.log('3. Setting up signal tracing...');
        await page.evaluate(() => {
            window.signalTrace = [];

            // Find cells we care about
            const cells = window.djCircuit._graph.getCells();
            let debouncerCell = null;
            let btnMessageCell = null;
            let uartTxCell = null;

            for (const cell of cells) {
                const label = cell.get('label') || '';
                if (label.includes('debouncer')) debouncerCell = cell;
                if (label.includes('btn_message')) btnMessageCell = cell;
                if (label.includes('uart_tx') && !label.includes('uart_tx_o')) uartTxCell = cell;
            }

            // Store for later access
            window.traceData = { debouncerCell, btnMessageCell, uartTxCell };

            // Set up monitor for pressed signal
            if (debouncerCell) {
                window.djCircuit.on('postUpdateGates', (tick) => {
                    const pressed = debouncerCell.get('outputSignals')?.pressed?.isHigh;
                    if (pressed) {
                        console.log('TRACE: pressed signal HIGH at tick', tick);
                        window.signalTrace.push({ tick, signal: 'pressed', value: true });
                    }
                });
            }

            console.log('TRACE: Hooks set up. debouncerCell:', !!debouncerCell, 'btnMessageCell:', !!btnMessageCell, 'uartTxCell:', !!uartTxCell);
        });

        // Go to I/O tab
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('4. Found', checkboxes.length, 'checkboxes');

        // Proper reset sequence
        console.log('5. Proper reset sequence...');

        // Set btn_n HIGH first (release button)
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(100);
        }
        console.log('   btn_n = HIGH');

        // Ensure rst_n is LOW (active reset)
        if (await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(100);
        }
        console.log('   rst_n = LOW (reset active)');

        // Start simulation briefly for reset to take effect
        console.log('   Running with reset active...');
        await page.evaluate(() => window.djCircuit.start());
        await sleep(200);
        await page.evaluate(() => window.djCircuit.stop());
        await sleep(100);

        // Release reset
        await checkboxes[0].click();
        console.log('   rst_n = HIGH (reset released)');

        // Run simulation briefly
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Check states after reset
        const afterReset = await page.evaluate(() => {
            const d = window.traceData;
            return {
                tick: window.djCircuit.tick,
                pressed: d.debouncerCell?.get('outputSignals')?.pressed?.isHigh,
                tx_start: d.btnMessageCell?.get('outputSignals')?.tx_start?.isHigh,
                uart_tx: d.uartTxCell?.get('outputSignals')?.tx?.isHigh
            };
        });
        console.log('   After reset: tick=' + afterReset.tick + ' pressed=' + afterReset.pressed + ' tx_start=' + afterReset.tx_start + ' uart_tx=' + afterReset.uart_tx);

        // Now press button
        console.log('\n6. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();  // Set btn_n LOW
        await sleep(100);

        // Verify btn_n changed
        const btnState = await checkboxes[1].isChecked();
        console.log('   btn_n checkbox is now:', btnState ? 'checked (HIGH)' : 'unchecked (LOW)');

        // Run simulation and watch for changes
        console.log('   Starting simulation...');
        await page.evaluate(() => window.djCircuit.start());

        // Monitor for 2 seconds, checking every 100ms
        for (let i = 0; i < 20; i++) {
            await sleep(100);
            const state = await page.evaluate(() => {
                const d = window.traceData;
                const cells = window.djCircuit._graph.getCells();

                // Also check uart_tx_o directly
                let uart_tx_o = null;
                for (const cell of cells) {
                    if (cell.get('net') === 'uart_tx_o') {
                        uart_tx_o = cell.get('inputSignals')?.in?.isHigh;
                    }
                }

                return {
                    tick: window.djCircuit.tick,
                    pressed: d.debouncerCell?.get('outputSignals')?.pressed?.isHigh,
                    tx_start: d.btnMessageCell?.get('outputSignals')?.tx_start?.isHigh,
                    tx_busy: d.uartTxCell?.get('outputSignals')?.busy?.isHigh,
                    uart_tx: d.uartTxCell?.get('outputSignals')?.tx?.isHigh,
                    uart_tx_o,
                    trace: window.signalTrace.length
                };
            });

            // Log every 5th check or if something interesting happens
            if (i % 5 === 0 || state.pressed || state.tx_start || state.tx_busy || !state.uart_tx_o) {
                console.log(`   [${i*100}ms] tick=${state.tick} pressed=${state.pressed} tx_start=${state.tx_start} busy=${state.tx_busy} uart_tx=${state.uart_tx} uart_tx_o=${state.uart_tx_o} traces=${state.trace}`);
            }
        }

        // Check if any trace events were captured
        const traceEvents = await page.evaluate(() => window.signalTrace);
        if (traceEvents.length > 0) {
            console.log('\n   Captured trace events:');
            for (const e of traceEvents) {
                console.log(`     tick ${e.tick}: ${e.signal} = ${e.value}`);
            }
        } else {
            console.log('\n   No trace events captured - pressed signal never went HIGH');
        }

        // Check terminal output
        const output = await page.evaluate(() => {
            return document.querySelector('.uart-output')?.textContent?.trim() || '';
        });
        console.log('\n7. UART Terminal output:', output || '(empty)');

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Take screenshot
        await page.screenshot({ path: 'test/async-trace-result.png', fullPage: true });
        console.log('\n8. Screenshot saved');

        // Investigate debouncer more closely
        console.log('\n9. Investigating debouncer internal state...');
        const debouncerState = await page.evaluate(() => {
            const cells = window.djCircuit._graph.getCells();
            for (const cell of cells) {
                const label = cell.get('label') || '';
                if (label.includes('debouncer')) {
                    return {
                        id: cell.id,
                        label,
                        inputSignals: JSON.stringify(cell.get('inputSignals')),
                        outputSignals: JSON.stringify(cell.get('outputSignals')),
                        graph: cell.get('graph') ? 'has subcircuit graph' : 'no subcircuit graph'
                    };
                }
            }
            return null;
        });

        if (debouncerState) {
            console.log('   Debouncer cell:', debouncerState.id);
            console.log('   Input signals:', debouncerState.inputSignals);
            console.log('   Output signals:', debouncerState.outputSignals);
        }

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
