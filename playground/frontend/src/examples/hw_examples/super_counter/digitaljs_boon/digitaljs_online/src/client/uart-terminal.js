"use strict";

import $ from 'jquery';
import { Vector3vl } from '3vl';

/**
 * UART Terminal Component (Edge-Triggered for Fast Mode Support)
 *
 * Uses synchronous monitors to catch every signal transition,
 * works correctly in both normal and fast-forward simulation modes.
 */
export class UartTerminal {
    constructor(options) {
        this.el = options.el;
        this.circuit = options.circuit;
        this.txCell = options.txCell;  // Output from FPGA (we read this)
        this.rxCell = options.rxCell;  // Input to FPGA (we write this)
        this.txWire = options.txWire;  // Wire for synchronous monitoring

        // UART parameters - match super_counter.sv simulation defaults
        this.clockHz = options.clockHz || 1000;    // 1kHz for fast sim
        this.baudRate = options.baudRate || 100;   // 100 baud = 10 cycles/bit

        // Clock propagation determines how many simulation ticks per clock half-cycle
        this.clockPropagation = options.clockPropagation || 100;
        this.ticksPerClockCycle = this.clockPropagation * 2;

        // cyclesPerBit in SIMULATION TICKS
        const clockCyclesPerBit = Math.round(this.clockHz / this.baudRate);
        this.cyclesPerBit = clockCyclesPerBit * this.ticksPerClockCycle;

        // TX state (receiving from FPGA) - edge-triggered
        this.txState = 'IDLE';
        this.txStartTick = 0;
        this.txBitIndex = 0;
        this.txShiftReg = 0;
        this.txLineBuffer = '';
        this.lastTxValue = null;  // null = unknown, wait for first HIGH
        this.txLastEdgeTick = 0;
        this.txLastEdgeValue = true;
        this.txMonitorId = null;

        // RX state (transmitting to FPGA)
        this.rxQueue = [];  // Queue of bytes to send
        this.rxBusy = false;
        this.rxAlarmIds = [];

        // History
        this.history = [];
        this.historyIndex = -1;

        this.render();
        this.bindEvents();
        this.setupSynchronousMonitor();

        // Set initial RX state (idle = high)
        if (this.rxCell) {
            this.rxCell.setInput(Vector3vl.one);
        }

        console.log(`[UART] Initialized: cyclesPerBit=${this.cyclesPerBit}, txWire=${this.txWire ? 'available' : 'none'}`);
    }

    render() {
        $(this.el).html(`
            <div class="uart-terminal">
                <div class="uart-header">
                    <span class="uart-title">UART Terminal</span>
                    <span class="uart-status" title="Baud: ${this.baudRate}, ${this.cyclesPerBit} ticks/bit">
                        <span class="uart-indicator tx-indicator">TX</span>
                        <span class="uart-indicator rx-indicator">RX</span>
                    </span>
                </div>
                <div class="uart-output"></div>
                <div class="uart-input-wrapper">
                    <span class="uart-prompt">&gt;</span>
                    <input type="text" class="uart-input" placeholder="Type command (e.g., ACK 100)">
                </div>
            </div>
        `);
        this.outputEl = this.el.querySelector('.uart-output');
        this.inputEl = this.el.querySelector('.uart-input');
        this.txIndicator = this.el.querySelector('.tx-indicator');
        this.rxIndicator = this.el.querySelector('.rx-indicator');
    }

