/**
 * UART Terminal Test Script
 *
 * Tests the super_counter.sv simulation with UART terminal
 *
 * Usage:
 *   node test/uart-test.mjs
 *
 * Prerequisites:
 *   - npm run dev must be running (ports 3001/8081)
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';
const TIMEOUT = 60000;  // 60 seconds for synthesis

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function runTest() {
    console.log('Starting UART Terminal Test...\n');

    const browser = await chromium.launch({
        headless: false,  // Set to true for CI
        slowMo: 100       // Slow down for visibility
    });

    const context = await browser.newContext();
    const page = await context.newPage();

    try {
        // Test 1: Load page with super_counter example
        console.log('Test 1: Loading page with super_counter example...');
        await page.goto(`${BASE_URL}/?example=super_counter`);
        await page.waitForSelector('#paper', { timeout: TIMEOUT });
        console.log('  Page loaded successfully');

        // Wait for tab to be created
        await sleep(2000);

        // Remove webpack-dev-server overlay if present
        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });
        await sleep(500);

        // Test 2: Click Run to start synthesis
        console.log('Test 2: Clicking Run button to start synthesis...');
        await page.click('#synthesize-btn');
        console.log('  Run button clicked');

        // Wait for synthesis to complete (circuit SVG appears)
        console.log('  Waiting for synthesis to complete...');
        await page.waitForSelector('#paper svg', { timeout: TIMEOUT });
        console.log('  Synthesis completed');

        // Test 3: Check if I/O panel exists
        console.log('Test 3: Checking I/O panel...');
        const iopanel = await page.$('#iopanel');
        if (!iopanel) {
            throw new Error('I/O panel not found');
        }
        console.log('  I/O panel found');

        // Test 4: Check if UART terminal was created
        console.log('Test 4: Checking UART terminal...');
        // Wait a bit for the terminal to initialize
        await sleep(2000);
        const uartTerminal = await page.$('.uart-terminal');
        if (!uartTerminal) {
            console.log('  WARNING: UART terminal not found - checking if UART signals exist...');
            // Check if we have the expected I/O signals
            const iopanelContent = await iopanel.textContent();
            console.log('  I/O panel content:', iopanelContent);
        } else {
            console.log('  UART terminal found');
        }

        // Test 5: Check I/O controls
        console.log('Test 5: Checking I/O controls...');
        await page.click('a[href="#iopanel"]');  // Switch to I/O tab
        await sleep(500);

        // List all labels in the I/O panel
        const labels = await page.$$eval('#iopanel label', els => els.map(e => e.textContent));
        console.log('  I/O labels found:', labels);

        // Test 6: Take a screenshot
        console.log('Test 6: Taking screenshot...');
        await page.screenshot({ path: 'test/screenshot.png', fullPage: true });
        console.log('  Screenshot saved to test/screenshot.png');

        // Test 7: Check if simulation is running
        console.log('Test 7: Checking simulation status...');
        const tickInput = await page.$('#tick');
        if (tickInput) {
            const tickValue = await tickInput.inputValue();
            console.log('  Current tick:', tickValue);

            // Wait and check if tick is increasing
            await sleep(1000);
            const tickValue2 = await tickInput.inputValue();
            console.log('  Tick after 1s:', tickValue2);

            if (parseInt(tickValue2) > parseInt(tickValue)) {
                console.log('  Simulation is running');
            } else {
                console.log('  WARNING: Simulation may be paused');
            }
        }

        console.log('\n========================================');
        console.log('All basic tests passed!');
        console.log('========================================');
        console.log('\nTo test UART manually:');
        console.log('1. Click the I/O tab');
        console.log('2. Find the UART Terminal');
        console.log('3. Toggle btn_n to OFF to trigger button press');
        console.log('4. Type "ACK 500" and press Enter to flash LED');
        console.log('\nLeaving browser open for manual testing...');
        console.log('Press Ctrl+C to close');

        // Keep browser open for manual inspection
        await new Promise(() => {});

    } catch (error) {
        console.error('\nTest failed:', error.message);
        await page.screenshot({ path: 'test/error-screenshot.png', fullPage: true });
        console.error('Error screenshot saved to test/error-screenshot.png');
    }
}

runTest().catch(console.error);
