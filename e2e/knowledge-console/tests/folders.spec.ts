import { test, expect } from '../fixture';
import { KnowledgePage } from '../knowledge.page';

test('folder rename rewrites links and folder delete protects surviving backlinks', async ({ page, console: app }) => {
  const ui = new KnowledgePage(page);
  await ui.open(app.url);
  await ui.folder('old');
  await ui.create('item.md', '# Folder item\n\nContent', 'old');
  await ui.create('outside.md', '# Outside\n\n[Item](old/item.md)');
  await page.getByRole('button', { name: 'Folders', exact: true }).click();
  await page.getByRole('dialog', { name: 'Manage folders' }).getByRole('button', { name: 'Rename folder', exact: true }).click();
  const rename = page.getByRole('dialog', { name: 'Rename folder', exact: true });
  await rename.getByLabel('New folder path', { exact: true }).fill('new');
  await rename.getByRole('button', { name: 'Rename', exact: true }).click();
  await expect(rename).toHaveCount(0);
  const outside = await (await app.api.get('/api/page?scope=project&path=outside.md')).json();
  expect(outside.content).toContain('new/item.md');
  await page.getByRole('button', { name: 'Folders', exact: true }).click();
  await page.getByRole('dialog', { name: 'Manage folders' }).getByRole('button', { name: 'Delete folder', exact: true }).click();
  const remove = page.getByRole('dialog', { name: 'Delete folder?', exact: true });
  await remove.getByLabel('Type new to confirm', { exact: true }).fill('new');
  await remove.getByRole('button', { name: 'Delete folder', exact: true }).click();
  await expect(remove.getByRole('alert')).toContainText(/backlink/i);
  expect((await app.api.get('/api/page?scope=project&path=new%2Fitem.md')).status()).toBe(200);
  await remove.getByRole('button', { name: 'Cancel', exact: true }).click();
});
