import { test, expect } from '@playwright/test';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(__dirname, 'fixture-settings.html');

test('Settings dialog: Shift+Tab from overlay wraps to last focusable', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  // The fixture auto-focuses the first focusable element (close button)
  const closeBtn = page.locator('.settings-panel .icon-btn').first();
  await expect(closeBtn).toBeFocused();

  // Shift+Tab should wrap to the last focusable element
  await page.keyboard.press('Shift+Tab');

  // Should now focus the "Check for Updates" button (last focusable)
  const lastBtn = page.locator('.btn-secondary');
  await expect(lastBtn).toBeFocused();
});

test('Settings dialog: Tab from last element wraps to first', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  // Focus the last focusable element
  const lastBtn = page.locator('.btn-secondary');
  await lastBtn.focus();
  await expect(lastBtn).toBeFocused();

  // Tab should wrap to the first focusable element (close button)
  await page.keyboard.press('Tab');

  const closeBtn = page.locator('.settings-panel .icon-btn').first();
  await expect(closeBtn).toBeFocused();
});

test('Settings dialog: Escape closes the dialog', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  const overlay = page.locator('.settings-overlay');
  await expect(overlay).toBeVisible();

  await page.keyboard.press('Escape');

  await expect(overlay).not.toBeVisible();
});

test('Settings dialog: aria-labelledby references existing heading', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  const overlay = page.locator('.settings-overlay');
  const labelledBy = await overlay.getAttribute('aria-labelledby');

  expect(labelledBy).toBe('modal-heading');

  // Verify the referenced element exists
  const heading = page.locator('#modal-heading');
  await expect(heading).toBeVisible();
  await expect(heading).toHaveText('Settings');
});
