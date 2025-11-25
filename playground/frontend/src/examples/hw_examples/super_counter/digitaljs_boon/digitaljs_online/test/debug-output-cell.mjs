/**
 * Debug Output cell signals - check if uart_tx_o is receiving data
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug Output Cell Signals\n');

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(3000);

        await page.evaluate(() => window.djCircuit.stop());

        // Find output cells
        console.log('\n2. Finding output cells...');
        const outputCells = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const outputs = [];

            for (const cell of cells) {
                if (cell.isOutput) {
                    const inputSignals = cell.get('inputSignals');
                    outputs.push({
                        id: cell.id,
                        net: cell.get('net'),
                        type: cell.get('type'),
                        hasInputSignals: !!inputSignals,
                        inValue: inputSignals?.in ? (inputSignals.in._avec?.[0] & inputSignals.in._bvec?.[0]) : null,
                        inHigh: inputSignals?.in?.isHigh
                    });
                }
            }
            return outputs;
        });
        console.log(JSON.stringify(outputCells, null, 2));

        // Set up inputs and run reset sequence
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        console.log('\n3. Reset sequence...');
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();  // rst_n = LOW
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click(); // btn_n = HIGH

        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        await checkboxes[0].click();  // rst_n = HIGH
        for (let i = 0; i < 50; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Press button
        console.log('4. Pressing button...');
        await checkboxes[1].click();  // btn_n = LOW

        // Run until UART TX starts
        console.log('5. Running simulation...');
        for (let batch = 0; batch < 20; batch++) {
            for (let i = 0; i < 100; i++) {
                await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            }

            const state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                let uartTxState = null;
                let outputCellState = null;

                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        const out = cell.get('outputSignals');
                        uartTxState = {
                            tx: out?.tx ? (out.tx._avec?.[0] & out.tx._bvec?.[0]) : null,
                            busy: out?.busy ? (out.busy._avec?.[0] & out.busy._bvec?.[0]) : null
                        };
                    }
                    if (cell.get('net') === 'uart_tx_o' && cell.isOutput) {
                        const inp = cell.get('inputSignals');
                        outputCellState = {
                            inValue: inp?.in ? (inp.in._avec?.[0] & inp.in._bvec?.[0]) : null,
                            inHigh: inp?.in?.isHigh
                        };
                    }
                }

                return {
                    tick: circuit.tick,
                    uartTx: uartTxState,
                    outputCell: outputCellState
                };
            });

            console.log(`   Batch ${batch + 1}: tick=${state.tick} uart_tx.tx=${state.uartTx?.tx} uart_tx.busy=${state.uartTx?.busy} outputCell.in=${state.outputCell?.inValue} isHigh=${state.outputCell?.inHigh}`);

            // If busy, check for a few more batches
            if (state.uartTx?.busy === 1 && batch < 5) {
                console.log('   *** UART TX BUSY ***');
            }
        }

        // Check links from uart_tx to output cell
        console.log('\n6. Links from uart_tx tx port...');
        const txLinks = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const links = circuit._graph.getLinks();

            let uartTxId = null;
            for (const cell of cells) {
                if (cell.get('label') === 'u_uart_tx') {
                    uartTxId = cell.id;
                    break;
                }
            }

            const txLinks = links.filter(l => l.get('source')?.id === uartTxId && l.get('source')?.port === 'tx');
            return txLinks.map(l => ({
                source: l.get('source'),
                target: l.get('target'),
                targetNet: circuit._graph.getCell(l.get('target').id)?.get('net'),
                targetType: circuit._graph.getCell(l.get('target').id)?.get('type')
            }));
        });
        console.log(JSON.stringify(txLinks, null, 2));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
