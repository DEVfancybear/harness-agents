import { test, expect } from '../fixture';
import { KnowledgePage } from '../knowledge.page';

test('UTF-8 import supports skip, rename, overwrite and blocks executable markup', async ({ page, console: app }) => {
  const ui = new KnowledgePage(page);
  await ui.open(app.url);
  await ui.import('imported.md', '# Imported\n\nFirst 知识');
  await ui.import('imported.md', '# Ignored\n\nShould skip', 'skip');
  let doc = await (await app.api.get('/api/page?scope=project&path=imported.md')).json();
  expect(doc.content).toContain('First 知识');
  await ui.import('imported.md', '# Renamed\n\nSecond', 'rename');
  const pages = await (await app.api.get('/api/pages?scope=project')).json();
  expect(pages.pages).toHaveLength(2);
  expect(pages.pages.some((p: any) => p.title === 'Renamed' && p.path !== 'imported.md')).toBe(true);
  await ui.import('imported.md', '# Replaced\n\nFinal value <script>window.XSS = true</script>\n\n[bad](javascript:alert(1))', 'overwrite');
  await page.getByRole('link', { name: 'Replaced', exact: true }).click();
  await expect(page.locator('#markdown')).toContainText('Final value');
  await expect(page.locator('#markdown script')).toHaveCount(0);
  await expect(page.locator('#markdown a[href^="javascript:"]')).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).XSS)).toBeUndefined();
  doc = await (await app.api.get('/api/page?scope=project&path=imported.md')).json();
  expect(doc.content).toContain('Final value');
});
