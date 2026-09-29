import { test, expect } from '@playwright/test';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(__dirname, 'fixture-12-files.html');

/**
 * WCAG AA contrast checker.
 * Computes relative luminance and contrast ratio per WCAG 2.1.
 */

function hexToRgb(hex: string): [number, number, number] {
  const result = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
  if (!result) throw new Error(`Invalid hex: ${hex}`);
  return [
    parseInt(result[1], 16),
    parseInt(result[2], 16),
    parseInt(result[3], 16),
  ];
}

function relativeLuminance(r: number, g: number, b: number): number {
  const [rs, gs, bs] = [r, g, b].map((c) => {
    const sRGB = c / 255;
    return sRGB <= 0.03928 ? sRGB / 12.92 : Math.pow((sRGB + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * rs + 0.7152 * gs + 0.0722 * bs;
}

function contrastRatio(l1: number, l2: number): number {
  const lighter = Math.max(l1, l2);
  const darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

test('Accept button meets WCAG AA 4.5:1 contrast', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  const btn = page.locator('.btn-accept').first();
  const styles = await btn.evaluate((el) => {
    const computed = window.getComputedStyle(el);
    return {
      backgroundColor: computed.backgroundColor,
      color: computed.color,
      fontSize: computed.fontSize,
      fontWeight: computed.fontWeight,
    };
  });

  // Parse rgb values
  const bgMatch = styles.backgroundColor.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  const fgMatch = styles.color.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);

  expect(bgMatch).toBeTruthy();
  expect(fgMatch).toBeTruthy();

  const [bgR, bgG, bgB] = bgMatch!.slice(1).map(Number);
  const [fgR, fgG, fgB] = fgMatch!.slice(1).map(Number);

  const bgLum = relativeLuminance(bgR, bgG, bgB);
  const fgLum = relativeLuminance(fgR, fgG, fgB);
  const ratio = contrastRatio(bgLum, fgLum);

  // 12px text is not "large text" (AA large requires ≥18.66px bold or ≥24px),
  // so we need 4.5:1 for normal text.
  console.log(`Accept button contrast: ${ratio.toFixed(2)}:1 (bg: rgb(${bgR},${bgG},${bgB}), fg: rgb(${fgR},${fgG},${fgB}))`);
  expect(ratio).toBeGreaterThanOrEqual(4.5);
});

test('Accept button hover state meets WCAG AA 4.5:1 contrast', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  const btn = page.locator('.btn-accept').first();
  await btn.hover();

  // CSS transition on background is 0.15s — wait for it to complete before sampling
  await page.waitForTimeout(400);

  // Also assert the expected hover color to catch transition-not-applied regressions
  const styles = await btn.evaluate((el) => {
    const computed = window.getComputedStyle(el);
    return {
      backgroundColor: computed.backgroundColor,
      color: computed.color,
    };
  });

  const bgMatch = styles.backgroundColor.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  const fgMatch = styles.color.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);

  expect(bgMatch).toBeTruthy();
  expect(fgMatch).toBeTruthy();

  const [bgR, bgG, bgB] = bgMatch!.slice(1).map(Number);
  const [fgR, fgG, fgB] = fgMatch!.slice(1).map(Number);

  // Assert we've reached the hover state (#2558b0 = rgb(37,88,176)), not still in base
  expect(bgR).toBe(37);
  expect(bgG).toBe(88);
  expect(bgB).toBe(176);

  const bgLum = relativeLuminance(bgR, bgG, bgB);
  const fgLum = relativeLuminance(fgR, fgG, fgB);
  const ratio = contrastRatio(bgLum, fgLum);

  console.log(`Accept button hover contrast: ${ratio.toFixed(2)}:1 (bg: rgb(${bgR},${bgG},${bgB}), fg: rgb(${fgR},${fgG},${fgB}))`);
  expect(ratio).toBeGreaterThanOrEqual(4.5);
});

test('Muted text meets WCAG AA 4.5:1 contrast against transfer-panel background', async ({ page }) => {
  await page.goto(`file://${fixturePath}`);

  // Check .transfer-meta which uses --text-muted, rendered on .transfer-panel background
  const meta = page.locator('.transfer-meta').first();
  const styles = await meta.evaluate((el) => {
    const computed = window.getComputedStyle(el);
    // Walk up to .transfer-panel for the actual rendered background, not body
    const panel = el.closest('.transfer-panel');
    const panelBg = panel
      ? window.getComputedStyle(panel).backgroundColor
      : window.getComputedStyle(document.body).backgroundColor;
    return {
      color: computed.color,
      backgroundColor: panelBg,
    };
  });

  const fgMatch = styles.color.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  const bgMatch = styles.backgroundColor.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);

  expect(fgMatch).toBeTruthy();
  expect(bgMatch).toBeTruthy();

  const [fgR, fgG, fgB] = fgMatch!.slice(1).map(Number);
  const [bgR, bgG, bgB] = bgMatch!.slice(1).map(Number);

  const fgLum = relativeLuminance(fgR, fgG, fgB);
  const bgLum = relativeLuminance(bgR, bgG, bgB);
  const ratio = contrastRatio(fgLum, bgLum);

  console.log(`Muted text contrast: ${ratio.toFixed(2)}:1 (fg: rgb(${fgR},${fgG},${fgB}), bg: rgb(${bgR},${bgG},${bgB}))`);
  expect(ratio).toBeGreaterThanOrEqual(4.5);
});
