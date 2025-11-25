/**
 * Debug how signals.in.isHigh behaves vs actual bit value
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug Signal Value Types\n');

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

        // Set up inputs
        await page.click('a[href="#iopanel"]');
        await sleep(300);
        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');

        // Reset sequence
        if (await checkboxes[0].isChecked()) await checkboxes[0].click();
        if (!await checkboxes[1].isChecked()) await checkboxes[1].click();

        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }
        await checkboxes[0].click();
        for (let i = 0; i < 100; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
        }

        // Press button
        await checkboxes[1].click();

        // Sample at specific ticks to check isHigh vs actual value
        console.log('2. Comparing isHigh vs actual bit value at key sample points...\n');

        // First find the start tick by running until TX goes low
        let startTick = null;
        for (let i = 0; i < 5000; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));

            const state = await page.evaluate(() => {
                const cells = window.djCircuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('net') === 'uart_tx_o' && cell.isOutput) {
                        const signals = cell.get('inputSignals');
                        const avec = signals.in._avec[0];
                        const bvec = signals.in._bvec[0];
                        return {
                            tick: window.djCircuit.tick,
                            isHigh: signals.in.isHigh,
                            avec,
                            bvec,
                            actualValue: avec & bvec
                        };
                    }
                }
                return null;
            });

            // Detect start bit (transition from HIGH to LOW)
            if (state && state.actualValue === 0 && startTick === null) {
                startTick = state.tick;
                console.log(`Start bit detected at tick ${startTick}`);
                console.log(`  isHigh: ${state.isHigh} (type: ${typeof state.isHigh})`);
                console.log(`  avec: ${state.avec}, bvec: ${state.bvec}, actual: ${state.actualValue}`);
                break;
            }
        }

        if (!startTick) {
            console.log('ERROR: Could not find start bit');
            return;
        }

        // Now sample at each bit's mid-point with cyclesPerBit=2000
        // Mid-start at: startTick + 1000
        // Bit N sampled at: startTick + 1000 + (N+1) * 2000 = startTick + 3000 + N*2000
        console.log('\n3. Sampling at mid-bit points...\n');

        const samplePoints = [];
        for (let bitIdx = 0; bitIdx < 10; bitIdx++) {
            // Mid-point of each bit (including start and stop)
            let targetTick;
            if (bitIdx === 0) {
                targetTick = startTick + 1000; // mid-start
            } else if (bitIdx <= 8) {
                targetTick = startTick + 1000 + bitIdx * 2000; // mid data bits
            } else {
                targetTick = startTick + 1000 + 9 * 2000; // mid stop
            }
            samplePoints.push({ bitIdx: bitIdx === 0 ? 'START' : (bitIdx <= 8 ? bitIdx - 1 : 'STOP'), targetTick });
        }

        // Run simulation and sample at target ticks
        let currentSampleIdx = 0;
        const results = [];

        for (let i = 0; i < 25000 && currentSampleIdx < samplePoints.length; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));

            const currentTick = await page.evaluate(() => window.djCircuit.tick);

            if (currentTick >= samplePoints[currentSampleIdx].targetTick) {
                const state = await page.evaluate(() => {
                    const cells = window.djCircuit._graph.getCells();
                    // Check both Output cell and uart_tx subcircuit
                    let outputCellState = null;
                    let uartTxState = null;

                    for (const cell of cells) {
                        if (cell.get('net') === 'uart_tx_o' && cell.isOutput) {
                            const signals = cell.get('inputSignals');
                            outputCellState = {
                                isHigh: signals.in.isHigh,
                                isHighType: typeof signals.in.isHigh,
                                avec: signals.in._avec[0],
                                bvec: signals.in._bvec[0],
                                actualValue: signals.in._avec[0] & signals.in._bvec[0]
                            };
                        }
                        if (cell.get('label') === 'u_uart_tx') {
                            const out = cell.get('outputSignals');
                            uartTxState = {
                                avec: out?.tx?._avec?.[0],
                                bvec: out?.tx?._bvec?.[0],
                                actualValue: (out?.tx?._avec?.[0] & out?.tx?._bvec?.[0]) ?? null
                            };
                        }
                    }
                    return { tick: window.djCircuit.tick, outputCell: outputCellState, uartTx: uartTxState };
                });

                results.push({
                    bitIdx: samplePoints[currentSampleIdx].bitIdx,
                    targetTick: samplePoints[currentSampleIdx].targetTick,
                    actualTick: state.tick,
                    outputCell: state.outputCell,
                    uartTx: state.uartTx
                });
                currentSampleIdx++;
            }
        }

        console.log('Sample Results:');
        console.log('Bit\tTarget\tActual\tisHigh\tisHighType\tActualVal\tuartTx');
        for (const r of results) {
            console.log(`${r.bitIdx}\t${r.targetTick}\t${r.actualTick}\t${r.outputCell?.isHigh}\t${r.outputCell?.isHighType}\t${r.outputCell?.actualValue}\t\t${r.uartTx?.actualValue}`);
        }

        // Decode based on isHigh
        console.log('\n4. Decode using isHigh:');
        let decodedIsHigh = 0;
        for (let i = 1; i <= 8; i++) {
            if (results[i]?.outputCell?.isHigh === true) {
                decodedIsHigh |= (1 << (i - 1));
            }
        }
        console.log(`   Using isHigh: 0x${decodedIsHigh.toString(16)} = ${decodedIsHigh} = '${String.fromCharCode(decodedIsHigh)}'`);

        // Decode based on actualValue
        console.log('5. Decode using actualValue:');
        let decodedActual = 0;
        for (let i = 1; i <= 8; i++) {
            if (results[i]?.outputCell?.actualValue === 1) {
                decodedActual |= (1 << (i - 1));
            }
        }
        console.log(`   Using actualValue: 0x${decodedActual.toString(16)} = ${decodedActual} = '${String.fromCharCode(decodedActual)}'`);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
