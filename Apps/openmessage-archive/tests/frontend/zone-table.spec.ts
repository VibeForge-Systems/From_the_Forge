// @vitest-environment node
// The zone offset table (M6 spec §4.6, D128, D145): sampled every 6 hours,
// each change bisected to the second, the window's start as the first
// transition, the 10-second bound, Rust's limits, and ICU itself for a
// known zone.
import { describe, expect, it } from 'vitest';
import {
  BUILD_BUDGET_MS,
  MAX_TRANSITIONS,
  SAMPLE_STEP_MS,
  WINDOW_MARGIN_MS,
  ZONE_NAME,
  buildZoneTable,
  intlOffsetAt,
  supportedZones,
  systemZoneName,
  withMargin,
  type OffsetAt,
} from '@/services/zoneTable';

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const T0 = Date.UTC(2024, 0, 1);

/** An offset source with changes at the given instants (ms), counting its calls. */
function stepped(initial: number, changes: [number, number][]): OffsetAt & { calls: number[] } {
  const calls: number[] = [];
  const fn = ((ms: number) => {
    calls.push(ms);
    let o = initial;
    for (const [at, off] of changes) if (ms >= at) o = off;
    return o;
  }) as OffsetAt & { calls: number[] };
  fn.calls = calls;
  return fn;
}

