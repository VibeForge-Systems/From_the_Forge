import { afterEach, describe, expect, it } from 'vitest';
import {
  dayBoundaryFor,
  dayBounds,
  dayIndex,
  formatInstant,
  gridBounds,
  isoAtPrecision,
  monthBounds,
  setIntlStyleSupported,
  countsPerDay,
  isBefore2007,
  strictBounds,
  systemDayBoundary,
  timeText,
  utcDayBoundary,
  type DayBoundaryBuilder,
} from '@/services/time';
import { TIME_STATE_TEXT } from '@/services/viewerText';
import type { DecimalString, TimeDto } from '@/types';

const DAY = 86_400_000;
const raw = (s: string) => s as DecimalString;

/** A fake zone whose days are all 10 units long, from 1000. */
const fake: DayBoundaryBuilder = (y, m, d) => 1000 + ((y - 2026) * 372 + m * 31 + d) * 10;

function time(overrides: Partial<TimeDto>): TimeDto {
  return { ms: null, raw: null, unit: null, state: null, before2007: false, ...overrides };
}

afterEach(() => setIntlStyleSupported(true));

describe('day boundaries with an injected builder', () => {
  it('asks the builder for every boundary', () => {
    expect(dayBounds(fake, 2026, 0, 1, 3)).toEqual([1010, 1020, 1030, 1040]);
  });

  it('refuses a builder that goes backwards, and more than 42 days', () => {
    const backwards: DayBoundaryBuilder = (_y, _m, d) => 1000 - d;
    expect(() => dayBounds(backwards, 2026, 0, 1, 2)).toThrow(RangeError);
    expect(() => dayBounds(() => Number.NaN, 2026, 0, 1, 2)).toThrow(RangeError);
    expect(() => dayBounds(fake, 2026, 0, 1, 43)).toThrow(RangeError);
    expect(() => dayBounds(fake, 2026, 0, 1, 0)).toThrow(RangeError);
  });

  it('takes equal neighbours as a skipped (empty) day, and sends Rust strict bounds', () => {
    // Day 2 of this fake zone never happened: it starts where day 3 does.
    const skips: DayBoundaryBuilder = (y, m, d) => (d >= 3 ? fake(y, m, d) - 10 : fake(y, m, d));
    const b = dayBounds(skips, 2026, 0, 1, 3);
    expect(b).toEqual([1010, 1020, 1020, 1030]);
    expect(dayIndex(b, 1020)).toBe(2);
    const strict = strictBounds(b);
    expect(strict).toEqual({ bounds: [1010, 1020, 1030], slots: [0, -1, 1] });
    expect(countsPerDay(strict.slots, [4, 7])).toEqual([4, 0, 7]);
  });

  it('builds a month and a six-week grid', () => {
    expect(monthBounds(utcDayBoundary, 2026, 1)).toHaveLength(29); // February: 28 days
    expect(monthBounds(utcDayBoundary, 2028, 1)).toHaveLength(30); // leap year
    const grid = gridBounds(utcDayBoundary, 2026, 9, 1); // October 2026 starts on a Thursday
    expect(grid.start).toEqual({ year: 2026, month: 8, day: 28 });
    expect(grid.bounds).toHaveLength(43);
    expect(grid.bounds[1]! - grid.bounds[0]!).toBe(DAY);
    expect(gridBounds(utcDayBoundary, 2026, 9, 0).start).toEqual({ year: 2026, month: 8, day: 27 });
  });

  it('finds the day an instant falls in', () => {
    const b = monthBounds(utcDayBoundary, 2026, 0);
    expect(dayIndex(b, Date.UTC(2026, 0, 1))).toBe(0);
    expect(dayIndex(b, Date.UTC(2026, 0, 31, 23, 59))).toBe(30);
    expect(dayIndex(b, Date.UTC(2026, 1, 1))).toBe(-1);
  });

  it('chooses the production builder by setting', () => {
    expect(dayBoundaryFor('utc')).toBe(utcDayBoundary);
    expect(dayBoundaryFor('system')).toBe(systemDayBoundary);
  });
});

