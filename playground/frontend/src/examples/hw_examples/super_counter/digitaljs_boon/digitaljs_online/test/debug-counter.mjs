/**
 * Debug counter feedback loop issue
 * Tests the minimal counter case to find where signals break
 */

import fs from 'fs';

const code = `
// Minimal counter test - 4-bit counter
module counter_test (
    input  wire clk,
    input  wire rst,
    output wire [3:0] count
);
    reg [3:0] counter;
    assign count = counter;

    always @(posedge clk) begin
        if (rst)
            counter <= 4'd0;
        else
            counter <= counter + 4'd1;
    end
endmodule
`;

async function test() {
    console.log('Debug counter feedback loop\n');

    // Synthesize to DigitalJS format
    const response = await fetch('http://localhost:8081/api/yosys2digitaljs', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({files: {'counter.sv': code}, options: {}})
    });
    const data = await response.json();

    if (data.error) {
        console.log('Synthesis error:', data.error);
        process.exit(1);
    }

    console.log('Synthesis successful');
    console.log('Devices:', Object.keys(data.output.devices).length);

    // Analyze the synthesized circuit
    console.log('\n=== Circuit Analysis ===');

    let additionDevs = [];
    let dffDevs = [];
    let muxDevs = [];

    for (const [id, dev] of Object.entries(data.output.devices)) {
        console.log(`${id}: type=${dev.type} ${dev.celltype || ''}`);
        if (dev.type === 'Addition' || dev.celltype === '$add') {
            additionDevs.push({id, dev});
        }
        if (dev.type === 'Dff' || dev.celltype === '$dff') {
            dffDevs.push({id, dev});
        }
        if (dev.type === 'Mux' || dev.celltype === '$mux') {
            muxDevs.push({id, dev});
        }
    }

    console.log('\n=== Key Components ===');
    console.log('Addition devices:', additionDevs.length);
    console.log('Dff devices:', dffDevs.length);
    console.log('Mux devices:', muxDevs.length);

    // Look at connectors to understand signal flow
    console.log('\n=== Counter Signal Path ===');

    // Find DFF output connections
    for (const {id, dev} of dffDevs) {
        console.log(`\nDff ${id}:`);
        console.log(`  bits: ${dev.bits}`);
        console.log(`  initial: ${dev.initial}`);
        console.log(`  polarity: ${JSON.stringify(dev.polarity)}`);

        // Find what connects to this DFF's input
        const inputs = data.output.connectors.filter(c => c.to.id === id);
        const outputs = data.output.connectors.filter(c => c.from.id === id);

        console.log(`  Inputs from:`);
        for (const conn of inputs) {
            console.log(`    ${conn.from.id}:${conn.from.port} -> :${conn.to.port}`);
        }
        console.log(`  Outputs to:`);
        for (const conn of outputs) {
            console.log(`    :${conn.from.port} -> ${conn.to.id}:${conn.to.port}`);
        }
    }

    // Find Addition connections
    for (const {id, dev} of additionDevs) {
        console.log(`\nAddition ${id}:`);
        console.log(`  bits: ${JSON.stringify(dev.bits)}`);

        const inputs = data.output.connectors.filter(c => c.to.id === id);
        const outputs = data.output.connectors.filter(c => c.from.id === id);

        console.log(`  Inputs from:`);
        for (const conn of inputs) {
            console.log(`    ${conn.from.id}:${conn.from.port} -> :${conn.to.port}`);
        }
        console.log(`  Outputs to:`);
        for (const conn of outputs) {
            console.log(`    :${conn.from.port} -> ${conn.to.id}:${conn.to.port}`);
        }
    }

    // Save the circuit JSON for debugging
    fs.writeFileSync('test/counter-circuit.json', JSON.stringify(data.output, null, 2));
    console.log('\nCircuit saved to test/counter-circuit.json');
}

test().catch(console.error);
