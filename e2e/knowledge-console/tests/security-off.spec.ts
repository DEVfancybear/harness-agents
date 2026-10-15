import { test, expect } from '../fixture';
import { KnowledgePage } from '../knowledge.page';

test('native HTTP auth, CSRF and project off gate both libraries', async ({ page, playwright, console: app }) => {
  const outsider = await playwright.request.newContext({ baseURL: app.url });
  expect((await outsider.get('/api/pages?scope=project')).status()).toBe(403);
  expect((await app.api.post('/api/manage', { headers: { Origin: 'https://attacker.invalid' }, data: { action: 'create_category', path: 'bad', scope: 'project' } })).status()).toBe(403);
  expect((await app.api.post('/api/manage', { headers: { 'Content-Type': 'text/plain' }, data: '{}' })).status()).toBe(400);
  expect((await app.api.post('/api/manage', { data: { scope: 'project', action: 'create_document', path: '../escape.md', content: 'bad' } })).status()).toBe(400);
  await outsider.dispose();
  const ui = new KnowledgePage(page);
  await ui.open(app.url);
  await ui.create('local.md', '# Local\n\nSECRET-LOCAL');
  expect((await app.api.post('/api/manage', { data: { scope: 'global', action: 'create_document', path: 'shared.md', content: '# Global\n\nSHARED' } })).status()).toBe(200);
  await page.getByRole('switch', { name: 'Enable project knowledge', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Knowledge is off', exact: true })).toBeVisible();
  await expect(page.locator('#markdown')).not.toBeVisible();
  expect((await app.api.get('/api/page?scope=global&path=shared.md')).status()).toBe(403);
  await page.getByLabel('Library', { exact: true }).selectOption('global');
  await expect(page.getByRole('switch', { name: 'Enable global knowledge', exact: true })).toBeDisabled();
  await page.getByLabel('Library', { exact: true }).selectOption('project');
  await page.getByRole('switch', { name: 'Enable project knowledge', exact: true }).click();
  await expect(page.getByRole('link', { name: 'Local', exact: true })).toBeVisible();
});
