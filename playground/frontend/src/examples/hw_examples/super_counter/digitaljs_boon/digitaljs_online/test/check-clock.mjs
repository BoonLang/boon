/**
 * Check if clock is toggling properly inside subcircuits
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Check clock toggling in subcircuits\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Reset sequence
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(200);
        await page.evaluate(() => window.djCircuit.stop());
        await checkboxes[0].click();
        await page.evaluate(() => window.djCircuit.start());
        await sleep(200);
        await page.evaluate(() => window.djCircuit.stop());

        console.log('2. Checking clock signal multiple times...');

        // Press button
        await checkboxes[1].click();
        await sleep(50);

        // Start simulation and sample rapidly
        await page.evaluate(() => window.djCircuit.start());

        let clkValues = [];
        for (let i = 0; i < 20; i++) {
            const clk = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        const clkSig = cell.get('inputSignals')?.clk;
                        const a = clkSig?._avec?.[0] ?? clkSig?._avec?.['0'] ?? 0;
                        const b = clkSig?._bvec?.[0] ?? clkSig?._bvec?.['0'] ?? 0;
                        return { tick: circuit.tick, clk: a === b ? a : 'X' };
                    }
                }
                return null;
            });
            clkValues.push(clk);
            await sleep(10);  // Very short delay
        }

        console.log('Clock samples:');
        for (const v of clkValues) {
            console.log(`  tick=${v?.tick} clk=${v?.clk}`);
        }

        // Check if clock changed
        const uniqueClk = [...new Set(clkValues.map(v => v?.clk))];
        console.log('\nUnique clock values:', uniqueClk);

        if (uniqueClk.length > 1) {
            console.log('Clock IS toggling');
        } else {
            console.log('Clock NOT toggling - may be stuck at', uniqueClk[0]);
        }

        // Check if subcircuit has internal state for clock domain
        console.log('\n3. Checking uart_tx internal cells...');
        const internalInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    // Get subcircuit info
                    const subCircuit = cell.get('circuitGraph');
                    if (subCircuit) {
                        const subCells = subCircuit.getCells();
                        const dffs = subCells.filter(c => c.get('type') === 'Dff');
                        return {
                            hasSubCircuit: true,
                            totalCells: subCells.length,
                            dffCount: dffs.length,
                            dffInfo: dffs.slice(0, 3).map(d => ({
                                id: d.id,
                                label: d.get('label'),
                                outputSignals: d.get('outputSignals')
                            }))
                        };
                    }
                    return { hasSubCircuit: false };
                }
            }
            return null;
        });

        console.log('uart_tx internal:', JSON.stringify(internalInfo, null, 2));

        await page.evaluate(() => window.djCircuit.stop());
        await sleep(2000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
