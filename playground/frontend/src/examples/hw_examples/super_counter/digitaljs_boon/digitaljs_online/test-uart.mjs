/**
 * Automated UART Test - Tests UART TX decoding
 * Run with: node test-uart.mjs
 */

import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Load digitaljs and 3vl
const require = createRequire(import.meta.url);
const digitaljs = require('digitaljs');
const { HeadlessCircuit } = digitaljs;
const { Vector3vl } = require('3vl');

// UART parameters matching super_counter.sv
const CLOCK_HZ = 1000;
const BAUD_RATE = 100;
const CLOCK_PROPAGATION = 100;
const TICKS_PER_CLOCK = CLOCK_PROPAGATION * 2;
const CYCLES_PER_BIT = Math.round(CLOCK_HZ / BAUD_RATE) * TICKS_PER_CLOCK;  // 2000

console.log(`[Test] UART parameters: cyclesPerBit=${CYCLES_PER_BIT}`);

/**
 * Decode UART byte from signal transitions
 */
function decodeUartByte(transitions, startTick, cyclesPerBit) {
    let byte = 0;
    for (let bit = 0; bit < 8; bit++) {
        const sampleTick = startTick + cyclesPerBit * (1.5 + bit);
        // Find the last transition before sampleTick
        let value = true;  // idle high
        for (const t of transitions) {
            if (t.tick <= sampleTick) {
                value = t.value;
            } else {
                break;
            }
        }
        if (value) {
            byte |= (1 << bit);
        }
    }
    return byte;
}

/**
 * Find start bits in transitions using state machine like uart-terminal.js
 * Only detects falling edges when in IDLE state (after completing previous byte)
 */
function findStartBitsStateMachine(transitions, cyclesPerBit) {
    const startBits = [];
    let state = 'WAIT_HIGH';  // Wait for first HIGH before accepting starts
    let byteStartTick = 0;

    for (let i = 0; i < transitions.length; i++) {
        const t = transitions[i];

        if (state === 'WAIT_HIGH') {
            // Haven't seen HIGH yet, wait for it
            if (t.value) {
                state = 'IDLE';
            }
        } else if (state === 'IDLE') {
            // Looking for falling edge (start bit)
            if (!t.value) {
                startBits.push(t.tick);
                byteStartTick = t.tick;
                state = 'RECEIVING';
            }
        } else if (state === 'RECEIVING') {
            // Wait for byte to complete (10 bit times from start)
            const byteEndTick = byteStartTick + cyclesPerBit * 10;
            if (t.tick >= byteEndTick) {
                // Byte should be complete, go back to idle
                state = 'IDLE';
                // Check if this transition is also a start bit
                if (!t.value) {
                    startBits.push(t.tick);
                    byteStartTick = t.tick;
                    state = 'RECEIVING';
                }
            }
        }
    }
    return startBits;
}

