/**
 * Test debouncer in isolation
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Test Debouncer in Isolation\n');

    const browser = await chromium.launch({ headless: false });
    const page = await browser.newPage();

    try {
        // Navigate to empty page
        await page.goto(`${BASE_URL}/`, { waitUntil: 'networkidle' });
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe[src="about:blank"]').forEach(el => el.remove());
        });
        await sleep(500);

        // Set the code to just the debouncer
        const debouncerCode = `
module debouncer(
    input wire clk,
    input wire rst,
    input wire btn_n,
    output reg pressed
);
    wire btn;
    assign btn = ~btn_n;

    reg [3:0] counter;
    reg stable;

    always @(posedge clk) begin
        if (rst) begin
            counter <= 0;
            stable <= 0;
            pressed <= 0;
        end else begin
            pressed <= 0;
            if (btn != stable) begin
                if (counter == 1) begin
                    stable <= btn;
                    counter <= 0;
                    if (btn)
                        pressed <= 1;
                end else begin
                    counter <= counter + 1;
                end
            end else begin
                counter <= 0;
            end
        end
    end
endmodule
`;

        await page.evaluate((code) => {
            document.getElementById('code').value = code;
        }, debouncerCode);

        console.log('1. Synthesizing standalone debouncer...');
        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Stop simulation
        await page.evaluate(() => window.djCircuit.stop());

        // Print devices
        const devices = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();
            return cells.map(c => ({
                id: c.id,
                type: c.get('type'),
                net: c.get('net'),
                label: c.get('label')
            })).filter(c => c.net || c.type === 'Dff');
        });

        console.log('\nDevices:');
        for (const d of devices) {
            console.log(`  ${d.id}: ${d.type} - ${d.net || d.label}`);
        }

        // Go to I/O
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('\n2. Found', checkboxes.length, 'checkboxes');

        for (let i = 0; i < checkboxes.length; i++) {
            const label = await checkboxes[i].evaluate(el => {
                const row = el.closest('tr');
                return row?.querySelector('label')?.textContent || `cb${i}`;
            });
            console.log(`  [${i}] ${label}`);
        }

        // Proper reset: rst HIGH first, then LOW
        console.log('\n3. Reset sequence...');

        // btn_n HIGH (released)
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(50);
        }
        console.log('   btn_n = HIGH');

        // rst HIGH (active)
        if (!await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(50);
        }
        console.log('   rst = HIGH (reset active)');

        // Run with reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        // Check state
        let state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            let pressed = null;
            let rst = null;
            let btn_n = null;

            for (const cell of cells) {
                const net = cell.get('net');
                if (net === 'pressed') {
                    pressed = cell.get('inputSignals')?.in;
                }
                if (net === 'rst') {
                    rst = cell.get('outputSignals')?.out;
                }
                if (net === 'btn_n') {
                    btn_n = cell.get('outputSignals')?.out;
                }
            }

            return {
                tick: circuit.tick,
                rst: rst?._avec?.[0] === 1 && rst?._bvec?.[0] === 1 ? 1 : 0,
                btn_n: btn_n?._avec?.[0] === 1 && btn_n?._bvec?.[0] === 1 ? 1 : 0,
                pressed: pressed?._avec?.[0] === 1 && pressed?._bvec?.[0] === 1 ? 1 : 0
            };
        });
        console.log(`   After reset active: tick=${state.tick} rst=${state.rst} btn_n=${state.btn_n} pressed=${state.pressed}`);

        // Release reset
        await checkboxes[0].click();
        console.log('   rst = LOW (reset released)');

        // Run
        await page.evaluate(() => window.djCircuit.start());
        await sleep(300);
        await page.evaluate(() => window.djCircuit.stop());

        state = await page.evaluate(() => {
            const circuit = window.djCircuit;
            const cells = circuit._graph.getCells();

            let pressed = null;

            for (const cell of cells) {
                if (cell.get('net') === 'pressed') {
                    pressed = cell.get('inputSignals')?.in;
                }
            }

            return {
                tick: circuit.tick,
                pressed: pressed?._avec?.[0] === 1 && pressed?._bvec?.[0] === 1 ? 1 : 0
            };
        });
        console.log(`   After reset release: tick=${state.tick} pressed=${state.pressed}`);

        // Now press button
        console.log('\n4. Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(50);

        const btnState = await checkboxes[1].isChecked();
        console.log(`   btn_n = ${btnState ? 'HIGH' : 'LOW'}`);

        // Monitor
        console.log('\n5. Monitoring pressed signal...');
        await page.evaluate(() => window.djCircuit.start());

        let pressedSeen = false;
        for (let i = 0; i < 20; i++) {
            await sleep(100);
            state = await page.evaluate(() => {
                const circuit = window.djCircuit;
                const cells = circuit._graph.getCells();

                let pressed = null;
                for (const cell of cells) {
                    if (cell.get('net') === 'pressed') {
                        pressed = cell.get('inputSignals')?.in;
                    }
                }

                return {
                    tick: circuit.tick,
                    pressed: pressed?._avec?.[0] === 1 && pressed?._bvec?.[0] === 1 ? 1 : 0
                };
            });

            if (i % 5 === 0 || state.pressed === 1) {
                console.log(`   [${i*100}ms] tick=${state.tick} pressed=${state.pressed}`);
            }
            if (state.pressed === 1) pressedSeen = true;
        }

        await page.evaluate(() => window.djCircuit.stop());

        if (pressedSeen) {
            console.log('\n   SUCCESS: pressed signal was seen HIGH!');
        } else {
            console.log('\n   FAILED: pressed signal never went HIGH');
        }

        await page.screenshot({ path: 'test/debouncer-isolated-result.png', fullPage: true });
        await sleep(5000);

    } catch (error) {
        console.error('Error:', error.message);
        console.error(error.stack);
    } finally {
        await browser.close();
    }
}

test().catch(console.error);
