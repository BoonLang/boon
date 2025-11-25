/**
 * Simple Debug - Check actual UART terminal configuration
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function debug() {
    console.log('Simple Debug\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        // Force reload to get latest code
        console.log('1. Loading with cache clear...');
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        // Click synthesize
        console.log('2. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Get terminal info via console
        console.log('3. Checking UART terminal config...');
        const terminalConfig = await page.evaluate(() => {
            // Find the UART terminal instance
            // It should be accessible through the DOM element's data
            const terminalEl = document.querySelector('.uart-terminal');
            if (!terminalEl) return { error: 'No terminal element found' };

            // Try to find through parent
            const container = document.getElementById('uart-terminal-container');
            if (!container) return { error: 'No container found' };

            // The UartTerminal stores itself on the element - check by inspecting
            return {
                terminalFound: !!terminalEl,
                containerFound: !!container,
                containerHtml: container.innerHTML.substring(0, 200)
            };
        });

        console.log('   Terminal found:', terminalConfig.terminalFound);
        console.log('   Container found:', terminalConfig.containerFound);

        // Check if we can access the cycles per bit through the network panel
        // For now, let's just manually inspect by adding some debug output

        // Add debug logging to the page
        await page.evaluate(() => {
            // Find all script modules and log their exports
            console.log('UART Terminal debug:');

            // Check if there's a way to access the terminal
            const scripts = document.querySelectorAll('script');
            console.log('Scripts:', scripts.length);
        });

        // Switch to I/O tab and check checkboxes
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        console.log('4. Setting up I/O...');

        // Set rst_n HIGH
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('   Found checkboxes:', checkboxes.length);

        // Check their states
        for (let i = 0; i < checkboxes.length; i++) {
            const checked = await checkboxes[i].isChecked();
            const label = await checkboxes[i].evaluate(el => {
                const row = el.closest('tr');
                return row?.querySelector('label')?.textContent || `checkbox ${i}`;
            });
            console.log(`   ${label}: ${checked ? 'HIGH' : 'LOW'}`);
        }

        // Set rst_n HIGH (first checkbox)
        if (!await checkboxes[0].isChecked()) {
            console.log('   Setting rst_n HIGH...');
            await checkboxes[0].click();
            await sleep(200);
        }

        // Set btn_n HIGH first (release)
        if (!await checkboxes[1].isChecked()) {
            console.log('   Setting btn_n HIGH (release)...');
            await checkboxes[1].click();
            await sleep(500);
        }

        // Now press button (btn_n LOW)
        console.log('   Pressing button (btn_n LOW)...');
        await checkboxes[1].click();

        // Get tick count
        const tick1 = await page.$eval('#tick', el => parseInt(el.value) || 0);
        console.log('   Tick at press:', tick1);

        // Wait and monitor
        console.log('5. Waiting 10 seconds...');
        for (let i = 0; i < 10; i++) {
            await sleep(1000);
            const tick = await page.$eval('#tick', el => parseInt(el.value) || 0);
            const output = await page.$eval('.uart-output', el => el.textContent.trim());
            console.log(`   [${i+1}s] tick=${tick} output="${output.slice(-30)}"`);
        }

        // Take screenshot
        await page.screenshot({ path: 'test/debug-result.png', fullPage: true });
        console.log('\n6. Screenshot saved');

        // Leave open
        console.log('\nBrowser open. Inspect the console for more debug info.');
        console.log('Press Ctrl+C to close.');
        await sleep(30000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

debug().catch(console.error);
