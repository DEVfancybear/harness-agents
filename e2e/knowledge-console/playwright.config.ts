import { defineConfig } from '@playwright/test';
import path from 'node:path';

export default defineConfig({
  testDir: './tests',
  outputDir: path.resolve(__dirname, '../../target/knowledge-console-e2e/results'),
  timeout: 45000,
  expect: { timeout: 8000 },
  retries: 0,
  repeatEach: 3,
  workers: 2,
  reporter: [['list'], ['json', { outputFile: '../../target/knowledge-console-e2e/report.json' }]],
  use: { browserName: 'chromium', channel: 'msedge', headless: true, trace: 'retain-on-failure', screenshot: 'only-on-failure' },
});
