import { test, expect } from '../fixture';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { writeFileSync, existsSync } from 'node:fs';
import path from 'node:path';

test('Release CLI enables semantic only explicitly, caches and falls back after provider failure', async ({ console: app }) => {
  test.setTimeout(90000);
  const binary = process.env.HA_E2E_BINARY || path.resolve(__dirname, '../../../target/release', process.platform === 'win32' ? 'ha.exe' : 'ha');
  const execute = promisify(execFile);
  const command = async (...args: string[]) => {
    const { stdout } = await execute(binary, ['knowledge', '--cwd', app.project, '--json', ...args], {
      env: { ...process.env, HA_HOME: path.join(app.root, 'home'), HA_OFFLINE: '1' }, windowsHide: true,
    });
    return JSON.parse(JSON.parse(stdout).lines[0]);
  };
  const calls: string[][] = [];
  let fail = false;
  const provider = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const { input } = JSON.parse(Buffer.concat(chunks).toString());
    calls.push(input);
    response.writeHead(fail ? 401 : 200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify(fail ? { error: 'provider-private-detail' } : {
      data: input.map((text: string, index: number) => ({ index, embedding: /orchard|apples/.test(text) ? [1, 0] : [0, 1] })),
    }));
  });
  const listen = (host: string) => new Promise<void>((resolve, reject) => {
    provider.once('error', reject);
    provider.listen(0, host, () => { provider.off('error', reject); resolve(); });
  });
  await listen('::1').catch(() => listen('127.0.0.1'));
  try {
    await app.api.post('/api/manage', { data: { scope: 'project', action: 'create_document', path: 'fruit.md', content: '# Fruit\n\napples at harvest' } });
    expect((await command('semantic', 'status')).enabled).toBe(false);
    expect((await command('search', 'apples')).retrieval).toBe('keyword_rrf');
    expect(calls).toHaveLength(0);
    expect(existsSync(path.join(app.project, '.harness/knowledge/.ha-embedding.sqlite'))).toBe(false);
    const address = provider.address() as { port: number; address: string };
    const host = address.address.includes(':') ? `[${address.address}]` : address.address;
    const config = path.join(app.root, 'embedding.json');
    const configure = (model: string) => writeFileSync(config, JSON.stringify({ enabled: false, endpoint: `http://${host}:${address.port}/v1/embeddings`, model }));
    configure('fixture-v1');
    expect((await command('semantic', 'configure', config)).enabled).toBe(false);
    expect(calls).toHaveLength(0);
    await command('semantic', 'on');
    expect((await command('semantic', 'index')).embedded).toBe(1);
    const search = await command('search', 'orchard');
    expect(search.retrieval).toBe('hybrid');
    expect(search.results[0].path).toBe('fruit.md');
    expect(calls).toHaveLength(2);
    await command('search', 'orchard');
    expect(calls).toHaveLength(2);
    configure('fixture-v2');
    await command('semantic', 'configure', config);
    await command('semantic', 'on');
    expect((await command('semantic', 'index')).embedded).toBe(1);
    fail = true;
    const fallback = await command('search', 'apples');
    expect(fallback.retrieval).toBe('keyword_rrf');
    expect(fallback.results[0].path).toBe('fruit.md');
    expect(fallback.warnings.join(' ')).toContain('HTTP 401');
    expect(JSON.stringify(fallback)).not.toContain('provider-private-detail');
    await command('semantic', 'off');
    const before = calls.length;
    await command('search', 'orchard');
    expect(calls).toHaveLength(before);
  } finally { await new Promise<void>(resolve => provider.close(() => resolve())); }
});
