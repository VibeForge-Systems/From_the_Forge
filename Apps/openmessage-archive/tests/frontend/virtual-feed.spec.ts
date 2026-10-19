// VirtualFeed (M5 §8.4, §13.8; the in-house feed of slice 5h0). jsdom does
// no layout, so the test supplies it: a hand-written ResizeObserver stub
// reports each row's height from a table, and the scroller's clientHeight
// is stubbed. A row's position on screen is then computed from the DOM the
// feed rendered (gap heights from their styles, row heights from the
// table) minus the scroller's scrollTop, which is exactly what a browser
// would paint.
import { readFileSync } from 'node:fs';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, type VueWrapper } from '@vue/test-utils';
import { h, nextTick } from 'vue';
import VirtualFeed from '@/components/viewer/VirtualFeed.vue';

interface Row {
  id: number;
}

const VIEWPORT = 500;
const range = (from: number, count: number): Row[] => Array.from({ length: count }, (_, i) => ({ id: from + i }));
/** Mixed heights, 30–119 px. */
const mixed = (id: number) => 30 + ((id * 37) % 90);

class FakeResizeObserver {
  static all: FakeResizeObserver[] = [];
  observed = new Set<Element>();
  constructor(private readonly callback: ResizeObserverCallback) {
    FakeResizeObserver.all.push(this);
  }
  observe(el: Element): void {
    this.observed.add(el);
  }
  unobserve(el: Element): void {
    this.observed.delete(el);
  }
  disconnect(): void {
    this.observed.clear();
  }
  deliver(): void {
    const entries = [...this.observed].map((target) => {
      const blockSize = heightOf(target);
      return {
        target,
        borderBoxSize: [{ blockSize, inlineSize: 300 }],
        contentBoxSize: [{ blockSize, inlineSize: 300 }],
        devicePixelContentBoxSize: [],
        contentRect: { height: blockSize } as DOMRectReadOnly,
      } as unknown as ResizeObserverEntry;
    });
    if (entries.length) this.callback(entries, this as unknown as ResizeObserver);
  }
}

let heights: (id: number) => number = mixed;

/** The height a browser would give an element the feed rendered. */
function heightOf(el: Element): number {
  if (el.classList.contains('oma-feed')) return VIEWPORT;
  const article = el.querySelector('article');
  return article ? heights(Number(article.id.slice(1))) : 0;
}

const realRO = globalThis.ResizeObserver;
const clientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'clientHeight');

beforeEach(() => {
  FakeResizeObserver.all = [];
  heights = mixed;
  globalThis.ResizeObserver = FakeResizeObserver as unknown as typeof ResizeObserver;
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return this.classList.contains('oma-feed') ? VIEWPORT : 0;
    },
  });
});

afterEach(() => {
  globalThis.ResizeObserver = realRO;
  if (clientHeight) Object.defineProperty(HTMLElement.prototype, 'clientHeight', clientHeight);
  document.body.innerHTML = '';
});

async function settle(): Promise<void> {
  for (let i = 0; i < 4; i += 1) {
    for (const ro of FakeResizeObserver.all) ro.deliver();
    await nextTick();
    await nextTick();
  }
}

type FeedProps = {
  items: Row[];
  keepMounted?: number[];
};

function mountFeed(props: FeedProps) {
  return mount(VirtualFeed, {
    attachTo: document.body,
    props: { itemKey: (r: unknown) => (r as Row).id, estimate: 50, ...props },
    slots: {
      default: ({ item }: { item: unknown }) => {
        const row = item as Row;
        return h('article', { id: `m${row.id}`, tabindex: 0 }, String(row.id));
      },
    },
  });
}

function scroller(w: VueWrapper): HTMLElement {
  return w.element as HTMLElement;
}

/** Where the row `id` is painted relative to the top of the viewport, or null when unmounted. */
function screenTop(w: VueWrapper, id: number): number | null {
  const el = scroller(w);
  let y = 0;
  for (const child of Array.from(el.children) as HTMLElement[]) {
    if (child.classList.contains('oma-feed-gap')) {
      y += parseFloat(child.style.height);
      continue;
    }
    const article = child.querySelector('article')!;
    if (article.id === `m${id}`) return y - el.scrollTop;
    y += heightOf(child);
  }
  return null;
}

function rendered(w: VueWrapper): number[] {
  return w.findAll('article').map((a) => Number(a.attributes('id')!.slice(1)));
}

async function scrollTo(w: VueWrapper, top: number): Promise<void> {
  scroller(w).scrollTop = top;
  scroller(w).dispatchEvent(new Event('scroll'));
  await settle();
}

