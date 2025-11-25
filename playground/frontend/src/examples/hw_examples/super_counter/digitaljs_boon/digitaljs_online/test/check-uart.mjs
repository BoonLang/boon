import fs from 'fs';

const code = fs.readFileSync('public/examples/super_counter.sv', 'utf8');
const response = await fetch('http://localhost:3001/api/yosys2digitaljs', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({files: {'super_counter.sv': code}, options: {}})
});
const data = await response.json();

if (data.error) {
    console.log('Error:', data.error);
    process.exit(1);
}

// Find uart_tx (name may be mangled with parameters)
let tx = null;
let txName = null;
for (const [name, sub] of Object.entries(data.output.subcircuits || {})) {
    if (name.includes('uart_tx')) {
        tx = sub;
        txName = name;
        break;
    }
}
if (!tx) {
    console.log('No uart_tx subcircuit');
    console.log('Subcircuits:', Object.keys(data.output.subcircuits || {}));
    process.exit(1);
}
console.log('Found uart_tx as:', txName);

console.log('uart_tx flip-flops:');
for (const [id, dev] of Object.entries(tx.devices)) {
    if (dev.type === 'Dff' || dev.type === 'Dffe') {
        console.log(`  ${id}: bits=${dev.bits} initial=${JSON.stringify(dev.initial)} label=${dev.label}`);
    }
}

// Look for specific signals
console.log('\nuart_tx all devices:');
for (const [id, dev] of Object.entries(tx.devices)) {
    const info = `${id}: type=${dev.type} bits=${JSON.stringify(dev.bits)} label=${dev.label || ''}`;
    // Print first 30 devices
    if (parseInt(id.replace('dev', '')) < 30) {
        console.log(`  ${info}`);
    }
}

// Check for Addition device (counter increment)
console.log('\nuart_tx Addition devices:');
for (const [id, dev] of Object.entries(tx.devices)) {
    if (dev.type === 'Addition') {
        console.log(`  ${id}: ${JSON.stringify(dev)}`);
    }
}

// Check for Dff devices (registers)
console.log('\nuart_tx Dff details:');
for (const [id, dev] of Object.entries(tx.devices)) {
    if (dev.type === 'Dff') {
        console.log(`  ${id}: bits=${dev.bits} polarity=${JSON.stringify(dev.polarity)} initial=${JSON.stringify(dev.initial)}`);
    }
}
