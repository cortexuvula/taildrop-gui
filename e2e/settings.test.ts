import { test, expect, type Page } from '@playwright/test';

/**
 * CSS-enabled, real-component coverage of the redesigned Settings panel.
 *
 * These tests load the PRODUCTION Settings component (plus the production
 * useModalWithLabel hook, ToastProvider, and App.css) from the harness at
 * e2e/settings.e2e.html. Only external boundaries are mocked: Tauri IPC via
 * e2e/tauri-mock.ts and the updater via the harness's stub prop. No real
 * autostart preference, update install, or file transfer is ever touched.
 *
 * The static markup/focus-order checks live in focus-trap.test.ts
 * (fixture-settings.html); this file exercises the real thing.
 */

const BASE_URL = 'http://localhost:1420';
const HARNESS_URL = `${BASE_URL}/e2e/settings.e2e.html`;
const DEBUG_HARNESS_URL = `${HARNESS_URL}?panel=debug`;

function harness(page: Page) {
  return {
    state: () => page.evaluate(() => window.__settingsHarness!.state()),
    open: () => page.evaluate(() => window.__settingsHarness!.open()),
    setSettings: (partial: Record<string, unknown>) =>
      page.evaluate((p) => window.__settingsHarness!.setSettings(p), partial),
    setPeers: (count: number) =>
      page.evaluate((n) => window.__settingsHarness!.setPeers(n), count),
    setSaveDirError: (message: string | null) =>
      page.evaluate((m) => window.__settingsHarness!.setSaveDirError(m), message),
  };
}

async function openHarness(page: Page, params = '') {
  await page.goto(`${HARNESS_URL}${params}`);
  await expect(page.locator('.settings-panel')).toBeVisible();
}

function isSwitchChecked(switchBtn: import('@playwright/test').Locator) {
  return switchBtn.getAttribute('aria-checked').then((v) => v === 'true');
}

// ---------------------------------------------------------------- structure

test('renders the grouped production layout with version footer', async ({ page }) => {
  await openHarness(page);

  await expect(page.locator('.settings-card-heading').nth(0)).toContainText('Receiving');
  await expect(page.locator('.settings-card-heading').nth(1)).toContainText('Application');
  await expect(page.locator('.settings-card-heading').nth(2)).toContainText('Device visibility');
  // getVersion resolves asynchronously from the mock ("0.0.0-e2e").
  await expect(page.locator('.settings-version')).toContainText('TailDrop v');
  await expect(page.getByRole('button', { name: 'Check for updates' })).toBeVisible();
  await expect(page.locator('.settings-device-row')).toHaveCount(30);
});

test('initial focus lands on the first focusable element (close button) and the background is inert', async ({ page }) => {
  await openHarness(page);

  await expect(page.getByRole('button', { name: 'Close settings' })).toBeFocused();

  // useModal inert-walk: the trigger outside the overlay is inerted through
  // its ancestor while the dialog is open.
  const inertedAncestry = () =>
    page.evaluate(() => !!document.querySelector('#trigger-settings')?.closest('[inert]'));
  expect(await inertedAncestry()).toBe(true);
});

// ------------------------------------------------- restored visibility toggles

test('show offline nodes / show Mullvad-exit nodes toggles update the correct settings keys', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  await page.getByRole('switch', { name: 'Show offline nodes' }).click();
  await page.getByRole('switch', { name: 'Show Mullvad / exit nodes' }).click();

  const { updates, settings } = await h.state();
  expect(updates).toEqual([{ showOfflineNodes: true }, { showExitNodes: true }]);
  expect(settings.showOfflineNodes).toBe(true);
  expect(settings.showExitNodes).toBe(true);
});

test('restored toggles reflect supplied persisted values', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  // Persisted true / false combinations render as the switch state.
  await h.setSettings({ showOfflineNodes: true, showExitNodes: false });
  await expect(
    isSwitchChecked(page.getByRole('switch', { name: 'Show offline nodes' })),
  ).resolves.toBe(true);
  await expect(
    isSwitchChecked(page.getByRole('switch', { name: 'Show Mullvad / exit nodes' })),
  ).resolves.toBe(false);

  await h.setSettings({ showOfflineNodes: true, showExitNodes: true });
  await expect(
    isSwitchChecked(page.getByRole('switch', { name: 'Show offline nodes' })),
  ).resolves.toBe(true);
  await expect(
    isSwitchChecked(page.getByRole('switch', { name: 'Show Mullvad / exit nodes' })),
  ).resolves.toBe(true);
});

