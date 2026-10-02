// Disposable real service and authenticated WebSocket peers; no private credentials
// enter process arguments, the environment, test traces, or fixture logs.
import { spawn } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomBytes, randomUUID } from 'node:crypto';
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

export async function startLiveDaemon({ commands = false } = {}) {
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
    const controlPath = commands ? write('control-grant', `uob1.demo.${randomBytes(32).toString('hex')}`) : undefined;
    const privilegedPath = commands ? write('privileged-grant', `uob1.demo.${randomBytes(32).toString('hex')}`) : undefined;
    const startAlpha = commands ? write('start-a', randomBytes(10).toString('hex')) : undefined;
    const startBravo = commands ? write('start-b', randomBytes(10).toString('hex')) : undefined;
    const alpha = write('station-a', randomBytes(32).toString('hex'));
    const bravo = write('station-b', randomBytes(32).toString('hex'));
    const alphaConnectors = commands ? 64 : 1;
    const alphaResources = Array.from({ length: alphaConnectors }, (_, index) => `[[charging.stations.resources]]
connector_id = "connector-${index + 1}"
native_connector_id = ${index + 1}
`).join('');
    // Keep the two observed connectors and fill the remaining configured-resource
    // budget with exact EVSE-only addresses. Native201 profile scope never includes
    // a connector; the command fixture reaches the daemon's 64-child limit.
    const bravoExtraResources = commands ? Array.from({ length: 60 }, (_, index) => `[[charging.stations.resources]]
evse_id = "evse-${index + 3}"
native_evse_id = ${index + 3}
`).join('') : '';
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
${commands ? `control_grant_file = ${JSON.stringify(controlPath)}
privileged_grant_file = ${JSON.stringify(privilegedPath)}
` : ''}
[[charging.stations]]
id = "station-a"
protocol = "ocpp16j"
credential_file = ${JSON.stringify(alpha)}
${commands ? `start_token_file = ${JSON.stringify(startAlpha)}
allow_stop = true
allow_charging_limit = true
change_availability = true
trigger_message = true
get_composite_schedule = true
set_charging_profile = true
clear_charging_profile = true
` : ''}
${alphaResources}[[charging.stations]]
id = "station-b"
protocol = "ocpp201"
credential_file = ${JSON.stringify(bravo)}
${commands ? `start_token_file = ${JSON.stringify(startBravo)}
allow_stop = true
allow_charging_limit = true
change_availability = true
set_charging_profile = true
clear_charging_profile = true
get_variables = true
` : ''}
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
${bravoExtraResources}
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
    peers = spawn(join(root, 'target/debug/examples/charging_browser_peer'), [String(charging), alpha, bravo, String(alphaConnectors)], {
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
      if (line !== expected && !(expected === 'counts' && line.startsWith('counts '))) throw new Error(`peer ${expected} failed`);
      return line;
    };
    const counts = async () => {
      if (!commands) throw new Error('peer counts require command fixture');
      const line = await phase('counts', 'counts');
      const fields = line.match(/^counts ((?:[ab]-(?:start|stop|limit|availability|started|ended|availability-observed)=\d+\s?)+)$/);
      if (!fields) throw new Error('invalid bounded peer counts');
      const entries = fields[1].trim().split(' ').map(pair => {
        const [name, raw] = pair.split('=');
        const value = Number(raw);
        if (!Number.isSafeInteger(value) || value > 1000) throw new Error('invalid bounded peer count');
        return [name, value];
      });
      if (entries.length !== 14 || new Set(entries.map(([name]) => name)).size !== 14) throw new Error('invalid peer counter set');
      return Object.fromEntries(entries);
    };
    await phase(null, 'ready');
    // Explicit fixture-operator intent, not production initialization: these three
    // station-privileged purpose-only Clears can remove existing charging policies.
    // Call only for the disposable full-native201 roster, before canonical controls.
    const initializeProfiles201 = async station => {
      if (!commands || station.station_id !== 'station-b' || station.resource) {
        throw new Error('profile baseline requires the disposable native201 station');
      }
      const ids = [];
      for (const purpose of ['ChargingStationMaxProfile', 'TxDefaultProfile', 'TxProfile']) {
        const requestId = randomUUID();
        const correlationId = randomUUID();
        const response = await fetch(`${management}/api/v1/commands`, {
          method: 'POST',
          headers: { Authorization: `Bearer ${readFileSync(privilegedPath, 'utf8')}`, 'Content-Type': 'application/json' },
          body: JSON.stringify({ request_id: requestId, correlation_id: correlationId, resource: station,
            operation: { kind: 'ocpp', parameters: { protocol: 'ocpp201', action: 'ClearChargingProfile',
              payload_schema: 'urn:OCPP:Cp:2:2020:3:ClearChargingProfileRequest',
              payload: { chargingProfileCriteria: { chargingProfilePurpose: purpose } } } },
            expires_at: new Date(Date.now() + 120000).toISOString() }),
          signal: AbortSignal.timeout(5000),
        });
        if (response.status !== 202) throw new Error('explicit profile baseline admission failed');
        let acknowledged = false;
        for (let attempt = 0; attempt < 60; attempt++) {
          const detail = await fetch(`${management}/api/v1/commands/${requestId}`, {
            headers: { Authorization: `Bearer ${readFileSync(grantPath, 'utf8')}` }, signal: AbortSignal.timeout(5000),
          });
          if (detail.status !== 200) throw new Error('explicit profile baseline status unavailable');
          const result = await detail.json();
          if (result.lifecycle?.stage === 'protocol_response') {
            const native = result.charging_profile_201;
            if (result.correlation_id !== correlationId || native?.action !== 'ClearChargingProfile' ||
                !['Accepted', 'Unknown'].includes(native.status) ||
                native.request?.charging_profile_criteria?.charging_profile_purpose !== purpose ||
                result.lifecycle.accepted !== (native.status === 'Accepted')) {
              throw new Error('explicit profile baseline acknowledgement invalid');
            }
            acknowledged = true;
            break;
          }
          if (['rejected', 'transmission_uncertain'].includes(result.lifecycle?.stage)) {
            throw new Error('explicit profile baseline was not acknowledged');
          }
          await delay(250);
        }
        if (!acknowledged) throw new Error('explicit profile baseline acknowledgement timed out');
        ids.push(requestId);
      }
      return ids;
    };
    return { base: management, grant: () => readFileSync(grantPath, 'utf8'),
      control: () => { if (!controlPath) throw new Error('control grant unavailable'); return readFileSync(controlPath, 'utf8'); },
      privileged: () => { if (!privilegedPath) throw new Error('privileged grant unavailable'); return readFileSync(privilegedPath, 'utf8'); },
      phase, counts, initializeProfiles201, cleanup };
  } catch (error) {
    await cleanup();
    throw error;
  }
}
