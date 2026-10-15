import { test, expect } from '../fixture';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import path from 'node:path';

test('native citations open the exact console document and never reuse a stopped console', async ({ page, console: app }) => {
  const binary = process.env.HA_E2E_BINARY || path.resolve(__dirname, '../../../target/release', process.platform === 'win32' ? 'ha.exe' : 'ha');
  const execute = promisify(execFile);
  const command = async (...args: string[]) => {
    const { stdout } = await execute(binary, ['knowledge', '--cwd', app.project, '--json', ...args], {
      env: { ...process.env, HA_HOME: path.join(app.root, 'home'), HA_OFFLINE: '1' }, windowsHide: true,
    });
    return JSON.parse(JSON.parse(stdout).lines[0]);
  };
  const documentPath = 'sources/知识 a&b#%.md';
  for (const scope of ['project', 'global']) {
    const create = await app.api.post('/api/manage', { data: {
      scope, action: 'create_document', path: documentPath, content: `# ${scope} source\n\nCITATION-${scope}`,
    } });
    expect(create.ok()).toBe(true);
    const output = await command('read', documentPath, ...(scope === 'global' ? ['--global'] : []));
    const row = output.results[0];
    const url = new URL(row.url);
    expect(url.origin).toBe(new URL(app.url).origin);
    expect(url.searchParams.get('page')).toBe(documentPath);
    expect(url.searchParams.get('scope')).toBe(scope);
    expect(url.username + url.password).toBe('');
    await page.goto(row.url);
    await expect(page.locator('#document-title')).toHaveText(`${scope} source`);
    await expect(page.locator('#document-path')).toHaveText(documentPath);
    await expect(page.locator('#markdown')).toContainText(`CITATION-${scope}`);
    await page.screenshot({ path: path.join(app.root, `citation-${scope}.png`), fullPage: true });
  }
  expect((await app.api.get('/api/identity')).status()).toBe(403);
  await app.stop();
  const fallback = await command('read', documentPath);
  expect(fallback.results[0].url).toBeUndefined();
  expect(fallback.results[0].file).toContain('知识 a&b#%.md');
});
