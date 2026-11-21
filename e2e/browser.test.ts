/**
 * Browser (M1) end-to-end — the PRODUCTION App against the browser-side
 * Tauri IPC mock (e2e/tauri-mock.ts), per the acceptance gates:
 * two-pane layout at 760×460 AND 1180×740, responsive column drop,
 * keyboard navigation, dialog focus trap, and copy-queue row states
 * including the Replace-on-exists flow (real collision semantics in the
 * mock FS: studio-mac:/notes.txt collides with local notes.txt).
 */
import { test, expect, type Page } from '@playwright/test';

const APP_URL = 'http://localhost:1420/e2e/app.e2e.html';

async function openFilesView(page: Page, qs = '') {
  await page.goto(qs ? `${APP_URL}?${qs}` : APP_URL);
  await page.getByRole('tab', { name: 'Files' }).click();
  const view = page.getByTestId('browser-view');
  await expect(view).toBeVisible();
  // Left pane starts on the local root listing.
  await expect(page.getByTestId('pane-left').getByText('notes.txt')).toBeVisible();
  return view;
}

function pane(page: Page, id: 'left' | 'right') {
  return page.getByTestId(`pane-${id}`);
}

async function focusList(page: Page, id: 'left' | 'right') {
  const list = pane(page, id).locator('.pane-list');
  await list.focus();
  return list;
}

async function pickMachine(page: Page, id: 'left' | 'right', name: string) {
  await pane(page, id).locator('.machine-picker-button').click();
  const menu = pane(page, id).locator('.machine-menu');
  await expect(menu).toBeVisible();
  await menu.getByText(name, { exact: true }).click();
}

test.describe('browser layout', () => {
  test('panes side by side; column drop follows the QML thresholds per pane width', async ({ page }) => {
    // Default window 1180×740: with the 260px Sidebar each pane is ~390px —
    // size column kept (>300), date column dropped (<=440). FilePane.qml:67-71
    // thresholds are per-PANE width (container queries), not window width.
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    const l = await pane(page, 'left').boundingBox();
    const r = await pane(page, 'right').boundingBox();
    expect(l).toBeTruthy();
    expect(r).toBeTruthy();
    // Side by side: right pane starts after the left ends.
    expect(r!.x).toBeGreaterThan(l!.x + l!.width - 1);
    const time = pane(page, 'left').locator('.pane-row .col-time').first();
    const size = pane(page, 'left').locator('.pane-row .col-size').first();
    expect(l!.width).toBeGreaterThan(300);
    expect(l!.width).toBeLessThanOrEqual(440);
    await expect(size).toBeVisible();
    await expect(time).toBeHidden();
    // No page-level horizontal overflow.
    let overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);

    // Wider window → panes cross 440 → the date column returns (live resize).
    await page.setViewportSize({ width: 1700, height: 800 });
    const wide = await pane(page, 'left').boundingBox();
    expect(wide!.width).toBeGreaterThan(440);
    await expect(time).toBeVisible();
    await expect(size).toBeVisible();
  });

  test('minimum 760×460: panes fit, both meta columns drop, names keep room', async ({ page }) => {
    await page.setViewportSize({ width: 760, height: 460 });
    await openFilesView(page);
    const l = await pane(page, 'left').boundingBox();
    const r = await pane(page, 'right').boundingBox();
    expect(l).toBeTruthy();
    expect(r).toBeTruthy();
    expect(r!.x).toBeGreaterThan(l!.x + l!.width - 1);
    // Each pane is ~180px: date AND size drop (<=300), names keep their room.
    expect(l!.width).toBeLessThanOrEqual(300);
    await expect(pane(page, 'left').locator('.pane-row .col-time').first()).toBeHidden();
    await expect(pane(page, 'left').locator('.pane-row .col-size').first()).toBeHidden();
    await expect(pane(page, 'left').locator('.pane-row .col-name').first()).toBeVisible();
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
  });
});