describe('formatting', () => {
  const at = Date.UTC(2026, 9, 19, 14, 5, 9, 999);

  it('formats in UTC with the chosen hour cycle and no sub-second digits', () => {
    const h23 = formatInstant(at, { timeFormat: 'h23', timeZone: 'utc', showSeconds: true });
    expect(h23).toContain('14:05:09');
    expect(h23).not.toContain('999');
    const h12 = formatInstant(at, { timeFormat: 'h12', timeZone: 'utc', showSeconds: false });
    expect(h12).toMatch(/2:05/);
    expect(h12).not.toContain(':09');
  });

  it('marks UTC times as UTC on the Intl path, as the fallback does, and dates not', () => {
    const utc = { timeFormat: 'h23', timeZone: 'utc', showSeconds: false } as const;
    expect(formatInstant(at, utc)).toMatch(/14:05 UTC$/);
    expect(formatInstant(at, utc, 'time')).toMatch(/14:05 UTC$/);
    expect(formatInstant(at, utc, 'date')).not.toContain('UTC');
    expect(formatInstant(at, { ...utc, timeZone: 'system' })).not.toContain('UTC');
  });

  it('marks instants before 2007 like Rust does (2007-01-01T00:00Z)', () => {
    expect(isBefore2007(Date.UTC(2006, 11, 31, 23, 59, 59))).toBe(true);
    expect(isBefore2007(Date.UTC(2007, 0, 1))).toBe(false);
    expect(isBefore2007(Number.NaN)).toBe(false);
  });

  it('falls back to a fixed format when the engine lacks dateStyle', () => {
    setIntlStyleSupported(false);
    expect(formatInstant(at, { timeFormat: 'h23', timeZone: 'utc', showSeconds: false })).toBe(
      '2026-10-19 14:05 UTC',
    );
    expect(formatInstant(at, { timeFormat: 'h12', timeZone: 'utc', showSeconds: true }, 'time')).toBe(
      '02:05:09 PM UTC',
    );
    expect(formatInstant(at, { timeFormat: 'h23', timeZone: 'utc', showSeconds: false }, 'date')).toBe(
      '2026-10-19',
    );
  });

  it('shows an instant only for seconds and nanoseconds units', () => {
    const t = timeText(time({ ms: at, raw: raw('1'), unit: 'nanoseconds', before2007: false }));
    expect(t.kind).toBe('instant');
    const early = timeText(time({ ms: Date.UTC(2005, 0, 1), unit: 'seconds', before2007: true }));
    expect(early.kind === 'instant' && early.before2007).toBe(true);
  });

  it('gives each undeterminable date its own sentence, never a date', () => {
    expect(timeText(time({ unit: 'zero', raw: raw('0') })).text).toBe('Date recorded as 0');
    expect(timeText(time({ unit: 'negative', raw: raw('-42') })).text).toBe('Negative date (raw -42)');
    expect(timeText(time({ unit: 'ambiguous', raw: raw('99999999999') })).text).toBe(
      'Date could not be interpreted (raw 99999999999)',
    );
    for (const state of ['null', 'absent', 'oversized', 'unreadable'] as const) {
      const t = timeText(time({ state }));
      expect(t).toEqual({ kind: 'sentence', text: TIME_STATE_TEXT[state] });
    }
    // A unit that would give an instant, but no instant: never invented.
    expect(timeText(time({ unit: 'seconds' })).kind).toBe('sentence');
  });
});

describe('the UTC ISO form at the unit precision', () => {
  it('gives whole seconds for a seconds value', () => {
    expect(isoAtPrecision(raw('700000000'), 'seconds')).toBe('2023-03-08T20:26:40Z');
  });

  it('gives nine fractional digits for a nanoseconds value', () => {
    expect(isoAtPrecision(raw('700000000123456789'), 'nanoseconds')).toBe(
      '2023-03-08T20:26:40.123456789Z',
    );
  });

  it('refuses other units and malformed values', () => {
    expect(isoAtPrecision(raw('0'), 'zero')).toBeNull();
    expect(isoAtPrecision(raw('-5'), 'seconds')).toBeNull();
    expect(isoAtPrecision(raw('12a'), 'seconds')).toBeNull();
    expect(isoAtPrecision(null, 'seconds')).toBeNull();
  });
});
