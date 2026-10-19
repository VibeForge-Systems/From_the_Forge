// @vitest-environment node
// The date dialog's day ranges and the search date filter across DST
// changes (M5 §8.7, §13.8, slice 5i), with the production builder. Runs
// once per `TZ` project; the first test checks that the zone took effect.
import { describe, expect, it } from 'vitest';
import { civilOf, dateFilter, dayRange, isoDate, monthGrid } from '@/services/calendar';
import { systemDayBoundary } from '@/services/time';

const HOUR = 3_600_000;

/** Per zone: one local day whose length is not 24 hours. */
const CASES: Record<string, { day: [number, number, number]; hours: number }> = {
  'Europe/London': { day: [2026, 2, 29], hours: 23 },
  'America/Sao_Paulo': { day: [2018, 10, 4], hours: 23 },
  'Australia/Lord_Howe': { day: [2026, 3, 5], hours: 24.5 },
};

const zone = process.env.TZ ?? '';
const c = CASES[zone];

describe(`calendar ranges in ${zone || '(no TZ)'}`, () => {
  it('runs in the zone its project set', () => {
    expect(c, `TZ=${zone} has no test case`).toBeDefined();
    expect(Intl.DateTimeFormat().resolvedOptions().timeZone).toBe(zone);
  });

  it('gives a DST day its real length in the dialog and the search filter', () => {
    const [year, month, day] = c!.day;
    const [start, end] = dayRange(systemDayBoundary, { year, month, day });
    expect((end - start) / HOUR).toBe(c!.hours);
    const iso = isoDate({ year, month, day });
    const f = dateFilter(systemDayBoundary, iso, iso);
    expect(f).toEqual({ ok: true, fromUtcMs: start, untilUtcMs: end });
    // Both ends are local days: the start reads back as that civil date.
    expect(civilOf(start, 'system')).toEqual({ year, month, day });
    expect(civilOf(end - 1, 'system')).toEqual({ year, month, day });
  });

  it('builds the month grid around the DST day with increasing bounds', () => {
    const [year, month] = c!.day;
    const g = monthGrid(systemDayBoundary, year, month);
    expect(g.bounds).toHaveLength(43);
    for (let i = 1; i < g.bounds.length; i += 1) expect(g.bounds[i]!).toBeGreaterThan(g.bounds[i - 1]!);
    g.days.forEach((d, i) => expect(civilOf(g.bounds[i]!, 'system')).toEqual(d));
  });
});
