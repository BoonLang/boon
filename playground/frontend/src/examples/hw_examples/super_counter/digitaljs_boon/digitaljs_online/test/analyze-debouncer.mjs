/**
 * Analyze debouncer synthesis and simulation behavior
 */

import { chromium } from 'playwright';

const BASE_URL = 'http://localhost:3001';

async function sleep(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function test() {
    console.log('Analyze Debouncer Logic\n');

    // First, analyze the synthesized circuit structure
    console.log('1. Synthesizing debouncer via API...\n');

    // Use explicit 4-bit constants to avoid DigitalJS width mismatch issues
    const debouncerCode = `
module debouncer(
    input wire clk,
    input wire rst,
    input wire btn_n,
    output reg pressed
);
    localparam CTR_WIDTH = 4;
    localparam [CTR_WIDTH-1:0] DEBOUNCE_TARGET = 4'd1;  // Explicit 4-bit constant

    wire btn;
    assign btn = ~btn_n;

    reg [CTR_WIDTH-1:0] counter;
    reg stable;

    always @(posedge clk) begin
        if (rst) begin
            counter <= 4'd0;
            stable <= 1'b0;
            pressed <= 1'b0;
        end else begin
            pressed <= 1'b0;
            if (btn != stable) begin
                if (counter == DEBOUNCE_TARGET) begin
                    stable <= btn;
                    counter <= 4'd0;
                    if (btn)
                        pressed <= 1'b1;
                end else begin
                    counter <= counter + 4'd1;
                end
            end else begin
                counter <= 4'd0;
            end
        end
    end
endmodule
`;

    try {
        const response = await fetch(`${BASE_URL}/api/yosys2digitaljs`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ files: { 'debouncer.sv': debouncerCode }, options: {} })
        });
        const data = await response.json();

        if (data.error) {
            console.log('Synthesis error:', data.error);
            return;
        }

        // Analyze flip-flop initial values
        console.log('Flip-flops:');
        for (const [id, dev] of Object.entries(data.output.devices)) {
            if (dev.type === 'Dff' || dev.type === 'Dffe') {
                console.log(`  ${id} (${dev.label}):`);
                console.log(`    bits: ${dev.bits}`);
                console.log(`    polarity: ${JSON.stringify(dev.polarity)}`);
                console.log(`    initial: ${JSON.stringify(dev.initial || 'NOT SET')}`);
            }
        }

        // Check the Eq and Ne comparisons
        console.log('\nComparisons:');
        for (const [id, dev] of Object.entries(data.output.devices)) {
            if (dev.type === 'Eq' || dev.type === 'Ne') {
                console.log(`  ${id} (${dev.label}): type=${dev.type} bits=${JSON.stringify(dev.bits)}`);
            }
        }

        // Now run in browser to test actual behavior
        console.log('\n2. Testing in browser...\n');

        const browser = await chromium.launch({ headless: false });
        const page = await browser.newPage();

        // Capture console
        page.on('console', msg => {
            const text = msg.text();
            if (text.includes('[TEST]')) {
                console.log(text);
            }
        });

        await page.goto(`${BASE_URL}/?example=super_counter`, { waitUntil: 'networkidle' });
        await sleep(2000);
        await page.evaluate(() => {
            document.getElementById('webpack-dev-server-client-overlay')?.remove();
            document.querySelectorAll('iframe').forEach(el => el.remove());
        });
        await sleep(500);

        // Modify the code to just the debouncer
        console.log('Setting debouncer-only code...');
        await page.evaluate((code) => {
            const editor = document.querySelector('.CodeMirror');
            if (editor && editor.CodeMirror) {
                editor.CodeMirror.setValue(code);
            } else {
                // Fallback to textarea
                const textarea = document.getElementById('code');
                if (textarea) textarea.value = code;
            }
        }, debouncerCode);

        await page.click('#synthesize-btn');
        await page.waitForSelector('#paper svg', { timeout: 60000 });
        await sleep(2000);

        // Use UI-based testing with proper async timing
        console.log('Finding I/O controls...');
        await page.click('a[href="#iopanel"]');
        await sleep(300);

        const checkboxes = await page.$$('#iopanel input[type="checkbox"]');
        console.log('Found', checkboxes.length, 'checkboxes');

        for (let i = 0; i < checkboxes.length; i++) {
            const label = await checkboxes[i].evaluate(el => {
                const row = el.closest('tr');
                return row?.querySelector('label')?.textContent || `cb${i}`;
            });
            const checked = await checkboxes[i].isChecked();
            console.log(`  [${i}] ${label}: ${checked ? 'HIGH' : 'LOW'}`);
        }

        // Reset sequence using UI
        console.log('\n[TEST] Setting up reset sequence...');

        // Ensure btn_n HIGH first (button released)
        if (!await checkboxes[1].isChecked()) {
            await checkboxes[1].click();
            await sleep(100);
        }
        console.log('[TEST] btn_n = HIGH');

        // Set rst HIGH (active reset)
        if (!await checkboxes[0].isChecked()) {
            await checkboxes[0].click();
            await sleep(100);
        }
        console.log('[TEST] rst = HIGH (reset active)');

        // Run simulation with reset
        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
        await page.evaluate(() => window.djCircuit.stop());

        // Check state
        let state = await page.evaluate(() => {
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
        console.log(`[TEST] After reset active: tick=${state.tick} pressed=${state.pressed}`);

        // Release reset
        await checkboxes[0].click();
        console.log('[TEST] rst = LOW (reset released)');

        // Run more simulation
        await page.evaluate(() => window.djCircuit.start());
        await sleep(500);
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
        console.log(`[TEST] After reset release: tick=${state.tick} pressed=${state.pressed}`);

        // Press button (btn_n LOW)
        console.log('\n[TEST] Pressing button (btn_n LOW)...');
        await checkboxes[1].click();
        await sleep(100);

        const btnState = await checkboxes[1].isChecked();
        console.log(`[TEST] btn_n checkbox: ${btnState ? 'HIGH' : 'LOW'}`);

        // Run and monitor pressed
        console.log('[TEST] Running and monitoring pressed signal...');
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
                console.log(`[TEST] [${i*100}ms] tick=${state.tick} pressed=${state.pressed}`);
            }
            if (state.pressed === 1) pressedSeen = true;
        }

        await page.evaluate(() => window.djCircuit.stop());

        if (pressedSeen) {
            console.log('\n[TEST] SUCCESS: pressed signal was seen HIGH!');
        } else {
            console.log('\n[TEST] FAILED: pressed signal never went HIGH');
        }

        await sleep(1000);

        await page.screenshot({ path: 'test/analyze-debouncer-result.png', fullPage: true });
        console.log('\nScreenshot saved');

        await sleep(5000);
        await browser.close();

    } catch (error) {
        console.error('Error:', error.message);
    }
}

test().catch(console.error);
