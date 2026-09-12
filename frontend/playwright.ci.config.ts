import { defineConfig } from '@playwright/test';
import base from './playwright.config';

export default defineConfig({
  ...base,
  forbidOnly: true,
  retries: 0,
  maxFailures: 5,
  globalTimeout: 10 * 60 * 1000,
  reporter: [['./scripts/safe-reporter.mjs']],
  outputDir: 'test-results/raw',
  preserveOutput: 'never',
  use: { ...base.use, trace: 'off', screenshot: 'off', video: 'off' },
  webServer: (Array.isArray(base.webServer) ? base.webServer : []).map(server => ({
    ...server, stdout: 'ignore', stderr: 'ignore',
    // Build before entering the browser file budget; compiler outputs are larger
    // than browser reports and must never be generated under this runtime cap.
    command: server.url?.includes(':39193/')
      ? './target/debug/uob serve --config frontend/tests/daemon.toml'
      : './target/debug/examples/browser_fixture',
  })),
});
