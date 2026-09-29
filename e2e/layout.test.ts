import { test, expect } from '@playwright/test';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(__dirname, 'fixture-12-files.html');

test('Last Accept button is reachable at 700×500 with 12 incoming files', async ({ page }) => {
  // Set viewport to the minimum supported size
  await page.setViewportSize({ width: 700, height: 500 });

  await page.goto(`file://${fixturePath}`);

  const lastBtn = page.getByTestId('last-accept-btn');
  await expect(lastBtn).toBeVisible();

  // Scroll the incoming section to reveal the last button
  const incomingSection = page.locator('.incoming-section');

  // Verify the section is scrollable
  const isScrollable = await incomingSection.evaluate((el) => {
    return el.scrollHeight > el.clientHeight;
  });
  expect(isScrollable).toBe(true);

  // Scroll to the bottom
  await incomingSection.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });

  // Wait for scroll to settle
  await page.waitForTimeout(100);

  // Check that the button is now visible in the viewport
  const btnBox = await lastBtn.boundingBox();
  expect(btnBox).toBeTruthy();

  // Verify the button's bottom edge is within the viewport
  const viewportHeight = 500;
  expect(btnBox!.y + btnBox!.height).toBeLessThan(viewportHeight);

  console.log(`Last Accept button position: y=${btnBox!.y.toFixed(0)}, height=${btnBox!.height.toFixed(0)}, bottom=${(btnBox!.y + btnBox!.height).toFixed(0)}px (viewport: ${viewportHeight}px)`);

  // Verify the button is hit-testable (can be clicked)
  await expect(lastBtn).toBeEnabled();
  await lastBtn.click();
});

test('Transfer history remains visible at 700×500', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });

  await page.goto(`file://${fixturePath}`);

  // All three transfer rows should be visible without scrolling the transfer-list
  const transferItems = page.locator('.transfer-list .transfer-item');
  await expect(transferItems).toHaveCount(3);

  for (let i = 0; i < 3; i++) {
    const item = transferItems.nth(i);
    await expect(item).toBeVisible();

    const box = await item.boundingBox();
    expect(box).toBeTruthy();

    // Bottom edge should be within viewport
    expect(box!.y + box!.height).toBeLessThan(500);
  }
});

test('Incoming section is bounded at 40% of transfer panel', async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 500 });

  await page.goto(`file://${fixturePath}`);

  const transferPanel = page.locator('.transfer-panel');
  const incomingSection = page.locator('.incoming-section');

  const panelHeight = await transferPanel.evaluate((el) => el.clientHeight);
  const incomingHeight = await incomingSection.evaluate((el) => el.clientHeight);

  const ratio = incomingHeight / panelHeight;
  console.log(`Transfer panel: ${panelHeight}px, Incoming section: ${incomingHeight}px (${(ratio * 100).toFixed(1)}%)`);

  // Should be capped at ~40% (allow some flex tolerance)
  expect(ratio).toBeLessThanOrEqual(0.42);
  expect(ratio).toBeGreaterThan(0.3); // Should use most of its budget
});
