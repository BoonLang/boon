import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Check Clock Frequency\n');

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

        // Find clock cell and check its settings
        const clockInfo = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            const clocks = [];

            for (const cell of cells) {
                if (cell.get('type') === 'Clock') {
                    clocks.push({
                        id: cell.id,
                        label: cell.get('label'),
                        net: cell.get('net'),
                        propagation: cell.get('propagation'),
                        attributes: cell.attributes
                    });
                }
            }
            return clocks;
        });

        console.log('Clock cells:');
        console.log(JSON.stringify(clockInfo, null, 2));

        // Track actual clock transitions over 200 ticks
        console.log('\n2. Tracking clock transitions...');
        let lastClk = null;
        let transitions = [];
        
        for (let i = 0; i < 200; i++) {
            await page.evaluate(() => window.djCircuit.updateGates({ synchronous: true }));
            
            const clk = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();
                for (const cell of cells) {
                    if (cell.get('type') === 'Clock') {
                        const out = cell.get('outputSignals');
                        return out?.out ? (out.out._avec?.[0] & out.out._bvec?.[0]) : null;
                    }
                }
                return null;
            });
            
            if (clk !== lastClk) {
                transitions.push({ tick: i+1, clk });
                lastClk = clk;
            }
        }
        
        console.log('Transitions (first 20):');
        console.log(transitions.slice(0, 20));
        console.log(`Total transitions in 200 ticks: ${transitions.length}`);

    } catch (error) {
        console.error('Error:', error.message);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
