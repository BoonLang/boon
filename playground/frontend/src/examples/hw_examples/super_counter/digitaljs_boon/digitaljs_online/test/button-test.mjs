/**
 * Button Press Test - Tests btn_n -> UART TX flow
 *
 * The button is active-low, so:
 * - Checked (HIGH) = NOT pressed
 * - Unchecked (LOW) = pressed
 *
 * The debouncer detects the falling edge (HIGH -> LOW transition)
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';
const TIMEOUT = 60000;

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function runButtonTest() {
    console.log('Button Press Test\n');
    console.log('Testing: btn_n (active-low) -> debouncer -> btn_message -> uart_tx -> UART Terminal\n');

    const browser = await chromium.launch({
        headless: false,
        slowMo: 50
    });

    const context = await browser.newContext();
    const page = await context.newPage();

    // Capture console errors
    const errors = [];
    page.on('pageerror', err => errors.push(err.message));

    try {
        // Load and synthesize
        console.log('1. Loading and synthesizing...');
        await page.goto(`${BASE_URL}/?example=super_counter`);
        await page.waitForSelector('#paper', { timeout: TIMEOUT });

        // Remove webpack overlay
        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: TIMEOUT });
        console.log('   Synthesis completed');

        // Wait for simulation to stabilize
        await sleep(2000);

        // Check for errors
        if (errors.some(e => e.includes('Vector3vl'))) {
            throw new Error('Vector3vl error detected!');
        }
        console.log('   No Vector3vl errors');

        // Switch to I/O tab
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        // First ensure rst_n is HIGH (not in reset)
        console.log('\n2. Setting up reset state...');
        const allCheckboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Find rst_n (first checkbox)
        let rstCheckbox = null;
        for (const cb of allCheckboxes) {
            const label = await cb.evaluate(el => {
                const row = el.closest('tr');
                if (row) {
                    const labelEl = row.querySelector('label');
                    return labelEl ? labelEl.textContent : '';
                }
                return '';
            });
            if (label.includes('rst_n')) {
                rstCheckbox = cb;
                break;
            }
        }

        if (!rstCheckbox) {
            rstCheckbox = allCheckboxes[0];
            console.log('   Using first checkbox (assuming rst_n)');
        } else {
            console.log('   Found rst_n checkbox');
        }

        const rstState = await rstCheckbox.isChecked();
        console.log(`   rst_n state: ${rstState ? 'HIGH (not reset)' : 'LOW (in reset!)'}`);

        if (!rstState) {
            console.log('   Setting rst_n HIGH (releasing reset)...');
            await rstCheckbox.click();
            await sleep(500);
            const newRstState = await rstCheckbox.isChecked();
            console.log(`   rst_n is now: ${newRstState ? 'HIGH (not reset)' : 'LOW (in reset!)'}`);
        }

        // Find btn_n checkbox
        console.log('\n3. Setting up button state...');

        // Get all checkboxes in iopanel
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log(`   Found ${checkboxes.length} checkboxes`);

        // Find btn_n specifically (should be second after rst_n)
        // Order: rst_n, btn_n, uart_rx_i, uart_tx_o, led_o
        let btnCheckbox = null;
        for (const cb of checkboxes) {
            const label = await cb.evaluate(el => {
                const row = el.closest('tr');
                if (row) {
                    const labelEl = row.querySelector('label');
                    return labelEl ? labelEl.textContent : '';
                }
                return '';
            });
            if (label.includes('btn_n')) {
                btnCheckbox = cb;
                break;
            }
        }

        if (!btnCheckbox) {
            // Fallback: use second checkbox
            btnCheckbox = checkboxes[1];
            console.log('   Using second checkbox (assuming btn_n)');
        } else {
            console.log('   Found btn_n checkbox');
        }

        // Check initial state
        const initialState = await btnCheckbox.isChecked();
        console.log(`   Initial btn_n state: ${initialState ? 'HIGH (not pressed)' : 'LOW (pressed)'}`);

        // Ensure button is NOT pressed first (set HIGH)
        if (!initialState) {
            console.log('   Setting btn_n HIGH (releasing button)...');
            await btnCheckbox.click();
            await sleep(500);
        }

        // Verify it's HIGH
        const stateAfterRelease = await btnCheckbox.isChecked();
        console.log(`   btn_n is now: ${stateAfterRelease ? 'HIGH (not pressed)' : 'LOW (pressed)'}`);

        // Get current tick
        const tickBefore = await page.$eval('#tick', el => parseInt(el.value) || 0);
        console.log(`   Current tick: ${tickBefore}`);

        // Clear UART output first
        const uartOutput = await page.$('.uart-output');
        const initialOutput = await uartOutput?.evaluate(el => el.textContent.trim()) || '';
        console.log(`   Initial UART output: "${initialOutput}"`);

        // Press the button (set LOW)
        console.log('\n4. Pressing button (btn_n -> LOW)...');
        await btnCheckbox.click();
        await sleep(100);

        const stateAfterPress = await btnCheckbox.isChecked();
        console.log(`   btn_n is now: ${stateAfterPress ? 'HIGH (not pressed)' : 'LOW (pressed)'}`);

        // Wait for debounce (2 cycles) and UART TX
        // "BTN 1\n" = 6 bytes * 10 bits * 10 cycles/bit = 600 cycles
        // At 100 ticks/sec, ~6 seconds
        console.log('   Waiting for UART TX (8 seconds)...');

        // Check progress every second
        for (let i = 1; i <= 8; i++) {
            await sleep(1000);
            const currentTick = await page.$eval('#tick', el => parseInt(el.value) || 0);
            const output = await uartOutput?.evaluate(el => el.textContent.trim()) || '';
            console.log(`   [${i}s] Tick: ${currentTick}, Output: "${output.slice(-50)}"`);

            if (output.includes('BTN')) {
                console.log('\n   SUCCESS: Button message detected!');
                break;
            }
        }

        // Release button
        console.log('\n5. Releasing button (btn_n -> HIGH)...');
        await btnCheckbox.click();

        // Final check
        const finalOutput = await uartOutput?.evaluate(el => el.textContent.trim()) || '';
        console.log(`\n6. Final UART output: "${finalOutput}"`);

        if (finalOutput.includes('BTN')) {
            console.log('\n========================================');
            console.log('SUCCESS: Button press generated UART TX!');
            console.log('========================================');
        } else {
            console.log('\n========================================');
            console.log('WARNING: No BTN message in UART output');
            console.log('========================================');
        }

        // Take screenshot
        await page.screenshot({ path: 'test/button-test-result.png', fullPage: true });
        console.log('\nScreenshot saved to test/button-test-result.png');

        // Report errors
        if (errors.length > 0) {
            console.log('\nBrowser errors:');
            errors.forEach(e => console.log('  -', e.substring(0, 100)));
        }

        // Keep open briefly
        await sleep(5000);

    } catch (error) {
        console.error('\nTest failed:', error.message);
        await page.screenshot({ path: 'test/button-error.png', fullPage: true });
    } finally {
        await browser.close();
    }
}

runButtonTest().catch(console.error);
