import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

// A hand-written fake (see AGENTS.md: rejections from vi.fn() are reported
// as unhandled by vitest 4).
const fake = vi.hoisted(() => ({
  calls: [] as [string, unknown][],
  handlers: {} as Record<string, (arg: unknown) => Promise<unknown>>,
}));
vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  const call =
    (name: string) =>
    (...args: unknown[]) => {
      const arg = args.length === 1 ? args[0] : args;
      fake.calls.push([name, arg]);
      const handler = fake.handlers[name];
      return handler ? handler(arg) : Promise.resolve(null);
    };
  return {
    ...actual,
    ipc: new Proxy({}, { get: (_t, name: string) => call(name) }),
  };
});
import { IpcError } from '@/services/ipc';
import {
  MAX_WINDOWS,
  MAX_WINDOW_ITEMS,
  PAGE_LIMIT,
  evictPages,
  useViewer,
  windowItemCount,
  type ChatWindow,
} from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import type { GetThreadPageRequest, SearchMessagesRequest } from '@/types';
import { ARCHIVE_ID, sid, snapshot, threadPage, viewerSession } from './helpers';

const range = (from: number, count: number) => Array.from({ length: count }, (_, i) => from + i);

/** A chat of message ids 1..total; pages of `limit` around anchors. */
function serveChat(total: number): void {
  fake.handlers.getThreadPage = (arg) => {
    const r = arg as GetThreadPageRequest;
    let ids: number[];
    if (r.anchor.kind === 'newest') ids = range(total - r.limit + 1, r.limit);
    else if (r.anchor.kind === 'message' && r.direction === 'older') {
      const end = r.anchor.message - 1;
      ids = range(Math.max(1, end - r.limit + 1), Math.min(r.limit, end));
    } else if (r.anchor.kind === 'message' && r.direction === 'newer') {
      const start = r.anchor.message + 1;
      ids = range(start, Math.min(r.limit, total - start + 1));
    } else ids = [];
    return Promise.resolve(
      threadPage(ids, { hasOlder: (ids[0] ?? 1) > 1, hasNewer: (ids[ids.length - 1] ?? total) < total }),
    );
  };
}

async function openSession() {
  fake.handlers.openViewer = () => Promise.resolve(viewerSession({ viewer: 7 }));
  const store = useViewer();
  await store.open(ARCHIVE_ID);
  return store;
}

beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = {};
  localStorage.clear();
});

describe('evictPages', () => {
  const page = (ids: number[]) => threadPage(ids);
  function win(pages: number[][]): Pick<ChatWindow, 'pages'> {
    return { pages: pages.map((ids) => ({ ...page(ids), hasOlder: false, hasNewer: false })) };
  }

  it('evicts the end furthest from the kept page and marks that edge as having more', () => {
    const w = win([range(1, 10), range(11, 10), range(21, 10), range(31, 10)]);
    expect(evictPages(w, 3, 25)).toBe(2);
    expect(w.pages.map((p) => p.items[0]!.id)).toEqual([21, 31]);
    expect(w.pages[0]!.hasOlder).toBe(true);
    expect(w.pages[0]!.olderAnchor).toBe(21);
  });

  it('evicts from the newer end when the kept page is the oldest', () => {
    const w = win([range(1, 10), range(11, 10), range(21, 10)]);
    evictPages(w, 0, 15);
    expect(w.pages.map((p) => p.items[0]!.id)).toEqual([1]);
    expect(w.pages[0]!.hasNewer).toBe(true);
  });

  it('never evicts the kept page, even when it alone is over the bound', () => {
    const w = win([range(1, 30)]);
    expect(evictPages(w, 0, 10)).toBe(0);
    expect(windowItemCount(w)).toBe(30);
  });
});

