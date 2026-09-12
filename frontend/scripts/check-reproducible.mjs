import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';

const files = ['index.html', 'assets/console.js', 'assets/console.css'];
const hashes = () => files.map(file => createHash('sha256')
  .update(readFileSync(new URL(`../../adapters/management/ui/${file}`, import.meta.url))).digest('hex'));
const before = hashes();
const build = spawnSync(process.execPath, ['node_modules/vite/bin/vite.js', 'build'], { stdio: 'inherit' });
if (build.status !== 0 || JSON.stringify(before) !== JSON.stringify(hashes())) {
  throw new Error('Repeated frontend build changed static asset bytes');
}
console.log('Repeated frontend build produced identical static assets');
