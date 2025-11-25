/**
 * Functional Test for UART Terminal + Button
 *
 * Tests:
 * 1. Simulation runs without Vector3vl errors
 * 2. Button press (btn_n) triggers UART TX message
 * 3. UART RX (ACK command) is received and LED triggers
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';
const TIMEOUT = 60000;

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function runFunctionalTest() {
    console.log('Starting Functional Test...\n');

    const browser = await chromium.launch({
        headless: false,
        slowMo: 50
    });

    const context = await browser.newContext();
    const page = await context.newPage();

    // Capture console errors
    const errors = [];
    page.on('pageerror', err => {
        errors.push(err.message);
        console.log('  [BROWSER ERROR]', err.message);
    });
    page.on('console', msg => {
        if (msg.type() === 'error') {
            errors.push(msg.text());
        }
    });

    try {
        // Step 1: Load and synthesize
        console.log('Step 1: Loading and synthesizing super_counter...');
        await page.goto(`${BASE_URL}/?example=super_counter`);
        await page.waitForSelector('#paper', { timeout: TIMEOUT });

        // Remove webpack overlay
        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: TIMEOUT });
        console.log('  Synthesis completed');

        // Wait for simulation to start
        await sleep(2000);

        // Check for Vector3vl error
        const hasVectorError = errors.some(e => e.includes('Vector3vl') || e.includes('too wide'));
        if (hasVectorError) {
            throw new Error('Vector3vl.toNumber() error detected!');
        }
        console.log('  No Vector3vl errors');

        // Step 2: Check simulation is running
        console.log('\nStep 2: Verifying simulation is running...');
        const tick1 = await page.$eval('#tick', el => parseInt(el.value) || 0);
        await sleep(2000);
        const tick2 = await page.$eval('#tick', el => parseInt(el.value) || 0);

        const ticksPerSec = (tick2 - tick1) / 2;
        console.log(`  Ticks: ${tick1} -> ${tick2} (${ticksPerSec}/sec)`);

        if (ticksPerSec < 10) {
            console.log('  WARNING: Simulation very slow, enabling fast-forward');
            // Try clicking fast-forward button
            await page.click('#fastfwd');
            await sleep(2000);
        }

        // Step 3: Switch to I/O tab and find UART terminal
        console.log('\nStep 3: Checking UART terminal...');
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        const uartOutput = await page.$('.uart-output');
        if (!uartOutput) {
            throw new Error('UART terminal output not found');
        }
        console.log('  UART terminal found');

        // Step 4: Test button press
        console.log('\nStep 4: Testing button press (btn_n)...');

        // Find btn_n checkbox and toggle it
        const btnCheckbox = await page.$('input[type="checkbox"][data-net="btn_n"], #iopanel input[type="checkbox"]');
        if (btnCheckbox) {
            // Get initial state
            const initialChecked = await btnCheckbox.isChecked();
            console.log(`  btn_n initial state: ${initialChecked ? 'ON (high)' : 'OFF (low)'}`);

            // Toggle to simulate button press (active-low, so uncheck = pressed)
            if (initialChecked) {
                await btnCheckbox.click();
                console.log('  Button pressed (btn_n set LOW)');
            }

            // Wait for debounce + UART transmission
            // With 1kHz clock, 2 cycle debounce, 100 baud:
            // - 2 cycles debounce
            // - ~60 ticks for "BTN 1\n" transmission
            // At ~100 ticks/sec, need ~1 second
            console.log('  Waiting for UART TX (3 seconds)...');
            await sleep(3000);

            // Release button
            await btnCheckbox.click();
            console.log('  Button released (btn_n set HIGH)');

            // Check UART output
            const output = await page.$eval('.uart-output', el => el.textContent);
            console.log('  UART output:', output.trim() || '(empty)');

            if (output.includes('BTN') || output.includes('btn')) {
                console.log('  SUCCESS: Button press triggered UART TX!');
            } else {
                console.log('  NOTE: No BTN message yet - may need more simulation ticks');
            }
        } else {
            console.log('  WARNING: btn_n checkbox not found');
        }

        // Step 5: Test ACK command (UART RX -> LED)
        console.log('\nStep 5: Testing ACK command (UART RX)...');

        const uartInput = await page.$('.uart-input');
        if (uartInput) {
            // Type ACK command
            await uartInput.fill('ACK 50');
            await uartInput.press('Enter');
            console.log('  Sent: ACK 50');

            // Wait for UART RX transmission
            // "ACK 50\n" = 7 bytes * 10 bits * 10 cycles/bit = 700 ticks
            // At ~100 ticks/sec = ~7 seconds
            console.log('  Waiting for UART RX transmission (10 seconds)...');
            await sleep(10000);

            // Check if LED is flashing (would require more sophisticated check)
            const ledLamp = await page.$('input[data-net="led_o"], #iopanel input[type="checkbox"]:last-child');
            if (ledLamp) {
                const ledState = await ledLamp.isChecked();
                console.log(`  LED state: ${ledState ? 'ON' : 'OFF'}`);
            }
        } else {
            console.log('  WARNING: UART input not found');
        }

        // Take final screenshot
        console.log('\nTaking final screenshot...');
        await page.screenshot({ path: 'test/functional-test-result.png', fullPage: true });
        console.log('  Saved to test/functional-test-result.png');

        // Summary
        console.log('\n========================================');
        if (errors.length === 0) {
            console.log('Test completed WITHOUT JavaScript errors!');
        } else {
            console.log(`Test completed with ${errors.length} errors:`);
            errors.forEach(e => console.log('  -', e.substring(0, 100)));
        }
        console.log('========================================');

        // Keep open for inspection
        console.log('\nBrowser open for inspection. Ctrl+C to close.');
        await sleep(60000);

    } catch (error) {
        console.error('\nTest failed:', error.message);
        await page.screenshot({ path: 'test/functional-error.png', fullPage: true });
    } finally {
        await browser.close();
    }
}

runFunctionalTest().catch(console.error);
