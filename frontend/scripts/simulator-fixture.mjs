// Disposable simulator and loopback OCPP peer for browser evidence tests.
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
[control_browser]
console_origin = "http://127.0.0.1:39193"
[[scenarios]]
id = "failed-assertion"
path = "failed-assertion.toml"
[[scenarios]]
id = "setup-failure"
path = "setup-failure.toml"
[[scenarios]]
id = "live-alpha"
path = "live-alpha.toml"
[[scenarios]]
id = "live-beta"
path = "live-beta.toml"
`);
write('simulator.toml', `schema_version = 1
station_capacity = 3
[[stations]]
id = "demo-alpha"
endpoint = "ws://127.0.0.1:39195/demo-alpha"
ocpp_version = "1.6"
[[stations]]
id = "demo-beta"
endpoint = "ws://127.0.0.1:39195/demo-beta"
ocpp_version = "2.0.1"
[[stations]]
id = "demo-missing"
endpoint = "ws://127.0.0.1:39195/demo-missing"
ocpp_version = "1.6"
`);
write('failed-assertion.toml', `schema_version = 1
seed = "18446744073709551615"
[[steps]]
id = "wait"
station = "demo-alpha"
action = "wait"
duration_ms = 1
timeout_ms = 1000
expect_event = "delay_elapsed"
expect_detail = "deliberately-mismatched-private-wire-text"
`);
write('setup-failure.toml', `schema_version = 1
seed = 19
[[steps]]
id = "connect"
station = "demo-missing"
action = "connect"
timeout_ms = 1000
expect_event = "connected"
`);

function liveScenario(station, seed, protocol) {
  return `schema_version = 1
seed = ${seed}
[[steps]]
id = "connect"
station = "${station}"
action = "connect"
timeout_ms = 2000
expect_event = "connected"
expect_detail = "${protocol}"
[[steps]]
id = "window"
station = "${station}"
action = "wait"
duration_ms = 6000
timeout_ms = 7000
expect_event = "delay_elapsed"
[[steps]]
id = "disconnect-checkpoint"
station = "${station}"
action = "wait"
duration_ms = 1
timeout_ms = 2000
[[steps]]
id = "reconnect-checkpoint"
station = "${station}"
action = "wait"
duration_ms = 1
timeout_ms = 2000
[[steps]]
id = "heartbeat"
station = "${station}"
action = "heartbeat"
timeout_ms = 2000
expect_message = "Heartbeat"
expect_event = "heartbeat_result"
expect_detail = "2026-09-01T00:00:00Z"
[[steps]]
id = "remote-start"
station = "${station}"
action = "await_remote_start"
timeout_ms = 3000
expect_event = "remote_start_received"
[[steps]]
id = "disconnect"
station = "${station}"
action = "disconnect"
timeout_ms = 2000
expect_event = "disconnected"
`;
}
write('live-alpha.toml', liveScenario('demo-alpha', 16, 'ocpp1.6'));
write('live-beta.toml', liveScenario('demo-beta', 17, 'ocpp2.0.1'));

const prebuilt = process.argv.includes('--prebuilt');
const root = new URL('../../', import.meta.url);
const programs = [
  {
    executable: prebuilt ? './target/debug/examples/browser_wire_peer' : 'cargo',
    args: prebuilt ? [] : ['run', '--locked', '-p', 'uob-sim', '--example', 'browser_wire_peer'],
  },
  {
    executable: prebuilt ? './target/debug/uob-sim' : 'cargo',
    args: prebuilt
      ? ['serve', '--config', join(directory, 'control.toml'), '--control-bind', '127.0.0.1:39194']
      : ['run', '--locked', '-p', 'uob-sim', '--', 'serve', '--config', join(directory, 'control.toml'), '--control-bind', '127.0.0.1:39194'],
  },
];
let shuttingDown = false;
let exitCode = 0;
const children = new Set();

function shutdown(code) {
  if (shuttingDown) return;
  shuttingDown = true;
  exitCode = code;
  for (const child of children) {
    if (child.pid && child.exitCode === null) {
      try { process.kill(-child.pid, 'SIGTERM'); } catch (error) {
        if (error.code !== 'ESRCH') throw error;
      }
    }
  }
  if (children.size === 0) process.exitCode = exitCode;
}

function launch({ executable, args }) {
  const child = spawn(executable, args, { cwd: root, stdio: 'inherit', detached: true });
  children.add(child);
  child.on('error', error => {
    console.error(`${executable}: ${error.message}`);
    shutdown(1);
  });
  child.on('exit', code => {
    if (!shuttingDown) shutdown(code || 1);
  });
  child.on('close', () => {
    children.delete(child);
    if (shuttingDown && children.size === 0) process.exitCode = exitCode;
  });
}

for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => shutdown(0));
process.on('exit', () => rmSync(directory, { recursive: true, force: true }));

launch(programs[0]);
for (let attempt = 0; attempt < 1200 && !shuttingDown; attempt++) {
  try {
    const response = await fetch('http://127.0.0.1:39195/observations', {
      signal: AbortSignal.timeout(500),
    });
    if (response.ok) break;
  } catch {
    // Cargo can still be compiling the example; never start the simulator before its peer.
  }
  await new Promise(resolve => setTimeout(resolve, 100));
  if (attempt === 1199) shutdown(1);
}
if (!shuttingDown) launch(programs[1]);