describe('viewer store', () => {
  it('opens a session and sends its generation with every request', async () => {
    const store = await openSession();
    serveChat(500);
    await store.openChat(sid(4));
    const req = fake.calls.find(([n]) => n === 'getThreadPage')![1] as GetThreadPageRequest;
    expect(req).toMatchObject({ viewer: 7, chatId: 4, anchor: { kind: 'newest' }, direction: 'older', limit: PAGE_LIMIT });
    expect(req.reveal).toBeUndefined();
    expect(store.openWindow?.pages[0]!.items).toHaveLength(PAGE_LIMIT);
  });

  it('sends reveal.earlierText only when the setting asks', async () => {
    const settings = useSettings();
    const snap = snapshot();
    snap.settings.viewer.revealEarlierText = true;
    settings.snapshot = snap;
    const store = await openSession();
    serveChat(100);
    await store.openChat(sid(4));
    const req = fake.calls.find(([n]) => n === 'getThreadPage')![1] as GetThreadPageRequest;
    expect(req.reveal).toEqual({ earlierText: true });
  });

  it('keeps at most 1,500 items per chat while scrolling, and re-fetches evicted pages by anchor', async () => {
    const store = await openSession();
    serveChat(5000);
    await store.openChat(sid(4));
    for (let i = 0; i < 60; i += 1) {
      await store.loadMore('older');
      expect(windowItemCount(store.openWindow!)).toBeLessThanOrEqual(MAX_WINDOW_ITEMS);
    }
    const w = store.openWindow!;
    expect(windowItemCount(w)).toBe(MAX_WINDOW_ITEMS);
    const newest = w.pages[w.pages.length - 1]!;
    expect(newest.hasNewer).toBe(true);
    const anchor = newest.newerAnchor!;
    // Scrolling back down fetches the evicted rows after the window's edge.
    fake.calls.length = 0;
    await store.loadMore('newer');
    const req = fake.calls.find(([n]) => n === 'getThreadPage')![1] as GetThreadPageRequest;
    expect(req.anchor).toEqual({ kind: 'message', message: anchor });
    expect(req.direction).toBe('newer');
    const items = store.openWindow!.pages.flatMap((p) => p.items.map((m) => m.id as number));
    expect(items).toEqual([...items].sort((a, b) => a - b));
    expect(new Set(items).size).toBe(items.length);
  });

  it('keeps the page in view when evicting', async () => {
    const store = await openSession();
    serveChat(5000);
    await store.openChat(sid(4));
    for (let i = 0; i < 29; i += 1) await store.loadMore('older');
    const w = store.openWindow!;
    expect(windowItemCount(w)).toBe(MAX_WINDOW_ITEMS);
    const inView = w.pages[w.pages.length - 1]!.items[0]!.id;
    await store.loadMore('older', inView);
    expect(store.openWindow!.pages.some((p) => p.items.some((m) => m.id === inView))).toBe(true);
  });

  it('keeps windows for the open chat and the last two visited only', async () => {
    const store = await openSession();
    serveChat(100);
    for (const chat of [1, 2, 3, 4]) await store.openChat(sid(chat));
    expect(store.windows.map((w) => w.chatId)).toEqual([2, 3, 4]);
    expect(store.windows).toHaveLength(MAX_WINDOWS);
    fake.calls.length = 0;
    await store.openChat(sid(3));
    expect(fake.calls).toEqual([]); // shown from its window
    expect(store.windows.map((w) => w.chatId)).toEqual([2, 4, 3]);
  });

  it('re-reads the newest page when the kept window does not reach it', async () => {
    const store = await openSession();
    serveChat(100);
    fake.handlers.getThreadPage = (arg) => {
      const r = arg as GetThreadPageRequest;
      return Promise.resolve(
        r.anchor.kind === 'oldest' ? threadPage(range(1, 50), { hasOlder: false, hasNewer: true }) : threadPage(range(51, 50)),
      );
    };
    await store.openChat(sid(4), { kind: 'oldest' });
    fake.calls.length = 0;
    await store.openChat(sid(4));
    expect(fake.calls.map(([n]) => n)).toEqual(['getThreadPage']);
    expect(store.openWindow!.pages[0]!.items[0]!.id).toBe(51);
  });

  it('reads the undated section forwards from its start', async () => {
    const store = await openSession();
    serveChat(100);
    await store.openChat(sid(4), { kind: 'undated' });
    const req = fake.calls.find(([n]) => n === 'getThreadPage')![1] as GetThreadPageRequest;
    expect(req).toMatchObject({ anchor: { kind: 'undated' }, direction: 'newer' });
  });

  it('loads conversation rows 200 at a time, only the missing ones', async () => {
    const store = await openSession();
    fake.handlers.getConversations = (arg) => {
      const [, ids] = arg as [number, number[]];
      return Promise.resolve({
        rows: ids.map((id) => ({ id: sid(id) })),
        skippedRows: 0,
      });
    };
    await store.loadRows(range(1, 450).map(sid));
    const sizes = fake.calls.filter(([n]) => n === 'getConversations').map(([, a]) => (a as [number, number[]])[1].length);
    expect(sizes).toEqual([200, 200, 50]);
    fake.calls.length = 0;
    await store.loadRows(range(440, 20).map(sid));
    expect(fake.calls.map(([, a]) => (a as [number, number[]])[1])).toEqual([range(451, 9)]);
  });

  it('forgets everything when the session is gone (viewerClosed)', async () => {
    const store = await openSession();
    serveChat(100);
    await store.openChat(sid(4));
    fake.handlers.getThreadPage = () =>
      Promise.reject(new IpcError('get_thread_page', 'viewerClosed', 'closed'));
    await store.loadMore('older');
    expect(store.session).toBeNull();
    expect(store.windows).toEqual([]);
  });

  it('closes the session and clears the store', async () => {
    const store = await openSession();
    serveChat(100);
    await store.openChat(sid(4));
    await store.close();
    expect(fake.calls.at(-1)).toEqual(['closeViewer', 7]);
    expect(store.session).toBeNull();
    expect(store.windows).toEqual([]);
    expect(store.inspections.size).toBe(0);
  });

  it('drops a superseded search silently and keeps the newer one', async () => {
    const store = await openSession();
    let release: (v: unknown) => void = () => undefined;
    fake.handlers.searchMessages = (arg) => {
      const [req] = arg as [SearchMessagesRequest];
      if (req.query === 'first') {
        return new Promise((_, reject) => {
          release = () => reject(new IpcError('search_messages', 'searchSuperseded', 'superseded'));
        });
      }
      return Promise.resolve({ searchId: 2, total: 1, dated: 1, capped: false, newestKept: true, mode: 'words', plan: 'global', coverage: null });
    };
    fake.handlers.getSearchResults = () => Promise.resolve({ hits: [{ message: sid(9) }], skippedRows: 0 });
    const base = { mode: 'auto' as const, filters: {}, includeEarlier: false, includeEvents: false };
    const first = store.startSearch({ ...base, query: 'first' });
    await store.startSearch({ ...base, query: 'second' });
    release(null);
    await first;
    expect(store.search.error).toBeNull();
    expect(store.search.searchId).toBe(2);
    expect(store.search.hits).toHaveLength(1);
    expect(store.search.running).toBe(false);
  });

  it('caches inspections and never writes to browser storage', async () => {
    const store = await openSession();
    fake.handlers.inspectAttachment = () => Promise.resolve({ playback: null });
    await store.inspect(sid(3));
    await store.inspect(sid(3));
    expect(fake.calls.filter(([n]) => n === 'inspectAttachment')).toHaveLength(1);
    await store.inspect(sid(3), true);
    expect(fake.calls.filter(([n]) => n === 'inspectAttachment')).toHaveLength(2);
    expect(localStorage.length).toBe(0);
    expect(sessionStorage.length).toBe(0);
  });
});
