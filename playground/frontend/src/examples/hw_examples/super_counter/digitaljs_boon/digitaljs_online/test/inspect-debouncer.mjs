/**
 * Inspect Debouncer - Deep dive into the synthesized debouncer structure
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Inspect Debouncer Subcircuit\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });

        await page.evaluate(() => {
            const overlay = document.getElementById('webpack-dev-server-client-overlay');
            if (overlay) overlay.remove();
        });

        console.log('1. Synthesizing...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Inspect debouncer
        console.log('\n2. Inspecting debouncer subcircuit...\n');
        const debouncerInfo = await page.evaluate(() => {
            const cells = window.djCircuit._graph.getCells();

            for (const cell of cells) {
                const label = cell.get('label') || '';
                if (label.includes('debouncer')) {
                    // Get the subcircuit graph
                    const subgraph = cell.get('graph');
                    if (!subgraph) {
                        return { error: 'No subcircuit graph found', label };
                    }

                    // List all cells in subcircuit
                    const subCells = [];
                    for (const key in subgraph.cells) {
                        const sc = subgraph.cells[key];
                        subCells.push({
                            id: key,
                            type: sc.type,
                            label: sc.label,
                            bits: sc.bits,
                            inputSignals: sc.inputSignals,
                            outputSignals: sc.outputSignals
                        });
                    }

                    // List all connectors
                    const connectors = [];
                    for (const key in subgraph.connectors) {
                        const conn = subgraph.connectors[key];
                        connectors.push({
                            id: key,
                            from: conn.from,
                            to: conn.to
                        });
                    }

                    return {
                        label,
                        cellId: cell.id,
                        numSubCells: subCells.length,
                        numConnectors: connectors.length,
                        subCells,
                        connectors
                    };
                }
            }
            return { error: 'Debouncer not found' };
        });

        if (debouncerInfo.error) {
            console.log('Error:', debouncerInfo.error);
        } else {
            console.log('Debouncer label:', debouncerInfo.label);
            console.log('Debouncer cell ID:', debouncerInfo.cellId);
            console.log('Number of internal cells:', debouncerInfo.numSubCells);
            console.log('Number of connectors:', debouncerInfo.numConnectors);

            console.log('\nInternal cells:');
            for (const cell of debouncerInfo.subCells) {
                console.log(`  ${cell.id}: type=${cell.type} label=${cell.label || 'N/A'} bits=${cell.bits || 'N/A'}`);
            }

            console.log('\nConnectors (first 20):');
            for (const conn of debouncerInfo.connectors.slice(0, 20)) {
                console.log(`  ${JSON.stringify(conn.from)} -> ${JSON.stringify(conn.to)}`);
            }
        }

        // Also inspect the raw synthesis output
        console.log('\n3. Getting raw synthesis JSON...');

        // Try to read the synthesis from localStorage or make a new request
        const synthesisJson = await page.evaluate(async () => {
            // Find the synthesis form and get the current source
            const source = document.getElementById('code')?.value;
            if (!source) return { error: 'No source code found' };

            // Make a direct call to the synthesis API
            try {
                const response = await fetch('http://localhost:3001/api/yosys2digitaljs', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({
                        files: { 'super_counter.sv': source },
                        options: { fsm: 'yes', fsmexpand: 'yes' }
                    })
                });
                const data = await response.json();
                return data;
            } catch (e) {
                return { error: e.message };
            }
        });

        if (synthesisJson.error) {
            console.log('Synthesis error:', synthesisJson.error);
        } else if (synthesisJson.output) {
            // Find the debouncer module
            const modules = synthesisJson.output.subcircuits || {};
            console.log('Available modules:', Object.keys(modules));

            // Find debouncer
            for (const [name, mod] of Object.entries(modules)) {
                if (name.toLowerCase().includes('debouncer')) {
                    console.log(`\nDebouncer module: ${name}`);
                    console.log('Devices:', Object.keys(mod.devices || {}).length);
                    console.log('Connectors:', (mod.connectors || []).length);

                    // Print devices
                    console.log('\nDevices in debouncer:');
                    for (const [devId, dev] of Object.entries(mod.devices || {})) {
                        console.log(`  ${devId}: type=${dev.type} bits=${dev.bits || 'N/A'} label=${dev.label || 'N/A'}`);
                        if (dev.type === 'Dff' || dev.type === 'Dffe') {
                            console.log(`    (flip-flop: polarity=${JSON.stringify(dev.polarity)}, initial=${JSON.stringify(dev.initial)})`);
                        }
                    }

                    // Print connectors
                    console.log('\nConnectors in debouncer:');
                    for (const conn of (mod.connectors || [])) {
                        console.log(`  ${JSON.stringify(conn.from)} -> ${JSON.stringify(conn.to)}`);
                    }
                }
            }
        }

        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
