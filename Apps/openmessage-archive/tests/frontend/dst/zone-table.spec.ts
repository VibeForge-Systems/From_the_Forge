// @vitest-environment node
// The export's zone table in this run's own zone (M6 §4.6, D128). It runs
// once per `TZ` of `vite.config.ts`; the first test checks that the zone
// took effect. The table built for "this computer's zone" must agree with
// the engine's `Date` offsets at every transition and one second before it,
// including Pacific/Apia's skipped day (2011-12-30).
import { describe, expect, it } from 'vitest';
import { buildZoneTable, systemZoneName } from '@/services/zoneTable';

/** The engine's offset at an instant, in seconds east of UTC. */
const dateOffset = (ms: number) => 0 - new Date(ms).getTimezoneOffset() * 60 || 0;

describe(`the zone table in ${process.env.TZ ?? '(no TZ)'}`, () => {
  it('runs in the zone this project set', () => {
    expect(process.env.TZ).toBeTruthy();
    expect(systemZoneName()).toBe(process.env.TZ);
  });

  it('matches the engine at every transition from 2005 to 2027', async () => {
    const name = systemZoneName();
    expect(name).not.toBeNull();
    const window: [number, number] = [Date.UTC(2005, 0, 1), Date.UTC(2027, 0, 1)];
    const r = await buildZoneTable(name!, window);
    expect(r.kind).toBe('ok');
    if (r.kind !== 'ok') return;
    const [head, ...rest] = r.table.transitions;
    const [first, firstOffset] = head!;
    expect(first).toBe(window[0]);
    expect(firstOffset).toBe(dateOffset(window[0]));
    for (const [at, offset] of rest) {
      // `getTimezoneOffset` has minute precision; every zone here changes on whole minutes.
      expect(offset).toBe(dateOffset(at));
      expect(dateOffset(at - 1000)).not.toBe(offset);
    }
    if (process.env.TZ === 'Pacific/Apia') {
      // Samoa crossed the date line: 2011-12-29 23:59:59 at -10:00 was
      // followed by 2011-12-31 00:00:00 at +14:00.
      expect(rest).toContainEqual([Date.UTC(2011, 11, 30, 10), 14 * 3600]);
    }
    if (process.env.TZ === 'Europe/London') {
      expect(rest).toContainEqual([Date.UTC(2026, 2, 29, 1), 3600]);
      expect(rest).toContainEqual([Date.UTC(2026, 9, 25, 1), 0]);
    }
  });
});