test.describe('browser keyboard navigation', () => {
  test('j/k move the cursor, Enter descends, Backspace goes up, Space marks, Tab switches panes', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    await focusList(page, 'left');

    // Rows: Documents (dir), notes.txt, photo.jpg — cursor starts on row 0.
    const rows = pane(page, 'left').locator('.pane-row');
    await expect(rows.nth(0)).toHaveClass(/cursor/);
    await page.keyboard.press('j');
    await expect(rows.nth(1)).toHaveClass(/cursor/);
    await page.keyboard.press('ArrowDown');
    await expect(rows.nth(2)).toHaveClass(/cursor/);
    await page.keyboard.press('k');
    await expect(rows.nth(1)).toHaveClass(/cursor/);
    await page.keyboard.press('Home');
    await expect(rows.nth(0)).toHaveClass(/cursor/);

    // Space marks the row under the cursor; footer counts it.
    await page.keyboard.press(' ');
    await expect(pane(page, 'left').locator('.pane-footer')).toContainText('1 marked');
    // Escape cascade: first Esc clears marks.
    await page.keyboard.press('Escape');
    await expect(pane(page, 'left').locator('.pane-footer')).not.toContainText('marked');

    // Enter descends into Documents, Backspace comes back up.
    // (Space advanced the cursor to row 1 and Escape only cleared marks,
    // so return to the top row first.)
    await page.keyboard.press('Home');
    await page.keyboard.press('Enter');
    await expect(pane(page, 'left').locator('.path-display')).toHaveText('/Documents');
    await expect(pane(page, 'left').getByText('report.pdf')).toBeVisible();
    await page.keyboard.press('Backspace');
    await expect(pane(page, 'left').locator('.path-display')).toHaveText('/');
    // goUp selects the folder we came from.
    await expect(rows.nth(0)).toHaveClass(/cursor/);

    // Tab switches the active pane.
    await expect(pane(page, 'left')).toHaveAttribute('data-active', 'true');
    await page.keyboard.press('Tab');
    await expect(pane(page, 'right')).toHaveAttribute('data-active', 'true');
    await expect(pane(page, 'left')).toHaveAttribute('data-active', 'false');
  });
});

test.describe('browser dialogs', () => {
  test('mkdir dialog traps focus, validates names, and Escape closes', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    await focusList(page, 'left');
    await page.keyboard.press('n');

    const dialog = page.getByRole('dialog');
    await expect(dialog.getByText('New folder')).toBeVisible();

    // Invalid name keeps the dialog open with an inline problem.
    await dialog.getByPlaceholder('Folder name').fill('bad/name');
    await dialog.getByRole('button', { name: 'Create' }).click();
    await expect(dialog.getByRole('alert')).toContainText('slash');
    await expect(dialog.getByText('New folder')).toBeVisible();

    // Focus trap: Tab cycles strictly inside the dialog.
    const focusables = dialog.locator('input, button');
    const count = await focusables.count();
    const seen = new Set<string>();
    for (let i = 0; i < count * 2; i++) {
      await page.keyboard.press('Tab');
      const inDialog = await page.evaluate(() => {
        const d = document.querySelector('[role="dialog"]');
        return !!d && d.contains(document.activeElement);
      });
      expect(inDialog).toBe(true);
      const id = await page.evaluate(() =>
        document.activeElement ? (document.activeElement as HTMLElement).textContent ?? document.activeElement.tagName : '',
      );
      seen.add(id);
    }
    expect(seen.size).toBeGreaterThanOrEqual(2);

    // Background is inert while open (useModal invariant).
    await expect(page.locator('.browser-panes')).toHaveAttribute('inert', '');

    // Escape closes without creating anything.
    await page.keyboard.press('Escape');
    await expect(dialog.getByText('New folder')).toHaveCount(0);
    const mkdirs = await page.evaluate(() =>
      window.__e2e!.calls.filter((c) => c.cmd === 'browser_mkdir').length,
    );
    expect(mkdirs).toBe(0);

    // A valid name creates the folder and reloads the listing.
    await page.keyboard.press('n');
    await dialog.getByPlaceholder('Folder name').fill('docs-new');
    await dialog.getByRole('button', { name: 'Create' }).click();
    await expect(pane(page, 'left').getByText('docs-new')).toBeVisible();
  });
});