    bindEvents() {
        this.inputEl.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                const command = this.inputEl.value.trim();
                if (command) {
                    this.sendCommand(command);
                    this.history.push(command);
                    this.historyIndex = this.history.length;
                    this.inputEl.value = '';
                }
            } else if (e.key === 'ArrowUp') {
                if (this.historyIndex > 0) {
                    this.historyIndex--;
                    this.inputEl.value = this.history[this.historyIndex];
                }
                e.preventDefault();
            } else if (e.key === 'ArrowDown') {
                if (this.historyIndex < this.history.length - 1) {
                    this.historyIndex++;
                    this.inputEl.value = this.history[this.historyIndex];
                } else {
                    this.historyIndex = this.history.length;
                    this.inputEl.value = '';
                }
                e.preventDefault();
            }
        });
    }

    setupSynchronousMonitor() {
        // Use synchronous monitor for TX - called on EVERY signal transition
        if (this.txWire) {
            console.log('[UART] Setting up synchronous monitor on TX wire');
            this.txMonitorId = this.circuit.monitorWire(this.txWire, (tick, signal) => {
                this.handleTxTransition(tick, signal.isHigh);
            }, { synchronous: true });
        } else if (this.txCell) {
            // Fallback: try to find the source gate and monitor directly
            console.log('[UART] TX wire not available, using fallback postUpdateGates');
            this.circuit.on('postUpdateGates', (tick) => {
                this.fallbackTick(tick);
            });
        }
    }

    appendOutput(text, className = '') {
        console.log(`[UART] appendOutput called: text="${text}", outputEl=${!!this.outputEl}`);
        const line = document.createElement('div');
        line.className = 'uart-line ' + className;
        line.textContent = text;
        this.outputEl.appendChild(line);
        this.outputEl.scrollTop = this.outputEl.scrollHeight;
        console.log(`[UART] Line appended, total children: ${this.outputEl.children.length}`);

        // Limit output lines
        while (this.outputEl.children.length > 1000) {
            this.outputEl.removeChild(this.outputEl.firstChild);
        }
    }

    sendCommand(command) {
        this.appendOutput('> ' + command, 'uart-sent');
        // Queue bytes for transmission (add newline)
        for (const char of command + '\n') {
            this.rxQueue.push(char.charCodeAt(0));
        }
        // Start sending if not already busy
        this.processRxQueue();
    }

    /**
     * Handle TX signal transition - called on EVERY edge, even in fast mode
     */
    handleTxTransition(tick, isHigh) {
        // console.log(`[TX] Edge at tick=${tick} value=${isHigh ? 'HIGH' : 'LOW'} state=${this.txState}`);

        // Process RECEIVING state first
        if (this.txState === 'RECEIVING') {
            // Calculate which bits we should have sampled by now
            // Start bit ends at startTick + cyclesPerBit
            // Bit N sampled at startTick + cyclesPerBit * (1.5 + N)

            while (this.txBitIndex < 8) {
                const sampleTick = this.txStartTick + this.cyclesPerBit * (1.5 + this.txBitIndex);
                if (tick >= sampleTick) {
                    // Determine bit value: use the signal state that was valid at sampleTick
                    // If the last edge was before sampleTick, use that edge's value
                    // Otherwise, use the value before this edge (inverted current)
                    let bitValue;
                    if (this.txLastEdgeTick < sampleTick) {
                        bitValue = this.txLastEdgeValue;
                    } else {
                        // This edge is before/at sample point, use previous value
                        bitValue = !isHigh;
                    }

                    if (bitValue) {
                        this.txShiftReg |= (1 << this.txBitIndex);
                    }
                    // console.log(`[TX] Bit ${this.txBitIndex} = ${bitValue ? 1 : 0} (sampled at ~${sampleTick})`);
                    this.txBitIndex++;
                } else {
                    break;
                }
            }

            // Check for stop bit (after all 8 data bits)
            if (this.txBitIndex >= 8) {
                const stopTick = this.txStartTick + this.cyclesPerBit * 9.5;
                if (tick >= stopTick) {
                    // Byte complete
                    console.log(`[TX] Byte complete: 0x${this.txShiftReg.toString(16)} = '${String.fromCharCode(this.txShiftReg)}'`);
                    this.handleReceivedByte(this.txShiftReg);
                    this.txState = 'IDLE';
                    this.txIndicator.classList.remove('active');
                    // Don't return - fall through to check if this edge is also a new start bit
                }
            }

            if (this.txState === 'RECEIVING') {
                // Still receiving, update edge tracking
                this.txLastEdgeTick = tick;
                this.txLastEdgeValue = isHigh;
            }
        }

        // Check for start bit (IDLE state, or just transitioned to IDLE after byte complete)
        if (this.txState === 'IDLE') {
            // Only detect start bit if:
            // 1. Line was HIGH (lastTxValue === true, not null or false)
            //    This prevents false start detection when signal starts LOW at tick 0
            // 2. Current transition is LOW (falling edge)
            if (this.lastTxValue === true && !isHigh) {
                // Valid falling edge after line was HIGH = start bit detected
                console.log(`[TX] START bit detected at tick=${tick}`);
                this.txStartTick = tick;
                this.txState = 'RECEIVING';
                this.txBitIndex = 0;
                this.txShiftReg = 0;
                this.txLastEdgeTick = tick;
                this.txLastEdgeValue = isHigh;
                this.txIndicator.classList.add('active');
            }
        }

        this.lastTxValue = isHigh;
    }

    /**
     * Fallback for when synchronous monitor isn't available
     */
    fallbackTick(currentTick) {
        if (!this.txCell) return;

        const signals = this.txCell.get('inputSignals');
        if (!signals || !signals.in) return;

        const isHigh = signals.in.isHigh;

        // Detect edges and call handleTxTransition
        if (isHigh !== this.lastTxValue) {
            this.handleTxTransition(currentTick, isHigh);
        }
    }

    handleReceivedByte(byte) {
        const char = String.fromCharCode(byte);
        console.log(`[TX] handleReceivedByte: byte=0x${byte.toString(16)} char='${char}' buffer="${this.txLineBuffer}"`);

        if (byte === 0x0A) {  // Newline
            console.log(`[TX] Newline detected, buffer="${this.txLineBuffer}", outputEl=${!!this.outputEl}`);
            if (this.txLineBuffer.length > 0) {
                console.log(`[TX] Line complete: "${this.txLineBuffer}"`);
                this.appendOutput(this.txLineBuffer, 'uart-received');
                this.txLineBuffer = '';
            }
        } else if (byte >= 0x20 && byte < 0x7F) {  // Printable ASCII
            this.txLineBuffer += char;
            console.log(`[TX] Buffer now: "${this.txLineBuffer}"`);
        }
    }

    /**
     * Process RX queue - send bytes to FPGA using alarm() for precise timing
     */
    processRxQueue() {
        if (this.rxBusy || this.rxQueue.length === 0) return;

        const byte = this.rxQueue.shift();
        this.rxBusy = true;
        this.rxIndicator.classList.add('active');

        console.log(`[RX] Sending byte 0x${byte.toString(16)} = '${String.fromCharCode(byte)}'`);

        const startTick = this.circuit.tick;

        // Start bit (low)
        this.rxCell.setInput(Vector3vl.zero);

        // Schedule data bits using alarm()
        for (let i = 0; i < 8; i++) {
            const bitTick = startTick + this.cyclesPerBit * (1 + i);
            const bit = (byte >> i) & 1;
            const alarmId = this.circuit.alarm(bitTick, () => {
                this.rxCell.setInput(bit ? Vector3vl.one : Vector3vl.zero);
            }, { synchronous: true });
            this.rxAlarmIds.push(alarmId);
        }

        // Stop bit (high)
        const stopTick = startTick + this.cyclesPerBit * 9;
        const stopAlarmId = this.circuit.alarm(stopTick, () => {
            this.rxCell.setInput(Vector3vl.one);
            this.rxBusy = false;
            this.rxIndicator.classList.remove('active');
            // Continue with next byte in queue
            this.processRxQueue();
        }, { synchronous: true });
        this.rxAlarmIds.push(stopAlarmId);
    }

    shutdown() {
        // Clean up monitors
        if (this.txMonitorId && this.circuit) {
            this.circuit.unmonitor(this.txMonitorId);
        }

        // Clean up alarms
        for (const alarmId of this.rxAlarmIds) {
            if (this.circuit) {
                this.circuit.unalarm(alarmId);
            }
        }

        if (this.circuit) {
            this.circuit.off('postUpdateGates');
        }
        $(this.el).empty();
    }
}

