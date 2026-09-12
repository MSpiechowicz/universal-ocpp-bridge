import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  testMatch: '*.browser.ts',
  workers: 1,
  timeout: 30000,
  use: {
    baseURL: 'http://127.0.0.1:39189',
    viewport: { width: 1280, height: 900 },
    trace: 'off',
    launchOptions: process.env.UOB_BROWSER_EXECUTABLE ? { executablePath: process.env.UOB_BROWSER_EXECUTABLE } : {},
  },
  webServer: [...[39189, 39190, 39191, 39192].map(port => ({
    command: 'cargo run --locked -p uob-management-adapter --example browser_fixture',
    cwd: '..',
    env: { UOB_BROWSER_TEST_PORT: String(port) },
    url: `http://127.0.0.1:${port}/api/v1/identity`,
    timeout: 120000,
    reuseExistingServer: false,
  })), {
    command: 'cargo run --locked -p uob-service --bin uob -- serve --config frontend/tests/daemon.toml',
    cwd: '..',
    url: 'http://127.0.0.1:39193/api/v1/identity',
    timeout: 120000,
    reuseExistingServer: false,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 5000 },
  }],
});
