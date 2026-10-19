// Time display for the viewer (M5 §8.5, §8.7, D104).
//
// Rust is time-zone-free: instants cross IPC as Unix milliseconds, raw
// values as decimal strings. The webview's ICU is the single time-zone
// source. Local day boundaries come from an injectable `DayBoundaryBuilder`
// so the calendar logic is tested with a fake builder, and the production
// builders are tested across DST changes in their own Vitest projects
// (`tests/frontend/dst/`, one per `TZ`).
import type { DecimalString, TimeDto, TimeFormat, TimeZoneChoice, ViewerPreferences } from '@/types';
import {
  BEFORE_2007_TEXT,
  TIME_STATE_TEXT,
  TIME_UNIT_TEXT,
  fill,
} from './viewerText';

/**
 * The instant (Unix ms) at which a calendar day starts. `month` is 0-based
 * and `day` may overflow, as with `Date`: (2026, 0, 32) is 1 February.
 */
export type DayBoundaryBuilder = (year: number, month: number, day: number) => number;

/** Local midnight in this computer's zone, following its history and DST. */
export const systemDayBoundary: DayBoundaryBuilder = (y, m, d) => new Date(y, m, d).getTime();

/** UTC midnight. */
export const utcDayBoundary: DayBoundaryBuilder = (y, m, d) => Date.UTC(y, m, d);

export function dayBoundaryFor(zone: TimeZoneChoice): DayBoundaryBuilder {
  return zone === 'utc' ? utcDayBoundary : systemDayBoundary;
}

/** The most bounds `get_day_counts` takes: 42 days (a 6-week grid) and an end. */
export const MAX_BOUNDS = 43;

/**
 * `count + 1` boundaries for `count` consecutive days from (year, month,
 * day): the start of each day and the end of the last. Throws when the
 * result would not be strictly increasing (a builder bug) or too long.
 */
export function dayBounds(
  build: DayBoundaryBuilder,
  year: number,
  month: number,
  day: number,
  count: number,
): number[] {
  if (!Number.isInteger(count) || count < 1 || count + 1 > MAX_BOUNDS) {
    throw new RangeError('dayBounds: 1 to 42 days');
  }
  const bounds: number[] = [];
  for (let i = 0; i <= count; i += 1) bounds.push(build(year, month, day + i));
  for (let i = 1; i < bounds.length; i += 1) {
    if (!(bounds[i]! > bounds[i - 1]!) || !Number.isFinite(bounds[i]!)) {
      throw new RangeError('dayBounds: boundaries must increase');
    }
  }
  return bounds;
}

/** The boundaries of every day of one month (29 to 32 values). */
export function monthBounds(build: DayBoundaryBuilder, year: number, month: number): number[] {
  const days = new Date(Date.UTC(year, month + 1, 0)).getUTCDate();
  return dayBounds(build, year, month, 1, days);
}

/**
 * The boundaries of a 6-week calendar grid that contains the month,
 * starting on `weekStart` (0 = Sunday, 1 = Monday): 43 values. Also returns
 * the grid's first date, so cells can be labelled without another builder.
 */
export function gridBounds(
  build: DayBoundaryBuilder,
  year: number,
  month: number,
  weekStart: 0 | 1 = 1,
): { start: { year: number; month: number; day: number }; bounds: number[] } {
  const firstWeekday = new Date(Date.UTC(year, month, 1)).getUTCDay();
  const lead = (firstWeekday - weekStart + 7) % 7;
  const first = new Date(Date.UTC(year, month, 1 - lead));
  const start = {
    year: first.getUTCFullYear(),
    month: first.getUTCMonth(),
    day: first.getUTCDate(),
  };
  return { start, bounds: dayBounds(build, start.year, start.month, start.day, 42) };
}

/** The index of the day interval that holds `ms`, or -1. */
export function dayIndex(bounds: readonly number[], ms: number): number {
  for (let i = 0; i + 1 < bounds.length; i += 1) {
    if (ms >= bounds[i]! && ms < bounds[i + 1]!) return i;
  }
  return -1;
}

// ------------------------------------------------------------- formatting

/** Which Intl options this engine supports (set by `services/features.ts`). */
let intlStyleSupported = detectIntlStyle();

function detectIntlStyle(): boolean {
  try {
    const opts = new Intl.DateTimeFormat(undefined, {
      dateStyle: 'medium',
      timeStyle: 'short',
      hourCycle: 'h23',
    }).resolvedOptions() as Intl.ResolvedDateTimeFormatOptions & { dateStyle?: string };
    return opts.dateStyle === 'medium';
  } catch {
    return false;
  }
}

/** For tests and feature detection: force the fixed-format fallback. */
export function setIntlStyleSupported(supported: boolean): void {
  intlStyleSupported = supported;
}

export function intlStyleIsSupported(): boolean {
  return intlStyleSupported;
}

export type TimePrefs = Pick<ViewerPreferences, 'timeFormat' | 'timeZone' | 'showSeconds'>;

export const DEFAULT_TIME_PREFS: TimePrefs = {
  timeFormat: 'system',
  timeZone: 'system',
  showSeconds: false,
};

function hourCycle(format: TimeFormat): 'h12' | 'h23' | undefined {
  return format === 'system' ? undefined : format;
}

