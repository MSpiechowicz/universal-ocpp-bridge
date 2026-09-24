// Disposable real service and authenticated WebSocket peers; no private credentials
// enter process arguments, the environment, test traces, or fixture logs.
import { spawn } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomBytes } from 'node:crypto';
import { createInterface } from 'node:readline';

const root = resolve(fileURLToPath(new URL('../../', import.meta.url)));
const management = 'http://127.0.0.1:39195';
const charging = 39196;
const delay = milliseconds => new Promise(done => setTimeout(done, milliseconds));
async function deadline(promise, milliseconds, label) {
  let timer;
  try {
    return await Promise.race([
      promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(label)), milliseconds); }),
    ]);
  } finally { clearTimeout(timer); }
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  await Promise.race([new Promise(resolve => child.once('exit', resolve)), delay(3000)]);
  if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await new Promise(resolve => child.once('exit', resolve)); }
}

export async function startLiveDaemon() {
  const directory = mkdtempSync(join(tmpdir(), 'uob-live-browser-'));
  chmodSync(directory, 0o700);
  let daemon;
  const terminate = () => {
    peers?.kill('SIGKILL');
    daemon?.kill('SIGKILL');
    rmSync(directory, { recursive: true, force: true });
  };
  process.once('exit', terminate);
  let peers;
  const write = (name, contents) => {
    const path = join(directory, name);
    writeFileSync(path, contents, { mode: 0o600, flag: 'wx' });
    return path;
  };
  const cleanup = async () => {
    try {
      await stop(peers);
      await stop(daemon);
    } finally {
      rmSync(directory, { recursive: true, force: true });
      process.off('exit', terminate);
    }
  };
  try {
    const state = join(directory, 'state');
    mkdirSync(state, { mode: 0o700 });
    const grantPath = write('read-grant', `uob1.demo.${randomBytes(32).toString('hex')}`);
    const alpha = write('station-a', randomBytes(32).toString('hex'));
    const bravo = write('station-b', randomBytes(32).toString('hex'));
    const config = write('bridge.toml', `[bridge]
id = "live-browser-demo"
environment = "demo"
[management]
listen_addr = "127.0.0.1:39195"
[charging]
enabled = true
listen_addr = "127.0.0.1:39196"
state_directory = ${JSON.stringify(state)}
read_grant_file = ${JSON.stringify(grantPath)}
[[charging.stations]]
id = "station-a"
protocol = "ocpp16j"
credential_file = ${JSON.stringify(alpha)}
[[charging.stations.resources]]
connector_id = "connector-1"
native_connector_id = 1
[[charging.stations]]
id = "station-b"
protocol = "ocpp201"
credential_file = ${JSON.stringify(bravo)}
[[charging.stations.resources]]
evse_id = "evse-1"
native_evse_id = 1
[[charging.stations.resources]]
evse_id = "evse-1"
connector_id = "connector-1"
native_evse_id = 1
native_connector_id = 1
[[charging.stations.resources]]
evse_id = "evse-2"
native_evse_id = 2
[[charging.stations.resources]]
evse_id = "evse-2"
connector_id = "connector-2"
native_evse_id = 2
native_connector_id = 1
`);
    const environment = { ...process.env };
    delete environment.NOTIFY_SOCKET;
    delete environment.WATCHDOG_USEC;
    delete environment.WATCHDOG_PID;
    daemon = spawn(join(root, 'target/debug/uob'), ['serve', '--config', config], {
      cwd: root, stdio: 'ignore', env: environment,
    });
    daemon.on('error', () => {});
    let ready = false;
    for (let attempt = 0; attempt < 240; attempt++) {
      if (daemon.exitCode !== null || daemon.pid === undefined) throw new Error('live daemon exited before readiness');
      try {
        const response = await fetch(`${management}/api/v1/identity`, { signal: AbortSignal.timeout(500) });
        if (response.ok && (await response.json()).bridge_id === 'live-browser-demo') { ready = true; break; }
      } catch { /* A newly bound listener is not ready yet. */ }
      await delay(250);
    }
    if (!ready) throw new Error('live daemon did not become ready');
    peers = spawn(join(root, 'target/debug/examples/charging_browser_peer'), [String(charging), alpha, bravo], {
      cwd: root, stdio: ['pipe', 'pipe', 'ignore'],
    });
    peers.on('error', () => {});
    const lines = createInterface({ input: peers.stdout });
    const pending = [];
    const received = [];
    lines.on('line', line => {
      const waiter = pending.shift();
      if (waiter) waiter(line);
      else received.push(line);
    });
    const phase = async (command, expected) => {
      if (peers.exitCode !== null || peers.pid === undefined) throw new Error(`peer exited before ${expected}`);
      if (command) peers.stdin.write(`${command}\n`);
      const line = await deadline(received.length ? Promise.resolve(received.shift()) : new Promise(resolve => pending.push(resolve)), 15000, `peer ${expected} timed out`);
      if (line !== expected) throw new Error(`peer ${expected} failed`);
    };
    await phase(null, 'ready');
    return { base: management, grant: () => readFileSync(grantPath, 'utf8'), phase, cleanup };
  } catch (error) {
    await cleanup();
    throw error;
  }
}
