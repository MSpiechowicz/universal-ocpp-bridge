import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { setTimeout as delay } from 'node:timers/promises';
import { StationStore } from '../src/stations/store';
import { parsePage, parseStation } from '../src/stations/schema';
import type { StationPage } from '../src/stations/schema';
import { Stations } from '../src/stations/Stations';
import type { ApiClient } from '../src/http';
import { matches, parseRow } from '../src/debug/buffer';

const bridge = 'bridge-a';
const caps = { operations: [], optional: [], protocol_details: [] };
const station = (id: string) => ({ schema_version: { major: 1, revision: 0 }, station: { bridge_id: bridge, station_id: id },
  observed_at: '2026-09-24T10:00:00Z', connectivity: { status: 'connected', protocol: 'ocpp201', connected_at: '2026-09-24T09:00:00Z' },
  capabilities: caps, resources: [], transactions: [], current_values: [] });
async function until(predicate: () => boolean) {
  for (let attempt = 0; attempt < 100; attempt++) { if (predicate()) return; await delay(10); }
  assert.fail('Expected station state did not arrive');
}

test('canonical topology retains distinct connector and EVSE addresses, exact decimals, false and missing', () => {
  const base = station('s1');
  const connector = { bridge_id: bridge, station_id: 's1', resource: { kind: 'connector', connector_id: 'connector/1' }, native_protocol_reference: { protocol: 'ocpp16', connector_id: 1 } };
  const evse = { bridge_id: bridge, station_id: 's1', resource: { kind: 'evse', evse_id: 'EVSE-2', connector_id: 'C-1' }, native_protocol_reference: { protocol: 'ocpp201', evse_id: 2, connector_id: 1 } };
  const observed = { point_id: 'power', observed_at: base.observed_at, source_time: '2026-09-24T09:59:59Z', quality: { level: 'bad', reason: 'device_fault' }, freshness: { status: 'stale' } };
  const snapshot = parseStation({ ...base, resources: [connector, evse].map(resource => ({ resource, availability: 'unknown', capabilities: caps, data_points: [], current_values: [] })),
    current_values: [{ ...observed, value: { type: 'decimal', value: '0.000000000000001' } },
      { ...observed, point_id: 'enabled', value: { type: 'boolean', value: false } }, { ...observed, point_id: 'unavailable' }] }, bridge);
  assert.deepEqual(snapshot.resources.map(resource => resource.resource.resource?.kind), ['connector', 'evse']);
  assert.deepEqual(snapshot.resources.map(resource => resource.resource.native_protocol_reference?.protocol), ['ocpp16', 'ocpp201']);
  assert.equal(snapshot.current_values[0].value?.value, '0.000000000000001');
  assert.equal(snapshot.current_values[1].value?.value, false);
  assert.equal(snapshot.current_values[2].value, undefined);
  assert.equal(snapshot.current_values[0].quality.level, 'bad');
  assert.equal(snapshot.current_values[0].freshness.status, 'stale');
  assert.equal(snapshot.current_values[0].source_time, observed.source_time);
  assert.deepEqual(snapshot.capabilities.operations, []);
  assert.throws(() => parseStation({ ...base, current_values: [{ ...observed, value: { type: 'unsigned_integer', value: Number.MAX_SAFE_INTEGER + 1 } }] }, bridge));
  assert.equal(parsePage({ items: [base], next_cursor: 'opaque+/==' }, bridge).next_cursor, 'opaque+/==');
  assert.throws(() => parsePage({ next_cursor: 'opaque' }, bridge));
});

test('a transaction cannot borrow another station or bridge identity', () => {
  const base = station('s1');
  const transaction = { transaction_id: 'tx-1', state: 'active', started_at: base.observed_at,
    resource: { bridge_id: bridge, station_id: 's1' } };
  assert.equal(parseStation({ ...base, transactions: [transaction] }, bridge).transactions[0].transaction_id, 'tx-1');
  for (const resource of [{ bridge_id: bridge, station_id: 's2' }, { bridge_id: 'another-bridge', station_id: 's1' }]) {
    assert.throws(() => parseStation({ ...base, transactions: [{ ...transaction, resource }] }, bridge), /Transaction identity mismatch/);
  }
});