// ------------------------------------------------------- labels & validation

test('directory and search inputs have explicit accessible names', async ({ page }) => {
  await openHarness(page);

  // getByLabel resolves through the <label for> associations, not placeholders.
  await expect(page.getByLabel('Save directory')).toBeVisible();
  await expect(page.getByLabel('Search devices')).toBeVisible();
});

test('save-directory validation is associated with its input and keeps alert semantics', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  const input = page.getByLabel('Save directory');
  await expect(input).not.toHaveAttribute('aria-invalid', 'true');

  await h.setSaveDirError('Directory "/nonexistent/path" does not exist or is not writable');
  const alert = page.locator('#save-directory-error');
  await expect(alert).toBeVisible();
  await expect(alert).toHaveAttribute('role', 'alert');
  await expect(alert).toContainText('does not exist or is not writable');
  await expect(input).toHaveAttribute('aria-invalid', 'true');
  await expect(input).toHaveAttribute('aria-describedby', 'save-directory-error');
});

// ------------------------------------------------------ search / empty states

test('search filters by name, no-match is distinct from no-devices, clear restores the list', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  const search = page.getByLabel('Search devices');
  // Clear button is conditionally rendered — absent when search is empty.
  await expect(page.locator('.settings-search-clear')).toHaveCount(0);

  // Name match (case-insensitive): only "Studio Mac …".
  await search.fill('studio');
  await expect(page.locator('.settings-device-row')).toHaveCount(1);
  await expect(page.locator('.settings-device-row')).toContainText('Studio Mac');
  await expect(page.locator('.settings-search-clear')).toBeVisible();

  // OS match: android devices occur at indexes 3,8,13,18,23,28 of 30.
  await search.fill('android');
  await expect(page.locator('.settings-device-row')).toHaveCount(6);

  // No match — distinct message, search control stays usable.
  await search.fill('zzz-no-such-device');
  await expect(page.locator('.settings-empty')).toHaveText('No matching devices');
  await expect(page.locator('.settings-device-row')).toHaveCount(0);

  // Clear restores the full list without touching visibility preferences.
  const before = await h.state();
  await page.locator('.settings-search-clear').click();
  await expect(page.locator('.settings-device-row')).toHaveCount(30);
  await expect(search).toHaveValue('');
  await expect(page.locator('.settings-search-clear')).toHaveCount(0);
  const after = await h.state();
  expect(after.updates).toEqual(before.updates); // no settings writes from search/clear
  expect(after.settings.showOfflineNodes).toBe(before.settings.showOfflineNodes);
  expect(after.settings.hiddenNodes).toEqual(before.settings.hiddenNodes);
});

test('"No devices discovered yet" is distinct from "No matching devices"', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  await h.setPeers(0); // only the self node remains
  await expect(page.locator('.settings-empty')).toHaveText('No devices discovered yet');

  // Even with a query, zero *devices* keeps the "no devices" wording.
  await page.getByLabel('Search devices').fill('studio');
  await expect(page.locator('.settings-empty')).toHaveText('No devices discovered yet');
});

test('clicking a device row toggle switches its hiddenNodes entry', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  const firstRow = page.locator('.settings-device-row').first();
  await expect(firstRow).toContainText('Studio Mac');

  // Regression: device rows do NOT duplicate the name text (compact toggle
  // label is sr-only). Verify bounded row height — 36px min-height + 8px
  // vertical padding = 52px max for a single unwrapped line.
  const firstHeight = await firstRow.evaluate((el) => el.getBoundingClientRect().height);
  expect(firstHeight).toBeLessThanOrEqual(48);

  // The first device is visible by default (hiddenNodes is empty). The
  // compact ToggleSwitch inside the row toggles the hidden state.
  const firstSwitch = firstRow.getByRole('switch');
  await expect(isSwitchChecked(firstSwitch)).resolves.toBe(true);

  // Click the switch to hide the device.
  await firstSwitch.click();
  let state = await h.state();
  expect(state.settings.hiddenNodes).toEqual(['dev-0']);
  expect(state.updates.at(-1)).toEqual({ hiddenNodes: ['dev-0'] });
  await expect(isSwitchChecked(firstSwitch)).resolves.toBe(false);

  // Click again to make it visible once more.
  await firstSwitch.click();
  state = await h.state();
  expect(state.settings.hiddenNodes).toEqual([]);
  await expect(isSwitchChecked(firstSwitch)).resolves.toBe(true);
});

