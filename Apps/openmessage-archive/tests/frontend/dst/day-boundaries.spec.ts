// @vitest-environment node
// Local day boundaries across DST changes (M5 §8.7, §13.8). This file runs
// once per Vitest project in `vite.config.ts`, each with its own `TZ`. The
// first test proves the zone took effect, so a runner that ignores `TZ`
// (whether Node honours it on Windows runners is UNVERIFIED) fails loudly
// instead of passing on UTC.
import { describe, expect, it } from 'vitest';
import { countsPerDay, dayBounds, gridBounds, monthBounds, strictBounds, systemDayBoundary } from '@/services/time';

const HOUR = 3_600_000;

interface ZoneCase {
  /** Two dates whose UTC offsets must differ in this zone. */
  offsets: { at: [number, number, number]; minutes: number }[];
  /** Local days with their length in hours. */
  days: { at: [number, number, number]; hours: number }[];
  /** A local day the zone skipped entirely (no instant falls in it). */
  skipped?: [number, number, number];
}

const CASES: Record<string, ZoneCase> = {
  'Europe/London': {
    offsets: [
      { at: [2026, 0, 15], minutes: 0 },
      { at: [2026, 6, 15], minutes: -60 },
    ],
    days: [
      { at: [2026, 2, 28], hours: 24 },
      { at: [2026, 2, 29], hours: 23 }, // clocks go forward at 01:00 UTC
      { at: [2026, 9, 25], hours: 25 }, // clocks go back
      { at: [1971, 5, 1], hours: 24 }, // British Standard Time: GMT+1 all year
    ],
  },
  'America/Sao_Paulo': {
    // No DST since 2019; the history still applies to older messages.
    offsets: [
      { at: [2018, 0, 15], minutes: 120 },
      { at: [2018, 6, 15], minutes: 180 },
      { at: [2026, 0, 15], minutes: 180 },
    ],
    days: [
      // DST began at local midnight on 2018-11-04: that midnight did not
      // exist, so the day starts at 01:00 and lasts 23 hours.
      { at: [2018, 10, 3], hours: 24 },
      { at: [2018, 10, 4], hours: 23 },
      { at: [2019, 1, 16], hours: 25 },
      { at: [2026, 0, 15], hours: 24 },
    ],
  },
  'Australia/Lord_Howe': {
    // A 30-minute DST shift.
    offsets: [
      { at: [2026, 0, 15], minutes: -660 },
      { at: [2026, 6, 15], minutes: -630 },
    ],
    days: [
      { at: [2026, 3, 5], hours: 24.5 },
      { at: [2026, 9, 4], hours: 23.5 },
      { at: [2026, 9, 5], hours: 24 },
    ],
  },
  'Pacific/Apia': {
    // Samoa moved across the date line: 2011-12-29 (UTC-10) was followed
    // by 2011-12-31 (UTC+14). The 30th never happened there.
    offsets: [
      { at: [2011, 5, 15], minutes: 660 },
      { at: [2011, 11, 29], minutes: 600 },
      { at: [2012, 0, 15], minutes: -840 },
    ],
    days: [
      { at: [2011, 8, 24], hours: 23 },
      { at: [2011, 11, 29], hours: 24 },
      { at: [2011, 11, 31], hours: 24 },
    ],
    skipped: [2011, 11, 30],
  },
};

const zone = process.env.TZ ?? '';
const c = CASES[zone];

describe(`day boundaries in ${zone || '(no TZ)'}`, () => {
  it('runs in the zone its project set', () => {
    expect(c, `TZ=${zone} has no test case`).toBeDefined();
    expect(Intl.DateTimeFormat().resolvedOptions().timeZone).toBe(zone);
    for (const o of c!.offsets) {
      expect(new Date(...o.at).getTimezoneOffset(), `offset on ${o.at.join('-')}`).toBe(o.minutes);
    }
  });

  it('gives each local day its real length across DST changes', () => {
    for (const d of c!.days) {
      const [y, m, day] = d.at;
      const [start, end] = dayBounds(systemDayBoundary, y, m, day, 1);
      expect((end! - start!) / HOUR, `length of ${d.at.join('-')}`).toBe(d.hours);
      expect(new Date(start!).getDate()).toBe(day);
    }
  });

  it('makes a skipped day an empty interval, and the month still builds', () => {
    if (!c!.skipped) return;
    const [y, m, d] = c!.skipped;
    const [start, end] = dayBounds(systemDayBoundary, y, m, d, 1);
    expect(end).toBe(start);
    const month = monthBounds(systemDayBoundary, y, m);
    expect(month[d]).toBe(month[d - 1]);
    const grid = gridBounds(systemDayBoundary, y, m);
    expect(grid.bounds).toHaveLength(43);
    // Rust gets strictly increasing bounds; the skipped day counts 0.
    const strict = strictBounds(grid.bounds);
    expect(strict.bounds).toHaveLength(42);
    for (let i = 1; i < strict.bounds.length; i += 1) expect(strict.bounds[i]!).toBeGreaterThan(strict.bounds[i - 1]!);
    const perDay = countsPerDay(strict.slots, Array.from({ length: 41 }, () => 1));
    expect(perDay).toHaveLength(42);
    expect(perDay.filter((n) => n === 0)).toHaveLength(1);
  });

  it('builds strictly increasing month and grid bounds that cover every day', () => {
    for (const [y, m] of [
      [2026, 2],
      [2026, 9],
      [2018, 10],
      [2019, 1],
      [2026, 3],
    ] as const) {
      const month = monthBounds(systemDayBoundary, y, m);
      const days = new Date(Date.UTC(y, m + 1, 0)).getUTCDate();
      expect(month).toHaveLength(days + 1);
      const grid = gridBounds(systemDayBoundary, y, m).bounds;
      expect(grid).toHaveLength(43);
      for (const b of [month, grid]) {
        for (let i = 1; i < b.length; i += 1) expect(b[i]!).toBeGreaterThan(b[i - 1]!);
      }
      // The sum of the day lengths is the span of the month, never a
      // multiple of 24 hours assumed.
      expect(month[month.length - 1]! - month[0]!).toBe(
        new Date(y, m + 1, 1).getTime() - new Date(y, m, 1).getTime(),
      );
    }
  });
});
