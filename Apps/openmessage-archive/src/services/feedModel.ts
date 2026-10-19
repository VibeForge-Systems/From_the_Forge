// The geometry of `components/viewer/VirtualFeed.vue` (M5 §8.4, slice 5h0).
//
// Spike 5s(1) found that virtua 0.53.1 did not keep the first visible item
// in place when items were prepended with the scroller at its very top on
// WebKitGTK 2.52.6, which has no native scroll anchoring. This in-house
// model replaces it. It is written from first principles, not from any
// library's code:
//
// - every row has a height: measured (from a ResizeObserver) or, until
//   then, an estimate;
// - row tops are prefix sums of those heights, so the rows that are not
//   rendered are represented by gaps of the right size;
// - the feed keeps an **anchor**: one row's key and where that row's top
//   (or bottom) sits relative to the top of the viewport. Whenever rows are
//   added or removed at either end, or a height changes, the scroll
//   position is recomputed from the anchor, so the anchored row does not
//   move on screen. Prepends at scrollTop 0 are no different from any
//   other: the scroll position grows by exactly the height added above.
//
// It holds keys and numbers only, never content.

export type FeedKey = string | number;

/**
 * Where the viewport is pinned: the scroll position is the anchored row's
 * top (or bottom) edge minus `offset`. A row scrolled partly above the
 * viewport has a negative offset.
 */
export interface FeedAnchor<K extends FeedKey> {
  key: K;
  edge: 'top' | 'bottom';
  offset: number;
}

export type FeedAlign = 'start' | 'end' | 'center' | 'nearest';

/** One rendered piece of the feed: a gap standing for unrendered rows, or a row. */
export type FeedPiece<K extends FeedKey> =
  | { kind: 'gap'; id: string; height: number }
  | { kind: 'row'; id: string; index: number; key: K };

export interface FeedWindowOptions<K extends FeedKey> {
  /** Extra pixels rendered above and below the viewport. */
  overscan: number;
  /** Rows that stay rendered wherever they are (focus, active option, playing medium). */
  keep: Iterable<K>;
  /** The most rows rendered for the viewport (kept rows come on top). */
  maxRows: number;
}

/** Heights within half a pixel are the same height (sub-pixel layout noise). */
const SAME_HEIGHT = 0.5;

export class FeedModel<K extends FeedKey> {
  private keys: K[] = [];
  private positions = new Map<K, number>();
  private heights = new Map<K, number>();
  /** `starts[i]` is the top of row i; `starts[n]` is the total height. */
  private starts = new Float64Array(1);
  private dirty = false;

  constructor(private estimate: number) {
    if (!(estimate > 0)) throw new RangeError('estimate must be positive');
  }

  get size(): number {
    return this.keys.length;
  }

  /**
   * Replace the rows. Measured heights are kept for keys that remain; the
   * heights of keys that left are forgotten (an evicted page is measured
   * again when it comes back). Duplicate keys keep their first position.
   */
  setKeys(keys: readonly K[]): void {
    this.keys = keys.slice();
    this.positions = new Map();
    this.keys.forEach((k, i) => {
      if (!this.positions.has(k)) this.positions.set(k, i);
    });
    for (const k of [...this.heights.keys()]) {
      if (!this.positions.has(k)) this.heights.delete(k);
    }
    this.dirty = true;
  }

  keyAt(index: number): K | undefined {
    return this.keys[index];
  }

  indexOf(key: K): number {
    return this.positions.get(key) ?? -1;
  }

  isMeasured(key: K): boolean {
    return this.heights.has(key);
  }

  /** Record a measured height. Returns whether the geometry changed. */
  setHeight(key: K, height: number): boolean {
    if (!this.positions.has(key) || !Number.isFinite(height) || height < 0) return false;
    const old = this.heights.get(key);
    if (old !== undefined && Math.abs(old - height) < SAME_HEIGHT) return false;
    this.heights.set(key, height);
    this.dirty = true;
    return true;
  }

  heightAt(index: number): number {
    const key = this.keys[index];
    if (key === undefined) return 0;
    return this.heights.get(key) ?? this.estimate;
  }

  private layout(): Float64Array {
    if (this.dirty || this.starts.length !== this.keys.length + 1) {
      const starts = new Float64Array(this.keys.length + 1);
      for (let i = 0; i < this.keys.length; i += 1) starts[i + 1] = starts[i]! + this.heightAt(i);
      this.starts = starts;
      this.dirty = false;
    }
    return this.starts;
  }

  top(index: number): number {
    const s = this.layout();
    return s[Math.min(Math.max(index, 0), this.keys.length)]!;
  }

  bottom(index: number): number {
    return this.top(index) + this.heightAt(index);
  }

  total(): number {
    return this.top(this.keys.length);
  }

