// `tauri icon` also emits Android, iOS, and Windows Store artwork. This
// project ships desktop bundles only, so keep exactly the files that
// src-tauri/tauri.conf.json references (plus icon.png, used on Linux).
import { readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';

const dir = new URL('../src-tauri/icons/', import.meta.url).pathname;
const keep = new Set([
  '32x32.png',
  '128x128.png',
  '128x128@2x.png',
  'icon.icns',
  'icon.ico',
  'icon.png',
]);

for (const entry of readdirSync(dir)) {
  if (!keep.has(entry)) {
    rmSync(join(dir, entry), { recursive: true, force: true });
    console.log(`removed ${entry}`);
  }
}
