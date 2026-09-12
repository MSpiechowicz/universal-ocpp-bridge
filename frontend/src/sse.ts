import { ApiError } from './http';

export interface SseRecord { event: string; data: string; id?: string }

// Incremental CR/LF/CRLF parsing. A peer cannot grow an unfinished line or record indefinitely.
export class SseParser {
  private line = '';
  private data: string[] = [];
  private event = '';
  private id: string | undefined;
  private bytes = 0;
  private carriageReturn = false;
  private decoder = new TextDecoder('utf-8', { fatal: true });
  constructor(private readonly emit: (record: SseRecord) => void) {}

  push(chunk: Uint8Array) {
    for (let start = 0; start < chunk.length; start += 8192) {
      const text = this.decoder.decode(chunk.subarray(start, start + 8192), { stream: true });
      for (const character of text) {
        if (this.carriageReturn && character === '\n') { this.carriageReturn = false; continue; }
        this.carriageReturn = character === '\r';
        this.bytes += character.codePointAt(0)! > 0xffff ? 4 : character.charCodeAt(0) > 0x7ff ? 3 : character.charCodeAt(0) > 0x7f ? 2 : 1;
        if (this.bytes > 264 * 1024) throw new ApiError(0, 'limit');
        if (character === '\n' || character === '\r') this.finishLine();
        else this.line += character;
      }
    }
  }

  private finishLine() {
    const line = this.line;
    this.line = '';
    if (line === '') {
      if (this.data.length || this.id !== undefined) this.emit({ event: this.event || 'message', data: this.data.join('\n'), id: this.id });
      this.data = []; this.event = ''; this.id = undefined; this.bytes = 0;
      return;
    }
    if (line.startsWith(':')) return;
    const colon = line.indexOf(':');
    const key = colon < 0 ? line : line.slice(0, colon);
    const value = colon < 0 ? '' : line.slice(colon + 1).replace(/^ /, '');
    if (key === 'data') this.data.push(value);
    if (key === 'event') this.event = value;
    if (key === 'id') {
      if (value.length > 512 || /[\u0000-\u001f\u007f]/.test(value)) throw new ApiError(0);
      this.id = value;
    }
  }
}
