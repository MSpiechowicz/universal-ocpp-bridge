import { expect, test } from '@playwright/test';
import type { Locator } from '@playwright/test';
import { createHash, randomUUID } from 'node:crypto';
import { startLiveDaemon } from '../scripts/live-daemon-fixture.mjs';
import { streamEffect } from './live-command/effects';
import type { CanonicalResource, Resource } from './live-command/effects';

type Command = { request_id: string; correlation_id: string; resource: Resource; operation: unknown; expires_at: string };
type Result = { correlation_id: string; return_route: { origin: { kind: string; principal_id: string } }; lifecycle: { stage: string; accepted?: boolean }; observed_effects?: { event_id: string; event_type: string }[] };
const submissionErrorCodes: Record<string, true> = {
  'command.admission_unavailable': true, 'command.authentication_required': true, 'command.busy': true,
  'command.expired': true, 'command.invalid_configuration_payload': true, 'command.invalid_configuration_schema': true,
  'command.invalid_request': true, 'command.persistence_unavailable': true, 'command.policy_rejected': true,
  'command.request_conflict': true, 'command.resource_unauthorized': true, 'command.schema_unavailable': true,
  'command.unauthorized': true, 'command.unsupported': true, 'command.unsupported_schema': true,
  'query.concurrency_limit': true, 'query.deadline_exceeded': true,
};
const lifecycleErrorCodes: Record<string, true> = {
  unauthorized: true, invalid_parameters: true, expired: true, unsupported_operation: true,
  station_disconnected: true, policy_rejected: true, protocol_rejected: true,
};
const safeSubmissionCode = (payload: unknown): string => {
  if (!payload || typeof payload !== 'object') return 'unavailable';
  const { error, lifecycle } = payload as Record<string, unknown>;
  if (typeof error === 'string' && submissionErrorCodes[error] === true) return error;
  if (lifecycle && typeof lifecycle === 'object') {
    const { stage, error: reason } = lifecycle as Record<string, unknown>;
    if (stage === 'rejected' && reason && typeof reason === 'object') {
      const code = (reason as Record<string, unknown>).code;
      if (typeof code === 'string' && lifecycleErrorCodes[code] === true) return code;
    }
  }
  return 'unavailable';
};

const draft = (resource: Resource, operation: unknown): Command => ({
  request_id: randomUUID(), correlation_id: randomUUID(), resource, operation,
  expires_at: new Date(Date.now() + 120000).toISOString(),
});

async function enterSecret(input: Locator, value: string) {
  await input.evaluate((element, secret) => {
    const password = element as HTMLInputElement;
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
    if (!setter || password.type !== 'password') throw new Error('Credential field unavailable');
    setter.call(password, secret);
    password.dispatchEvent(new Event('input', { bubbles: true }));
  }, value);
}

