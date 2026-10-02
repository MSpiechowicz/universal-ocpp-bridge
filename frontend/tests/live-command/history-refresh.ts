import { expect } from '@playwright/test';
import type { Locator, Page, Route } from '@playwright/test';

export async function enterSecret(input: Locator, value: string) {
  await input.evaluate((element, secret) => {
    const password = element as HTMLInputElement;
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
    if (!setter || password.type !== 'password') throw new Error('Credential field unavailable');
    setter.call(password, secret);
    password.dispatchEvent(new Event('input', { bubbles: true }));
  }, value);
}

// Delay only delivery of authenticated, unmodified service responses. Release every
// gate on failure too, so this case cannot strand the browser or later station work.
export async function historyDuringSnapshotRefresh(page: Page, base: string, station: string, commandId: string, control: string) {
  const panel = page.locator('#commands');
  const stale = page.locator('#stations').getByText('Snapshot stale or refreshing. Do not treat displayed observations as live.');
  const credential = panel.getByLabel('Independent control or privileged credential');
  const operation = panel.getByLabel('Advertised operation');
  const confirmation = panel.getByLabel(/Confirm this submission only:/);
  const refreshHistory = panel.getByRole('button', { name: 'Refresh history' });
  const row = panel.getByText(`Request ${commandId}`, { exact: false }).first();

  await expect(stale).toBeHidden();
  await expect(credential).toBeEnabled();
  await enterSecret(credential, control);
  await panel.getByRole('button', { name: 'Load protected control options' }).click();
  await expect(panel.getByText('Protected control options loaded for this credential and station.')).toBeVisible();
  await operation.selectOption('start');
  await confirmation.check();
  await expect(row).toBeHidden();

  const origin = new URL(base).origin;
  const historyUrl = (url: URL) => url.origin === origin && url.pathname === '/api/v1/commands' && url.searchParams.get('station_id') === station;
  const snapshotUrl = (url: URL) => url.origin === origin &&
    (url.pathname === '/api/v1/stations' || url.pathname === `/api/v1/stations/${station}`);
  let historyHeld = false;
  let snapshotHeld = false;
  let snapshotArmed = false;
  let releaseHistory!: () => void;
  let releaseSnapshot!: () => void;
  const historyGate = new Promise<void>(resolve => { releaseHistory = resolve; });
  const snapshotGate = new Promise<void>(resolve => { releaseSnapshot = resolve; });
  const holdHistory = async (route: Route) => {
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    historyHeld = true;
    await historyGate;
    await route.fulfill({ response });
  };
  const holdSnapshot = async (route: Route) => {
    if (!snapshotArmed) { await route.continue(); return; }
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    snapshotHeld = true;
    await snapshotGate;
    await route.fulfill({ response });
  };
  await page.route(historyUrl, holdHistory);
  await page.route(snapshotUrl, holdSnapshot);
  try {
    await refreshHistory.click();
    await expect.poll(() => historyHeld).toBe(true);
    // Drain any earlier automatic refresh before arming the snapshot gate. This
    // guarantees a fresh-to-stale transition during this pending durable read.
    await expect(stale).toBeHidden();
    snapshotArmed = true;

    // An SSE-triggered refresh may already own the snapshot lane. In that case
    // use its real invalidation instead of waiting to click a disabled button.
    if (!await stale.isVisible()) {
      await page.getByRole('button', { name: 'Refresh snapshot' }).evaluate(element => {
        const button = element as HTMLButtonElement;
        if (!button.disabled) button.click();
      });
    }
    await expect(stale).toBeVisible();
    await expect.poll(() => snapshotHeld).toBe(true);
    await expect(confirmation).not.toBeChecked();
    await expect(operation).toHaveValue('');
    await expect.poll(() => credential.evaluate(element => (element as HTMLInputElement).value === '')).toBe(true);
    await expect(refreshHistory).toBeDisabled();
    await expect(row).toBeHidden();

    releaseHistory();
    await expect(row).toBeVisible();
    await expect(refreshHistory).toBeEnabled();
    await expect(stale).toBeVisible();
    await expect(operation).toBeDisabled();
    await expect(credential).toBeDisabled();
    await expect(confirmation).toBeDisabled();
    await expect(panel.getByRole('button', { name: 'Load protected control options' })).toBeDisabled();
    await expect(panel.getByRole('button', { name: 'Submit once' })).toBeDisabled();
  } finally {
    releaseHistory();
    releaseSnapshot();
    await page.unroute(historyUrl, holdHistory);
    await page.unroute(snapshotUrl, holdSnapshot);
  }
  await expect(stale).toBeHidden();
  await expect(confirmation).not.toBeChecked();
  await expect(operation).toHaveValue('');
  await expect(panel.getByRole('button', { name: 'Submit once' })).toBeDisabled();
}
