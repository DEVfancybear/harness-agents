import { test, expect } from '../fixture';
import { KnowledgePage } from '../knowledge.page';

test('a stale browser edit cannot overwrite a concurrent external edit', async ({ page, console: app }) => {
  const ui = new KnowledgePage(page);
  await ui.open(app.url);
  await ui.create('shared.md', '# Shared\n\noriginal');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const edit = page.getByRole('dialog', { name: 'Edit document' });
  await edit.getByLabel('Markdown', { exact: true }).fill('# Shared\n\nstale browser');
  const current = await (await app.api.get('/api/page?scope=project&path=shared.md')).json();
  const external = await app.api.post('/api/manage', { data: { scope: 'project', action: 'update_document', path: 'shared.md', expected_hash: current.hash, content: '# Shared\n\nnewer external' } });
  expect(external.status()).toBe(200);
  await edit.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect(edit.getByRole('alert')).toContainText(/hash|changed|conflict/i);
  const persisted = await (await app.api.get('/api/page?scope=project&path=shared.md')).json();
  expect(persisted.content).toContain('newer external');
  expect(persisted.content).not.toContain('stale browser');
  await edit.getByRole('button', { name: 'Cancel', exact: true }).click();
});
