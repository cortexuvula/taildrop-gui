import { test, expect, type Page } from '@playwright/test';

/**
 * CSS-enabled, real-component coverage of the staging recovery UI.
 *
 * Unlike layout.test.ts (standalone HTML fixture), these tests load the
 * PRODUCTION App mounted against a browser-side Tauri IPC mock
 * (e2e/tauri-mock.ts), so the actual composition bug class — the recovery
 * banner as an accidental third flex column inside .app — is exercised for
 * real at multiple viewports, including live transfer activity.
 */

const BASE_URL = 'http://localhost:1420';
const APP_URL = `${BASE_URL}/e2e/app.e2e.html`;

const VIEWPORTS = [
  { width: 700, height: 500 },
  { width: 960, height: 640 },
  { width: 1280, height: 800 },
];

async function openApp(page: Page, params: Record<string, string | number> = {}) {
  const qs = new URLSearchParams(
    Object.entries(params).map(([k, v]) => [k, String(v)] as [string, string]),
  ).toString();
  await page.goto(qs ? `${APP_URL}?${qs}` : APP_URL);
  // The app polls asynchronously — wait until the workspace has mounted.
  await expect(page.locator('.app')).toBeVisible();
  await expect(page.locator('.sidebar')).toBeVisible();
}

function countCalls(page: Page, cmd: string) {
  return page.evaluate(
    ({ cmd }) => (window.__e2e?.calls ?? []).filter((c) => c.cmd === cmd).length,
    { cmd },
  );
}

/** Assert the workspace composition invariants that prevent the third column. */
async function assertWorkspaceComposition(page: Page, vp: { width: number; height: number }) {
  // .app is exactly a two-child flex row: sidebar + main.
  const appChildren = await page.evaluate(() =>
    Array.from(document.querySelector('.app')!.children).map((el) => el.className),
  );
  expect(appChildren.length).toBe(2);
  expect(appChildren[0]).toContain('sidebar');
  expect(appChildren[1]).toContain('main');

  // The notice lives INSIDE .main, above the send area.
  expect(await page.evaluate(() => !!document.querySelector('.main > .recovery-notice'))).toBe(true);

  // No horizontal overflow anywhere in the workspace.
  const overflow = await page.evaluate(() => {
    const app = document.querySelector('.app') as HTMLElement;
    const main = document.querySelector('.main') as HTMLElement;
    return {
      app: app.scrollWidth - app.clientWidth,
      main: main.scrollWidth - main.clientWidth,
    };
  });
  expect(overflow.app).toBeLessThanOrEqual(0);
  expect(overflow.main).toBeLessThanOrEqual(0);

  // Send controls are neither concealed nor pushed off-screen.
  const dropzone = page.locator('.dropzone');
  await expect(dropzone).toBeVisible();
  const dzBox = (await dropzone.boundingBox())!;

  // The notice must sit fully above the drop area, not on top of it.
  const noticeBox = (await page.locator('.recovery-notice').boundingBox())!;
  expect(noticeBox.y + noticeBox.height).toBeLessThanOrEqual(dzBox.y + 1);

  // The transfer list stays reachable inside the viewport.
  const panelBox = (await page.locator('.transfer-panel').boundingBox())!;
  expect(panelBox.y).toBeGreaterThanOrEqual(0);
  expect(panelBox.y + panelBox.height).toBeLessThanOrEqual(vp.height + 0.5);
  await expect(page.locator('.transfer-panel .transfer-header')).toBeVisible();
}

for (const vp of VIEWPORTS) {
  test(`workspace with ten duplicate-filename groups + incoming + active transfer stays a two-column layout at ${vp.width}x${vp.height}`, async ({ page }) => {
    await page.setViewportSize(vp);
    await openApp(page, { scenario: 'ten', incoming: 3, hangAccept: 1 });

    // Start one active (receiving) transfer alongside the incoming backlog.
    await page.getByRole('button', { name: 'Accept incoming-0.zip' }).click();
    await expect(page.locator('.transfer-item.receiving').first()).toBeVisible();

    await assertWorkspaceComposition(page, vp);

    // Incoming Accept buttons remain reachable (scroll + hit-test).
    const lastAccept = page.getByRole('button', { name: 'Accept incoming-2.zip' });
    await lastAccept.scrollIntoViewIfNeeded();
    await expect(lastAccept).toBeEnabled();

    // The notice itself: correct title/description and Review control.
    await expect(page.locator('.recovery-notice-title')).toHaveText('Files need attention');
    await expect(page.getByText('10 recovery groups contain files that may be incomplete.')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Review' })).toBeVisible();
  });

  test(`single recovery group keeps the workspace usable at ${vp.width}x${vp.height}`, async ({ page }) => {
    await page.setViewportSize(vp);
    await openApp(page, { scenario: 'one', incoming: 1, hangAccept: 1 });
    await page.getByRole('button', { name: 'Accept incoming-0.zip' }).click();
    await expect(page.locator('.transfer-item.receiving').first()).toBeVisible();

    await assertWorkspaceComposition(page, vp);
    await expect(page.getByText('1 recovery group contains files that may be incomplete.')).toBeVisible();
  });
}

test('no recovery groups: no notice, workspace unchanged', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });
  await openApp(page, { scenario: 'none' });

  await expect(page.locator('.recovery-notice')).toHaveCount(0);
  const appChildren = await page.evaluate(() =>
    Array.from(document.querySelector('.app')!.children).map((el) => el.className),
  );
  expect(appChildren.length).toBe(2);
  await expect(page.locator('.dropzone')).toBeVisible();
  await expect(page.locator('.transfer-panel')).toBeVisible();
});