/**
 * Detect UART signals in the circuit
 * Looks for signals named uart_tx, uart_rx, uart_tx_o, uart_rx_i, etc.
 */
export function detectUartSignals(circuit) {
    const cells = circuit._graph.getCells();
    let txCell = null;
    let rxCell = null;

    for (const cell of cells) {
        const net = cell.get('net');
        if (!net) continue;

        const netLower = net.toLowerCase();

        // TX is output from FPGA (we read it)
        if (netLower.includes('uart_tx') || netLower === 'tx' || netLower === 'uart_tx_o') {
            if (cell.isOutput) {
                txCell = cell;
            }
        }

        // RX is input to FPGA (we write it)
        if (netLower.includes('uart_rx') || netLower === 'rx' || netLower === 'uart_rx_i') {
            if (cell.isInput) {
                rxCell = cell;
            }
        }
    }

    return { txCell, rxCell };
}

/**
 * Find wires connected to UART cells for synchronous monitoring
 */
export function findUartWires(circuit, uartSignals) {
    const links = circuit._graph.getLinks();
    let txWire = null;
    let rxWire = null;

    for (const link of links) {
        const targetCell = link.getTargetElement();
        if (!targetCell) continue;

        // TX wire: connects to the output display cell (uart_tx_o)
        if (uartSignals.txCell && targetCell.id === uartSignals.txCell.id) {
            txWire = link;
        }

        // RX wire: connects FROM the input cell (uart_rx_i)
        const sourceCell = link.getSourceElement();
        if (uartSignals.rxCell && sourceCell && sourceCell.id === uartSignals.rxCell.id) {
            rxWire = link;
        }
    }

    return { txWire, rxWire };
}
