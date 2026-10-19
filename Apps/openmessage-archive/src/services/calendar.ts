// Calendar arithmetic for the date-jump dialog and the search date filter
// (M5 §8.7, slice 5i). Dates here are civil dates (year, 0-based month,
// day) with no zone; an instant is reached only through an injected
// `DayBoundaryBuilder` (`services/time.ts`), so the system zone and its
// history are used in production, UTC in UTC mode, and a fake in tests.
// Rust stays time-zone-free (D104).
import type { TimeZoneChoice } from '@/types';
import { dayBounds, gridBounds, type DayBoundaryBuilder } from './time';

export interface CivilDate {
  year: number;
  /** 0-based, as with `Date`. */
  month: number;
  day: number;
}

/** The years a typed date may name. Instants must fit Rust's nanosecond range. */
export const MIN_YEAR = 1900;
export const MAX_YEAR = 2200;

/** The calendar's first weekday: Monday (ISO 8601). */
export const WEEK_START = 1 as const;

/** Normalise through `Date.UTC`, so day and month may overflow. */
export function civil(year: number, month: number, day: number): CivilDate {
  const d = new Date(Date.UTC(year, month, day));
  return { year: d.getUTCFullYear(), month: d.getUTCMonth(), day: d.getUTCDate() };
}

export function addDays(c: CivilDate, n: number): CivilDate {
  return civil(c.year, c.month, c.day + n);
}

/** The same day `n` months on, clamped to the target month's length. */
export function addMonths(c: CivilDate, n: number): CivilDate {
  const first = civil(c.year, c.month + n, 1);
  const length = new Date(Date.UTC(first.year, first.month + 1, 0)).getUTCDate();
  return { ...first, day: Math.min(c.day, length) };
}

export function sameDay(a: CivilDate, b: CivilDate): boolean {
  return a.year === b.year && a.month === b.month && a.day === b.day;
}

/** Monday-first weekday index, 0 to 6. */
function weekdayIndex(c: CivilDate): number {
  const w = new Date(Date.UTC(c.year, c.month, c.day)).getUTCDay();
  return (w - WEEK_START + 7) % 7;
}

export function startOfWeek(c: CivilDate): CivilDate {
  return addDays(c, -weekdayIndex(c));
}

export function endOfWeek(c: CivilDate): CivilDate {
  return addDays(c, 6 - weekdayIndex(c));
}

const pad = (n: number, width = 2) => String(n).padStart(width, '0');

/** `YYYY-MM-DD`. */
export function isoDate(c: CivilDate): string {
  return `${pad(c.year, 4)}-${pad(c.month + 1)}-${pad(c.day)}`;
}

/**
 * A typed `YYYY-MM-DD` date, or `null` when it is malformed, not a real
 * date (2026-02-30), or outside `MIN_YEAR`–`MAX_YEAR`.
 */
export function parseIsoDate(text: string): CivilDate | null {
  const m = /^\s*(\d{4})-(\d{2})-(\d{2})\s*$/.exec(text);
  if (!m) return null;
  const year = Number(m[1]);
  const month = Number(m[2]) - 1;
  const day = Number(m[3]);
  if (year < MIN_YEAR || year > MAX_YEAR || month < 0 || month > 11 || day < 1) return null;
  const c = civil(year, month, day);
  return c.year === year && c.month === month && c.day === day ? c : null;
}

/** The civil date of an instant in the chosen zone. */
export function civilOf(ms: number, zone: TimeZoneChoice): CivilDate {
  const d = new Date(ms);
  return zone === 'utc'
    ? { year: d.getUTCFullYear(), month: d.getUTCMonth(), day: d.getUTCDate() }
    : { year: d.getFullYear(), month: d.getMonth(), day: d.getDate() };
}

/** `[start, end)` of one local day, through the builder. Throws on a builder bug. */
export function dayRange(build: DayBoundaryBuilder, c: CivilDate): [number, number] {
  const b = dayBounds(build, c.year, c.month, c.day, 1);
  return [b[0]!, b[1]!];
}

/** The 42 days of the 6-week grid that holds a month, and its 43 bounds. */
export function monthGrid(
  build: DayBoundaryBuilder,
  year: number,
  month: number,
): { days: CivilDate[]; bounds: number[] } {
  const { start, bounds } = gridBounds(build, year, month, WEEK_START);
  const days = Array.from({ length: 42 }, (_, i) => civil(start.year, start.month, start.day + i));
  return { days, bounds };
}

/**
 * The search date filter: `from` and `to` are typed `YYYY-MM-DD` dates
 * (either may be empty), both inclusive. The result is the half-open UTC
 * range Rust filters on: the start of `from`'s local day and the start of
 * the day after `to`.
 */
export type DateFilter =
  | { ok: true; fromUtcMs?: number; untilUtcMs?: number }
  /** `empty`: every day of the range was skipped by the zone (no instant in it). */
  | { ok: false; problem: 'from' | 'to' | 'order' | 'empty' };

export function dateFilter(build: DayBoundaryBuilder, from: string, to: string): DateFilter {
  const out: { ok: true; fromUtcMs?: number; untilUtcMs?: number } = { ok: true };
  let a: CivilDate | null = null;
  let b: CivilDate | null = null;
  if (from.trim() !== '') {
    a = parseIsoDate(from);
    if (!a) return { ok: false, problem: 'from' };
  }
  if (to.trim() !== '') {
    b = parseIsoDate(to);
    if (!b) return { ok: false, problem: 'to' };
  }
  try {
    if (a) out.fromUtcMs = dayRange(build, a)[0];
    if (b) out.untilUtcMs = dayRange(build, b)[1];
  } catch {
    return { ok: false, problem: a && !b ? 'from' : 'to' };
  }
  if (a && b && isoDate(b) < isoDate(a)) return { ok: false, problem: 'order' };
  // Days the zone skipped hold no instant (dayBounds): a range made only of
  // them is empty, and Rust refuses an empty range.
  if (out.fromUtcMs !== undefined && out.untilUtcMs !== undefined && out.untilUtcMs <= out.fromUtcMs) {
    return { ok: false, problem: 'empty' };
  }
  return out;
}

// ------------------------------------------------------------- labels

function utcFormat(options: Intl.DateTimeFormatOptions, c: CivilDate, fallback: string): string {
  try {
    return new Intl.DateTimeFormat(undefined, { ...options, timeZone: 'UTC' }).format(
      new Date(Date.UTC(c.year, c.month, c.day)),
    );
  } catch {
    return fallback;
  }
}

/** "October 2026" (or `2026-10` without Intl). */
export function monthLabel(year: number, month: number): string {
  return utcFormat({ month: 'long', year: 'numeric' }, { year, month, day: 1 }, `${pad(year, 4)}-${pad(month + 1)}`);
}

/** A day's full name for assistive technology ("Monday, 19 October 2026"). */
export function fullDayLabel(c: CivilDate): string {
  return utcFormat({ weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' }, c, isoDate(c));
}

/** The seven weekday headers, Monday first: short and long forms. */
export function weekdayLabels(): { short: string; long: string }[] {
  // 2024-01-01 was a Monday.
  return Array.from({ length: 7 }, (_, i) => {
    const c = { year: 2024, month: 0, day: 1 + i };
    return {
      short: utcFormat({ weekday: 'short' }, c, ['Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa', 'Su'][i]!),
      long: utcFormat({ weekday: 'long' }, c, ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'][i]!),
    };
  });
}