test('Review dialog: ten distinct groups, scroll to the last, activate its Recover; others survive', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });
  await openApp(page, { scenario: 'ten' });

  await page.getByRole('button', { name: 'Review' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute('aria-modal', 'true');

  // Header and footer stay reachable while the list is long.
  await expect(dialog.locator('.staging-dialog-title')).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Done' })).toBeVisible();

  // Ten DISTINCT rows despite identical filenames; full paths disclosed.
  await expect(dialog.locator('.staging-group')).toHaveCount(10);
  await expect(dialog.getByText('/tmp/taildrop-accept-0000')).toBeVisible();

  // Scroll naturally to the final group and hit-test its actions.
  const last = dialog.locator('.staging-group').nth(9);
  await last.scrollIntoViewIfNeeded();
  await expect(last).toBeVisible();
  await last.getByRole('button', { name: 'Recover 3 files from taildrop-accept-0009' }).click();

  // Handling the last group removes ONLY that group.
  await expect(dialog.locator('.staging-group')).toHaveCount(9);
  await expect(dialog.locator('.staging-group[data-staging-path="/tmp/taildrop-accept-0009"]')).toHaveCount(0);
  await expect(dialog.locator('.staging-group[data-staging-path="/tmp/taildrop-accept-0000"]')).toHaveCount(1);

  // Salvaged receipts surfaced through replay keep their unverified warning.
  await expect(page.locator('.transfer-warning').first()).toBeVisible();
  await expect(page.getByText('Unverified recovery — check file integrity').first()).toBeVisible();

  await dialog.getByRole('button', { name: 'Done' }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByText('9 recovery groups contain files that may be incomplete.')).toBeVisible();
});

test('exact sizes are shown: 40 KB, 4 B, 8 B and 0 B; zero-byte files are never auto-deleted', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openApp(page, { scenario: 'one' });
  await page.getByRole('button', { name: 'Review' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();

  await expect(dialog.getByText('vacation-photo.jpg').first()).toBeVisible();
  // "40 KB" appears both in the summary meta and the per-file size column.
  await expect(dialog.getByText('40 KB').first()).toBeVisible();
  await expect(dialog.getByText('notes.txt')).toBeVisible();
  await expect(dialog.getByText('4 B')).toBeVisible();

  // No backend mutation happened by merely reviewing.
  expect(await countCalls(page, 'discard_staging_dir')).toBe(0);
  expect(await countCalls(page, 'recover_staging_files')).toBe(0);

  // The 0 B case from the ten-group scenario.
  await openApp(page, { scenario: 'ten' });
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(page.getByRole('dialog').getByText('0 B').first()).toBeVisible();
  expect(await countCalls(page, 'discard_staging_dir')).toBe(0);
});

test('discard: confirmation names group and files; Cancel is default focus and never discards; Escape cancels confirmation', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openApp(page, { scenario: 'one' });
  await page.getByRole('button', { name: 'Review' }).click();
  const dialog = page.getByRole('dialog');

  await dialog.getByRole('button', { name: 'Discard…' }).click();

  const confirmText = dialog.locator('.staging-confirm-text');
  await expect(confirmText).toBeVisible();
  await expect(confirmText).toContainText('Discard 2 files (40 KB) from taildrop-accept-deadbeef');
  await expect(dialog.getByText('They may be the only copies')).toBeVisible();
  // Files with sizes remain disclosed next to the confirmation.
  await expect(dialog.getByText('vacation-photo.jpg')).toBeVisible();

  // Cancel is the default (focused) control.
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();

  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(confirmText).toHaveCount(0);
  expect(await countCalls(page, 'discard_staging_dir')).toBe(0);

  // Escape while the confirmation is open cancels it, not the dialog.
  await dialog.getByRole('button', { name: 'Discard…' }).click();
  await expect(confirmText).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(confirmText).toHaveCount(0);
  await expect(dialog).toBeVisible();
  expect(await countCalls(page, 'discard_staging_dir')).toBe(0);

  // Confirming discards exactly the targeted group.
  await dialog.getByRole('button', { name: 'Discard…' }).click();
  await dialog.getByRole('button', { name: 'Discard 2 files' }).click();
  await expect(page.getByRole('dialog').getByText('All recovery groups have been handled.')).toBeVisible();
  expect(await countCalls(page, 'discard_staging_dir')).toBe(1);

  await page.getByRole('dialog').getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('dialog')).not.toBeVisible();
  await expect(page.locator('.recovery-notice')).toHaveCount(0);
});