test.describe('browser copy queue', () => {
  test('collision → failed row with Replace → consented replace succeeds', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);

    // Right pane → studio-mac (its root already contains notes.txt).
    await pickMachine(page, 'right', 'studio-mac');
    await expect(pane(page, 'right').locator('.machine-label')).toHaveText('studio-mac');
    await expect(pane(page, 'right').getByText('music')).toBeVisible();

    // Select local notes.txt and copy left → right.
    await pane(page, 'left').locator('.pane-row[data-name="notes.txt"]').click();
    await focusList(page, 'left');
    await page.keyboard.press('c');

    // A queue row appears and reaches the failed/exists state.
    const row = page.getByTestId('queue-row-1');
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-state', 'failed', { timeout: 5000 });
    await expect(row).toContainText('already exist');

    // Replace offer → confirm dialog → consented re-enqueue.
    await row.getByRole('button', { name: 'Replace' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog.getByText('Replace existing items?')).toBeVisible();
    await dialog.getByRole('button', { name: 'Replace' }).click();

    // Second job runs to done; the first row stays failed until dismissed.
    const row2 = page.getByTestId('queue-row-2');
    await expect(row2).toHaveAttribute('data-state', 'done', { timeout: 5000 });

    // The wire proves consent semantics: first copy replace=false, second true.
    const copies = await page.evaluate(() =>
      window.__e2e!.calls.filter((c) => c.cmd === 'browser_copy').map((c) => c.args),
    );
    expect(copies).toHaveLength(2);
    expect((copies[0] as { job: { replace: boolean } }).job.replace).toBe(false);
    expect((copies[1] as { job: { replace: boolean } }).job.replace).toBe(true);

    // The overwrite landed in the mock FS (1024 = local notes.txt size).
    const size = await page.evaluate(() => {
      const fs = window.__e2e!.browserFsNow('studio-mac') as {
        children: Record<string, { size?: number }>;
      };
      return fs.children['notes.txt'].size;
    });
    expect(size).toBe(120);
  });

  test('cancel stops a running job and the row shows cancelled', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    // slowCopy=1 stretches the simulated transfer so Cancel always lands
    // while the job is still running (deterministic, no race).
    await openFilesView(page, 'slowCopy=1');
    await pickMachine(page, 'right', 'studio-mac');
    // Copy photo.jpg (no collision) then cancel mid-run.
    await pane(page, 'left').locator('.pane-row[data-name="photo.jpg"]').click();
    await focusList(page, 'left');
    await page.keyboard.press('c');
    const row = page.getByTestId('queue-row-1');
    await expect(row).toBeVisible();
    await row.getByRole('button', { name: 'Cancel' }).click();
    await expect(row).toHaveAttribute('data-state', 'cancelled', { timeout: 5000 });
    // Dismiss removes the terminal row.
    await row.getByRole('button', { name: 'Dismiss' }).click();
    await expect(row).toHaveCount(0);
  });
});

test.describe('browser copy scope', () => {
  test('remote→remote copy is refused in the UI with the M2 note (B5)', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    // Put BOTH panes on a remote machine.
    await pickMachine(page, 'left', 'studio-mac');
    await pickMachine(page, 'right', 'studio-mac');
    await expect(pane(page, 'left').locator('.machine-label')).toHaveText('studio-mac');
    await expect(pane(page, 'right').locator('.machine-label')).toHaveText('studio-mac');
    // Select a file and try to copy — must be refused before any IPC.
    await pane(page, 'left').locator('.pane-row[data-name="notes.txt"]').click();
    await focusList(page, 'left');
    await page.keyboard.press('c');
    await expect(page.locator('.browser-status')).toContainText('M2');
    const copies = await page.evaluate(
      () => window.__e2e!.calls.filter((c) => c.cmd === 'browser_copy').length,
    );
    expect(copies).toBe(0);
  });
});

test.describe('browser peer behaviors', () => {
  test('sidebar peer selection seeds the right pane', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    // The Taildrop sidebar lists the peer by display name (mock: Studio Mac).
    await page.getByText('Studio Mac').first().click();
    await expect(pane(page, 'right').locator('.machine-label')).toHaveText('studio-mac', {
      timeout: 5000,
    });
    await expect(pane(page, 'right')).toHaveAttribute('data-active', 'true');
  });

  test('windows peer: browse + pull only (write actions disabled with a note)', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    await pickMachine(page, 'right', 'winbox');
    await expect(pane(page, 'right').getByText('stuff')).toBeVisible();
    await expect(pane(page, 'right').locator('.pane-notice')).toContainText(
      'browse and pull only',
    );
    await expect(
      pane(page, 'right').getByRole('button', { name: 'New folder' }),
    ).toBeDisabled();
    await expect(pane(page, 'right').locator('.footer-ro')).toHaveText('read-only');
  });

  test('android/phone peers stay out of the machine picker', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page);
    await pane(page, 'left').locator('.machine-picker-button').click();
    const menu = pane(page, 'left').locator('.machine-menu');
    await expect(menu).toBeVisible();
    await expect(menu.getByText('pixel')).toHaveCount(0);
    await expect(menu.getByText('studio-mac')).toBeVisible();
  });

  test('auth failure shows the error pane with Change user + Retry', async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    await openFilesView(page, 'browserFailAuth=1');
    await pickMachine(page, 'right', 'studio-mac');
    const err = pane(page, 'right').locator('.pane-error');
    await expect(err).toHaveAttribute('data-kind', 'auth');
    await expect(err).toContainText('Sign-in refused');
    await expect(err.getByRole('button', { name: 'Change user' })).toBeVisible();
    await expect(err.getByRole('button', { name: 'Retry' })).toBeVisible();
  });
});