async function main() {
    // Read super_counter.sv
    const svPath = join(__dirname, 'public', 'examples', 'super_counter.sv');
    const svContent = readFileSync(svPath, 'utf-8');
    console.log(`[Test] Loaded ${svPath}`);

    // Synthesize via backend API
    console.log('[Test] Synthesizing circuit...');
    const response = await fetch('http://localhost:8081/api/yosys2digitaljs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
            files: { 'super_counter.sv': svContent },
            options: { optimize: true }
        })
    });

    if (!response.ok) {
        const text = await response.text();
        console.error('[Test] Synthesis failed:', text);
        process.exit(1);
    }

    const result = await response.json();
    if (result.error) {
        console.error('[Test] Synthesis error:', result.error);
        process.exit(1);
    }

    console.log('[Test] Synthesis successful');

    // Create headless circuit
    const circuit = new HeadlessCircuit(result.output, {
        clockPropagation: CLOCK_PROPAGATION
    });

    // Find input/output cells
    const cells = circuit._graph.getCells();
    let clkCell, rstCell, btnCell, txCell;

    for (const cell of cells) {
        const net = cell.get('net');
        if (!net) continue;
        const netLower = net.toLowerCase();
        if (netLower === 'clk' || netLower === 'clk_i') {
            clkCell = cell;
        } else if (netLower.includes('rst')) {
            rstCell = cell;
        } else if (netLower.includes('btn')) {
            btnCell = cell;
        } else if (netLower.includes('uart_tx')) {
            txCell = cell;
        }
    }

    console.log(`[Test] Found cells: clk=${!!clkCell}, rst=${!!rstCell}, btn=${!!btnCell}, tx=${!!txCell}`);

    if (!clkCell || !rstCell || !btnCell || !txCell) {
        console.error('[Test] Missing required cells');
        process.exit(1);
    }

    // Find TX wire for monitoring
    const links = circuit._graph.getLinks();
    let txWire = null;
    for (const link of links) {
        const target = link.getTargetElement();
        if (target && target.id === txCell.id) {
            txWire = link;
            break;
        }
    }

    if (!txWire) {
        console.error('[Test] TX wire not found');
        process.exit(1);
    }

    // Collect TX transitions
    const txTransitions = [];
    circuit.monitorWire(txWire, (tick, signal) => {
        txTransitions.push({ tick, value: signal.isHigh });
    }, { synchronous: true });

    // Initialize: rst_n = 0 (active low reset)
    console.log('[Test] Initializing with reset...');
    rstCell.setInput(Vector3vl.zero);
    btnCell.setInput(Vector3vl.one);  // btn_n = 1 (not pressed)

    // Run for a bit with reset active
    for (let i = 0; i < 50; i++) {
        circuit.updateGates();
    }

    // Release reset
    console.log('[Test] Releasing reset...');
    rstCell.setInput(Vector3vl.one);

    // Run a few cycles to stabilize
    for (let i = 0; i < 200; i++) {
        circuit.updateGates();
    }

    // Record tick before button press
    const beforeBtnTick = circuit.tick;
    console.log(`[Test] Tick before button: ${beforeBtnTick}`);
    console.log(`[Test] TX transitions so far: ${txTransitions.length}`);

    // Press button (btn_n = 0, active low)
    console.log('[Test] Pressing button...');
    btnCell.setInput(Vector3vl.zero);

    // Run for enough ticks to transmit a full message
    // "BTN 1\n" = 6 bytes = ~60 bit times = ~120,000 ticks
    console.log('[Test] Running simulation...');
    const targetTicks = 150000;
    for (let i = 0; i < targetTicks; i++) {
        circuit.updateGates();
    }

    console.log(`[Test] Final tick: ${circuit.tick}`);
    console.log(`[Test] Total TX transitions: ${txTransitions.length}`);

    // Show first 20 transitions
    console.log('[Test] First 20 TX transitions:');
    for (let i = 0; i < Math.min(20, txTransitions.length); i++) {
        const t = txTransitions[i];
        console.log(`  tick=${t.tick} value=${t.value ? 'HIGH' : 'LOW'}`);
    }

    // Find start bits and decode using state machine
    const startBits = findStartBitsStateMachine(txTransitions, CYCLES_PER_BIT);
    console.log(`[Test] Found ${startBits.length} start bits at ticks: ${startBits.slice(0, 10).join(', ')}`);

    const decodedBytes = [];
    for (const startTick of startBits) {
        const byte = decodeUartByte(txTransitions, startTick, CYCLES_PER_BIT);
        decodedBytes.push({ tick: startTick, byte, char: String.fromCharCode(byte) });
    }

    console.log('[Test] Decoded bytes:');
    for (const d of decodedBytes) {
        console.log(`  tick=${d.tick} byte=0x${d.byte.toString(16).padStart(2, '0')} char='${d.char}'`);
    }

    // Build string
    const message = decodedBytes.map(d => d.char).join('');
    console.log(`[Test] Decoded message: "${message}"`);

    // Verify
    const expected = 'BTN 1\n';
    if (message.includes('BTN')) {
        console.log('[Test] SUCCESS: Message contains "BTN"');
    } else {
        console.log('[Test] FAIL: Message does not contain "BTN"');
        console.log(`[Test] Expected something like: "${expected}"`);
    }

    // Additional analysis: show bit-by-bit for first few bytes
    console.log('\n[Test] Detailed bit analysis for first byte:');
    if (startBits.length > 0) {
        const startTick = startBits[0];
        console.log(`  Start bit at tick ${startTick}`);
        for (let bit = 0; bit < 8; bit++) {
            const sampleTick = startTick + CYCLES_PER_BIT * (1.5 + bit);
            let value = true;
            let lastTransitionTick = 0;
            for (const t of txTransitions) {
                if (t.tick <= sampleTick) {
                    value = t.value;
                    lastTransitionTick = t.tick;
                }
            }
            console.log(`  Bit ${bit}: sample at tick ${sampleTick.toFixed(0)}, last transition at ${lastTransitionTick}, value=${value ? 1 : 0}`);
        }
    }
}

main().catch(e => {
    console.error('[Test] Error:', e);
    process.exit(1);
});