test('failed recover shows an inline error on the correct group; repeated clicks never duplicate the request', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openApp(page, { scenario: 'ten', failRecover: 1, slowRecover: 1 });
  await page.getByRole('button', { name: 'Review' }).click();
  const dialog = page.getByRole('dialog');

  // Double-click fast: while the first request is in flight the controls are
  // disabled, so the backend sees exactly one recover request.
  const recover = dialog.locator('.staging-group').nth(0).getByRole('button', { name: 'Recover 3 files from taildrop-accept-0000' });
  await recover.dblclick();
  await expect(dialog.getByText('Couldn’t handle this group: recover failed: destination disk full')).toBeVisible();
  expect(await countCalls(page, 'recover_staging_files')).toBe(1);

  // The failed group is still there — and so is the untouched ninth group.
  await expect(dialog.locator('.staging-group')).toHaveCount(10);
  await expect(dialog.locator('.staging-group[data-staging-path="/tmp/taildrop-accept-0009"]')).toHaveCount(1);

  // Retry after failure is allowed (a second, deliberate request).
  await recover.click();
  await expect(await countCalls(page, 'recover_staging_files')).toBeGreaterThanOrEqual(2);
});

test('keyboard: safe initial focus, wrap cycle, details activation, background inertness, Escape restores Review', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openApp(page, { scenario: 'one' });

  const review = page.getByRole('button', { name: 'Review' });
  await review.focus();
  await expect(review).toBeFocused();
  await page.keyboard.press('Enter');

  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();

  // Initial focus is the first group's summary — safe, not a destructive control.
  const summary = dialog.locator('.staging-group-summary').first();
  await expect(summary).toBeFocused();

  // Immediate Shift+Tab wraps forward-to-last (the Done button).
  await page.keyboard.press('Shift+Tab');
  await expect(dialog.getByRole('button', { name: 'Done' })).toBeFocused();

  // Full forward cycle: Tab wraps back to the first summary.
  await page.keyboard.press('Tab');
  await expect(summary).toBeFocused();

  // Details activation via keyboard toggles the disclosure.
  const details = dialog.locator('.staging-group-details').first();
  const wasOpen = await details.evaluate((el) => (el as HTMLDetailsElement).open);
  await page.keyboard.press('Enter');
  await expect
    .poll(() => details.evaluate((el) => (el as HTMLDetailsElement).open))
    .toBe(!wasOpen);

  // Background inertness: the sidebar is inert while the dialog is open, and
  // focus never escapes the dialog while cycling.
  expect(await page.evaluate(() => document.querySelector('.sidebar')!.hasAttribute('inert'))).toBe(true);
  for (let i = 0; i < 8; i++) {
    await page.keyboard.press(i % 2 === 0 ? 'Tab' : 'Shift+Tab');
    expect(
      await page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]')),
    ).toBe(true);
  }

  // Escape closes the dialog (never discards) and restores Review focus.
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(review).toBeFocused();
  expect(await page.evaluate(() => document.querySelector('.sidebar')!.hasAttribute('inert'))).toBe(false);
  expect(await countCalls(page, 'discard_staging_dir')).toBe(0);
});

