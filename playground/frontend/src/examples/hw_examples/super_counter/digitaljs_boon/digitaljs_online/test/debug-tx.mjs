/**
 * Debug TX Signal
 */

import { chromium } from 'playwright';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function debug() {
    console.log('Debug TX Signal\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    // Capture console logs
    page.on('console', msg => {
        if (msg.text().includes('UART DEBUG')) {
            console.log('BROWSER:', msg.text());
        }
    });

    try {
        await page.goto('http://localhost:3001/?example=super_counter', { waitUntil: 'networkidle' });

        // Wait for any overlays and remove them
        await sleep(2000);
        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
            // Also try removing by iframe selector
            document.querySelectorAll('iframe[src="about:blank"]').forEach(el => el.remove());
        });
        await sleep(500);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Go to I/O tab
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('   Checkboxes found:', checkboxes.length);

        // Get labels
        for (let i = 0; i < checkboxes.length; i++) {
            const label = await checkboxes[i].evaluate(el => {
                const row = el.closest('tr');
                return row?.querySelector('label')?.textContent || `cb${i}`;
            });
            const checked = await checkboxes[i].isChecked();
            console.log(`   ${i}: ${label} = ${checked ? 'HIGH' : 'LOW'}`);
        }

        // IMPORTANT: Set btn_n HIGH BEFORE releasing reset
        // This ensures clean state when circuit starts
        console.log('\n2. Setting btn_n HIGH BEFORE releasing reset...');
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(200);
        }
        console.log('   btn_n is now HIGH (not pressed)');

        // Now release reset
        console.log('\n3. Setting rst_n HIGH (releasing reset)...');
        if (!await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(500);
        }
        console.log('   rst_n is now HIGH, circuit running');

        // Wait for circuit to fully stabilize (important!)
        console.log('   Waiting 3 seconds for circuit to stabilize...');
        await sleep(3000);

        // Check tick
        const tick1 = await page.$eval('#tick', el => parseInt(el.value) || 0);
        console.log('   Tick before press:', tick1);

        // Get current uart_tx_o state
        const txBefore = await checkboxes[3]?.isChecked();
        console.log(`   uart_tx_o before press: ${txBefore ? 'HIGH (idle)' : 'LOW (stuck!)'}`);


        // Make sure btn_n is HIGH before pressing
        const btnState = await checkboxes[1].isChecked();
        console.log(`   btn_n current state: ${btnState ? 'HIGH' : 'LOW'}`);

        if (!btnState) {
            console.log('   btn_n is already LOW, setting HIGH first...');
            await checkboxes[1].click();
            await sleep(500);
        }

        // Press button
        console.log('\n4. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(100);
        console.log('   Button pressed');

        // Wait and check for TX activity
        console.log('\n5. Monitoring for 15 seconds...');
        console.log('   (Looking for UART DEBUG messages in browser console)');

        for (let i = 0; i < 15; i++) {
            await sleep(1000);
            const tick = await page.$eval('#tick', el => parseInt(el.value) || 0);
            const output = await page.$eval('.uart-output', el => el.textContent.trim());

            // Check uart_tx_o checkbox state
            const txState = await checkboxes[3]?.isChecked();

            console.log(`   [${i+1}s] tick=${tick} uart_tx_o=${txState ? 'HIGH' : 'LOW'} output="${output}"`);
        }

        await page.screenshot({ path: 'test/debug-tx-result.png', fullPage: true });
        console.log('\n6. Screenshot saved');

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

debug().catch(console.error);
