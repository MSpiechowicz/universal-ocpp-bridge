// Disposable real simulator for browser evidence tests; no bridge-private imports.
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';

const directory = mkdtempSync(join(tmpdir(), 'uob-browser-sim-'));
const write = (name, text) => writeFileSync(join(directory, name), text, { mode: 0o600 });
write('control-token', 'b'.repeat(64));
write('read-token', 'a'.repeat(64));
write('control.toml', `schema_version = 1
environment = "demo"
token_file = "control-token"
simulator_file = "simulator.toml"
[debug]
console_origin = "http://127.0.0.1:39193"
token_file = "read-token"
[[scenarios]]
id = "failed-assertion"
path = "scenario.toml"
`);
write('simulator.toml', `schema_version = 1
[[stations]]
id = "demo-alpha"
endpoint = "ws://127.0.0.1:39999/demo-alpha"
ocpp_version = "1.6"
`);
write('scenario.toml', `schema_version = 1
seed = 18446744073709551615
[[steps]]
id = "wait"
station = "demo-alpha"
action = "wait"
duration_ms = 1
timeout_ms = 1000
expect_event = "delay_elapsed"
expect_detail = "deliberately-mismatched-private-wire-text"
`);
const args = ['serve', '--config', join(directory, 'control.toml'), '--control-bind', '127.0.0.1:39194'];
const prebuilt = process.argv.includes('--prebuilt');
const child = spawn(prebuilt ? './target/debug/uob-sim' : 'cargo', prebuilt ? args : ['run', '--locked', '-p', 'uob-sim', '--', ...args], {
  cwd: new URL('../../', import.meta.url), stdio: 'inherit',
});
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill('SIGINT'));
child.on('error', () => { rmSync(directory, { recursive: true, force: true }); process.exit(1); });
child.on('exit', code => { rmSync(directory, { recursive: true, force: true }); process.exit(code ?? 1); });
