import { test } from 'node:test';
import assert from 'node:assert/strict';
import { importCapture, FILE_LIMIT, LINE_LIMIT } from '../src/debug/offline';
import { BYTE_LIMIT, DETAIL_LIMIT } from '../src/debug/buffer';
import { captureLines, encodeCapture, trace } from './offline-fixture';
const load = (text: string) => importCapture(new Blob([text]));

test('valid capture retains file identity, correlation, gaps, truncation and inert detail', async () => {
  const result = await load(encodeCapture());
  assert.equal(result.identity.runtime.environment, 'staging');
  assert.equal(result.summary.missing_sequences, 1);
  assert.equal(result.window.dropped_records, 3);
  assert.equal(result.summary.truncated_records, 2);
  assert.equal(result.buffer.rows[0].index.correlation, 'offline-correlation');
  assert.match(result.buffer.detail(2)!, /<img src=x onerror=alert\(1\)>/);
  const empty = await load(encodeCapture(captureLines([])));
  assert.equal(empty.records, 0);
  const ordered = captureLines();
  ordered.manifest.trace_schema_version = { revision: 0, major: 1 };
  await load(encodeCapture(ordered));
});

test('UTF-8 split across slice boundaries and display evictions remain bounded', async () => {
  const text = encodeCapture(captureLines(Array.from({ length: 180 }, (_, i) => trace(i, '界'.repeat(16000)))));
  assert.ok(new TextEncoder().encode(text).length < FILE_LIMIT);
  const result = await load(text);
  assert.equal(result.records, 180); assert.ok(result.buffer.evicted > 0);
  assert.ok(result.buffer.bytes <= BYTE_LIMIT);
  for (const row of result.buffer.rows) { result.buffer.inspection(row.sequence); assert.ok(result.buffer.detailBytes <= DETAIL_LIMIT); }
});

test('file, line, metadata, record and nesting ceilings reject without retaining partial data', async () => {
  let read = false;
  const huge = { size: FILE_LIMIT + 1, slice() { read = true; throw new Error(); } } as unknown as Blob;
  await assert.rejects(importCapture(huge)); assert.equal(read, false);
  await assert.rejects(load(' '.repeat(LINE_LIMIT + 1)));
  const metadata = captureLines(); metadata.manifest.build_version = 'x'.repeat(17000);
  await assert.rejects(load(encodeCapture(metadata)));
  await assert.rejects(load(encodeCapture(captureLines([trace(1, 'x'.repeat(65536))]))));
  await assert.rejects(load(encodeCapture(captureLines(Array.from({ length: 2001 }, (_, i) => trace(i))))));
  await assert.rejects(load('{"deep":' + '['.repeat(33) + '0' + ']'.repeat(33) + '}\n'));
  const declared = captureLines(); declared.manifest.limits.records = 1;
  await assert.rejects(load(encodeCapture(declared)));
});

test('malformed, incomplete, unsupported and inconsistent captures fail closed', async () => {
  const original = encodeCapture();
  for (const text of [original.slice(0, -1), original.split('\n').slice(1).join('\n'), original.slice(0, original.lastIndexOf('{"type":"summary"')), original + '{}\n', '<script>alert(1)</script>\n']) await assert.rejects(load(text));
  const changes: ((file: ReturnType<typeof captureLines>) => void)[] = [
    file => { file.manifest.schema_version = '2.0'; },
    file => { file.manifest.history_complete = true; },
    file => { file.records[0].schema_version.revision = 1; },
    file => { file.records[0].process_instance_id = 'other'; },
    file => { file.records[0].observed_at = '2026-02-30T12:00:00Z'; },
    file => { file.records[1].trace_sequence = 2; },
    file => { file.records[1].trace_sequence = 5; },
    file => { file.summary.exported_records++; },
    file => { file.summary.missing_sequences++; },
    file => { file.summary.truncated_records++; },
    file => { file.summary.last_sequence = 9; },
    file => { file.summary.reason = 'timeout'; },
    file => { file.manifest.limits.bytes = 1; },
    file => { (file.records[0] as unknown as Record<string, unknown>).trace_id = null; },
    file => { (file.records[0].redacted_details.fields as Record<string, unknown>).bad = { html: 'arbitrary nested data' }; },
  ];
  for (const change of changes) { const file = captureLines(); change(file); await assert.rejects(load(encodeCapture(file))); }
  await assert.rejects(load(original.replace('"bytes_before_summary":', '"bytes_before_summary":9,"ignored":')));
  await assert.rejects(importCapture(new Blob([new Uint8Array([255, 10])])));
});

test('cancellation stops slice reads and successful partial exports remain explicitly incomplete', async () => {
  const controller = new AbortController(); controller.abort();
  await assert.rejects(importCapture(new Blob([encodeCapture()]), controller.signal));
  const midRead = new AbortController();
  const bytes = new Blob([encodeCapture(captureLines(Array.from({ length: 200 }, (_, i) => trace(i))))]);
  let slices = 0;
  const interrupted = {
    size: bytes.size,
    slice(start: number, end: number) {
      slices++;
      const part = bytes.slice(start, end);
      return { async arrayBuffer() { const chunk = await part.arrayBuffer(); if (slices === 2) midRead.abort(); return chunk; } };
    },
  } as Blob;
  await assert.rejects(importCapture(interrupted, midRead.signal));
  assert.equal(slices, 2);
  const fixture = captureLines(); fixture.manifest.window.retained_records = 3;
  fixture.summary.unexported_initial_records = 1; fixture.summary.retained_window_complete = false; fixture.summary.reason = 'timeout';
  const result = await load(encodeCapture(fixture));
  assert.equal(result.summary.retained_window_complete, false); assert.equal(result.summary.reason, 'timeout');
});