const pad = (n: number, width = 2) => String(n).padStart(width, '0');

/** "2026-10-19 14:05" (or with seconds), in the chosen zone. */
function fixedFormat(ms: number, prefs: TimePrefs, part: 'date' | 'time' | 'both'): string {
  const d = new Date(ms);
  const utc = prefs.timeZone === 'utc';
  const y = utc ? d.getUTCFullYear() : d.getFullYear();
  const mo = (utc ? d.getUTCMonth() : d.getMonth()) + 1;
  const da = utc ? d.getUTCDate() : d.getDate();
  let h = utc ? d.getUTCHours() : d.getHours();
  const mi = utc ? d.getUTCMinutes() : d.getMinutes();
  const s = utc ? d.getUTCSeconds() : d.getSeconds();
  const date = `${pad(y, 4)}-${pad(mo)}-${pad(da)}`;
  let suffix = '';
  if (prefs.timeFormat === 'h12') {
    suffix = h < 12 ? ' AM' : ' PM';
    h = h % 12 === 0 ? 12 : h % 12;
  }
  const time = `${pad(h)}:${pad(mi)}${prefs.showSeconds ? `:${pad(s)}` : ''}${suffix}`;
  const zone = utc ? ' UTC' : '';
  if (part === 'date') return date;
  if (part === 'time') return `${time}${zone}`;
  return `${date} ${time}${zone}`;
}

function formatter(prefs: TimePrefs, part: 'date' | 'time' | 'both'): Intl.DateTimeFormat {
  const options: Intl.DateTimeFormatOptions = {
    timeZone: prefs.timeZone === 'utc' ? 'UTC' : undefined,
  };
  if (part !== 'time') options.dateStyle = 'medium';
  if (part !== 'date') {
    options.timeStyle = prefs.showSeconds ? 'medium' : 'short';
    const cycle = hourCycle(prefs.timeFormat);
    if (cycle) options.hourCycle = cycle;
  }
  return new Intl.DateTimeFormat(undefined, options);
}

/**
 * An instant as text. Seconds are shown only when the setting asks; no
 * sub-second digits are ever shown, so a seconds-unit value never claims
 * more precision than it has.
 */
export function formatInstant(
  ms: number,
  prefs: TimePrefs = DEFAULT_TIME_PREFS,
  part: 'date' | 'time' | 'both' = 'both',
): string {
  if (!Number.isFinite(ms)) return '';
  if (!intlStyleSupported) return fixedFormat(ms, prefs, part);
  try {
    return formatter(prefs, part).format(new Date(ms));
  } catch {
    return fixedFormat(ms, prefs, part);
  }
}

/** What a `TimeDto` shows: an instant, or a sentence that is never dated. */
export type TimeText =
  | { kind: 'instant'; ms: number; text: string; before2007: boolean }
  | { kind: 'sentence'; text: string };

/**
 * The display of a recorded time. Undeterminable values each have their
 * own sentence and are never placed on a calendar (§8.5).
 */
export function timeText(t: TimeDto, prefs: TimePrefs = DEFAULT_TIME_PREFS): TimeText {
  if (t.ms !== null && (t.unit === 'seconds' || t.unit === 'nanoseconds')) {
    return {
      kind: 'instant',
      ms: t.ms,
      text: formatInstant(t.ms, prefs),
      before2007: t.before2007,
    };
  }
  if (t.state !== null) return { kind: 'sentence', text: TIME_STATE_TEXT[t.state] };
  if (t.unit !== null) {
    return { kind: 'sentence', text: fill(TIME_UNIT_TEXT[t.unit], { raw: t.raw ?? '?' }) };
  }
  return { kind: 'sentence', text: TIME_STATE_TEXT.null };
}

export { BEFORE_2007_TEXT };

// ------------------------------------------------------------- raw values

/** The Cocoa epoch (2001-01-01T00:00:00Z) in Unix seconds. */
const COCOA_EPOCH_UNIX_S = 978_307_200n;
const DIGITS = /^-?[0-9]{1,20}$/;

/**
 * The UTC ISO form of a raw Cocoa value, at its unit's precision (§7.3):
 * whole seconds for a seconds value, nine fractional digits only for a
 * nanoseconds value. `null` for any other unit or a malformed value.
 */
export function isoAtPrecision(raw: DecimalString | null, unit: TimeDto['unit']): string | null {
  if (raw === null || !DIGITS.test(raw)) return null;
  if (unit !== 'seconds' && unit !== 'nanoseconds') return null;
  const value = BigInt(raw);
  if (value <= 0n) return null;
  const unixS = unit === 'seconds' ? value + COCOA_EPOCH_UNIX_S : value / 1_000_000_000n + COCOA_EPOCH_UNIX_S;
  const ms = Number(unixS) * 1000;
  const d = new Date(ms);
  if (!Number.isFinite(d.getTime())) return null;
  const whole =
    `${pad(d.getUTCFullYear(), 4)}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}` +
    `T${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
  if (unit === 'seconds') return `${whole}Z`;
  const frac = (value % 1_000_000_000n).toString().padStart(9, '0');
  return `${whole}.${frac}Z`;
}
