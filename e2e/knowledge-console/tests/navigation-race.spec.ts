import { test, expect } from '../fixture';
import { KnowledgePage } from '../knowledge.page';

test('Back to home cannot be overwritten by a delayed prior page response', async ({ page, console: app }) => {
  const ui = new KnowledgePage(page);
  await ui.open(app.url);
  await ui.create('alpha.md', '# Alpha\n\nFirst page');
  await ui.create('beta.md', '# Beta\n\nSecond page');
  // Creating a document replaces the selected-page route. Establish browser
  // history through the actual document navigation links before testing Back.
  await ui.open(app.url);
  await page.getByRole('link', { name: 'Alpha', exact: true }).click();
  await expect(page.locator('#document-title')).toHaveText('Alpha');
  await page.getByRole('link', { name: 'Beta', exact: true }).click();
  await expect(page.locator('#document-title')).toHaveText('Beta');
  let release!: () => void;
  const hold = new Promise<void>(resolve => { release = resolve; });
  let received!: () => void;
  const pending = new Promise<void>(resolve => { received = resolve; });
  await page.route('**/api/page?**', async route => {
    if (new URL(route.request().url()).searchParams.get('path') !== 'alpha.md') return route.continue();
    const response = await route.fetch();
    received();
    await hold;
    await route.fulfill({ response });
  });
  await page.goBack();
  await pending;
  await page.goBack();
  await expect(page.locator('#empty-state')).toBeVisible();
  const delivered = page.waitForResponse(response => new URL(response.url()).pathname === '/api/page' && new URL(response.url()).searchParams.get('path') === 'alpha.md');
  release();
  await (await delivered).finished();
  await expect(page.locator('#document')).toBeHidden();
  await expect(page).toHaveURL(/\/(?:\?scope=project)?$/);
  await page.getByRole('button', { name: 'Refresh knowledge', exact: true }).click();
  await expect(page.locator('#empty-state')).toBeVisible();
  await expect(page.locator('#document')).toBeHidden();
});
