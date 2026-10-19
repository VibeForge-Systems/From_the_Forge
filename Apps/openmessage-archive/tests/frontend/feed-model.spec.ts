// @vitest-environment node
// The geometry of the in-house VirtualFeed (slice 5h0): prefix sums,
// windows, and the anchor that keeps a row in place across prepends,
// evictions and height corrections.
import { describe, expect, it } from 'vitest';
import { FeedModel } from '@/services/feedModel';

const range = (from: number, count: number) => Array.from({ length: count }, (_, i) => from + i);

function model(keys: number[], heights?: (k: number) => number, estimate = 50): FeedModel<number> {
  const m = new FeedModel<number>(estimate);
  m.setKeys(keys);
  if (heights) for (const k of keys) m.setHeight(k, heights(k));
  return m;
}

/** Where row `key` sits relative to the top of the viewport. */
function screenTop(m: FeedModel<number>, key: number, scrollTop: number): number {
  return m.top(m.indexOf(key)) - scrollTop;
}

describe('FeedModel geometry', () => {
  it('lays rows out by measured heights, with the estimate until measured', () => {
    const m = model([1, 2, 3], undefined, 40);
    expect(m.total()).toBe(120);
    m.setHeight(2, 100);
    expect(m.top(2)).toBe(140);
    expect(m.total()).toBe(180);
    expect(m.isMeasured(2)).toBe(true);
    expect(m.isMeasured(1)).toBe(false);
  });

  it('ignores sub-pixel noise, unknown keys and impossible heights', () => {
    const m = model([1, 2], () => 50);
    expect(m.setHeight(1, 50.2)).toBe(false);
    expect(m.setHeight(9, 10)).toBe(false);
    expect(m.setHeight(1, Number.NaN)).toBe(false);
    expect(m.setHeight(1, -3)).toBe(false);
    expect(m.setHeight(1, 61)).toBe(true);
  });

  it('finds the row at an offset by binary search, clamped to the ends', () => {
    const m = model(range(0, 1000), (k) => 10 + (k % 7));
    for (const offset of [0, 1, 999, 5000, 12_000]) {
      const i = m.indexAt(offset);
      if (offset < m.total()) {
        expect(m.top(i)).toBeLessThanOrEqual(offset);
        expect(m.bottom(i)).toBeGreaterThan(offset);
      }
    }
    expect(m.indexAt(-5)).toBe(0);
    expect(m.indexAt(m.total() + 10)).toBe(999);
    expect(new FeedModel<number>(10).indexAt(0)).toBe(-1);
  });

  it('forgets the heights of keys that left, keeps the others', () => {
    const m = model([1, 2, 3], () => 70);
    m.setKeys([2, 3, 4]);
    expect(m.isMeasured(1)).toBe(false);
    expect(m.isMeasured(2)).toBe(true);
    expect(m.total()).toBe(70 + 70 + 50);
  });

  it('refuses a non-positive estimate', () => {
    expect(() => new FeedModel<number>(0)).toThrow(RangeError);
  });
});

