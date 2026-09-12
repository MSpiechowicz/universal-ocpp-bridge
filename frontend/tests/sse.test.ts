import { test } from 'node:test';
import assert from 'node:assert/strict';
import { SseParser } from '../src/sse';
import type { SseRecord } from '../src/sse';

const bytes = (value: string) => new TextEncoder().encode(value);

test('fragmented UTF-8, CRLF, multiline records and id-only checkpoints survive', () => {
  const records: SseRecord[] = [];
  const parser = new SseParser(record => records.push(record));
  for (const byte of bytes(': keep-alive\r\n\r\nid: cursor-1\r\nevent: durable\r\ndata: żółw\r\ndata: second\r\n\r\nid: cursor-2\n\nid:\r\r')) parser.push(new Uint8Array([byte]));
  assert.deepEqual(records, [
    { event: 'durable', id: 'cursor-1', data: 'żółw\nsecond' },
    { event: 'message', id: 'cursor-2', data: '' },
    { event: 'message', id: '', data: '' },
  ]);
});

test('large unterminated records, oversized cursors and invalid UTF-8 fail boundedly', () => {
  assert.throws(() => new SseParser(() => {}).push(bytes(`data: ${'x'.repeat(265 * 1024)}`)));
  assert.throws(() => new SseParser(() => {}).push(bytes(`id: ${'x'.repeat(513)}\n`)));
  assert.throws(() => new SseParser(() => {}).push(new Uint8Array([0xff])));
});

test('high-volume streams retain no record history inside the parser', () => {
  let count = 0;
  const parser = new SseParser(() => count++);
  const batch = bytes('event: durable\ndata: {}\n\n'.repeat(10000));
  parser.push(batch);
  parser.push(batch);
  assert.equal(count, 20000);
});