describe('VirtualFeed', () => {
  it('keeps the first visible item in place when 50 mixed-height items are prepended at scrollTop 0', async () => {
    const w = mountFeed({ items: range(1000, 200) });
    await settle();
    expect(scroller(w).scrollTop).toBe(0);
    expect(screenTop(w, 1000)).toBe(0);
    for (let batch = 0; batch < 4; batch += 1) {
      const first = 1000 - 50 * batch;
      await w.setProps({ items: [...range(first - 50, 50), ...(w.props('items') as Row[])] });
      await settle();
      // The item that was at the top is still exactly at the top.
      expect(screenTop(w, 1000)).toBe(0);
    }
    // The scroll position grew by the height of everything added above.
    expect(scroller(w).scrollTop).toBeGreaterThan(200 * 30);
  });

  it('keeps a row read in the middle in place across a prepend', async () => {
    const w = mountFeed({ items: range(1000, 300) });
    await settle();
    await scrollTo(w, 4321);
    const id = rendered(w).find((r) => (screenTop(w, r) ?? -1) >= 0)!;
    const before = screenTop(w, id);
    await w.setProps({ items: [...range(950, 50), ...range(1000, 300)] });
    await settle();
    expect(screenTop(w, id)).toBe(before);
  });

  it('keeps the view when items far above are evicted', async () => {
    const w = mountFeed({ items: range(1, 400) });
    await settle();
    await scrollTo(w, 15_000);
    const id = rendered(w).find((r) => (screenTop(w, r) ?? -1) >= 0)!;
    const before = screenTop(w, id);
    await w.setProps({ items: range(101, 300) });
    await settle();
    expect(screenTop(w, id)).toBe(before);
  });

  it('renders only a window of rows', async () => {
    const w = mountFeed({ items: range(1, 2000) });
    await settle();
    await scrollTo(w, 60_000);
    const rows = rendered(w);
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThanOrEqual(200);
    expect(w.findAll('.oma-feed-row')).toHaveLength(rows.length);
  });

  it('keeps the focused item mounted and focused while scrolled far away', async () => {
    const w = mountFeed({ items: range(1, 500) });
    await settle();
    const article = w.find('#m3').element as HTMLElement;
    article.focus();
    await w.setProps({ keepMounted: [3] });
    await scrollTo(w, 20_000);
    expect(article.isConnected).toBe(true);
    expect(document.activeElement).toBe(article);
    // A prepend keeps the same node.
    await w.setProps({ items: [...range(-49, 50), ...range(1, 500)], keepMounted: [3] });
    await settle();
    expect(w.find('#m3').element).toBe(article);
    // Without keepMounted it would be unmounted.
    await w.setProps({ keepMounted: [] });
    await settle();
    expect(article.isConnected).toBe(false);
  });

  it('aligns a row to the end and keeps it there when it grows', async () => {
    const w = mountFeed({ items: range(1, 100) });
    await settle();
    (w.vm as unknown as { scrollToEnd(): void }).scrollToEnd();
    await settle();
    expect(screenTop(w, 100)! + mixed(100)).toBe(VIEWPORT);
    heights = (id) => (id === 100 ? 300 : mixed(id));
    await settle();
    expect(screenTop(w, 100)! + 300).toBe(VIEWPORT);
  });

  it('reports the visible range and the ends it comes near', async () => {
    const w = mountFeed({ items: range(1, 300) });
    await settle();
    expect(w.emitted('nearStart')).toBeTruthy();
    expect(w.emitted('nearEnd')).toBeFalsy();
    const visible = w.emitted('visible')!.at(-1)![0] as { first: number; firstIndex: number };
    expect(visible).toMatchObject({ first: 1, firstIndex: 0 });
    (w.vm as unknown as { scrollToEnd(): void }).scrollToEnd();
    await settle();
    expect(w.emitted('nearEnd')).toBeTruthy();
  });

  it('turns native scroll anchoring off on its scroller', () => {
    const w = mountFeed({ items: range(1, 3) });
    expect(scroller(w).classList.contains('oma-feed')).toBe(true);
    const css = readFileSync('src/assets/styles/viewer.css', 'utf8');
    const rule = /\.oma-feed\s*\{([^}]*)\}/.exec(css)?.[1] ?? '';
    expect(rule).toMatch(/overflow-anchor:\s*none/);
    expect(/\.oma-feed-row\s*\{[^}]*display:\s*flow-root/.test(css)).toBe(true);
  });

  it('gives media placeholders a fixed size, so a late image cannot reflow the feed', () => {
    const css = readFileSync('src/assets/styles/viewer.css', 'utf8');
    const rule = /\.oma-media-box\s*\{([^}]*)\}/.exec(css)?.[1] ?? '';
    expect(rule).toMatch(/width:\s*320px/);
    expect(rule).toMatch(/aspect-ratio:\s*4\s*\/\s*3/);
    expect(rule).toMatch(/object-fit:\s*contain/);
  });
});