test('selection discards late detail, paginates with opaque cursor, and reconnect refreshes stale data', async () => {
  const first = parseStation(station('first'), bridge), second = parseStation(station('second'), bridge);
  let releaseFirst: (() => void) | undefined;
  let pageCalls = 0;

  const requests: string[] = [];
  const api = {
    stations: async (after?: string) => { requests.push(after ?? 'first-page'); pageCalls++;
      return after ? { items: [second] } : { items: [first], next_cursor: 'opaque+/==' }; },
    station: async (id: string) => {
      if (id === 'first') await new Promise<void>(resolve => { releaseFirst = resolve; });
      return id === 'first' ? first : second;
    },
  } as ApiClient;
  const store = new StationStore(api, () => {});
  store.initialize({ items: [first], next_cursor: 'opaque+/==' });
  store.select('first');
  await until(() => !!releaseFirst);
  store.select('second'); releaseFirst!();
  await until(() => store.state.detail?.station.station_id === 'second');
  assert.equal(store.state.selected, 'second');
  store.more();
  await until(() => store.state.page?.items.length === 2);
  assert.ok(requests.includes('opaque+/=='));
  store.stream(false);
  assert.equal(store.state.stale, true);
  store.stream(true);
  store.refresh(); store.refresh();
  await until(() => !store.state.stale && !store.state.loading);
  assert.equal(store.state.detail?.station.station_id, 'second');
  assert.ok(pageCalls >= 2);
  store.close();
});
test('a stale page failure after station selection leaves pagination retryable', async () => {
  const first = parseStation(station('first'), bridge);
  const second = parseStation(station('second'), bridge);
  let rejectPage: ((error: Error) => void) | undefined;
  const cursors: string[] = [];
  const api = {
    stations: async (after?: string) => {
      if (!after) return { items: [first] };
      cursors.push(after);
      if (cursors.length === 1) return await new Promise<StationPage>((_, reject) => {
        rejectPage = reject;
      });
      return { items: [second] };
    },
    station: async (id: string) => id === 'first' ? first : second,
  } as ApiClient;
  const store = new StationStore(api, () => {});
  store.initialize({ items: [first], next_cursor: 'opaque-next' });
  store.more();
  await until(() => !!rejectPage);
  store.select('second');
  rejectPage!(new Error('Timed out'));
  await until(() => store.state.detail?.station.station_id === 'second');
  assert.equal(store.state.selected, 'second');
  assert.equal(store.state.moreLoading, false);
  assert.equal(store.state.error, undefined);
  assert.deepEqual(store.state.page?.items.map(item => item.station.station_id), ['first']);
  store.more();
  await until(() => store.state.page?.items.length === 2);
  assert.deepEqual(cursors, ['opaque-next', 'opaque-next']);
  assert.deepEqual(store.state.page?.items.map(item => item.station.station_id), ['first', 'second']);
  store.close();
});

test('read-only surface distinguishes false, exact zero and missing observations', () => {
  const base = station('s1');
  const point = (id: string, value?: unknown) => ({ point_id: id, value, observed_at: base.observed_at,
    quality: { level: 'good' }, freshness: { status: 'unknown' } });
  const detail = parseStation({ ...base, capabilities: { ...caps, operations: [{ operation: { kind: 'stop' }, parameters: [] }] },
    current_values: [point('zero', { type: 'decimal', value: '0.00' }), point('disabled', { type: 'boolean', value: false }), point('missing')] }, bridge);
  const html = renderToStaticMarkup(createElement(Stations, { state: { page: { items: [detail] }, selected: 's1', detail, stale: false, loading: false, moreLoading: false },
    store: { refresh() {}, select() {}, more() {} } as StationStore, scope: '' }));
  assert.ok(html.includes('0.00'));
  assert.match(html, /False/);
  assert.match(html, /Unavailable/);
  assert.match(html, /<strong>stop<\/strong>/);
  assert.doesNotMatch(html, /<strong>reset<\/strong>/);
});

