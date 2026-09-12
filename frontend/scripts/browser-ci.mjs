import { spawn } from 'node:child_process';
import { readFileSync, rmSync, statSync } from 'node:fs';
import { MAX_REPORT_BYTES } from './safe-reporter.mjs';

// Only this sanitized report is retained or printed. Discard even launcher errors:
// Playwright may otherwise include reflected credentials in assertion diagnostics.
rmSync('test-results/ci-summary.json', { force: true });
const child = spawn(process.execPath, ['node_modules/@playwright/test/cli.js', 'test', '--config=playwright.ci.config.ts'], {
  stdio: 'ignore', detached: true,
  env: { ...process.env, CI: 'true', UOB_BROWSER_REPORT_ONLY: '1', PLAYWRIGHT_NO_COPY_PROMPT: '1' },
});
const stop = () => { if (child.pid) { try { process.kill(-child.pid, 'SIGKILL'); } catch { /* already stopped */ } } };
const timer = setTimeout(stop, 12 * 60 * 1000);
let passed = false;
try {
  const code = await new Promise(resolve => {
    child.on('error', () => resolve(1));
    child.on('close', value => resolve(value));
  });
  if (statSync('test-results/ci-summary.json').size <= MAX_REPORT_BYTES) {
    const report = JSON.parse(readFileSync('test-results/ci-summary.json', 'utf8'));
    passed = code === 0 && report.status === 'passed';
  }
} catch { /* Missing or oversized report is a failed check. */ }
finally {
  clearTimeout(timer);
  stop();
  rmSync('test-results/raw', { recursive: true, force: true });
}
console.log(`Browser checks ${passed ? 'passed' : 'failed'}; only test-results/ci-summary.json may be retained.`);
process.exitCode = passed ? 0 : 1;
