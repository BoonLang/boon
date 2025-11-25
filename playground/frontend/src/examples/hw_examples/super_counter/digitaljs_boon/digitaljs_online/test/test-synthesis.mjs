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
                console.log('STDERR:', result.yosys_stderr);
            } else {
                console.log('SUCCESS: Circuit generated');
                console.log('Devices:', Object.keys(result.output.devices).length);
                const types = [...new Set(Object.values(result.output.devices).map(d => d.type))];
                console.log('Device types:', types.join(', '));

                // Check for UART and other I/O signals
                const devices = result.output.devices;
                console.log('\nI/O Devices:');
                for (const [id, dev] of Object.entries(devices)) {
                    const name = dev.net || id;
                    if (name.toLowerCase().includes('uart') ||
                        name.toLowerCase().includes('btn') ||
                        name.toLowerCase().includes('led') ||
                        name.toLowerCase().includes('rst') ||
                        name.toLowerCase().includes('clk')) {
                        console.log(' -', name, '(' + dev.type + ')');
                    }
                }
            }
        } catch(e) {
            console.log('Parse error:', e.message);
            console.log('Response:', data.substring(0, 1000));
        }
    });
});

req.on('error', e => console.log('Request error:', e.message));
req.write(payload);
req.end();
