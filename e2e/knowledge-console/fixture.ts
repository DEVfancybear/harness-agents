import { test as base, expect, APIRequestContext } from '@playwright/test';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import path from 'node:path';

type ConsoleFixture = { url: string; project: string; root: string; api: APIRequestContext };
export const test = base.extend<{ console: ConsoleFixture }>({
  console: async ({ playwright }, use, testInfo) => {
    const repo = path.resolve(__dirname, '../..');
    const artifacts = path.join(repo, 'target', 'knowledge-console-e2e');
    mkdirSync(artifacts, { recursive: true });
    const root = mkdtempSync(path.join(artifacts, 'run-'));
    const project = path.join(root, 'project');
    mkdirSync(project);
    const git = spawnSync('git', ['init', '-q'], { cwd: project, windowsHide: true });
    if (git.status !== 0) throw new Error('Cannot initialize isolated test project');
    const binary = process.env.HA_E2E_BINARY || path.join(repo, 'target', 'release', process.platform === 'win32' ? 'ha.exe' : 'ha');
    const child = spawn(binary, ['knowledge', 'serve', '--cwd', project, '--port', '0', '--json'], {
      cwd: project, windowsHide: true,
      env: { ...process.env, HA_HOME: path.join(root, 'home'), HA_OFFLINE: '1', HA_AUTO_REFINE: 'off' },
    });
    let stdout = '', stderr = '';
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    let api: APIRequestContext | undefined;
    try {
      const url = await new Promise<string>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Console startup timed out: ' + stderr)), 15000);
        child.on('error', (error) => { clearTimeout(timeout); reject(error); });
        child.on('exit', (code) => { clearTimeout(timeout); reject(new Error(`Console exited (${code}): ${stderr}`)); });
        child.stdout.on('data', (chunk) => {
          stdout += chunk;
          const line = stdout.split('\n')[0];
          if (!stdout.includes('\n')) return;
          try { const info = JSON.parse(line); clearTimeout(timeout); resolve(info.url); }
          catch { clearTimeout(timeout); reject(new Error('Invalid console startup response')); }
        });
      });
      api = await playwright.request.newContext({ baseURL: url });
      await api.get('/'); // Authenticate this isolated API context through the real server.
      await use({ url, project, root, api });
    } finally {
      await api?.dispose();
      child.kill();
      writeFileSync(path.join(root, 'server.log'), stdout + stderr);
      await testInfo.attach('native-console-log', { path: path.join(root, 'server.log'), contentType: 'text/plain' });
    }
  },
});
export { expect };

const diagnostics = new WeakMap<object, { failures: unknown[]; errors: string[] }>();
test.beforeEach(async ({ page }) => {
  const evidence = { failures: [] as unknown[], errors: [] as string[] };
  diagnostics.set(page, evidence);
  page.on('requestfailed', request => evidence.failures.push({ url: request.url(), method: request.method(), error: request.failure()?.errorText }));
  page.on('pageerror', error => evidence.errors.push(error.message));
});
test.afterEach(async ({ page }, testInfo) => {
  await testInfo.attach('browser-network-and-errors', {
    body: Buffer.from(JSON.stringify(diagnostics.get(page), null, 2)), contentType: 'application/json',
  });
});
