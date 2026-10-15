import { Page, expect } from '@playwright/test';

export class KnowledgePage {
  constructor(readonly page: Page) {}
  async open(url: string) {
    await this.page.goto(url);
    await expect(this.page.getByLabel('Knowledge documents')).toHaveAttribute('aria-busy', 'false');
  }
  async create(name: string, content: string, folder = '') {
    await this.page.getByRole('button', { name: 'New document', exact: true }).click();
    const dialog = this.page.getByRole('dialog', { name: 'New document', exact: true });
    await dialog.getByLabel('Folder', { exact: true }).selectOption(folder);
    await dialog.getByLabel('File name', { exact: true }).fill(name);
    await dialog.getByLabel('Document', { exact: true }).fill(content);
    await dialog.getByRole('button', { name: 'Create document', exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await expect(this.page.locator('#document-path')).toHaveText([folder, name].filter(Boolean).join('/'));
  }
  async folder(name: string) {
    await this.page.getByRole('button', { name: 'Folders', exact: true }).click();
    const dialog = this.page.getByRole('dialog', { name: 'Manage folders', exact: true });
    await dialog.getByLabel('New folder path', { exact: true }).fill(name);
    await dialog.getByRole('button', { name: 'Create folder', exact: true }).click();
    await expect(dialog).toHaveCount(0);
  }
  async import(name: string, content: string, mode = 'skip') {
    await this.page.getByRole('button', { name: 'Import', exact: true }).click();
    const dialog = this.page.getByRole('dialog', { name: 'Import documents', exact: true });
    await dialog.getByLabel('Markdown or text files', { exact: true }).setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.from(content) });
    await dialog.getByLabel('When a file already exists', { exact: true }).selectOption(mode);
    await dialog.getByRole('button', { name: 'Import', exact: true }).click();
    await expect(dialog).toHaveCount(0);
  }
}