  /** The largest scroll position for a viewport of `viewport` pixels. */
  maxScroll(viewport: number): number {
    return Math.max(0, this.total() - viewport);
  }

  /** The row covering `offset` (clamped to the first and last rows), or -1 when empty. */
  indexAt(offset: number): number {
    const n = this.keys.length;
    if (n === 0) return -1;
    const s = this.layout();
    if (offset <= 0) return 0;
    if (offset >= s[n]!) return n - 1;
    let lo = 0;
    let hi = n - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (s[mid]! <= offset) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }

  /** The anchor that describes a scroll position: the row at its top edge. */
  anchorAt(scrollTop: number): FeedAnchor<K> | null {
    const i = this.indexAt(scrollTop);
    if (i < 0) return null;
    return { key: this.keys[i]!, edge: 'top', offset: this.top(i) - scrollTop };
  }

  /** The anchor that shows the row `key` aligned in a viewport of `viewport` pixels. */
  alignAnchor(key: K, align: Exclude<FeedAlign, 'nearest'>, viewport: number): FeedAnchor<K> {
    if (align === 'end') return { key, edge: 'bottom', offset: -viewport };
    if (align === 'center') {
      const i = this.indexOf(key);
      const h = i < 0 ? 0 : this.heightAt(i);
      return { key, edge: 'top', offset: Math.max(0, (viewport - h) / 2) };
    }
    return { key, edge: 'top', offset: 0 };
  }

  /**
   * The scroll position that keeps `anchor`, clamped to the scrollable
   * range, or null when the anchored row is gone.
   */
  scrollTopFor(anchor: FeedAnchor<K>, viewport: number): number | null {
    const i = this.indexOf(anchor.key);
    if (i < 0) return null;
    const edge = anchor.edge === 'top' ? this.top(i) : this.bottom(i);
    return Math.min(Math.max(edge - anchor.offset, 0), this.maxScroll(viewport));
  }

  /** The first and last rows intersecting the viewport, or null when empty. */
  visible(scrollTop: number, viewport: number): { first: number; last: number } | null {
    if (this.keys.length === 0) return null;
    const first = this.indexAt(scrollTop);
    const last = Math.max(first, this.indexAt(scrollTop + Math.max(viewport, 1) - 0.5));
    return { first, last };
  }

  /**
   * What to render: the rows around the viewport (plus `overscan`), at most
   * `maxRows` of them, every kept row, and gaps standing for the rest, in
   * order. Gap heights come from the same prefix sums as the scroll
   * position, so a rendered row sits exactly where the model says.
   */
  window(scrollTop: number, viewport: number, options: FeedWindowOptions<K>): FeedPiece<K>[] {
    const n = this.keys.length;
    if (n === 0) return [];
    const top = Math.max(0, scrollTop);
    let start = this.indexAt(top - options.overscan);
    let end = this.indexAt(top + Math.max(viewport, 0) + options.overscan);
    const maxRows = Math.max(1, Math.floor(options.maxRows));
    if (end - start + 1 > maxRows) {
      const first = this.indexAt(top);
      start = Math.max(start, first - Math.floor(maxRows / 4));
      end = Math.min(end, start + maxRows - 1);
    }
    const rows = new Set<number>();
    for (let i = start; i <= end; i += 1) rows.add(i);
    for (const k of options.keep) {
      const i = this.indexOf(k);
      if (i >= 0) rows.add(i);
    }
    const ordered = [...rows].sort((a, b) => a - b);
    const pieces: FeedPiece<K>[] = [];
    let cursor = 0;
    let gaps = 0;
    for (const i of ordered) {
      const at = this.top(i);
      if (at - cursor > 0) pieces.push({ kind: 'gap', id: `g${(gaps += 1)}`, height: at - cursor });
      const key = this.keys[i]!;
      pieces.push({ kind: 'row', id: `r${typeof key}:${String(key)}`, index: i, key });
      cursor = this.bottom(i);
    }
    const total = this.total();
    if (total - cursor > 0) pieces.push({ kind: 'gap', id: `g${(gaps += 1)}`, height: total - cursor });
    return pieces;
  }
}

/** What `VirtualFeed` reports as visible: the first and last rows in the viewport. */
export interface FeedRange<K extends FeedKey> {
  first: K;
  last: K;
  firstIndex: number;
  lastIndex: number;
}

/** The methods `VirtualFeed` exposes to its parent through a template ref. */
export interface FeedHandle<K extends FeedKey> {
  scroller: HTMLElement | null;
  scrollToIndex(index: number, align?: FeedAlign): void;
  scrollToKey(key: K, align?: FeedAlign): void;
  scrollToStart(): void;
  scrollToEnd(): void;
  firstVisibleKey(): K | null;
  isMounted(key: K): boolean;
}