// ------------------------------------------------------------- native boundaries

test('start-on-boot toggles the mocked autostart plugin, not real preferences', async ({ page }) => {
  await openHarness(page);

  const toggle = page.getByRole('switch', { name: 'Start on boot' });
  await expect(isSwitchChecked(toggle)).resolves.toBe(false); // mock isEnabled() → false

  await toggle.click();
  await expect(isSwitchChecked(toggle)).resolves.toBe(true);

  const cmds = await page.evaluate(() => window.__e2e!.calls.map((c) => c.cmd));
  expect(cmds).toContain('plugin:autostart|enable');
});

test('Browse opens the mocked directory dialog and leaves the path unchanged on cancel', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  const input = page.getByLabel('Save directory');
  const before = await input.inputValue();
  await page.getByRole('button', { name: 'Browse' }).click();

  const cmds = await page.evaluate(() => window.__e2e!.calls.map((c) => c.cmd));
  expect(cmds).toContain('plugin:dialog|open');
  await expect(input).toHaveValue(before); // mock returns null → no write
  expect((await h.state()).updates).toEqual([]);
});

test('Check for updates consults the updater and reports up-to-date via the production toast', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  await page.getByRole('button', { name: 'Check for updates' }).click();

  expect((await h.state()).updaterCheckCalls).toBe(1);
  await expect(page.locator('.toast')).toContainText("You're up to date");
});

// -------------------------------------------------------------- focus & close

test('Tab wraps from last to first, Shift+Tab wraps from first to last', async ({ page }) => {
  await openHarness(page);

  const close = page.getByRole('button', { name: 'Close settings' });
  const checkUpdates = page.getByRole('button', { name: 'Check for updates' });

  await checkUpdates.focus();
  await page.keyboard.press('Tab');
  await expect(close).toBeFocused(); // wrap forward

  await page.keyboard.press('Shift+Tab');
  await expect(checkUpdates).toBeFocused(); // wrap backward

  // From the search input, Tab lands on the first device switch.
  await page.getByLabel('Search devices').focus();
  await page.keyboard.press('Tab');
  await expect(
    page.locator('.settings-device-row').first().getByRole('switch'),
  ).toBeFocused();
});

test('Escape closes, focus returns to the trigger, and the dialog reopens cleanly', async ({ page }) => {
  await openHarness(page);
  const h = harness(page);

  await page.keyboard.press('Escape');
  await expect(page.locator('.settings-panel')).toHaveCount(0);
  expect((await h.state()).closeCount).toBe(1);
  await expect(page.locator('#trigger-settings')).toBeFocused();
  expect(
    await page.evaluate(() => !!document.querySelector('#trigger-settings')?.closest('[inert]')),
  ).toBe(false);

  await h.open();
  await expect(page.locator('.settings-panel')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Close settings' })).toBeFocused();

  // Close button path: same close contract.
  await page.getByRole('button', { name: 'Close settings' }).click();
  expect((await h.state()).closeCount).toBe(2);
  await expect(page.locator('#trigger-settings')).toBeFocused();
});

// ----------------------------------------------------------------- layout

test('at the 700x500 minimum, header and footer stay reachable and the body scrolls the device list', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });
  await openHarness(page);
  const h = harness(page);

  // Long path + long error: worst case for horizontal overflow.
  await h.setSettings({ saveDirectory: `/Users/me/${'very-long-segment/'.repeat(12)}downloads` });
  await h.setSaveDirError(
    'Save directory is not writable: the operating system reports a read-only filesystem at this location',
  );

  const header = page.locator('.settings-header');
  const footer = page.locator('.settings-footer');
  await expect(header).toBeVisible();
  await expect(footer).toBeVisible();
  for (const loc of [header, footer]) {
    const box = await loc.boundingBox();
    expect(box!.y).toBeGreaterThanOrEqual(0);
    expect(box!.y + box!.height).toBeLessThanOrEqual(500);
  }

  // No horizontal overflow from the long path/error/name content.
  const overflow = await page.evaluate(() => {
    const panel = document.querySelector('.settings-panel') as HTMLElement;
    const body = document.querySelector('.settings-body') as HTMLElement;
    return {
      panel: panel.scrollWidth - panel.clientWidth,
      body: body.scrollWidth - body.clientWidth,
    };
  });
  expect(overflow.panel).toBeLessThanOrEqual(0);
  expect(overflow.body).toBeLessThanOrEqual(0);

  // 30 devices overflow the body; scrolling reaches the last device while
  // the footer stays pinned in view.
  const body = page.locator('.settings-body');
  const scrolls = await body.evaluate((el) => el.scrollHeight > el.clientHeight);
  expect(scrolls).toBe(true);

  await page.locator('.settings-device-row .toggle-switch').last().scrollIntoViewIfNeeded();
  const lastBox = await page.locator('.settings-device-row').last().boundingBox();
  expect(lastBox!.y).toBeGreaterThanOrEqual(0);
  expect(lastBox!.y + lastBox!.height).toBeLessThanOrEqual(500);
  const footerBox = await footer.boundingBox();
  expect(footerBox!.y + footerBox!.height).toBeLessThanOrEqual(500);

  // And the last device is actually interactive in that state (toggle
  // starts checked = visible; toggling hides it → hiddenNodes).
  const lastToggle = page.locator('.settings-device-row .toggle-switch').last().getByRole('switch');
  await lastToggle.click();
  const state = await h.state();
  expect(state.settings.hiddenNodes).toContain('dev-29');
});

