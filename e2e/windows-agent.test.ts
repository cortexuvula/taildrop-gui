/** Real App + IPC boundary fake; no C# or remote host execution is implied. */
import { expect, test, type Page } from '@playwright/test';

async function openWindows(page: Page, query: string) {
  await page.goto(`http://localhost:1420/e2e/app.e2e.html?${query}`);
  await page.getByRole('tab', { name: 'Files' }).click();
  const right = page.getByTestId('pane-right');
  await right.locator('.machine-picker-button').click();
  await right.getByRole('listbox', { name: 'Choose machine' }).getByText('winbox', { exact: true }).click();
  await expect(right.getByRole('option', { name: /stuff/ })).toBeVisible();
  return right;
}

async function commandCalls(page: Page, cmd: string) {
  return page.evaluate((command) => window.__e2e!.calls.filter((c) => c.cmd === command).map((c) => c.args), cmd);
}

test('proven Windows account enables mkdir/rename/delete and copy-INTO through production controls', async ({ page }, info) => {
  await page.setViewportSize({ width: 1180, height: 740 });
  const right = await openWindows(page, 'winAgent=1&savedUser=winbox:alice');
  await expect(right.getByRole('button', { name: 'New folder' })).toBeEnabled();
  await expect(right.getByRole('button', { name: 'Rename' })).toBeEnabled();
  await expect(right.getByRole('button', { name: 'Delete' })).toBeEnabled();
  expect(await commandCalls(page, 'browser_probe')).toContainEqual({ host: 'winbox', asUser: 'alice' });
  await expect(right.locator('.pane-notice')).toHaveCount(0);
  await page.screenshot({ path: info.outputPath('windows-proven.png') });

  await right.getByRole('button', { name: 'New folder' }).click();
  let dialog = page.getByRole('dialog');
  await dialog.getByPlaceholder('Folder name').fill('new-folder');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(right.getByRole('option', { name: /new-folder/ })).toBeVisible();
  expect(await commandCalls(page, 'browser_mkdir')).toEqual([{ target: 'winbox', path: '/new-folder', asUser: 'alice' }]);

  await right.getByRole('button', { name: 'Rename' }).click();
  dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox').fill('renamed-folder');
  await dialog.getByRole('button', { name: 'Rename' }).click();
  await expect(right.getByRole('option', { name: /renamed-folder/ })).toBeVisible();
  expect(await commandCalls(page, 'browser_rename')).toEqual([{ target: 'winbox', old: '/new-folder', new: '/renamed-folder', asUser: 'alice' }]);

  await right.getByRole('button', { name: 'Delete' }).click();
  dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('PERMANENTLY');
  await dialog.getByRole('button', { name: 'Delete' }).click();
  await expect(right.getByRole('option', { name: /renamed-folder/ })).toHaveCount(0);
  expect(await commandCalls(page, 'browser_delete')).toEqual([{ target: 'winbox', paths: ['/renamed-folder'], asUser: 'alice' }]);

  await page.getByTestId('pane-left').getByRole('option', { name: /notes.txt/ }).click();
  await page.getByRole('button', { name: 'Copy →' }).click();
  await expect(page.getByTestId('queue-row-1')).toHaveAttribute('data-state', 'done');
  expect(await commandCalls(page, 'browser_copy')).toEqual([{ job: {
    srcTarget: 'local', srcUser: null, srcDir: '/', dstTarget: 'winbox',
    dstUser: 'alice', dstDir: '/', names: ['notes.txt'], replace: false,
  } }]);
  await right.getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(right.getByRole('option', { name: /notes.txt/ })).toBeVisible();
});

for (const [name, query] of [
  ['missing proof', ''],
  ['malformed proof and unknown OS', 'winAgent=string&winOsUnknown=1'],
  ['failed agent probe', 'winAgent=fail'],
]) {
  test(`unproven Windows ${name}: buttons and keyboard refuse writes, pull stays available`, async ({ page }, info) => {
    await page.setViewportSize({ width: 1180, height: 740 });
    const right = await openWindows(page, query);
    await expect(right.locator('.pane-notice')).toContainText('browse and pull only');
    for (const action of ['New folder', 'Rename', 'Delete']) {
      await expect(right.getByRole('button', { name: action })).toBeDisabled();
    }
    await expect(page.getByRole('button', { name: 'Copy →' })).toBeDisabled();
    await page.screenshot({ path: info.outputPath('windows-unproven.png') });
    await right.locator('.pane-list').focus();
    for (const key of ['n', 'R', 'D']) {
      await page.keyboard.press(key);
      await expect(page.getByRole('dialog')).toHaveCount(0);
    }
    await page.getByTestId('pane-left').getByRole('option', { name: /notes.txt/ }).click();
    await page.getByTestId('pane-left').locator('.pane-list').focus();
    await page.keyboard.press('c');
    await expect(page.locator('.browser-status')).toContainText('browse and pull only');
    for (const cmd of ['browser_mkdir', 'browser_rename', 'browser_delete', 'browser_copy']) {
      expect(await commandCalls(page, cmd)).toHaveLength(0);
    }
    await page.getByRole('button', { name: '← Copy' }).click();
    await expect(page.getByTestId('queue-row-1')).toHaveAttribute('data-state', 'done');
    const calls = await commandCalls(page, 'browser_copy');
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ job: { srcTarget: 'winbox', srcUser: 'peeruser', dstTarget: 'local' } });
  });
}
