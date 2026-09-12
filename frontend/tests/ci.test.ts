import { test } from 'node:test';
import assert from 'node:assert/strict';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import SafeReporter, { MAX_RESULTS, MAX_REPORT_BYTES } from '../scripts/safe-reporter.mjs';

test('report boundary discards credentials and caps retained results', () => {
  const reporter = new SafeReporter();
  reporter.onBegin({}, { allTests: () => Array(MAX_RESULTS + 10) });
  for (let index = 0; index < MAX_RESULTS + 10; index++) {
    reporter.onTestEnd({ title: 'private-token', location: { file: '/private-token/secret.browser.ts', line: 3 } }, {
      status: 'failed', duration: 4, error: { message: 'private-token' },
      stdout: ['private-token'], stderr: ['private-token'], attachments: [{ body: 'private-token' }],
    });
  }
  const summary = reporter.summary('passed');
  assert.equal(summary.results.length, MAX_RESULTS);
  assert.equal(summary.omitted, 10);
  assert.equal(summary.status, 'failed');
  assert.ok(Buffer.byteLength(JSON.stringify(summary)) < MAX_REPORT_BYTES);
  assert.equal(JSON.stringify(summary).includes('private-token'), false);
});

test('real Playwright failure is nonzero with no credential in its report or output', () => {
  const directory = mkdtempSync(join(tmpdir(), 'uob-browser-gate-'));
  const secret = 'uob1.demo.sentinel-never-retain';
  try {
    const playwright = pathToFileURL(resolve('node_modules/@playwright/test/index.mjs')).href;
    writeFileSync(join(directory, 'probe.spec.mjs'), `
      import { test, expect } from ${JSON.stringify(playwright)};
      test('credential reflection', async () => {
        console.log(${JSON.stringify(secret)});
        console.error(${JSON.stringify(secret)});
        expect(${JSON.stringify(secret)}).toBe('different');
      });
    `);
    writeFileSync(join(directory, 'config.mjs'), `export default {
      testDir: '.', testMatch: '*.spec.mjs', workers: 1, retries: 0,
      reporter: [[${JSON.stringify(resolve('scripts/safe-reporter.mjs'))}]],
      preserveOutput: 'never', outputDir: 'raw',
    };`);
    const run = spawnSync(process.execPath, [resolve('node_modules/@playwright/test/cli.js'), 'test', '--config=config.mjs'], {
      cwd: directory, encoding: 'utf8', timeout: 30000, maxBuffer: MAX_REPORT_BYTES,
      env: { ...process.env, PLAYWRIGHT_NO_COPY_PROMPT: '1' },
    });
    assert.equal(run.status, 1);
    const content = readFileSync(join(directory, 'test-results/ci-summary.json'), 'utf8');
    assert.equal(JSON.parse(content).status, 'failed');
    assert.equal(JSON.parse(content).results.length, 1);
    assert.equal((content + run.stdout + run.stderr).includes(secret), false);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test('pinned type and lint tools reject independent invalid fixtures', () => {
  const directory = mkdtempSync(join(tmpdir(), 'uob-frontend-gates-'));
  try {
    const typed = join(directory, 'type.ts');
    writeFileSync(typed, 'export const invalid: number = "text";\n');
    const type = spawnSync(process.execPath, ['node_modules/typescript/bin/tsc', '--ignoreConfig', '--noEmit', '--skipLibCheck', typed], { timeout: 30000 });
    assert.notEqual(type.status, 0);
    assert.match(type.stdout.toString(), /TS2322/);
    const linted = join(directory, 'lint.ts');
    writeFileSync(linted, 'debugger;\n');
    const lint = spawnSync('node_modules/.bin/oxlint', ['--config=.oxlintrc.json', '--deny-warnings', linted], { timeout: 30000 });
    assert.equal(lint.status, 1);
    assert.match(lint.stdout.toString() + lint.stderr.toString(), /no-debugger/);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test('static build budget rejects oversized asset output', () => {
  const directory = mkdtempSync(join(tmpdir(), 'uob-asset-gate-'));
  try {
    mkdirSync(join(directory, 'frontend/scripts'), { recursive: true });
    mkdirSync(join(directory, 'adapters/management/ui/assets'), { recursive: true });
    copyFileSync('scripts/check-budget.mjs', join(directory, 'frontend/scripts/check-budget.mjs'));
    writeFileSync(join(directory, 'adapters/management/ui/index.html'), '<html></html>');
    writeFileSync(join(directory, 'adapters/management/ui/assets/console.css'), '');
    writeFileSync(join(directory, 'adapters/management/ui/assets/console.js'), 'x'.repeat(301 * 1024));
    const build = spawnSync(process.execPath, [join(directory, 'frontend/scripts/check-budget.mjs')], { timeout: 30000 });
    assert.equal(build.status, 1);
    assert.match(build.stderr.toString(), /Pi console asset budget exceeded/);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
