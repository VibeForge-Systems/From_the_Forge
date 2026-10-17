// License audit for JavaScript dependencies, read from package-lock.json.
//
// Runtime dependencies (no `dev` flag in the lockfile) end up in the shipped
// application, so their licenses must be on the allowlist or the check
// fails. Dev-only tools are not distributed; a license outside the list is
// reported for review but does not fail the check.
//
// Rust dependencies are audited separately by cargo-deny (deny.toml).
// Usage: node scripts/check-licenses.mjs [--list]
import { readFileSync } from 'node:fs';

const ALLOWED = new Set([
  'MIT',
  'MIT-0',
  'ISC',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'Apache-2.0',
  'Apache-2.0 OR MIT',
  'MIT OR Apache-2.0',
  '0BSD',
  'CC0-1.0',
  'Unlicense',
  'Zlib',
  'BlueOak-1.0.0',
]);

const lock = JSON.parse(readFileSync(new URL('../package-lock.json', import.meta.url), 'utf8'));
if (lock.lockfileVersion < 2) {
  console.error('package-lock.json v2+ is required (it records each package license).');
  process.exit(2);
}

const list = process.argv.includes('--list');
const runtimeProblems = [];
const devNotes = [];
let runtimeCount = 0;

for (const [path, meta] of Object.entries(lock.packages)) {
  if (path === '' || meta.link) continue;
  const name = path.replace(/^.*node_modules\//, '');
  const license = typeof meta.license === 'string' ? meta.license : null;
  const dev = meta.dev === true || meta.devOptional === true;
  if (!dev) runtimeCount += 1;
  if (list && !dev) console.log(`${name}@${meta.version}\t${license ?? 'UNKNOWN'}`);
  if (license && ALLOWED.has(license)) continue;
  (dev ? devNotes : runtimeProblems).push(`${name}@${meta.version}: ${license ?? 'no license field'}`);
}

for (const note of devNotes) console.warn(`review (dev only): ${note}`);
if (runtimeProblems.length) {
  console.error('Runtime dependencies with licenses outside the allowlist:');
  for (const p of runtimeProblems) console.error(`  ${p}`);
  process.exit(1);
}
console.log(`licenses ok: ${runtimeCount} runtime packages, all on the allowlist`);
