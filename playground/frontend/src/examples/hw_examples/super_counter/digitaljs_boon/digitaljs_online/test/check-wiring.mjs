/**
 * Check how clk is wired to subcircuits
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Check clock wiring\n');

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

        await page.evaluate(() => window.djCircuit.stop());

        // Check all cells and their connections
        console.log('2. Checking circuit structure...');
        const structure = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            const result = {
                cells: [],
                clkConnections: []
            };

            for (const cell of cells) {
                const info = {
                    id: cell.id,
                    type: cell.get('type'),
                    label: cell.get('label'),
                    net: cell.get('net'),
                    inputCount: Object.keys(cell.get('inputSignals') || {}).length,
                    outputCount: Object.keys(cell.get('outputSignals') || {}).length
                };

                // Check if this is a subcircuit with clk input
                const inputs = cell.get('inputSignals') || {};
                if (inputs.clk !== undefined) {
                    const clkSig = inputs.clk;
                    const a = clkSig?._avec?.[0] ?? clkSig?._avec?.['0'] ?? 0;
                    const b = clkSig?._bvec?.[0] ?? clkSig?._bvec?.['0'] ?? 0;
                    info.clkValue = a === b ? a : 'X';
                }

                if (info.label || info.net) {
                    result.cells.push(info);
                }

                // Check if this is the clock input
                if (info.net === 'clk') {
                    const outputs = cell.get('outputSignals') || {};
                    const outSig = outputs.out;
                    const a = outSig?._avec?.[0] ?? outSig?._avec?.['0'] ?? 0;
                    const b = outSig?._bvec?.[0] ?? outSig?._bvec?.['0'] ?? 0;
                    info.clkOutputValue = a === b ? a : 'X';
                    result.clkSource = info;
                }
            }

            return result;
        });

        console.log('\nClock source:');
        console.log(JSON.stringify(structure.clkSource, null, 2));

        console.log('\nSubcircuits with clk input:');
        for (const cell of structure.cells) {
            if (cell.clkValue !== undefined) {
                console.log(`  ${cell.label}: type=${cell.type} clkValue=${cell.clkValue}`);
            }
        }

        // Now run simulation and check if clock changes
        console.log('\n3. Running simulation and checking clk...');
        await page.evaluate(() => window.djCircuit.start());

        for (let i = 0; i < 5; i++) {
            await sleep(100);
            const clkInfo = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                const result = { tick: circuit.tick };

                for (const cell of cells) {
                    const net = cell.get('net');
                    const label = cell.get('label');

                    if (net === 'clk') {
                        const outputs = cell.get('outputSignals') || {};
                        const outSig = outputs.out;
                        const a = outSig?._avec?.[0] ?? outSig?._avec?.['0'] ?? 0;
                        result.clkSource = a;
                    }

                    if (label === 'u_uart_tx') {
                        const inputs = cell.get('inputSignals') || {};
                        const clkSig = inputs.clk;
                        const a = clkSig?._avec?.[0] ?? clkSig?._avec?.['0'] ?? 0;
                        result.uart_tx_clk = a;
                    }

                    if (label === 'u_debouncer') {
                        const inputs = cell.get('inputSignals') || {};
                        const clkSig = inputs.clk;
                        const a = clkSig?._avec?.[0] ?? clkSig?._avec?.['0'] ?? 0;
                        result.debouncer_clk = a;
                    }
                }

                return result;
            });

            console.log(`  tick=${clkInfo.tick} clkSource=${clkInfo.clkSource} debouncer_clk=${clkInfo.debouncer_clk} uart_tx_clk=${clkInfo.uart_tx_clk}`);
        }

        await page.evaluate(() => window.djCircuit.stop());
        await sleep(2000);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
