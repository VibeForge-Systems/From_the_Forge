// The zone offset table an export carries (M6 spec §4.6, D128, D145).
//
// Rust links no zone database (D104): the webview's ICU is the one source
// of zone rules. For an export in a zone other than UTC, the webview
// builds a table of that zone's offsets over a window Rust names, and Rust
// validates it (name, at most 4,096 transitions, strictly increasing,
// offsets within ±18 h, the window covered) and formats every time from
// it. Here the offset is sampled every 6 hours with
// `Intl.DateTimeFormat(…).formatToParts`, by differencing the zone's wall
// clock against UTC, and every change is bisected to the second. A change
// that is undone within one 6-hour step is not seen; no zone has had one
// since 1900 that ICU reports, but that is a property of the data, not a
// guarantee, and the manifest records the table so it can be audited.
//
// The build is bounded: it yields to the event loop every few thousand
// samples and stops after 10 s, and the dialog then offers UTC instead.
// Nothing here is remembered between exports.
import type { ZoneTableInputDto } from '@/types';

/** The sampling step (§4.6). */
export const SAMPLE_STEP_MS = 6 * 3_600_000;
/** The build stops after this long (§4.6). */
export const BUILD_BUDGET_MS = 10_000;
/** Rust refuses a table with more transitions (§4.6). */
export const MAX_TRANSITIONS = 4_096;
/** Rust refuses an offset beyond ±18 h. */
export const MAX_OFFSET_SECONDS = 18 * 3_600;
/** The names Rust accepts (`oma_export::ZoneTable::new`). */
export const ZONE_NAME = /^[A-Za-z0-9_+\-/]{1,64}$/;
/**
 * Added to the end of the window Rust asked for: Rust checks the table
 * again against "now" when the draft gets it, a little later.
 */
export const WINDOW_MARGIN_MS = 2 * 86_400_000;

/** A zone's offset from UTC, in seconds, at an instant (Unix ms); `null` when unknown. */
export type OffsetAt = (utcMs: number) => number | null;

export type ZoneTableResult =
  | { kind: 'ok'; table: ZoneTableInputDto }
  /** The 10-second bound was reached. */
  | { kind: 'timedOut' }
  /** More changes than Rust accepts. */
  | { kind: 'tooManyTransitions' }
  /** The engine does not know the zone, or its name is one Rust refuses. */
  | { kind: 'unsupported' }
  /** The caller stopped the build. */
  | { kind: 'cancelled' };

export interface BuildOptions {
  /** A monotonic clock in ms (`performance.now` by default). */
  now?: () => number;
  budgetMs?: number;
  stepMs?: number;
  /** Samples between two yields to the event loop. */
  yieldEvery?: number;
  /** The offset source (tests); ICU through `Intl.DateTimeFormat` by default. */
  offsetAt?: OffsetAt;
  /** Stops the build (the dialog closed, or the export changed). */
  signal?: AbortSignal;
}

/** The engine's own zone, or `null` when it names none Rust accepts. */
export function systemZoneName(): string | null {
  try {
    const name = new Intl.DateTimeFormat().resolvedOptions().timeZone;
    return typeof name === 'string' && ZONE_NAME.test(name) ? name : null;
  } catch {
    return null;
  }
}

/**
 * The zones the engine lists (`Intl.supportedValuesOf`), filtered to names
 * Rust accepts; `null` where the engine has no such list (feature-detected,
 * §4.6: support in WebKitGTK and WebView2 is unverified).
 */
export function supportedZones(): string[] | null {
  const intl = Intl as unknown as { supportedValuesOf?: (key: string) => string[] };
  if (typeof intl.supportedValuesOf !== 'function') return null;
  try {
    const all = intl.supportedValuesOf('timeZone');
    return Array.isArray(all) ? all.filter((z) => ZONE_NAME.test(z)) : null;
  } catch {
    return null;
  }
}

/**
 * The zone's offset at an instant, from ICU: the wall clock there, read
 * as if it were UTC, minus the instant (both to the second).
 */