describe('FeedModel anchors', () => {
  const mixed = (k: number) => 30 + ((k * 37) % 90);

  it('keeps the first visible row in place when rows are prepended at scrollTop 0', () => {
    const m = model(range(1000, 200), mixed);
    const anchor = m.anchorAt(0)!;
    expect(anchor).toMatchObject({ key: 1000, edge: 'top', offset: 0 });
    m.setKeys([...range(950, 50), ...range(1000, 200)]);
    let top = m.scrollTopFor(anchor, 600)!;
    expect(screenTop(m, 1000, top)).toBe(0);
    // The new rows are measured after they render: still in place.
    for (const k of range(950, 50)) m.setHeight(k, mixed(k));
    top = m.scrollTopFor(anchor, 600)!;
    expect(screenTop(m, 1000, top)).toBe(0);
    expect(top).toBe(range(950, 50).reduce((n, k) => n + mixed(k), 0));
  });

  it('keeps a row partly scrolled out in place across a prepend', () => {
    const m = model(range(100, 100), mixed);
    const scrollTop = m.top(40) + 13;
    const anchor = m.anchorAt(scrollTop)!;
    const before = screenTop(m, anchor.key, scrollTop);
    expect(before).toBe(-13);
    m.setKeys([...range(50, 50), ...range(100, 100)]);
    for (const k of range(50, 50)) m.setHeight(k, mixed(k));
    expect(screenTop(m, anchor.key, m.scrollTopFor(anchor, 500)!)).toBe(before);
  });

  it('keeps the anchor when rows above it are evicted or re-measured', () => {
    const m = model(range(1, 300), mixed);
    const scrollTop = m.top(250);
    const anchor = m.anchorAt(scrollTop)!;
    m.setKeys(range(101, 200));
    expect(screenTop(m, anchor.key, m.scrollTopFor(anchor, 400)!)).toBe(0);
    m.setHeight(150, 400);
    expect(screenTop(m, anchor.key, m.scrollTopFor(anchor, 400)!)).toBe(0);
  });

  it('pins a bottom-aligned row to the bottom of the viewport as its height changes', () => {
    const m = model(range(1, 20), () => 50);
    const anchor = m.alignAnchor(20, 'end', 300);
    expect(m.scrollTopFor(anchor, 300)).toBe(1000 - 300);
    m.setHeight(20, 120);
    expect(m.scrollTopFor(anchor, 300)).toBe(1070 - 300);
  });

  it('clamps to the scrollable range and reports a vanished anchor', () => {
    const m = model(range(1, 10), () => 50);
    expect(m.scrollTopFor({ key: 10, edge: 'top', offset: 0 }, 300)).toBe(200);
    expect(m.scrollTopFor({ key: 1, edge: 'top', offset: 40 }, 300)).toBe(0);
    expect(m.scrollTopFor({ key: 99, edge: 'top', offset: 0 }, 300)).toBeNull();
  });
});

describe('FeedModel windows', () => {
  it('renders the rows around the viewport and gaps that add up to the total', () => {
    const m = model(range(0, 1000), () => 40);
    const pieces = m.window(20_000, 400, { overscan: 200, keep: [], maxRows: 200 });
    const rows = pieces.filter((p) => p.kind === 'row');
    expect(rows.length).toBeLessThanOrEqual(1 + (400 + 400) / 40 + 1);
    expect(rows[0]!.index).toBe(m.indexAt(20_000 - 200));
    const height = pieces.reduce((n, p) => n + (p.kind === 'gap' ? p.height : m.heightAt(p.index)), 0);
    expect(height).toBe(m.total());
  });

  it('keeps kept rows rendered wherever they are, in order', () => {
    const m = model(range(0, 1000), () => 40);
    const pieces = m.window(20_000, 400, { overscan: 0, keep: [3, 990], maxRows: 200 });
    const indexes = pieces.flatMap((p) => (p.kind === 'row' ? [p.index] : []));
    expect(indexes[0]).toBe(3);
    expect(indexes[indexes.length - 1]).toBe(990);
    expect([...indexes].sort((a, b) => a - b)).toEqual(indexes);
  });

  it('caps the rendered rows', () => {
    const m = model(range(0, 5000), () => 2);
    const rows = m.window(4000, 2000, { overscan: 600, keep: [], maxRows: 200 }).filter((p) => p.kind === 'row');
    expect(rows).toHaveLength(200);
    expect(rows.some((p) => p.kind === 'row' && p.index === m.indexAt(4000))).toBe(true);
  });

  it('names the visible rows', () => {
    const m = model(range(0, 10), () => 100);
    expect(m.visible(150, 300)).toEqual({ first: 1, last: 4 });
    expect(new FeedModel<number>(5).visible(0, 100)).toBeNull();
  });
});
