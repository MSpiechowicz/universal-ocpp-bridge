import { readFileSync, readdirSync } from 'node:fs';
import { gzipSync } from 'node:zlib';

const root = new URL('../../adapters/management/ui/', import.meta.url);
const files = ['index.html', ...readdirSync(new URL('assets/', root)).map(name => `assets/${name}`)];
if (files.sort().join(',') !== 'assets/console.css,assets/console.js,index.html') {
  throw new Error('Unexpected asset: update the explicit Rust routing and budget before shipping');
}
let raw = 0;
let compressed = 0;
for (const file of files) {
  const bytes = readFileSync(new URL(file, root));
  raw += bytes.length;
  compressed += gzipSync(bytes).length;
}
if (raw > 300 * 1024 || compressed > 100 * 1024) throw new Error('Pi console asset budget exceeded');
console.log(`Static console: ${raw} bytes raw; ${compressed} bytes gzip (measurement only)`);
