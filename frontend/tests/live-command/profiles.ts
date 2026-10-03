import { expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import type { Resource } from './effects';
import { enterSecret } from './history-refresh';

type Descriptor = { resource: Resource; protocol: string; action: string; payload_schema: string;
  fields: { name: string; value_type: string; required: boolean }[] };
type Snapshot = { station: Resource; resources: { resource: Resource }[] };

export function nativeInventory(items: Descriptor[], snapshot: Snapshot, protocol: string) {
  const resources = [snapshot.station, ...snapshot.resources.map(item => item.resource)];
  const key = (resource: Resource) => JSON.stringify([resource.bridge_id, resource.station_id,
    resource.resource?.kind, resource.resource?.kind === 'evse' ? resource.resource.evse_id : undefined,
    resource.resource?.connector_id]);
  expect(new Set(items.map(item => `${key(item.resource)}:${item.action}`)).size).toBe(items.length);
  expect(items.every(item => item.protocol === protocol && resources.some(resource => key(resource) === key(item.resource)))).toBe(true);
  expect(items.every(item => item.fields.length <= 24 && item.fields.every(field => field.name.length <= 128))).toBe(true);
  for (const resource of resources) {
    const descriptors = items.filter(item => key(item.resource) === key(resource));
    for (const descriptor of descriptors) expect(descriptor.resource).toEqual(resource);
    const connector = resource.resource?.connector_id !== undefined;
    const actions = protocol === 'ocpp16j'
      ? ['ClearChargingProfile', 'GetCompositeSchedule', 'SetChargingProfile', 'TriggerMessage']
      : connector ? ['GetVariables'] : ['ClearChargingProfile', 'GetVariables', 'SetChargingProfile'];
    if (!resource.resource) actions.push('ChangeAvailability');
    expect(descriptors.map(item => item.action).sort()).toEqual(actions.sort());
    if (protocol === 'ocpp201' && connector) continue;
    for (const action of ['SetChargingProfile', 'ClearChargingProfile']) {
      expect(descriptors.find(item => item.action === action)?.payload_schema).toBe(
        protocol === 'ocpp16j' ? `urn:OCPP:1.6:2019:12:${action}Request` : `urn:OCPP:Cp:2:2020:3:${action}Request`);
    }
    expect(descriptors.find(item => item.action === 'SetChargingProfile')?.fields).toContainEqual(
      expect.objectContaining({ name: protocol === 'ocpp16j'
        ? 'csChargingProfiles.chargingSchedule.chargingSchedulePeriod[].numberPhases'
        : 'chargingProfile.chargingSchedule[].chargingSchedulePeriod[].phaseToUse', value_type: 'signed_integer', required: false }));
  }
  if (protocol === 'ocpp201') {
    expect(snapshot.resources.filter(item => item.resource.resource?.connector_id === undefined)
      .map(item => item.resource.resource?.kind === 'evse' ? item.resource.resource.evse_id : '').sort())
      .toEqual(Array.from({ length: 62 }, (_, index) => `evse-${index + 1}`).sort());
  }
}

export async function browserProfiles(page: Page, credential: string, protocol: string, commandUrl: string) {
  const panel = page.locator('#commands');
  await enterSecret(panel.getByLabel('Independent control or privileged credential'), credential);
  await panel.getByRole('button', { name: 'Load protected control options' }).click();
  const target = panel.getByLabel('Target resource');
  const operations = panel.getByLabel('Advertised operation');
  await expect(target).toBeEnabled();
  if (protocol === 'ocpp16j') {
    await target.selectOption({ label: 'Connector connector-64' });
    await expect.poll(() => operations.locator('option').allTextContents().then(labels => labels.sort())).toEqual([
      'Choose an operation', 'set charging limit', 'Privileged ocpp16j / SetChargingProfile',
      'Privileged ocpp16j / ClearChargingProfile', 'Privileged ocpp16j / TriggerMessage', 'Privileged ocpp16j / GetCompositeSchedule',
    ].sort());
    await operations.selectOption({ label: 'Privileged ocpp16j / SetChargingProfile' });
    await expect(operations).toBeEnabled();
    await expect(panel.getByRole('textbox', { name: 'csChargingProfiles.chargingSchedule.chargingSchedulePeriod[].numberPhases', exact: true })).toBeVisible();
    await operations.selectOption({ label: 'Privileged ocpp16j / ClearChargingProfile' });
    await panel.getByRole('textbox', { name: 'connectorId', exact: true }).fill('64');
    await panel.getByLabel(/Confirm this submission only:/).check();
    await expect(panel.getByRole('button', { name: 'Submit once' })).toBeEnabled();
    return;
  }

  await target.selectOption({ label: 'EVSE evse-62' });
  await expect.poll(() => operations.locator('option').allTextContents().then(labels => labels.sort())).toEqual([
    'Choose an operation', 'set charging limit', 'Privileged ocpp201 / SetChargingProfile',
    'Privileged ocpp201 / ClearChargingProfile', 'Privileged ocpp201 / GetVariables',
  ].sort());
  await operations.selectOption({ label: 'Privileged ocpp201 / SetChargingProfile' });
  await expect(operations).toBeEnabled();
  await expect(panel.getByRole('textbox', { name: 'chargingProfile.chargingSchedule[].chargingSchedulePeriod[].phaseToUse', exact: true })).toBeVisible();
  await panel.getByRole('textbox', { name: 'evseId', exact: true }).fill('62');
  await panel.getByLabel(/Confirm this submission only:/).check();
  let posted = 0;
  const observe = (request: { url(): string; method(): string }) => {
    if (request.url() === commandUrl && request.method() === 'POST') posted++;
  };
  page.on('request', observe);
  try {
    await panel.getByRole('button', { name: 'Submit once' }).click();
    await expect(panel.getByRole('status')).toContainText('Unsupported schema field');
    expect(posted).toBe(0);
    await expect(panel.getByRole('button', { name: 'Intentionally retry exact request' })).toHaveCount(0);
  } finally {
    page.off('request', observe);
  }
  // Exact connector targets do not acquire their parent EVSE's native profile actions.
  await target.selectOption({ label: 'EVSE evse-2 / connector-2' });
  await expect.poll(() => operations.locator('option').allTextContents().then(labels =>
    labels.filter(label => label.includes('ChargingProfile')))).toEqual([]);
  await target.selectOption({ label: 'EVSE evse-1' });
  await operations.selectOption({ label: 'set charging limit' });
  await expect(panel.getByRole('textbox', { name: 'value', exact: true })).toBeVisible();
}
