import { test, expect } from '../fixture';
import { KnowledgePage } from '../knowledge.page';

test('dropping files previews import without writing and mobile UI stays within viewport', async ({ page, console: app }) => {
  const ui = new KnowledgePage(page);
  await ui.open(app.url);
  const files = await page.evaluateHandle(() => {
    const data = new DataTransfer();
    data.items.add(new File(['# Dropped\n\nKnowledge fixture'], 'dropped.md', { type: 'text/markdown' }));
    return data;
  });
  await page.locator('body').dispatchEvent('drop', { dataTransfer: files });
  const dialog = page.getByRole('dialog', { name: 'Import documents', exact: true });
  await expect(dialog).toContainText('dropped.md');
  expect((await (await app.api.get('/api/pages?scope=project')).json()).pages).toEqual([]);
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  expect((await (await app.api.get('/api/pages?scope=project')).json()).pages).toEqual([]);
  await page.setViewportSize({ width: 390, height: 844 });
  await ui.create('mobile.md', '# Mobile\n\n' + 'long-word-'.repeat(100));
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.getByRole('tab', { name: 'Graph', exact: true }).click();
  await expect(page.locator('#graph-count')).toHaveText('(1)');
  await page.locator('#graph').focus();
  await page.keyboard.press('+');
  await page.keyboard.press('ArrowRight');
  await page.keyboard.press('Home');
  await page.locator('#graph').scrollIntoViewIfNeeded();
  const graph = await page.locator('#graph').boundingBox();
  if (!graph) throw new Error('Graph has no rendered bounds');
  await page.mouse.click(graph.x + graph.width / 2, graph.y + graph.height / 2);
  await expect(page.locator('#graph-selection-title')).toHaveText('Mobile');
  await page.getByRole('button', { name: 'Open document', exact: true }).click();
  await expect(page.locator('#document-title')).toHaveText('Mobile');
  await page.screenshot({ path: app.root + '/mobile.png', fullPage: true });
});