describe('buildZoneTable', () => {
  it('samples every 6 hours and starts with the window start', async () => {
    const off = stepped(3600, []);
    const r = await buildZoneTable('Test/Zone', [T0, T0 + 10 * DAY], { offsetAt: off });
    expect(r).toEqual({ kind: 'ok', table: { name: 'Test/Zone', windowUtcMs: [T0, T0 + 10 * DAY], transitions: [[T0, 3600]] } });
    // The window start, then one sample per 6 hours to the end.
    expect(off.calls.length).toBe(1 + (10 * DAY) / SAMPLE_STEP_MS);
    const gaps = off.calls.slice(1).map((c, i) => c - off.calls[i]!);
    expect(new Set(gaps)).toEqual(new Set([SAMPLE_STEP_MS]));
  });

  it('bisects a change to the exact second', async () => {
    const at = T0 + 3 * DAY + 5 * HOUR + 17 * 60_000 + 43_000;
    const r = await buildZoneTable('Test/Zone', [T0, T0 + 10 * DAY], { offsetAt: stepped(3600, [[at, 7200]]) });
    expect(r.kind).toBe('ok');
    if (r.kind !== 'ok') return;
    expect(r.table.transitions).toEqual([
      [T0, 3600],
      [at, 7200],
    ]);
  });

  it('finds every change, in order, strictly increasing', async () => {
    const changes: [number, number][] = [
      [T0 + DAY + 1_000, 7200],
      [T0 + 2 * DAY + 7 * HOUR, 3600],
      [T0 + 2 * DAY + 13 * HOUR + 1_000, -1800],
      [T0 + 9 * DAY + 23 * HOUR + 59 * 60_000 + 59_000, 0],
    ];
    const r = await buildZoneTable('Test/Zone', [T0, T0 + 10 * DAY], { offsetAt: stepped(3600, changes) });
    expect(r.kind === 'ok' && r.table.transitions).toEqual([[T0, 3600], ...changes]);
  });

  it('does not see a change undone within one 6-hour step (the documented limit)', async () => {
    const r = await buildZoneTable('Test/Zone', [T0, T0 + DAY], {
      offsetAt: stepped(0, [
        [T0 + HOUR, 1800],
        [T0 + 2 * HOUR, 0],
      ]),
    });
    expect(r.kind === 'ok' && r.table.transitions).toEqual([[T0, 0]]);
  });

  it('stops at the time bound and reports it', async () => {
    let clock = 0;
    const r = await buildZoneTable('Test/Zone', [T0, T0 + 1000 * DAY], {
      offsetAt: () => 0,
      now: () => (clock += 1),
      budgetMs: 50,
    });
    expect(r).toEqual({ kind: 'timedOut' });
    expect(BUILD_BUDGET_MS).toBe(10_000);
  });

  it('refuses more transitions than Rust accepts', async () => {
    // A change every 12 hours over 2,100 days: more than 4,096.
    const r = await buildZoneTable('Test/Zone', [T0, T0 + 2_100 * DAY], {
      offsetAt: (ms) => (Math.floor((ms - T0) / (12 * HOUR)) % 2 === 0 ? 0 : 3600),
      yieldEvery: 1_000_000,
    });
    expect(r).toEqual({ kind: 'tooManyTransitions' });
    expect(MAX_TRANSITIONS).toBe(4_096);
  });

  it('refuses names Rust refuses, empty windows and offsets beyond 18 hours', async () => {
    expect(await buildZoneTable('Bad Zone', [T0, T0 + DAY], { offsetAt: () => 0 })).toEqual({ kind: 'unsupported' });
    expect(await buildZoneTable('x'.repeat(65), [T0, T0 + DAY], { offsetAt: () => 0 })).toEqual({ kind: 'unsupported' });
    expect(await buildZoneTable('Test/Zone', [T0, T0], { offsetAt: () => 0 })).toEqual({ kind: 'unsupported' });
    expect(await buildZoneTable('Test/Zone', [T0, T0 + DAY], { offsetAt: () => 19 * 3600 })).toEqual({
      kind: 'unsupported',
    });
    expect(await buildZoneTable('Test/Zone', [T0, T0 + DAY], { offsetAt: () => null })).toEqual({ kind: 'unsupported' });
    expect(await buildZoneTable('No/Such_Zone', [T0, T0 + DAY])).toEqual({ kind: 'unsupported' });
  });

  it('stops when its signal is aborted', async () => {
    const abort = new AbortController();
    abort.abort();
    expect(await buildZoneTable('Test/Zone', [T0, T0 + DAY], { offsetAt: () => 0, signal: abort.signal })).toEqual({
      kind: 'cancelled',
    });
  });

  it('yields to the event loop while it builds', async () => {
    let ticks = 0;
    const timer = setInterval(() => (ticks += 1), 0);
    const r = await buildZoneTable('Test/Zone', [T0, T0 + 400 * DAY], { offsetAt: () => 0, yieldEvery: 100 });
    clearInterval(timer);
    expect(r.kind).toBe('ok');
    expect(ticks).toBeGreaterThan(0);
  });

  it('reads ICU: Europe/Berlin in 2024 changes at 01:00 UTC on 31 March and 27 October', async () => {
    const r = await buildZoneTable('Europe/Berlin', [T0, Date.UTC(2025, 0, 1)]);
    expect(r).toEqual({
      kind: 'ok',
      table: {
        name: 'Europe/Berlin',
        windowUtcMs: [T0, Date.UTC(2025, 0, 1)],
        transitions: [
          [T0, 3600],
          [Date.UTC(2024, 2, 31, 1), 7200],
          [Date.UTC(2024, 9, 27, 1), 3600],
        ],
      },
    });
  });

  it('reads ICU offsets with seconds and before 1970', () => {
    const off = intlOffsetAt('UTC');
    expect(off?.(Date.UTC(1900, 0, 1))).toBe(0);
    const ny = intlOffsetAt('America/New_York');
    expect(ny?.(Date.UTC(1960, 6, 1))).toBe(-4 * 3600);
    expect(intlOffsetAt('No/Such_Zone')).toBeNull();
  });

  it('builds a whole clamped window (1900 to two years ahead) for a zone with many changes', async () => {
    const r = await buildZoneTable('America/New_York', [Date.UTC(1900, 0, 1), Date.UTC(2028, 0, 1)]);
    expect(r.kind).toBe('ok');
    if (r.kind !== 'ok') return;
    expect(r.table.transitions.length).toBeGreaterThan(150);
    expect(r.table.transitions.length).toBeLessThanOrEqual(MAX_TRANSITIONS);
    for (let i = 1; i < r.table.transitions.length; i += 1) {
      expect(r.table.transitions[i]![0]).toBeGreaterThan(r.table.transitions[i - 1]![0]);
      expect(r.table.transitions[i]![1]).not.toBe(r.table.transitions[i - 1]![1]);
    }
  });
});

describe('the engine zone list and the window margin', () => {
  it('names the system zone in a form Rust accepts', () => {
    const z = systemZoneName();
    expect(z === null || ZONE_NAME.test(z)).toBe(true);
    const all = supportedZones();
    if (all !== null) expect(all.every((n) => ZONE_NAME.test(n))).toBe(true);
  });

  it('adds the margin to the window end only', () => {
    expect(withMargin([1, 2])).toEqual([1, 2 + WINDOW_MARGIN_MS]);
  });
});
