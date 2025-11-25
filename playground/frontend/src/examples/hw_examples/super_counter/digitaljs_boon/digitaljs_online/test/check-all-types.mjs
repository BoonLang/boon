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

            // Collect all device types
            const allTypes = new Map();

            const collectTypes = (devices, prefix) => {
                for (const [id, dev] of Object.entries(devices)) {
                    const key = dev.type;
                    if (!allTypes.has(key)) {
                        allTypes.set(key, []);
                    }
                    allTypes.get(key).push({
                        id: prefix + id,
                        bits: dev.bits,
                        extend: dev.extend,
                        signed: dev.signed
                    });
                }
            };

            collectTypes(result.output.devices, '');
            if (result.output.subcircuits) {
                for (const [name, sub] of Object.entries(result.output.subcircuits)) {
                    if (sub.devices) {
                        collectTypes(sub.devices, name + '/');
                    }
                }
            }

            console.log('=== All device types and their instances ===\n');
            for (const [type, instances] of allTypes.entries()) {
                console.log(`${type}:`);
                for (const inst of instances) {
                    let info = `  ${inst.id}`;
                    if (inst.bits !== undefined) info += ` (${inst.bits} bits)`;
                    if (inst.extend !== undefined) info += ` extend=${inst.extend}`;
                    if (inst.signed !== undefined) info += ` signed=${inst.signed}`;
                    console.log(info);
                }
            }

            // Look for potential issues
            console.log('\n=== Potential Problem Areas ===');
            const problemTypes = ['Shift', 'Memory', 'ROM', 'RAM', 'Arith', 'Mux', 'ZeroExtend', 'SignExtend'];
            for (const [type, instances] of allTypes.entries()) {
                if (problemTypes.some(p => type.includes(p))) {
                    console.log(`${type}:`);
                    for (const inst of instances) {
                        console.log(`  ${inst.id} bits=${inst.bits}`);
                    }
                }
            }

            // Save full output for analysis
            fs.writeFileSync('test/synthesis-output.json', JSON.stringify(result, null, 2));
            console.log('\nFull synthesis output saved to test/synthesis-output.json');

        } catch(e) {
            console.log('Parse error:', e.message);
        }
    });
});

req.on('error', e => console.log('Request error:', e.message));
req.write(payload);
req.end();