test('transaction and point links search station-scoped Debug traces without unsupported identifiers', () => {
  const base = station('s1');
  const owner = { bridge_id: bridge, station_id: 's1', resource: { kind: 'evse', evse_id: 'evse-1', connector_id: 'port-2' } };
  const point = (point_id: string) => ({ point_id, observed_at: base.observed_at, quality: { level: 'good' }, freshness: { status: 'current' } });
  const detail = parseStation({ ...base, transactions: [{ transaction_id: 'tx-1', resource: owner, state: 'active', started_at: base.observed_at }],
    current_values: [point('station-power')],
    resources: [{ resource: owner, availability: 'available', capabilities: caps, data_points: [], current_values: [point('power')] }] }, bridge);
  const other = parseStation(station('s2'), bridge);
  const html = renderToStaticMarkup(createElement(Stations, { state: { page: { items: [detail, other] }, selected: 's1', detail, stale: false, loading: false, moreLoading: false },
    store: { refresh() {}, select() {}, more() {} } as StationStore, scope: 's1' }));
  assert.match(html, /class="station-select" aria-current="true"[^>]*>s1<\/button>/);
  assert.match(html, /class="station-select" disabled=""[^>]*>s2<\/button>/);
  assert.match(html, /transaction-list[^]*?href="#debug"/);
  assert.match(html, /resource-card[^]*?href="#debug"/);
  const links = html.match(/<a href="#debug"[^>]*>[^<]*<\/a>/g) ?? [];
  assert.equal(links.length, 4);
  for (const link of links) assert.match(link, /Search retained Debug traces for station s1 \(station-scoped only\)/);
  assert.match(html, /not transaction, resource, or point matches/);
  const trace = parseRow(JSON.stringify({ schema_version: { major: 1, revision: 0 }, process_instance_id: 'p1',
    trace_sequence: 1, stage: 'ocpp.received', direction: 'inbound', observed_at: base.observed_at,
    target: { instance_id: 'main', kind: 'ocpp' }, outcome: { status: 'succeeded' },
    redacted_details: { fields: { station_id: 's1', protocol: 'ocpp201' } } }), 'p1');
  assert.equal(matches(trace, { station: 's1' }), true);
  assert.equal(matches(trace, { station: 's1', transaction: 'tx-1' }), false);
  assert.equal(matches(trace, { station: 's1', evse: 'evse-1' }), false);
  assert.equal(matches(trace, { station: 's1', connector: 'port-2' }), false);
  assert.equal(matches(trace, { station: 's2' }), false);
});

test('a newly selected station remains stale until its own page and detail queries both succeed', async () => {
  const first = parseStation(station('first'), bridge);
  const second = parseStation(station('second'), bridge);
  let releasePage: (() => void) | undefined;
  let releaseDetail: (() => void) | undefined;
  let rejectNextPage = false;
  const api = {
    stations: async () => {
      if (rejectNextPage) throw new Error('read unavailable');
      await new Promise<void>(resolve => { releasePage = resolve; });
      return { items: [first, second] };
    },
    station: async (id: string) => {
      if (id === 'second') await new Promise<void>(resolve => { releaseDetail = resolve; });
      return id === 'first' ? first : second;
    },
  } as ApiClient;
  const store = new StationStore(api, () => {});
  store.initialize({ items: [first, second] });
  store.select('second');
  await until(() => !!releaseDetail);
  assert.equal(store.state.stale, true);
  const finishSelection = releaseDetail!;
  releaseDetail = undefined;
  finishSelection();
  await until(() => store.state.detail?.station.station_id === 'second');
  assert.equal(store.state.stale, true);
  store.stream(true);
  await until(() => !!releasePage);
  releasePage!();
  await until(() => !!releaseDetail && store.state.loading);
  assert.equal(store.state.stale, true);
  releaseDetail!();
  await until(() => !store.state.loading);
  assert.equal(store.state.stale, false);
  rejectNextPage = true;
  store.refresh();
  await until(() => store.state.error !== undefined);
  assert.equal(store.state.stale, true);
  assert.equal(store.state.detail?.station.station_id, 'second');
  store.close();
});

test('a pre-disconnect read cannot clear stale after reconnect until a later snapshot succeeds', async () => {
  const snapshot = parseStation(station('s1'), bridge);
  const release: (() => void)[] = [];
  let calls = 0;
  const api = { stations: async () => { calls++; await new Promise<void>(resolve => { release.push(resolve); }); return { items: [snapshot] }; },
    station: async () => snapshot } as ApiClient;
  const store = new StationStore(api, () => {});
  store.stream(true);
  await until(() => release.length > 0);
  store.stream(false);
  store.stream(true);
  release.shift()!();
  await until(() => calls >= 2);
  assert.equal(store.state.stale, true);
  release.shift()!();
  await until(() => !store.state.stale);
  assert.equal(store.state.stale, false);
  store.close();
});