test('panel keeps its ~460px width at the configured 960x640 default', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openHarness(page);

  const box = await page.locator('.settings-panel').boundingBox();
  expect(box!.width).toBeGreaterThanOrEqual(455);
  expect(box!.width).toBeLessThanOrEqual(465);
});

test('enlarged-text emulation (~150% zoom) keeps controls reachable', async ({ page }) => {
  // 700x500 at 150% scale ≈ 467x334 effective CSS pixels — the same layout
  // math the webview performs when the OS scales text/UI up.
  await page.setViewportSize({ width: 467, height: 334 });
  await openHarness(page);

  const footer = page.locator('.settings-footer');
  await expect(footer).toBeVisible();
  const box = await footer.boundingBox();
  expect(box!.y + box!.height).toBeLessThanOrEqual(334);

  const bodyScrolls = await page.locator('.settings-body').evaluate(
    (el) => el.scrollHeight > el.clientHeight,
  );
  expect(bodyScrolls).toBe(true);
  await page.locator('.settings-device-row .toggle-switch').last().scrollIntoViewIfNeeded();
  const lastBox = await page.locator('.settings-device-row').last().boundingBox();
  expect(lastBox!.y + lastBox!.height).toBeLessThanOrEqual(334);
});

// -------------------------------------------------------------- DebugPanel

test('DebugPanel retains its scrolling layout over the shared panel CSS', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });
  await page.goto(DEBUG_HARNESS_URL);
  await expect(page.locator('.debug-panel')).toBeVisible();

  // Shared classes DebugPanel still consumes keep their styling.
  const labelStyle = await page.locator('.settings-label').first().evaluate((el) => {
    const s = getComputedStyle(el);
    return { fontSize: s.fontSize, marginBottom: s.marginBottom };
  });
  expect(labelStyle.fontSize).toBe('13px');
  expect(labelStyle.marginBottom).toBe('6px');

  const sectionStyle = await page.locator('.debug-panel .settings-section').first().evaluate((el) => {
    const s = getComputedStyle(el);
    return { paddingTop: s.paddingTop, paddingLeft: s.paddingLeft, marginBottom: s.marginBottom };
  });
  expect(sectionStyle.paddingTop).toBe('16px');
  expect(sectionStyle.paddingLeft).toBe('20px');
  expect(sectionStyle.marginBottom).toBe('20px');

  // 30 peers overflow: the panel itself is the single scrolling region…
  const panel = page.locator('.debug-panel');
  const scrolls = await panel.evaluate((el) => el.scrollHeight > el.clientHeight);
  expect(scrolls).toBe(true);

  // …and the last section (Logs) is reachable at the bottom.
  await page.locator('.debug-panel .settings-section').last().scrollIntoViewIfNeeded();
  const lastBox = await page.locator('.debug-panel .settings-section').last().boundingBox();
  expect(lastBox!.y).toBeGreaterThanOrEqual(0);
  expect(lastBox!.y + lastBox!.height).toBeLessThanOrEqual(500);
  await expect(page.locator('.debug-logs')).toBeVisible();
});