// All private grants travel in headers or password inputs. Neither requests, URLs, nor assertions
// print them; the live Playwright configuration disables trace capture and screenshots.
test('real daemon separates command admission, protocol replies and station observations for both editions', async ({ page, request }) => {
  const fixture = await startLiveDaemon({ commands: true });
  const { base } = fixture;
  const read = { Authorization: `Bearer ${fixture.grant()}` };
  const control = { Authorization: `Bearer ${fixture.control()}` };
  const privileged = { Authorization: `Bearer ${fixture.privileged()}` };
  const commandUrl = `${base}/api/v1/commands`;
  const submittedBodies = new Map<string, string[]>();
  const urls: string[] = [];
  page.on('request', outgoing => {
    urls.push(outgoing.url());
    if (outgoing.url() !== commandUrl || outgoing.method() !== 'POST') return;
    const body = outgoing.postData() ?? '';
    const id = JSON.parse(body).request_id as string;
    const digest = createHash('sha256').update(body).digest('hex');
    submittedBodies.set(id, [...(submittedBodies.get(id) ?? []), digest]);
  });
  const status = async (id: string): Promise<Result> => {
    const response = await request.get(`${commandUrl}/${encodeURIComponent(id)}`, { headers: read });
    expect(response.status()).toBe(200);
    return response.json();
  };
  const history = async (station: string) => {
    const response = await request.get(`${commandUrl}?station_id=${station}`, { headers: read });
    expect(response.status()).toBe(200);
    return response.json();
  };
  const post = async (body: Command, headers: Record<string, string>, expected: number) => {
    const response = await request.post(commandUrl, { headers, data: body });
    if (response.status() !== expected) {
      let code = 'unavailable';
      try {
        code = safeSubmissionCode(await response.json());
      } catch { /* Non-JSON failures have no safe code to report. */ }
      expect(response.status(), `Command response safe code: ${code}`).toBe(expected);
    }
    return response;
  };
  const settled = async (id: string, accepted: boolean) => {
    await expect.poll(async () => (await status(id)).lifecycle.stage).toBe('protocol_response');
    const result = await status(id);
    expect(result.lifecycle.accepted).toBe(accepted);
    return result;
  };
  const effect = async (id: string, station: string, resource: Resource, eventType: string) => {
    await expect.poll(async () => (await status(id)).observed_effects?.some(item => item.event_type === eventType)).toBe(true);
    const result = await status(id);
    const eventId = result.observed_effects?.find(item => item.event_type === eventType)?.event_id;
    expect(eventId).toBeTruthy();
    await streamEffect(base, read, station, resource, eventId!, eventType);
  };
  let selectedStation = '';
  const snapshotNotice = page.locator('#stations').getByText('Snapshot stale or refreshing. Do not treat displayed observations as live.');
  const browserEvents = page.locator('.stream-panel dl div')
    .filter({ has: page.getByText('Events received', { exact: true }) }).locator('dd');
  const selectStation = async (station: string) => {
    await page.getByRole('button', { name: station, exact: true }).click();
    await page.getByRole('button', { name: 'Refresh snapshot' }).click();
    await expect(page.getByRole('heading', { name: `Authorized commands · ${station}` })).toBeVisible();
    await expect(page.getByRole('article', { name: `Station ${station} detail` })).toBeVisible();
    await expect(snapshotNotice).toBeHidden();
    selectedStation = station;
  };
  const panel = page.locator('#commands');
  const submit = async (credential: string, operation: string, resource = 'Station', fields: Record<string, string> = {}) => {
    const resourceSelect = panel.getByLabel('Target resource');
    const operationSelect = panel.getByLabel('Advertised operation');
    const limitValueInput = panel.getByRole('textbox', { name: 'value', exact: true });
    const unitSelect = panel.locator('label').filter({ hasText: /^Engineering unit/ }).getByRole('combobox');
    const phasesInput = panel.getByRole('textbox', { name: 'phases', exact: true });
    const stopTransaction = panel.getByLabel('Open transaction');
    let expectedResource = '';
    let expectedOperation = '';
    let stage = 'loading protected options';

    const selectionState = async () => {
      const [actualResource, actualOperation, stale, hidden, operationDisabled, operationRendered,
        valueCount, unitCount, phasesCount, namedUnitCount] = await Promise.all([
        resourceSelect.inputValue().catch(() => 'unavailable'),
        operationSelect.inputValue().catch(() => 'unavailable'),
        snapshotNotice.isVisible(),
        panel.getByText('Tab hidden: refresh station observations before preparing another action.').isVisible(),
        operationSelect.isDisabled().catch(() => true),
        operationSelect.locator('option').allTextContents().then(labels => labels.includes(operation)),
        limitValueInput.count(), unitSelect.count(), phasesInput.count(),
        panel.getByRole('combobox', { name: 'Engineering unit', exact: true }).count(),
      ]);
      return `resource=${actualResource} (expected ${expectedResource}), operation=${actualOperation} (expected ${expectedOperation}), ` +
        `snapshot stale=${stale}, command hidden=${hidden}, operation disabled=${operationDisabled}, requested operation rendered=${operationRendered}, ` +
        `limit value input count=${valueCount}, engineering unit select count=${unitCount}, phases input count=${phasesCount}, named unit select count=${namedUnitCount}`;
    };
    const assertSelection = async () => {
      const [actualResource, actualOperation] = await Promise.all([resourceSelect.inputValue(), operationSelect.inputValue()]);
      if (actualResource !== expectedResource || actualOperation !== expectedOperation) {
        throw new Error(`Command form lost selection after ${stage}: ${await selectionState()}`);
      }
    };

    try {
      await enterSecret(panel.getByLabel('Independent control or privileged credential'), credential);
      await panel.getByRole('button', { name: 'Load protected control options' }).click({ timeout: 3000 });
      await expect(panel.getByText('Protected control options loaded for this credential and station.')).toBeVisible({ timeout: 3000 });

      stage = 'selecting target resource';
      expectedResource = (await resourceSelect.selectOption({ label: resource === 'Station' ? `Station ${selectedStation}` : resource }, { timeout: 3000 }))[0];
      await assertSelection();

      stage = 'waiting for resource-specific operation';
      await expect(operationSelect.locator('option').filter({ hasText: operation })).toHaveCount(1, { timeout: 3000 });
      stage = 'selecting advertised operation';
      expectedOperation = (await operationSelect.selectOption({ label: operation }, { timeout: 3000 }))[0];
      await assertSelection();
      if (operation === 'stop' && fields['Open transaction']) {
        stage = 'waiting for open transaction in fresh snapshot';
        const transactionId = fields['Open transaction'];
        await expect(stopTransaction.getByRole('option', { name: transactionId, exact: true }))
          .toHaveAttribute('value', transactionId, { timeout: 3000 });
        await expect(snapshotNotice).toBeHidden();
      }
      if (operation === 'set charging limit') {
        stage = 'checking charging limit fields';
        const [valueCount, unitCount, phasesCount] = await Promise.all([
          limitValueInput.count(), unitSelect.count(), phasesInput.count(),
        ]);
        if (valueCount !== 1 || unitCount !== 1 || phasesCount !== 1) {
          throw new Error('Charging limit fields are not rendered.');
        }
      }

      for (const [field, value] of Object.entries(fields)) {
        stage = `filling ${field}`;
        let input: Locator;
        if (field === 'Engineering unit') input = unitSelect;
        else if (field === 'Open transaction') input = stopTransaction;
        else if (field === 'type' || field === 'operationalStatus') {
          input = panel.locator('label').filter({ hasText: new RegExp(`^${field}`) }).getByRole('combobox');
          await expect(input).toHaveCount(1);
        } else input = panel.getByLabel(field.replaceAll('_', ' '), { exact: true });
        if (field === 'Engineering unit' || await input.evaluate(element => element.tagName === 'SELECT', undefined, { timeout: 3000 })) {
          await input.selectOption(value, { timeout: 3000 });
          if (field === 'Open transaction') await expect(stopTransaction).toHaveValue(value);
          if (field === 'type' || field === 'operationalStatus') await expect(input).toHaveValue(value);
          if (field === 'Engineering unit' && await unitSelect.inputValue() !== value) {
            throw new Error('Engineering unit did not retain the requested choice.');
          }
        } else {
          await input.fill(value, { timeout: 3000 });
        }
        await assertSelection();
      }

      stage = 'confirming destination';
      await panel.getByLabel(/Confirm this submission only:/).check({ timeout: 3000 });
      await assertSelection();
    } catch {
      throw new Error(`Command form failed at ${stage}: ${await selectionState()}`);
    }

    const submitButton = panel.getByRole('button', { name: 'Submit once' });
    try {
      await expect(submitButton).toBeEnabled({ timeout: 3000 });
    } catch {
      throw new Error(`Command form failed before submitting: ${await selectionState()}`);
    }
    const response = page.waitForResponse(reply => reply.url() === commandUrl && reply.request().method() === 'POST');
    try {
      await submitButton.click({ timeout: 3000 });
    } catch {
      void response.catch(() => {});
      throw new Error(`Command form failed while submitting: ${await selectionState()}`);
    }
    const submission = await response;
    if (submission.status() !== 202) {
      let code = 'unavailable';
      try {
        code = safeSubmissionCode(await submission.json());
      } catch { /* Non-JSON failures have no safe code to report. */ }
      expect(submission.status(), `Command response safe code: ${code}`).toBe(202);
    }
    const id = (await panel.locator('p').filter({ hasText: 'Immutable request' }).locator('code').first().textContent())!;
    await expect(panel.getByText('Submission returned. Read status to distinguish admission, protocol response and observed effects.')).toBeVisible();
    return id;
  };
  const reset = async () => { await panel.getByRole('button', { name: 'Prepare new request' }).click(); };
  const retry = async (id: string, credential: string, before: Record<string, number>) => {
    await enterSecret(panel.getByLabel('Independent control or privileged credential'), credential);
    await panel.getByLabel(/Confirm this submission only:/).check();
    const response = page.waitForResponse(reply => reply.url() === commandUrl && reply.request().method() === 'POST');
    await panel.getByRole('button', { name: 'Intentionally retry exact request' }).click();
    expect((await response).status()).toBe(202);
    await expect(panel.getByText(`Immutable request ${id}`, { exact: false })).toBeVisible();
    const digests = submittedBodies.get(id);
    expect(digests?.length).toBe(2);
    expect(digests?.[0]).toBe(digests?.[1]);
    expect(await fixture.counts()).toEqual(before);
  };

  try {
    expect(new Set([fixture.grant(), fixture.control(), fixture.privileged()]).size).toBe(3);
    await fixture.phase('prepare-controls', 'controls-prepared');
    for (const station of ['station-a', 'station-b']) {
      await expect.poll(async () => {
        const response = await request.get(`${base}/api/v1/stations/${station}`, { headers: read });
        const snapshot = await response.json();
        return snapshot.transactions.filter((tx: { state: string }) => tx.state !== 'ended').length;
      }, { timeout: 15000 }).toBe(0);
    }
    const a = await (await request.get(`${base}/api/v1/stations/station-a`, { headers: read })).json();
    const b = await (await request.get(`${base}/api/v1/stations/station-b`, { headers: read })).json();
    const stations = [
      { id: 'station-a', snapshot: a, prefix: 'a', protocol: 'ocpp16j', resource: 'Connector connector-1', field: 'connectorId', transaction: 'transaction.started' },
      { id: 'station-b', snapshot: b, prefix: 'b', protocol: 'ocpp201', resource: 'EVSE evse-1', field: 'operationalStatus', transaction: 'transaction.started' },
    ];
    const submitted: Record<string, string[]> = { 'station-a': [], 'station-b': [] };

    expect((await request.get(`${base}/api/v1/command-schemas?station_id=station-a`, { headers: read })).status()).toBe(401);
    expect((await request.get(`${base}/api/v1/command-schemas?station_id=station-b`, { headers: control })).status()).toBe(200);
    const controlSchemaAResponse = await request.get(`${base}/api/v1/command-schemas?station_id=station-a`, { headers: control });
    expect(controlSchemaAResponse.status()).toBe(200);
    const controlSchemaA = await controlSchemaAResponse.json();
    expect(controlSchemaA.start?.resource).toEqual(a.station);
    const authorizationReference = controlSchemaA.start?.authorization_reference;
    expect(typeof authorizationReference === 'string' && authorizationReference.length > 0).toBe(true);
    const schemaA = await (await request.get(`${base}/api/v1/command-schemas?station_id=station-a`, { headers: privileged })).json();
    expect(schemaA.start).toBeNull();
    expect(schemaA.items.some((item: { action: string }) => item.action === 'ChangeAvailability')).toBe(true);
    const rejected = draft(a.station, { kind: 'start', parameters: { authorization_reference: 'not-provisioned' } });
    await post(rejected, read, 401);
    await post(rejected, privileged, 403);
    await post({ ...rejected, request_id: randomUUID(), origin: { kind: 'management', principal_id: 'attacker' } } as Command, control, 400);
    const invalidReference = await post(draft(a.station, { kind: 'start', parameters: { authorization_reference: 'not-provisioned' } }), control, 400);
    expect((await invalidReference.json()).error).toBe('command.policy_rejected');
    await post(draft(a.station, { kind: 'set_charging_limit', parameters: { value: '16', unit: 'ampere' } }), control, 422);
    await post(draft(a.resources[0].resource, { kind: 'set_charging_limit', parameters: { value: '-1', unit: 'ampere' } }), control, 400);
    await post({ ...draft(a.station, { kind: 'start', parameters: { authorization_reference: authorizationReference } }),
      expires_at: new Date(Date.now() - 60000).toISOString() }, control, 410);
    const unsupportedChildStop = await post(draft(b.resources[2].resource, { kind: 'stop', parameters: { transaction_id: 'unsupported-child-stop' } }), control, 422);
    expect((await unsupportedChildStop.json()).lifecycle.error.code).toBe('unsupported_operation');
    const privilegedAction = { kind: 'ocpp', parameters: { protocol: 'ocpp16j', action: 'ChangeAvailability', payload_schema: schemaA.items[0].payload_schema, payload: { connectorId: 0, type: 'Inoperative' } } };
    await post(draft(a.station, privilegedAction), control, 403);
    await post(draft(b.station, privilegedAction), privileged, 422);
    await post(draft(a.station, { kind: 'ocpp', parameters: { ...privilegedAction.parameters, payload: { connectorId: 1, type: 'Inoperative' } } }), privileged, 400);
    await post(draft(a.station, { kind: 'ocpp', parameters: { protocol: 'ocpp16j', action: 'Reset',
      payload_schema: 'urn:OCPP:1.6:2019:12:ResetRequest', payload: { type: 'Soft' } } }), privileged, 400);
    expect(await fixture.counts()).toMatchObject({ 'a-start': 0, 'b-start': 0, 'a-stop': 0, 'b-stop': 0, 'a-limit': 0, 'b-limit': 0, 'a-availability': 0, 'b-availability': 0 });

    await page.goto(base);
    await enterSecret(page.getByLabel('Management read credential'), fixture.grant());
    await page.getByRole('button', { name: 'Connect to demo' }).click();
    await expect(page.locator('.status')).toHaveText('live');

    for (const { id, snapshot, prefix, protocol, resource, field, transaction } of stations) {
      await selectStation(id);
      await enterSecret(panel.getByLabel('Independent control or privileged credential'), fixture.grant());
      await panel.getByRole('button', { name: 'Load protected control options' }).click();
      await expect(panel.getByText('Access denied. Check the credential and resource scope.')).toBeVisible();
      const start = await submit(fixture.control(), 'start');
      submitted[id].push(start);
      const reply = await settled(start, true);
      expect(reply.return_route.origin).toEqual({ kind: 'management', principal_id: 'management-control' });
      expect(reply.observed_effects ?? []).toEqual([]);
      await post({ ...draft(snapshot.station, { kind: 'stop', parameters: { transaction_id: 'different' } }),
        request_id: start, correlation_id: reply.correlation_id }, control, 409);
      await panel.getByRole('button', { name: 'Check request status' }).click();
      await expect(panel.getByText('Charger accepted protocol request; physical effect not established').first()).toBeVisible();
      const beforeStart = await fixture.counts();
      expect(beforeStart[`${prefix}-start`]).toBe(1);
      expect(beforeStart[`${prefix}-started`]).toBe(0);
      await retry(start, fixture.control(), beforeStart);
      const beforeStartEvents = Number(await browserEvents.textContent());
      await fixture.phase(`start-${prefix}`, `started-${prefix}`);
      await expect.poll(async () => (await (await request.get(`${base}/api/v1/stations/${id}`, { headers: read })).json()).transactions.filter((tx: { state: string }) => tx.state !== 'ended').length).toBe(1);
      const fresh = await (await request.get(`${base}/api/v1/stations/${id}`, { headers: read })).json();
      const openTransactions = fresh.transactions.filter((tx: { state: string }) => tx.state !== 'ended') as { transaction_id: string; state: string; resource: Resource }[];
      expect(openTransactions).toHaveLength(1);
      const openTransaction = openTransactions[0];
      expect(openTransaction.state).toBe('pending');
      expect(openTransaction.resource).toMatchObject({
        station_id: id,
        resource: id === 'station-a'
          ? { kind: 'connector', connector_id: 'connector-1' }
          : { kind: 'evse', evse_id: 'evse-1', connector_id: 'connector-1' },
      });
      await effect(start, id, openTransaction.resource, transaction);
      // The browser subscribes to station events, so it receives the station
      // snapshot invalidation; effect() separately proves the resource event.
      await expect.poll(async () => Number(await browserEvents.textContent())).toBeGreaterThanOrEqual(beforeStartEvents + 1);
      await selectStation(id);
      const openRow = page.getByRole('article', { name: `Station ${id} detail` }).locator('.transaction-list li')
        .filter({ has: page.getByText(openTransaction.transaction_id, { exact: true }) });
      await expect(openRow.locator('strong')).toHaveText('pending');
      await expect(snapshotNotice).toBeHidden();

      await reset();

      const limit = await submit(fixture.control(), 'set charging limit', resource, { value: '16', 'Engineering unit': 'ampere', phases: '1' });
      submitted[id].push(limit);
      const limitReply = await settled(limit, true);
      expect(limitReply.observed_effects ?? []).toEqual([]);
      const afterLimit = await (await request.get(`${base}/api/v1/stations/${id}`, { headers: read })).json();
      const stillOpen = afterLimit.transactions.filter((tx: { state: string }) => tx.state !== 'ended');
      expect(stillOpen).toHaveLength(1);
      expect(stillOpen[0]).toMatchObject({ transaction_id: openTransaction.transaction_id, state: 'pending' });
      const beforeLimit = await fixture.counts();
      expect(beforeLimit[`${prefix}-limit`]).toBe(1);
      await retry(limit, fixture.control(), beforeLimit);
      await reset();

      await selectStation(id);
      await expect(openRow.locator('strong')).toHaveText('pending');
      await expect(snapshotNotice).toBeHidden();
      const stop = await submit(fixture.control(), 'stop', 'Station', { 'Open transaction': openTransaction.transaction_id });
      submitted[id].push(stop);
      const stopReply = await settled(stop, true);
      expect(stopReply.observed_effects ?? []).toEqual([]);
      const beforeStop = await fixture.counts();
      expect(beforeStop[`${prefix}-stop`]).toBe(1);
      await retry(stop, fixture.control(), beforeStop);
      const beforeStopEvents = Number(await browserEvents.textContent());
      await fixture.phase(`stop-${prefix}`, `ended-${prefix}`);
      await effect(stop, id, openTransaction.resource, 'transaction.ended');
      // The station-scoped browser stream receives only the station snapshot
      // invalidation; effect() separately proves the resource event.
      // Allow SSE delivery and the tab's 1s event-count publisher before refreshing.
      await expect.poll(async () => Number(await browserEvents.textContent()), { timeout: 15000 })
        .toBeGreaterThanOrEqual(beforeStopEvents + 1);
      await reset();

      await selectStation(id);
      const schema = await (await request.get(`${base}/api/v1/command-schemas?station_id=${id}`, { headers: privileged })).json();
      expect(schema.items).toEqual(expect.arrayContaining([expect.objectContaining({ resource: snapshot.station, protocol, action: 'ChangeAvailability' })]));
      const fields = field === 'connectorId' ? { connectorId: '0', type: 'Inoperative' } : { operationalStatus: 'Inoperative' };
      const availability = await submit(fixture.privileged(), `Privileged ${protocol} / ChangeAvailability`, 'Station', fields);
      submitted[id].push(availability);
      const availabilityReply = await settled(availability, true);
      expect(availabilityReply.return_route.origin).toEqual({ kind: 'management', principal_id: 'management-privileged' });
      expect(availabilityReply.observed_effects ?? []).toEqual([]);
      const beforeAvailability = await fixture.counts();
      expect(beforeAvailability[`${prefix}-availability`]).toBe(1);
      await retry(availability, fixture.privileged(), beforeAvailability);
      await fixture.phase(`availability-${prefix}`, `availability-observed-${prefix}`);
      await expect.poll(async () => {
        const observed = await (await request.get(`${base}/api/v1/stations/${id}`, { headers: read })).json() as {
          resources: { resource: Resource; availability: string }[];
        };
        const availabilityFor = (target: CanonicalResource) => observed.resources.find(({ resource: { resource } }) =>
          resource?.kind === target.kind && resource.connector_id === target.connector_id &&
          (target.kind !== 'evse' || (resource.kind === 'evse' && resource.evse_id === target.evse_id))
        )?.availability;

        if (id === 'station-a') return [availabilityFor({ kind: 'connector', connector_id: 'connector-1' })];

        // EVSE 2's native connector 1 has canonical connector identity connector-2.
        return [
          availabilityFor({ kind: 'evse', evse_id: 'evse-1', connector_id: 'connector-1' }),
          availabilityFor({ kind: 'evse', evse_id: 'evse-2', connector_id: 'connector-2' }),
          availabilityFor({ kind: 'evse', evse_id: 'evse-1' }),
          availabilityFor({ kind: 'evse', evse_id: 'evse-2' }),
        ];
      }).toEqual(id === 'station-a'
        ? ['unavailable']
        : ['unavailable', 'unavailable', 'unknown', 'unknown']);
      await effect(availability, id, snapshot.station, 'station.availability.observed');
      await reset();
      const rows = await history(id);
      for (const commandId of submitted[id]) {
        const summary = rows.items.find((item: { request_id: string }) => item.request_id === commandId);
        expect(summary?.correlation_id).toBeTruthy();
        expect(summary?.admitted_at).toBeTruthy();
        expect((await status(commandId)).correlation_id).toBe(summary.correlation_id);
      }
      await panel.getByRole('button', { name: 'Refresh history' }).click();
      await expect(panel.getByText(`Request ${start}`, { exact: false }).first()).toBeVisible();
      const linked = rows.items.find((item: { request_id: string }) => item.request_id === start);
      await panel.getByRole('link', { name: `Search retained diagnostics by correlation ${linked.correlation_id}` }).first().click();
      await expect(page.getByLabel('Filter correlation')).toHaveValue(linked.correlation_id);
      expect(await fixture.counts()).toMatchObject({ [`${prefix}-start`]: 1, [`${prefix}-stop`]: 1, [`${prefix}-limit`]: 1, [`${prefix}-availability`]: 1,
        [`${prefix}-started`]: 1, [`${prefix}-ended`]: 1, [`${prefix}-availability-observed`]: 1 });
      expect((await status(start)).observed_effects).toHaveLength(1);
      expect((await status(stop)).observed_effects).toHaveLength(1);
      expect((await status(limit)).observed_effects ?? []).toEqual([]);
      if (id === 'station-a') {
        const other = await (await request.get(`${base}/api/v1/stations/station-b`, { headers: read })).json();
        const observations = (station: {
          station: Resource;
          resources: { resource: Resource; availability: string }[];
          transactions: { transaction_id: string; resource: Resource; state: string; started_at: string; ended_at?: string }[];
        }) => ({
          station: station.station,
          resources: station.resources
            .map(({ resource, availability }) => ({ resource: resource.resource, availability }))
            .sort((left, right) => JSON.stringify(left.resource).localeCompare(JSON.stringify(right.resource))),
          transactions: [...station.transactions].sort((left, right) => left.transaction_id.localeCompare(right.transaction_id)),
        });
        expect(observations(other)).toEqual(observations(b));
        expect(await fixture.counts()).toMatchObject({ 'b-start': 0, 'b-stop': 0, 'b-limit': 0, 'b-availability': 0 });
        expect((await history('station-b')).items).toEqual([]);
      }
    }

    const first = submitted['station-a'][0];
    const aRows = await history('station-a');
    const bRows = await history('station-b');
    expect(aRows.items.every((item: { resource: Resource }) => item.resource.station_id === 'station-a')).toBe(true);
    expect(bRows.items.every((item: { resource: Resource }) => item.resource.station_id === 'station-b')).toBe(true);
    expect(aRows.items.some((item: { request_id: string }) => submitted['station-b'].includes(item.request_id))).toBe(false);
    expect((await status(first)).lifecycle.accepted).toBe(true);
    expect(urls.every(url => url.startsWith(base) && ![fixture.grant(), fixture.control(), fixture.privileged()].some(secret => url.includes(secret)))).toBe(true);
    await fixture.phase('stop', 'stopped');
  } finally { await fixture.cleanup(); }
});
