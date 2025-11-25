import fs from 'fs';
import http from 'http';

const content = fs.readFileSync('public/examples/super_counter.sv', 'utf8');
const payload = JSON.stringify({files: {'super_counter.sv': content}, options: {}});

const req = http.request({
    hostname: 'localhost',
    port: 8081,
    path: '/api/yosys2digitaljs',
    method: 'POST',
    headers: {'Content-Type': 'application/json'}
}, (res) => {
    let data = '';
    res.on('data', chunk => data += chunk);
    res.on('end', () => {
        try {
            const result = JSON.parse(data);
            if (result.error) {
                console.log('ERROR:', result.error);
                return;
            }

            console.log('Checking signal widths in synthesized circuit...\n');

            const devices = result.output.devices;
            const connectors = result.output.connectors;

            // Check devices
            console.log('=== Devices with wide signals ===');
            for (const [id, dev] of Object.entries(devices)) {
                if (dev.bits && dev.bits >= 24) {
                    console.log(`${id}: ${dev.bits} bits (${dev.type})`);
                }
                // Check subcircuit ports
                if (dev.type === 'Subcircuit' && dev.celltype) {
                    const sub = result.output.subcircuits[dev.celltype];
                    if (sub && sub.devices) {
                        for (const [sid, sdev] of Object.entries(sub.devices)) {
                            if (sdev.bits && sdev.bits >= 24) {
                                console.log(`  ${dev.celltype}/${sid}: ${sdev.bits} bits (${sdev.type})`);
                            }
                        }
                    }
                }
            }

            // Check connectors for wide signals
            console.log('\n=== Connectors with wide signals ===');
            for (const conn of connectors) {
                if (conn.from && conn.from.port) {
                    const fromDev = devices[conn.from.id];
                    if (fromDev && fromDev.bits >= 24) {
                        console.log(`${conn.from.id}.${conn.from.port} -> ${conn.to.id}.${conn.to.port}`);
                    }
                }
            }

            // Recursively check subcircuits
            console.log('\n=== Subcircuit internals ===');
            if (result.output.subcircuits) {
                for (const [name, sub] of Object.entries(result.output.subcircuits)) {
                    console.log(`\n--- ${name} ---`);
                    if (sub.devices) {
                        for (const [id, dev] of Object.entries(sub.devices)) {
                            if (dev.bits && dev.bits >= 24) {
                                console.log(`  ${id}: ${dev.bits} bits (${dev.type})`);
                            }
                        }
                    }
                }
            }

            // Check for specific problem: Shift operations
            console.log('\n=== Looking for Shift operations ===');
            const checkForShifts = (devices, prefix = '') => {
                for (const [id, dev] of Object.entries(devices)) {
                    if (dev.type === 'Shift') {
                        console.log(`${prefix}${id}: Shift op, bits=${dev.bits}, extend=${dev.extend}`);
                    }
                }
            };
            checkForShifts(devices);
            if (result.output.subcircuits) {
                for (const [name, sub] of Object.entries(result.output.subcircuits)) {
                    if (sub.devices) {
                        checkForShifts(sub.devices, `${name}/`);
                    }
                }
            }

        } catch(e) {
            console.log('Parse error:', e.message);
            console.log('Response:', data.substring(0, 500));
        }
    });
});

req.on('error', e => console.log('Request error:', e.message));
req.write(payload);
req.end();
