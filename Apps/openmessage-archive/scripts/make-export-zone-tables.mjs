// Writes the fixed test zone tables of the JSON zone oracle (M6 slice 6f,
// spec 14.4 "Zone"): tests/fixtures/export/zones/zone-<n>.json, one per
// zone below, each a zone offset table in the form the webview will hand
// Rust (spec 4.6, D128; the webview's own builder is slice 6k): the zone's
// name, the window it covers in Unix milliseconds, and its transitions
// `[utcMs, offsetSeconds]`, the first at the window's start.
//
// The offsets come from this Node's ICU (`Intl.DateTimeFormat`), the same
// kind of source as the webview. They are sampled every 6 hours across the
// window, and each change is narrowed to the exact second by bisection, so
// the table holds every transition ICU knows inside the window that is
// more than 6 hours from the next one. The window is 1900-01-01T00:00Z to
// the catalogues' import start (2027-01-01) plus 366 days, the clamp
// `oma-export`'s `ZoneTable` applies for those exports.
//
// Run from the app directory and commit the result:
//
//   node scripts/make-export-zone-tables.mjs
//
// The Python oracle checks every exported local time against the standard
// library's `zoneinfo` for the same zone, an independent tz database.
import { mkdirSync, writeFileSync } from 'node:fs';

const OUT = 'tests/fixtures/export/zones';
const ZONES = ['America/New_York', 'Asia/Kolkata', 'Australia/Lord_Howe', 'Europe/London'];
const FROM_MS = Date.UTC(1900, 0, 1);
const MODEL_NOW_MS = Date.UTC(2027, 0, 1);
const TO_MS = MODEL_NOW_MS + 366 * 86_400_000;
const STEP_MS = 6 * 3_600_000;

function offsetAt(fmt, ms) {
  // Local wall time minus UTC, to the second.
  const t = Math.floor(ms / 1000) * 1000;
  const parts = Object.fromEntries(fmt.formatToParts(new Date(t)).map((p) => [p.type, p.value]));
  const y = Number(parts.year) * (parts.era === 'BC' ? -1 : 1);
  const local = Date.UTC(y, Number(parts.month) - 1, Number(parts.day), Number(parts.hour) % 24,
    Number(parts.minute), Number(parts.second));
  return Math.round((local - t) / 1000);
}

function table(zone) {
  const fmt = new Intl.DateTimeFormat('en-US', {
    timeZone: zone, hourCycle: 'h23', era: 'short', year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
  });
  const transitions = [[FROM_MS, offsetAt(fmt, FROM_MS)]];
  let prev = transitions[0][1];
  for (let ms = FROM_MS + STEP_MS; ms <= TO_MS; ms += STEP_MS) {
    const o = offsetAt(fmt, ms);
    if (o === prev) continue;
    // Bisect (in whole seconds) for the first second with the new offset.
    let lo = ms - STEP_MS;
    let hi = ms;
    while (hi - lo > 1000) {
      const mid = lo + Math.floor((hi - lo) / 2000) * 1000;
      if (offsetAt(fmt, mid) === prev) lo = mid;
      else hi = mid;
    }
    transitions.push([hi, offsetAt(fmt, hi)]);
    prev = transitions[transitions.length - 1][1];
  }
  return { name: zone, windowUtcMs: [FROM_MS, TO_MS], transitions };
}

mkdirSync(OUT, { recursive: true });
ZONES.forEach((zone, i) => {
  const doc = {
    format: 'oma-export-zone-table',
    formatVersion: 1,
    source: `Node ${process.versions.node}, ICU ${process.versions.icu}, tz ${process.versions.tz}`,
    ...table(zone),
  };
  const path = `${OUT}/zone-${i + 1}.json`;
  writeFileSync(path, `${JSON.stringify(doc, null, 1)}\n`);
  console.log(`wrote ${path}: ${doc.transitions.length} transitions`);
});
