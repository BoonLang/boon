/**
 * Debug link connections in the circuit
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Debug Link Connections\n');

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

        // Find Clock cell and check its connections
        console.log('\n2. Clock cell connections:');
        const clockLinks = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const links = circuit._graph.getLinks();

            let clockId = null;
            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    clockId = cell.id;
                    break;
                }
            }

            const clockOutLinks = links.filter(link => link.get('source')?.id === clockId);
            return {
                clockId,
                outLinkCount: clockOutLinks.length,
                outLinks: clockOutLinks.map(link => ({
                    linkId: link.id,
                    source: link.get('source'),
                    target: link.get('target'),
                    targetLabel: circuit._graph.getCell(link.get('target').id)?.get('label') ||
                                  circuit._graph.getCell(link.get('target').id)?.get('type')
                }))
            };
        });
        console.log(JSON.stringify(clockLinks, null, 2));

        // Check uart_tx connections
        console.log('\n3. uart_tx input connections:');
        const uartLinks = await page.evaluate(() => {
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

            const uartInLinks = links.filter(link => link.get('target')?.id === uartTxId);
            return {
                uartTxId,
                inLinkCount: uartInLinks.length,
                inLinks: uartInLinks.map(link => ({
                    linkId: link.id,
                    source: link.get('source'),
                    sourceLabel: circuit._graph.getCell(link.get('source').id)?.get('label') ||
                                  circuit._graph.getCell(link.get('source').id)?.get('type'),
                    target: link.get('target')
                }))
            };
        });
        console.log(JSON.stringify(uartLinks, null, 2));

        // Check if there's an intermediate gate between Clock and uart_tx
        console.log('\n4. Tracing clock path to uart_tx:');
        const clockPath = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const links = circuit._graph.getLinks();

            let clockId = null;
            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    clockId = cell.id;
                    break;
                }
            }

            // Find all targets from clock (direct and indirect)
            const visited = new Set();
            const path = [];

            function trace(gateId, port, depth = 0) {
                if (depth > 5) return;
                if (visited.has(gateId + ':' + port)) return;
                visited.add(gateId + ':' + port);

                const gate = circuit._graph.getCell(gateId);
                const outLinks = links.filter(l => l.get('source')?.id === gateId && l.get('source')?.port === port);

                for (const link of outLinks) {
                    const target = link.get('target');
                    const targetGate = circuit._graph.getCell(target.id);
                    const targetLabel = targetGate?.get('label') || targetGate?.get('type') || target.id;

                    path.push({
                        depth,
                        from: gate?.get('label') || gate?.get('type') || gateId,
                        fromPort: port,
                        to: targetLabel,
                        toPort: target.port
                    });

                    // If target is uart_tx, we found it
                    if (targetLabel === 'u_uart_tx') {
                        path.push({ depth: depth + 1, found: 'FOUND uart_tx!' });
                    }

                    // Continue tracing if not a subcircuit input
                    const targetType = targetGate?.get('type');
                    if (targetType !== 'Subcircuit') {
                        // Get all output ports of target
                        const targetPorts = targetGate?.get('ports')?.items || [];
                        for (const p of targetPorts) {
                            if (p.dir === 'out') {
                                trace(target.id, p.id, depth + 1);
                            }
                        }
                    }
                }
            }

            trace(clockId, 'out');
            return path;
        });
        console.log(JSON.stringify(clockPath, null, 2));

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