// TD-UI-06: per-path in-flight op state is owned by useReceipts, so closing
// the Review dialog (Done/Escape/backdrop) must never reset it or allow a
// second request for the same path while the first is unresolved.
test('in-flight recover survives Done/Escape/backdrop close+reopen — exactly one request until settlement', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openApp(page, { scenario: 'ten', holdRecover: 1 });
  await page.getByRole('button', { name: 'Review' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();

  // Hold group 0000's recover pending.
  const target = dialog.locator('.staging-group[data-staging-path="/tmp/taildrop-accept-0000"]');
  await target.getByRole('button', { name: 'Recover 3 files from taildrop-accept-0000' }).click();
  await expect(target.getByText('Recovering…')).toBeVisible();
  await expect(target.getByRole('button', { name: 'Discard…' })).toBeDisabled();

  // Unrelated groups stay fully operable while it is in flight.
  const sibling = dialog.locator('.staging-group[data-staging-path="/tmp/taildrop-accept-0001"]');
  await expect(sibling.getByRole('button', { name: 'Recover 3 files from taildrop-accept-0001' })).toBeEnabled();

  // --- Close via Done; reopen: pending state survives, controls still locked. ---
  await dialog.getByRole('button', { name: 'Done' }).click();
  await expect(dialog).not.toBeVisible();
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(target.getByText('Recovering…')).toBeVisible();
  await expect(target.getByRole('button', { name: /Recover 3 files/ })).toBeDisabled();

  // --- Close via Escape; reopen: still pending. ---
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(target.getByText('Recovering…')).toBeVisible();

  // --- Close via backdrop click (overlay padding, outside the card); reopen. ---
  await dialog.click({ position: { x: 8, y: 8 } });
  await expect(dialog).not.toBeVisible();
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(target.getByText('Recovering…')).toBeVisible();

  // Reopened busy controls are inert in a real browser too — still one request.
  await expect(target.getByRole('button', { name: /Recover 3 files/ })).toBeDisabled();
  expect(await countCalls(page, 'recover_staging_files')).toBe(1);

  // --- Settlement: exactly one request total; only group 0000 handled. ---
  await page.evaluate(() => window.__e2e!.releaseRecover());
  await expect(target).toHaveCount(0);
  await expect(dialog.locator('.staging-group')).toHaveCount(9);
  expect(await countCalls(page, 'recover_staging_files')).toBe(1);
  expect(await countCalls(page, 'discard_staging_dir')).toBe(0);

  // Salvaged receipts still surfaced through replay after the reopen cycle.
  await expect(page.locator('.transfer-warning').first()).toBeVisible();

  await dialog.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByText('9 recovery groups contain files that may be incomplete.')).toBeVisible();
});

test('live staging-recovery-found event renders the notice through the event route', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openApp(page, { scenario: 'none' });
  await expect(page.locator('.recovery-notice')).toHaveCount(0);

  await page.evaluate(() =>
    window.__e2e!.emitStagingFound([
      {
        path: '/tmp/taildrop-accept-liveevent',
        files: [{ name: 'late-arrival.txt', size: 8 }],
      },
    ]),
  );

  await expect(page.getByText('1 recovery group contains files that may be incomplete.')).toBeVisible();
  await page.getByRole('button', { name: 'Review' }).click();
  await expect(page.getByRole('dialog').getByText('/tmp/taildrop-accept-liveevent')).toBeVisible();
  await expect(page.getByRole('dialog').getByText('8 B', { exact: true })).toBeVisible();
});

test('long paths and 200% text never collide with controls', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });
  await openApp(page, { scenario: 'long' });
  await page.getByRole('button', { name: 'Review' }).click();
  const dialog = page.getByRole('dialog');

  const noOverflow = () =>
    dialog.locator('.staging-dialog').evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
  await expect(noOverflow()).resolves.toBe(true);

  // Emulate 200% text on the disclosure content and re-check wrapping and
  // hit-testability of the row's actions.
  await page.addStyleTag({
    content: [
      '.staging-path, .staging-file-name, .staging-group-name,',
      '.staging-confirm-text, .staging-error, .staging-dialog-subtitle { font-size: 24px !important; }',
    ].join(' '),
  });

  await expect(noOverflow()).resolves.toBe(true);

  const recoverBtn = dialog.getByRole('button', { name: /Recover 2 files from taildrop-accept-cafebabe/ });
  await recoverBtn.scrollIntoViewIfNeeded();
  await expect(recoverBtn).toBeVisible();

  // The action button sits fully inside the dialog and below the file list.
  const dialogBox = (await dialog.locator('.staging-dialog').boundingBox())!;
  const btnBox = (await recoverBtn.boundingBox())!;
  expect(btnBox.x).toBeGreaterThanOrEqual(dialogBox.x - 0.5);
  expect(btnBox.x + btnBox.width).toBeLessThanOrEqual(dialogBox.x + dialogBox.width + 0.5);
  expect(btnBox.y + btnBox.height).toBeLessThanOrEqual(dialogBox.y + dialogBox.height + 0.5);

  // Still clickable at the doubled text size.
  await recoverBtn.click();
  await expect(page.getByRole('dialog').getByText('All recovery groups have been handled.')).toBeVisible();
});
