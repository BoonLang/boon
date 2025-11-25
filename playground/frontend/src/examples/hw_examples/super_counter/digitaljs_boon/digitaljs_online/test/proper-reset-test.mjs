/**
 * Proper Reset Test - Ensures circuit is properly reset before testing TX
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Proper Reset Test for UART TX\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    // Capture console logs
    page.on('console', msg => {
        const text = msg.text();
        if (text.includes('UART') || text.includes('tick') || text.includes('TX') || text.includes('reset')) {
            console.log('BROWSER:', text);
        }
    });

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        // Remove overlay
        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop simulation immediately
        console.log('2. Stopping simulation...');
        await page.evaluate(() => {
            if (window.djCircuit) {
                window.djCircuit.stop();
                console.log('Simulation stopped at tick:', window.djCircuit.tick);
            }
        });

        // Go to I/O tab
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('3. Found', checkboxes.length, 'checkboxes');

        // Log initial states
        for (let i = 0; i < checkboxes.length; i++) {
            const label = await checkboxes[i].evaluate(el => {
                const row = el.closest('tr');
                return row?.querySelector('label')?.textContent || `cb${i}`;
            });
            const checked = await checkboxes[i].isChecked();
            console.log(`   [${i}] ${label}: ${checked ? 'HIGH' : 'LOW'}`);
        }

        // Find uart_tx_o and check initial value
        console.log('\n4. Checking uart_tx_o initial value...');
        const txInitial = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                const net = cell.get('net');
                if (net === 'uart_tx_o') {
                    const sig = cell.get('inputSignals');
                    console.log('uart_tx_o cell found, inputSignals:', JSON.stringify(sig));
                    return sig?.in?.isHigh;
                }
            }
            return null;
        });
        console.log('   uart_tx_o initial:', txInitial === true ? 'HIGH' : txInitial === false ? 'LOW' : 'undefined');

        // IMPORTANT: Ensure proper reset sequence
        console.log('\n5. Proper reset sequence:');

        // Step 5a: Set rst_n LOW (active reset)
        console.log('   5a. Setting rst_n LOW (activating reset)...');
        if (await checkboxes[0].isChecked()) {
            await checkboxes[0].click();  // Uncheck to set LOW
            await sleep(100);
        }
        console.log('       rst_n is now LOW (reset active)');

        // Also set btn_n HIGH (button released) to avoid spurious presses
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();  // Check to set HIGH
            await sleep(100);
        }
        console.log('       btn_n is now HIGH (button released)');

        // Step 5b: Run a few ticks with reset active
        console.log('   5b. Running 10 ticks with reset active...');
        const afterResetActive = await page.evaluate(() => {
            const circuit = window.djCircuit;
            for (let i = 0; i < 10; i++) {
                circuit.updateGates();
            }
            // Check uart_tx_o
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                const net = cell.get('net');
                if (net === 'uart_tx_o') {
                    const sig = cell.get('inputSignals');
                    return { tick: circuit.tick, isHigh: sig?.in?.isHigh };
                }
            }
            return { tick: circuit.tick, isHigh: null };
        });
        console.log(`       After reset active: tick=${afterResetActive.tick}, uart_tx_o=${afterResetActive.isHigh ? 'HIGH' : 'LOW'}`);

        // Step 5c: Release reset (rst_n HIGH)
        console.log('   5c. Releasing reset (rst_n HIGH)...');
        await checkboxes[0].click();  // Check to set HIGH
        await sleep(100);
        console.log('       rst_n is now HIGH (reset released)');

        // Step 5d: Run more ticks and check TX state
        console.log('   5d. Running 20 ticks after reset release...');
        const afterResetRelease = await page.evaluate(() => {
            const circuit = window.djCircuit;
            for (let i = 0; i < 20; i++) {
                circuit.updateGates();
            }
            // Check uart_tx_o
            const cells = circuit._graph.getCells();
            for (const cell of cells) {
                const net = cell.get('net');
                if (net === 'uart_tx_o') {
                    const sig = cell.get('inputSignals');
                    return { tick: circuit.tick, isHigh: sig?.in?.isHigh };
                }
            }
            return { tick: circuit.tick, isHigh: null };
        });
        console.log(`       After reset release: tick=${afterResetRelease.tick}, uart_tx_o=${afterResetRelease.isHigh ? 'HIGH' : 'LOW'}`);

        // Check if TX is now HIGH (correct idle state)
        if (afterResetRelease.isHigh === true) {
            console.log('\n   SUCCESS: uart_tx_o is HIGH after proper reset!');
        } else {
            console.log('\n   ISSUE: uart_tx_o is still LOW after reset - investigating...');
        }

        // Now test button press
        console.log('\n6. Testing button press (btn_n LOW)...');
        await checkboxes[1].click();  // Uncheck to set LOW (press button)
        await sleep(100);

        // Run ticks and monitor TX
        console.log('   Monitoring TX for 200 ticks after button press:');
        let txWentLow = false;
        let txTransitions = 0;
        let lastTx = afterResetRelease.isHigh;

        for (let batch = 0; batch < 20; batch++) {
            const result = await page.evaluate(() => {
                const circuit = window.djCircuit;
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                circuit.updateGates();
                const cells = circuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('net') === 'uart_tx_o') {
                        const sig = cell.get('inputSignals');
                        return { tick: circuit.tick, isHigh: sig?.in?.isHigh };
                    }
                }
                return { tick: circuit.tick, isHigh: null };
            });

            if (result.isHigh !== lastTx) {
                txTransitions++;
                console.log(`   [tick ${result.tick}] TX transitioned to ${result.isHigh ? 'HIGH' : 'LOW'}`);
                lastTx = result.isHigh;
            }
            if (result.isHigh === false) {
                txWentLow = true;
            }
        }

        if (txWentLow || txTransitions > 0) {
            console.log(`\n   TX showed activity! Transitions: ${txTransitions}`);
        } else {
            console.log('\n   TX showed no activity - TX path may still be broken');
        }

        // Resume simulation and check terminal output
        console.log('\n7. Resuming simulation for 5 seconds...');
        await page.evaluate(() => window.djCircuit.start());

        for (let i = 0; i < 5; i++) {
            await sleep(1000);
            const result = await page.evaluate(() => {
                const output = document.querySelector('.uart-output')?.textContent?.trim() || '';
                return {
                    tick: window.djCircuit.tick,
                    output
                };
            });
            console.log(`   [${i+1}s] tick=${result.tick} output="${result.output}"`);
        }

        // Take screenshot
        await page.screenshot({ path: 'test/proper-reset-result.png', fullPage: true });
        console.log('\n8. Screenshot saved');

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
