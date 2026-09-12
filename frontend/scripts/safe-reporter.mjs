import { mkdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { basename } from 'node:path';

export const MAX_RESULTS = 256;
export const MAX_REPORT_BYTES = 64 * 1024;
const statuses = new Set(['passed', 'failed', 'timedOut', 'skipped', 'interrupted']);
const integer = value => Number.isSafeInteger(value) && value >= 0 ? Math.min(value, 1_000_000_000) : 0;

// No titles, paths, errors, attachments, stdout, stderr, payloads or credentials
// cross this boundary. A source filename hash plus line number locates a test.
export default class SafeReporter {
  rows = [];
  omitted = 0;
  errors = 0;
  total = 0;

  printsToStdio() { return true; }
  onBegin(_config, suite) { this.total = integer(suite.allTests().length); }
  onStdOut() {}
  onStdErr() {}
  onError() { this.errors = Math.min(this.errors + 1, 1_000_000_000); }
  onTestEnd(test, result) {
    if (this.rows.length >= MAX_RESULTS) { this.omitted = Math.min(this.omitted + 1, 1_000_000_000); return; }
    this.rows.push({
      file_sha256: createHash('sha256').update(basename(test.location.file)).digest('hex'),
      line: integer(test.location.line),
      status: statuses.has(result.status) ? result.status : 'failed',
      duration_ms: integer(Math.round(result.duration)),
    });
  }
  summary(status) {
    return {
      schema_version: 1,
      status: status === 'passed' && this.total > 0 && !this.errors && !this.omitted ? 'passed' : 'failed',
      total: this.total, errors: this.errors, omitted: this.omitted, results: this.rows,
    };
  }
  onEnd(result) {
    const report = this.summary(result.status);
    const content = JSON.stringify(report) + '\n';
    if (Buffer.byteLength(content) > MAX_REPORT_BYTES) throw new Error('Browser report budget exceeded');
    mkdirSync('test-results', { recursive: true });
    writeFileSync('test-results/ci-summary.json', content, { mode: 0o600 });
    return { status: report.status };
  }
}
