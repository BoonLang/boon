/**
 * Test that validates uart-terminal.js state machine logic directly
 * Uses the same transitions from the circuit but applies uart-terminal.js logic
 */

import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));

const require = createRequire(import.meta.url);
const digitaljs = require('digitaljs');
const { HeadlessCircuit } = digitaljs;
const { Vector3vl } = require('3vl');

const CLOCK_HZ = 1000;
const BAUD_RATE = 100;
const CLOCK_PROPAGATION = 100;
const TICKS_PER_CLOCK = CLOCK_PROPAGATION * 2;
const CYCLES_PER_BIT = Math.round(CLOCK_HZ / BAUD_RATE) * TICKS_PER_CLOCK;

console.log(`[Test] cyclesPerBit=${CYCLES_PER_BIT}`);

/**
 * Simulates the EXACT uart-terminal.js handleTxTransition logic
 */
class UartTerminalSimulator {
    constructor(cyclesPerBit) {
        this.cyclesPerBit = cyclesPerBit;
        this.txState = 'IDLE';
        this.txStartTick = 0;
        this.txBitIndex = 0;
        this.txShiftReg = 0;
        this.lastTxValue = null;  // null = unknown, wait for first HIGH
        this.txLastEdgeTick = 0;
        this.txLastEdgeValue = true;
        this.decodedBytes = [];
    }

    handleTxTransition(tick, isHigh) {
        // Process RECEIVING state first
        if (this.txState === 'RECEIVING') {
            while (this.txBitIndex < 8) {
                const sampleTick = this.txStartTick + this.cyclesPerBit * (1.5 + this.txBitIndex);
                if (tick >= sampleTick) {
                    let bitValue;
                    if (this.txLastEdgeTick < sampleTick) {
                        bitValue = this.txLastEdgeValue;
                    } else {
                        bitValue = !isHigh;
                    }
                    if (bitValue) {
                        this.txShiftReg |= (1 << this.txBitIndex);
                    }
                    this.txBitIndex++;
                } else {
                    break;
                }
            }

            if (this.txBitIndex >= 8) {
                const stopTick = this.txStartTick + this.cyclesPerBit * 9.5;
                if (tick >= stopTick) {
                    console.log(`[SIM] Byte complete at tick=${tick}: 0x${this.txShiftReg.toString(16)} = '${String.fromCharCode(this.txShiftReg)}'`);
                    this.decodedBytes.push({
                        tick: this.txStartTick,
                        byte: this.txShiftReg,
                        char: String.fromCharCode(this.txShiftReg)
                    });
                    this.txState = 'IDLE';
                }
            }

            if (this.txState === 'RECEIVING') {
                this.txLastEdgeTick = tick;
                this.txLastEdgeValue = isHigh;
            }
        }

        // Check for start bit
        if (this.txState === 'IDLE') {
            if (this.lastTxValue === true && !isHigh) {
                console.log(`[SIM] START bit detected at tick=${tick}`);
                this.txStartTick = tick;
                this.txState = 'RECEIVING';
                this.txBitIndex = 0;
                this.txShiftReg = 0;
                this.txLastEdgeTick = tick;
                this.txLastEdgeValue = isHigh;
            }
        }

        this.lastTxValue = isHigh;
    }

    getMessage() {
        return this.decodedBytes.map(d => d.char).join('');
    }
}

async function main() {
    const svPath = join(__dirname, 'public', 'examples', 'super_counter.sv');
    const svContent = readFileSync(svPath, 'utf-8');
    console.log(`[Test] Loaded ${svPath}`);

    console.log('[Test] Synthesizing...');
    const response = await fetch('http://localhost:8081/api/yosys2digitaljs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
            files: { 'super_counter.sv': svContent },
            options: { optimize: true }
        })
    });

    const result = await response.json();
    if (result.error) {
        console.error('[Test] Synthesis error:', result.error);
        process.exit(1);
    }

    const circuit = new HeadlessCircuit(result.output, { clockPropagation: CLOCK_PROPAGATION });

    const cells = circuit._graph.getCells();
    let rstCell, btnCell, txCell;
    for (const cell of cells) {
        const net = cell.get('net');
        if (!net) continue;
        const netLower = net.toLowerCase();
        if (netLower.includes('rst')) rstCell = cell;
        else if (netLower.includes('btn')) btnCell = cell;
        else if (netLower.includes('uart_tx')) txCell = cell;
    }

    const links = circuit._graph.getLinks();
    let txWire = null;
    for (const link of links) {
        const target = link.getTargetElement();
        if (target && target.id === txCell.id) {
            txWire = link;
            break;
        }
    }

    // Create simulator with EXACT uart-terminal.js logic
    const simulator = new UartTerminalSimulator(CYCLES_PER_BIT);

    // Monitor transitions and feed to simulator
    circuit.monitorWire(txWire, (tick, signal) => {
        simulator.handleTxTransition(tick, signal.isHigh);
    }, { synchronous: true });

    // Reset
    rstCell.setInput(Vector3vl.zero);
    btnCell.setInput(Vector3vl.one);
    for (let i = 0; i < 50; i++) circuit.updateGates();

    rstCell.setInput(Vector3vl.one);
    for (let i = 0; i < 200; i++) circuit.updateGates();

    // Press button
    console.log('[Test] Pressing button...');
    btnCell.setInput(Vector3vl.zero);
    for (let i = 0; i < 150000; i++) circuit.updateGates();

    // Results
    console.log('\n[Test] Results:');
    console.log(`Decoded bytes: ${simulator.decodedBytes.length}`);
    for (const d of simulator.decodedBytes) {
        console.log(`  tick=${d.tick} byte=0x${d.byte.toString(16).padStart(2, '0')} char='${d.char}'`);
    }

    const message = simulator.getMessage();
    console.log(`\nDecoded message: "${message}"`);

    if (message.includes('BTN 1')) {
        console.log('\n✓ SUCCESS: uart-terminal.js logic correctly decodes "BTN 1"');
    } else {
        console.log('\n✗ FAIL: Expected "BTN 1", got different message');
        process.exit(1);
    }
}

main().catch(e => {
    console.error('[Test] Error:', e);
    process.exit(1);
});
