/**
 * Debug UART bit timing - track exact TX transitions
 */
import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug UART Bit Timing\n');

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

        // Track TX transitions for first character
        console.log('2. Tracking TX transitions...\n');

        let lastTx = null;
        let transitions = [];
        let startTick = null;

        for (let tick = 0; tick < 25000; tick++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));

            const state = await page.evaluate(() => {
                const cells = window.djCircuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('label') === 'u_uart_tx') {
                        const out = cell.get('outputSignals');
                        return {
                            tick: window.djCircuit.tick,
                            tx: out?.tx ? (out.tx._avec?.[0] & out.tx._bvec?.[0]) : null,
                            busy: out?.busy ? (out.busy._avec?.[0] & out.busy._bvec?.[0]) : null
                        };
                    }
                }
                return null;
            });

            if (state && state.tx !== lastTx) {
                if (lastTx !== null) {
                    const delta = startTick !== null ? state.tick - startTick : 0;
                    transitions.push({
                        tick: state.tick,
                        tx: state.tx,
                        delta: transitions.length > 0 ? state.tick - transitions[transitions.length - 1].tick : 0
                    });
                    if (state.tx === 0 && lastTx === 1 && startTick === null) {
                        startTick = state.tick;
                        console.log(`START bit falling edge at tick ${state.tick}`);
                    }
                }
                lastTx = state.tx;
            }

            // Stop after we've seen the stop bit (tx goes back to 1 after being low for the character)
            if (transitions.length > 0 && state?.busy === 0) {
                break;
            }
        }

        console.log('\nTransitions (showing delta ticks between each):');
        for (let i = 0; i < Math.min(transitions.length, 20); i++) {
            const t = transitions[i];
            console.log(`  tick=${t.tick} tx=${t.tx} delta=${t.delta} ticks`);
        }

        // Analyze timing
        if (transitions.length > 2) {
            const deltas = transitions.slice(1).map(t => t.delta);
            const avgDelta = deltas.reduce((a, b) => a + b, 0) / deltas.length;
            console.log(`\nAverage ticks between transitions: ${avgDelta.toFixed(1)}`);
            console.log(`Expected (2000 ticks/bit): varies based on bit pattern`);
        }

        // Now decode what the FPGA actually sent by sampling at mid-bit
        // First find the start bit
        const startEdge = transitions.find(t => t.tx === 0);
        if (startEdge) {
            console.log('\n3. Manual bit decode (sampling at mid-bit)...');
            console.log(`   Start bit at tick ${startEdge.tick}`);

            // Sample mid-points (assuming 2000 ticks per bit)
            // Start bit mid-point: startEdge.tick + 1000
            // Bit 0 mid-point: startEdge.tick + 3000 (1000 + 2000)
            // Bit 1 mid-point: startEdge.tick + 5000 (1000 + 2000 + 2000)
            // etc.

            const bits = [];
            for (let bitIdx = 0; bitIdx < 8; bitIdx++) {
                const midPoint = startEdge.tick + 1000 + (bitIdx + 1) * 2000;
                // Find the TX value at this tick from transitions
                let txValue = 1; // idle
                for (const t of transitions) {
                    if (t.tick <= midPoint) {
                        txValue = t.tx;
                    } else {
                        break;
                    }
                }
                bits.push(txValue);
                console.log(`   Bit ${bitIdx} @ tick ${midPoint}: ${txValue}`);
            }

            const byteValue = bits.reduce((acc, bit, idx) => acc | (bit << idx), 0);
            console.log(`   Decoded byte: 0x${byteValue.toString(16)} = ${byteValue} = '${String.fromCharCode(byteValue)}'`);
        }

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
