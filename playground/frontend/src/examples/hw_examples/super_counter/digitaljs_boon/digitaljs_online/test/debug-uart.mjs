/**
 * Debug UART Detection
 *
 * This script checks if the UART signals are being correctly detected
 * and monitored by the UART terminal.
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';
const TIMEOUT = 60000;

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function debugUart() {
    console.log('Debug UART Detection\n');

    const browser = await chromium.launch({
        headless: false,
        slowMo: 50
    });

    const context = await browser.newContext();
    const page = await context.newPage();

    try {
        // Load and synthesize
        console.log('1. Loading and synthesizing...');
        await page.goto(`${BASE_URL}/?example=super_counter`);
        await page.waitForSelector('#paper', { timeout: TIMEOUT });

        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: TIMEOUT });
        console.log('   Synthesis completed');

        await sleep(2000);

        // Get circuit info
        console.log('\n2. Checking circuit cells...');
        const circuitInfo = await page.evaluate(() => {
            // Access the circuit from the global window
            const circuit = window.circuit;
            if (!circuit) return { error: 'No circuit found' };

            const cells = circuit._graph.getCells();
            const info = {
                totalCells: cells.length,
                inputs: [],
                outputs: [],
                uartCells: []
            };

            for (const cell of cells) {
                const net = cell.get('net');
                const type = cell.get('type');
                const isInput = cell.isInput;
                const isOutput = cell.isOutput;

                if (isInput) {
                    info.inputs.push({ net, type });
                }
                if (isOutput) {
                    info.outputs.push({ net, type });
                }

                if (net && net.toLowerCase().includes('uart')) {
                    const signals = cell.get('inputSignals') || cell.get('outputSignals');
                    info.uartCells.push({
                        net,
                        type,
                        isInput,
                        isOutput,
                        hasSignals: !!signals,
                        signalKeys: signals ? Object.keys(signals) : []
                    });
                }
            }

            return info;
        });

        console.log('   Total cells:', circuitInfo.totalCells);
        console.log('   Inputs:', circuitInfo.inputs.map(i => i.net).join(', '));
        console.log('   Outputs:', circuitInfo.outputs.map(o => o.net).join(', '));
        console.log('   UART cells:');
        for (const uc of circuitInfo.uartCells) {
            console.log(`     - ${uc.net}: isInput=${uc.isInput}, isOutput=${uc.isOutput}, signals=${uc.signalKeys}`);
        }

        // Check UART terminal
        console.log('\n3. Checking UART terminal...');
        const terminalInfo = await page.evaluate(() => {
            const terminal = window.uartTerminal;
            if (!terminal) return { error: 'No UART terminal found' };

            return {
                hasTxCell: !!terminal.txCell,
                hasRxCell: !!terminal.rxCell,
                txCellNet: terminal.txCell?.get('net'),
                rxCellNet: terminal.rxCell?.get('net'),
                clockHz: terminal.clockHz,
                baudRate: terminal.baudRate,
                cyclesPerBit: terminal.cyclesPerBit,
                txState: terminal.txState,
                rxState: terminal.rxState,
                lastTxValue: terminal.lastTxValue
            };
        });

        if (terminalInfo.error) {
            console.log('   ERROR:', terminalInfo.error);
        } else {
            console.log('   TX cell:', terminalInfo.txCellNet || 'NOT FOUND');
            console.log('   RX cell:', terminalInfo.rxCellNet || 'NOT FOUND');
            console.log('   Clock Hz:', terminalInfo.clockHz);
            console.log('   Baud rate:', terminalInfo.baudRate);
            console.log('   Cycles per bit:', terminalInfo.cyclesPerBit);
            console.log('   TX state:', terminalInfo.txState);
            console.log('   RX state:', terminalInfo.rxState);
            console.log('   Last TX value:', terminalInfo.lastTxValue);
        }

        // Monitor uart_tx_o for a few seconds
        console.log('\n4. Monitoring uart_tx_o signal...');

        // Set up reset and button
        await page.click('a[href="#iopanel"]');
        await sleep(500);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        // rst_n = first, btn_n = second
        await checkboxes[0].evaluate(el => { if (!el.checked) el.click(); });
        await sleep(200);

        console.log('   Releasing button first...');
        await checkboxes[1].evaluate(el => { if (!el.checked) el.click(); });
        await sleep(500);

        console.log('   Pressing button now...');
        await checkboxes[1].evaluate(el => { if (el.checked) el.click(); });

        // Monitor TX signal changes
        for (let i = 0; i < 10; i++) {
            await sleep(500);

            const state = await page.evaluate(() => {
                const terminal = window.uartTerminal;
                if (!terminal || !terminal.txCell) return { error: 'No terminal' };

                const signals = terminal.txCell.get('inputSignals');
                const tick = window.circuit?.tick || 0;

                return {
                    tick,
                    txState: terminal.txState,
                    lastTxValue: terminal.lastTxValue,
                    hasSignals: !!signals,
                    signalIn: signals?.in?.toString(),
                    isHigh: signals?.in?.isHigh
                };
            });

            console.log(`   [${i * 0.5}s] tick=${state.tick} txState=${state.txState} lastTx=${state.lastTxValue} signalIn=${state.signalIn} isHigh=${state.isHigh}`);
        }

        // Check final UART output
        const uartOutput = await page.$eval('.uart-output', el => el.textContent.trim());
        console.log('\n5. UART output:', uartOutput || '(empty)');

        await sleep(5000);

    } catch (error) {
        console.error('\nError:', error.message);
    } finally {
        await browser.close();
    }
}

debugUart().catch(console.error);
