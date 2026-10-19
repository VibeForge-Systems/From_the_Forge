// @vitest-environment node
// Calendar arithmetic for the date-jump dialog and the search date filter
// (M5 §8.7, slice 5i), with a fake day-boundary builder; the production
// builders run across DST changes in `tests/frontend/dst/`.
import { describe, expect, it } from 'vitest';
import {
  addDays,
  addMonths,
  civilOf,
  dateFilter,
  dayRange,
  endOfWeek,
  isoDate,
  monthGrid,
  parseIsoDate,
  startOfWeek,
  weekdayLabels,
} from '@/services/calendar';
import type { DayBoundaryBuilder } from '@/services/time';

/** A fake zone where every day is 1,000 ms long, from day 0 = 2026-01-01. */
const DAY = 1000;
const fake: DayBoundaryBuilder = (y, m, d) => Math.round((Date.UTC(y, m, d) - Date.UTC(2026, 0, 1)) / 86_400_000) * DAY;

describe('typed dates', () => {
  it('accepts real YYYY-MM-DD dates within 1900–2200', () => {
    expect(parseIsoDate('2026-10-19')).toEqual({ year: 2026, month: 9, day: 19 });
    expect(parseIsoDate(' 2024-02-29 ')).toEqual({ year: 2024, month: 1, day: 29 });
    expect(parseIsoDate('1900-01-01')).not.toBeNull();
    expect(parseIsoDate('2200-12-31')).not.toBeNull();
  });

  it('refuses malformed, unreal and out-of-range dates', () => {
    for (const bad of ['', '2026-2-1', '2026/02/01', '2026-02-30', '2025-02-29', '2026-13-01', '2026-00-10', '1899-12-31', '2201-01-01', '20261019', '2026-10-19T00:00']) {
      expect(parseIsoDate(bad), bad).toBeNull();
    }
  });

  it('formats as YYYY-MM-DD', () => {
    expect(isoDate({ year: 987, month: 0, day: 5 })).toBe('0987-01-05');
  });
});

describe('day arithmetic', () => {
  it('moves by days across months and years', () => {
    expect(addDays({ year: 2026, month: 11, day: 31 }, 1)).toEqual({ year: 2027, month: 0, day: 1 });
    expect(addDays({ year: 2024, month: 2, day: 1 }, -1)).toEqual({ year: 2024, month: 1, day: 29 });
  });

  it('moves by months, clamping the day', () => {
    expect(addMonths({ year: 2026, month: 0, day: 31 }, 1)).toEqual({ year: 2026, month: 1, day: 28 });
    expect(addMonths({ year: 2024, month: 1, day: 29 }, 12)).toEqual({ year: 2025, month: 1, day: 28 });
    expect(addMonths({ year: 2026, month: 0, day: 15 }, -1)).toEqual({ year: 2025, month: 11, day: 15 });
  });

  it('finds the Monday-first week', () => {
    // 2026-10-19 is a Monday.
    expect(startOfWeek({ year: 2026, month: 9, day: 21 })).toEqual({ year: 2026, month: 9, day: 19 });
    expect(endOfWeek({ year: 2026, month: 9, day: 19 })).toEqual({ year: 2026, month: 9, day: 25 });
    expect(startOfWeek({ year: 2026, month: 9, day: 25 })).toEqual({ year: 2026, month: 9, day: 19 });
  });

  it('reads the civil date of an instant in UTC', () => {
    expect(civilOf(Date.UTC(2026, 9, 19, 23, 59), 'utc')).toEqual({ year: 2026, month: 9, day: 19 });
  });

  it('labels the weekdays Monday first', () => {
    const w = weekdayLabels();
    expect(w).toHaveLength(7);
    expect(w[0]!.long).toMatch(/Monday|Mon/);
  });
});

describe('the month grid', () => {
  it('has 42 days from the Monday on or before the 1st, and 43 bounds from the builder', () => {
    // October 2026 starts on a Thursday.
    const g = monthGrid(fake, 2026, 9);
    expect(g.days).toHaveLength(42);
    expect(g.days[0]).toEqual({ year: 2026, month: 8, day: 28 });
    expect(g.days[3]).toEqual({ year: 2026, month: 9, day: 1 });
    expect(g.bounds).toHaveLength(43);
    expect(g.bounds[0]).toBe(fake(2026, 8, 28));
    expect(g.bounds[42]! - g.bounds[0]!).toBe(42 * DAY);
  });

  it('gives one day as a half-open range', () => {
    expect(dayRange(fake, { year: 2026, month: 0, day: 3 })).toEqual([2 * DAY, 3 * DAY]);
  });
});

describe('the search date filter', () => {
  it('sends nothing for empty fields', () => {
    expect(dateFilter(fake, '', ' ')).toEqual({ ok: true });
  });

  it('covers whole local days, the end inclusive', () => {
    expect(dateFilter(fake, '2026-01-02', '2026-01-04')).toEqual({ ok: true, fromUtcMs: DAY, untilUtcMs: 4 * DAY });
    expect(dateFilter(fake, '2026-01-02', '2026-01-02')).toEqual({ ok: true, fromUtcMs: DAY, untilUtcMs: 2 * DAY });
    expect(dateFilter(fake, '', '2026-01-02')).toEqual({ ok: true, untilUtcMs: 2 * DAY });
  });

  it('names the field that is wrong', () => {
    expect(dateFilter(fake, '2026-02-30', '')).toEqual({ ok: false, problem: 'from' });
    expect(dateFilter(fake, '', 'tomorrow')).toEqual({ ok: false, problem: 'to' });
    expect(dateFilter(fake, '2026-01-05', '2026-01-04')).toEqual({ ok: false, problem: 'order' });
  });
});