export function intlOffsetAt(zone: string): OffsetAt | null {
  let format: Intl.DateTimeFormat;
  try {
    format = new Intl.DateTimeFormat('en-US', {
      timeZone: zone,
      hourCycle: 'h23',
      era: 'short',
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    });
  } catch {
    return null;
  }
  return (utcMs: number) => {
    const t = Math.floor(utcMs / 1000) * 1000;
    let parts: Intl.DateTimeFormatPart[];
    try {
      parts = format.formatToParts(t);
    } catch {
      return null;
    }
    const get = (type: Intl.DateTimeFormatPartTypes): number => {
      const p = parts.find((x) => x.type === type);
      return p ? Number(p.value) : Number.NaN;
    };
    const era = parts.find((x) => x.type === 'era')?.value ?? 'AD';
    const year = era.startsWith('B') ? 1 - get('year') : get('year');
    // Some engines say 24 for midnight even under h23.
    const hour = get('hour') % 24;
    const wall = new Date(0);
    wall.setUTCFullYear(year, get('month') - 1, get('day'));
    wall.setUTCHours(hour, get('minute'), get('second'), 0);
    const offset = (wall.getTime() - t) / 1000;
    // `|| 0`: never -0.
    return Number.isFinite(offset) ? Math.round(offset) || 0 : null;
  };
}

function yieldToLoop(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/**
 * Build the table for `name` over `[from, to]` (Unix ms). The first
 * transition is at `from` (Rust needs one at or before the window's
 * start); each later one is the first second with a new offset.
 */
export async function buildZoneTable(
  name: string,
  window: readonly [number, number],
  options: BuildOptions = {},
): Promise<ZoneTableResult> {
  const now = options.now ?? (() => performance.now());
  const budget = options.budgetMs ?? BUILD_BUDGET_MS;
  const step = options.stepMs ?? SAMPLE_STEP_MS;
  const yieldEvery = options.yieldEvery ?? 4_096;
  const [from, to] = window;
  if (!ZONE_NAME.test(name) || !(from < to) || !Number.isFinite(from) || !Number.isFinite(to)) {
    return { kind: 'unsupported' };
  }
  const offsetAt = options.offsetAt ?? intlOffsetAt(name);
  if (!offsetAt) return { kind: 'unsupported' };
  const started = now();
  const off = (ms: number): number | null => {
    const o = offsetAt(ms);
    return o === null || Math.abs(o) > MAX_OFFSET_SECONDS ? null : o;
  };

  let prev = off(from);
  if (prev === null) return { kind: 'unsupported' };
  const transitions: [number, number][] = [[from, prev]];
  let t = from;
  let samples = 0;
  while (t < to) {
    if (options.signal?.aborted) return { kind: 'cancelled' };
    if (now() - started > budget) return { kind: 'timedOut' };
    samples += 1;
    if (samples % yieldEvery === 0) {
      await yieldToLoop();
      if (options.signal?.aborted) return { kind: 'cancelled' };
    }
    const next = Math.min(t + step, to);
    const o = off(next);
    if (o === null) return { kind: 'unsupported' };
    if (o === prev) {
      t = next;
      continue;
    }
    // Bisect to the first second whose offset is not `prev`.
    let lo = Math.floor(t / 1000);
    let hi = Math.floor(next / 1000);
    while (hi - lo > 1) {
      const mid = Math.floor((lo + hi) / 2);
      const m = off(mid * 1000);
      if (m === null) return { kind: 'unsupported' };
      if (m === prev) lo = mid;
      else hi = mid;
    }
    const at = hi * 1000;
    const changed = off(at);
    if (changed === null) return { kind: 'unsupported' };
    transitions.push([at, changed]);
    if (transitions.length > MAX_TRANSITIONS) return { kind: 'tooManyTransitions' };
    prev = changed;
    t = at;
  }
  return { kind: 'ok', table: { name, windowUtcMs: [from, to], transitions } };
}

/** The window Rust asked for, with the margin added to its end. */
export function withMargin(window: readonly [number, number]): [number, number] {
  return [window[0], window[1] + WINDOW_MARGIN_MS];
}
