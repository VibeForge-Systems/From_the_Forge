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
import type {
  InspectAttachmentRequest,
  GetMessageDetailRequest,
  GetMessageTextRequest,
  GetThreadPageRequest,
  MessageDetailDto,
  SearchMessagesRequest,
  TextChunkDto,
} from '@/types';
import {
  ARCHIVE_ID,
  attachment,
  inspection,
  message,
  messageDetail,
  sid,
  snapshot,
  textPiece,
  threadPage,
  viewerSession,
} from './helpers';

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

/** A detail reply whose message carries the request's reveal as its earlier text. */
function serveDetail(): void {
  fake.handlers.getMessageDetail = (arg) => {
    const r = arg as GetMessageDetailRequest;
    const m = message(r.messageId, {
      edit: {
        label: 'unsentParts',
        editedParts: [],
        unsentParts: [],
        partCount: null,
        unsentWhole: false,
        storedVersions: 2,
        earlier: r.reveal?.earlierText
          ? { kind: 'revealed', reason: 'unsent', pieces: [textPiece('before')], session: sid(1) }
          : { kind: 'withheld', reason: 'unsent' },
      },
    });
    return Promise.resolve({ message: m } as unknown as MessageDetailDto);
  };
}

describe('viewer store: reveals and full text (5h2)', () => {
  it('reveals one message on request only, as an overlay, and hides it again', async () => {
    const store = await openSession();
    serveChat(100);
    serveDetail();
    await store.openChat(sid(4));
    const item = store.openWindow!.pages[0]!.items.at(-1)!;
    expect(fake.calls.some(([n]) => n === 'getMessageDetail')).toBe(false);
    expect(store.shown(item)).toBe(item);

    expect(await store.revealMessage(item.id, { earlierText: true })).toBeNull();
    const [, req] = fake.calls.find(([n]) => n === 'getMessageDetail')!;
    expect(req).toEqual({ viewer: 7, messageId: item.id, reveal: { earlierText: true } });
    expect(store.shown(item).edit.earlier.kind).toBe('revealed');
    // The page's own copy is unchanged.
    expect(item.edit.earlier.kind).toBe('none');

    // A second reveal keeps the first.
    await store.revealMessage(item.id, { oneTimeCode: true });
    const last = fake.calls.filter(([n]) => n === 'getMessageDetail').at(-1)![1];
    expect(last).toMatchObject({ reveal: { earlierText: true, oneTimeCode: true } });

    // Hiding the earlier text re-reads with the code reveal only.
    await store.unreveal(item.id, { earlierText: true });
    const again = fake.calls.filter(([n]) => n === 'getMessageDetail').at(-1)![1];
    expect(again).toMatchObject({ reveal: { oneTimeCode: true } });
    expect((again as GetMessageDetailRequest).reveal?.earlierText).toBeUndefined();
    expect(store.shown(item).edit.earlier.kind).toBe('withheld');

    await store.unreveal(item.id, { oneTimeCode: true });
    expect(store.shown(item)).toBe(item);
    expect(store.revealed.size).toBe(0);
  });

  it('returns the error sentence when a reveal fails, and keeps nothing', async () => {
    const store = await openSession();
    fake.handlers.getMessageDetail = () =>
      Promise.reject(new IpcError('get_message_detail', 'messageNotFound', 'gone'));
    const error = await store.revealMessage(sid(3), { earlierText: true });
    expect(error).toBe('That message is no longer in the archive.');
    expect(store.revealed.size).toBe(0);
  });

  it('reads a truncated body chunk by chunk, masked unless the code was revealed', async () => {
    const store = await openSession();
    const full = 70_000;
    fake.handlers.getMessageText = (arg) => {
      const r = arg as GetMessageTextRequest;
      const end = Math.min(full, r.fromUtf16 + 32_768);
      const chunk: TextChunkDto = {
        body: r.bodyId,
        fromUtf16: r.fromUtf16,
        endUtf16: end,
        fullUtf16: full,
        text: { kind: 'text', pieces: [textPiece(`chunk from ${r.fromUtf16}`)] },
      };
      return Promise.resolve(chunk);
    };
    await store.loadFullText(sid(9), sid(3));
    await store.loadFullText(sid(9), sid(3));
    await store.loadFullText(sid(9), sid(3));
    await store.loadFullText(sid(9), sid(3));
    const reqs = fake.calls.filter(([n]) => n === 'getMessageText').map(([, a]) => a as GetMessageTextRequest);
    expect(reqs.map((r) => r.fromUtf16)).toEqual([0, 32_768, 65_536]);
    expect(reqs.every((r) => r.reveal === undefined)).toBe(true);
    const text = store.fullTexts.get(sid(9))!;
    expect(text.end).toBe(full);
    expect(text.pieces).toHaveLength(3);
  });

  it('stops at withheld text and forgets reveals and texts on close', async () => {
    const store = await openSession();
    serveDetail();
    fake.handlers.getMessageText = () =>
      Promise.resolve({ body: sid(9), fromUtf16: 0, endUtf16: 0, fullUtf16: 10, text: { kind: 'withheld', reason: 'unsent' } });
    await store.loadFullText(sid(9), sid(3));
    await store.loadFullText(sid(9), sid(3));
    expect(fake.calls.filter(([n]) => n === 'getMessageText')).toHaveLength(1);
    expect(store.fullTexts.get(sid(9))!.withheld).toBe('unsent');
    await store.revealMessage(sid(3), { earlierText: true });
    store.focusOnOpen(sid(5), sid(3));
    await store.close();
    expect(store.revealed.size).toBe(0);
    expect(store.fullTexts.size).toBe(0);
    expect(store.pendingFocus).toBeNull();
  });

  it('hands a pending focus to the chat it names, once', async () => {
    const store = await openSession();
    store.focusOnOpen(sid(5), sid(30));
    expect(store.takePendingFocus(sid(6))).toBeNull();
    expect(store.takePendingFocus(sid(5))).toBe(30);
    expect(store.takePendingFocus(sid(5))).toBeNull();
  });
});

describe('viewer store: attachments, playback and the panel (5h3)', () => {
  function playable(id: number, forPlayback: boolean) {
    return inspection(id, {
      content: { kind: 'sniffed', sniffed: 'mp4', class: { kind: 'playable', structureChecked: true }, storedBytes: 9 },
      playback: forPlayback ? { kind: 'allowed', structureChecked: true } : null,
    });
  }

  it('keeps a playback verdict when a plain inspection is asked again', async () => {
    const store = await openSession();
    fake.handlers.inspectAttachment = (arg) => {
      const r = arg as InspectAttachmentRequest;
      return Promise.resolve(playable(r.attachmentId, r.forPlayback));
    };
    await store.inspect(sid(3), true);
    await store.inspect(sid(3), false, true);
    expect(store.inspections.get(sid(3))?.playback).toEqual({ kind: 'allowed', structureChecked: true });
  });

  it('asks for confirmation once per file per session, and again after the session closes', async () => {
    useSettings().snapshot = snapshot();
    const store = await openSession();
    fake.handlers.inspectAttachment = (arg) => {
      const r = arg as InspectAttachmentRequest;
      return Promise.resolve(playable(r.attachmentId, r.forPlayback));
    };
    await store.requestPlay(attachment(3));
    expect(store.pendingPlay?.id).toBe(3);
    await store.answerPlay('play');
    expect(store.media).toEqual({ attachment: attachment(3), mode: 'play' });
    store.closeMedia();
    await store.requestPlay(attachment(3));
    expect(store.pendingPlay).toBeNull();
    expect(store.media?.mode).toBe('play');

    store.openDetails(sid(3));
    expect(store.panel).toEqual({ kind: 'details', message: 3, reactions: false });
    await store.close();
    expect(store.panel).toBeNull();
    expect(store.media).toBeNull();
    await store.open(ARCHIVE_ID);
    await store.requestPlay(attachment(3));
    expect(store.pendingPlay?.id).toBe(3);
  });

  it('repeats only the reveals the user asked for when reading details', async () => {
    const store = await openSession();
    fake.handlers.getMessageDetail = () => Promise.resolve(messageDetail(message(5)));
    await store.loadDetail(sid(5));
    expect(fake.calls.at(-1)?.[1]).toEqual({ viewer: 7, messageId: 5 });
    await store.revealMessage(sid(5), { oneTimeCode: true });
    await store.loadDetail(sid(5), null);
    expect(fake.calls.at(-1)?.[1]).toEqual({ viewer: 7, messageId: 5, reveal: { oneTimeCode: true }, reactionPage: {} });
  });
});

describe('viewer store: search and dates (5i)', () => {
  it('opens the search panel scoped or global, and asks the panel for focus each time', async () => {
    const store = await openSession();
    store.openSearch(sid(4));
    expect(store.panel).toEqual({ kind: 'search' });
    expect(store.searchForm.chatId).toBe(4);
    const focus = store.searchFocus;
    store.openSearch(null);
    expect(store.searchForm.chatId).toBeNull();
    expect(store.searchFocus).toBe(focus + 1);
  });

  it('opens the search panel once the requested archive’s session opens', async () => {
    const store = useViewer();
    store.searchOnOpen(ARCHIVE_ID);
    expect(store.panel).toBeNull();
    fake.handlers.openViewer = () => Promise.resolve(viewerSession({ viewer: 7 }));
    await store.open(ARCHIVE_ID);
    expect(store.panel).toEqual({ kind: 'search' });
    // Only once.
    await store.close();
    await store.open(ARCHIVE_ID);
    expect(store.panel).toBeNull();
  });

  it('keeps the search form until the viewer closes, and persists none of it', async () => {
    const store = await openSession();
    store.searchForm.query = 'synthetic words';
    store.searchForm.from = '2026-01-01';
    store.dateJumpOpen = true;
    await store.close();
    expect(store.searchForm.query).toBe('');
    expect(store.searchForm.from).toBe('');
    expect(store.dateJumpOpen).toBe(false);
    expect(localStorage.length).toBe(0);
  });

  it('cancels a scan still running by the id its progress names', async () => {
    const store = await openSession();
    let progress: ((p: { searchId: number; examined: number; matched: number }) => void) | null = null;
    fake.handlers.searchMessages = (arg) => {
      progress = (arg as [SearchMessagesRequest, typeof progress])[1];
      return new Promise(() => {});
    };
    void store.startSearch({ query: 'abc', mode: 'exact', filters: {}, includeEarlier: false, includeEvents: false });
    await Promise.resolve();
    progress!({ searchId: 12, examined: 5000, matched: 0 });
    expect(store.search.progress?.searchId).toBe(12);
    await store.cancelSearch();
    expect(fake.calls.find(([n]) => n === 'cancelSearch')?.[1]).toEqual({ viewer: 7, searchId: 12 });
    expect(store.search.running).toBe(false);
  });

  it('keeps the error code of a failed search, for its own sentence', async () => {
    const store = await openSession();
    fake.handlers.searchMessages = () => Promise.reject(new IpcError('search_messages', 'queryTooBroad', 'Too broad.'));
    await store.startSearch({ query: 'abc', mode: 'auto', filters: {}, includeEarlier: false, includeEvents: false });
    expect(store.search.errorCode).toBe('queryTooBroad');
    expect(store.search.error).toBeTruthy();
  });

  it('counts days and seeks dates for the open session, returning errors as sentences', async () => {
    const store = await openSession();
    fake.handlers.getDayCounts = () => Promise.resolve({ counts: [2, 0] });
    fake.handlers.seekDate = () => Promise.resolve({ messageId: 33 });
    expect(await store.dayCounts(sid(4), [0, 1000, 2000])).toEqual({ counts: [2, 0] });
    expect(fake.calls.at(-1)).toEqual(['getDayCounts', { viewer: 7, chatId: 4, boundsUtcMs: [0, 1000, 2000] }]);
    expect(await store.seekDate(sid(4), 1000, 'before')).toEqual({ id: 33 });
    expect(fake.calls.at(-1)).toEqual(['seekDate', { viewer: 7, chatId: 4, utcMs: 1000, dir: 'before' }]);
    fake.handlers.seekDate = () => Promise.reject(new IpcError('seek_date', 'anchorNotFound', 'Gone.'));
    expect(await store.seekDate(sid(4), 1000, 'onOrAfter')).toEqual({ error: 'That position is no longer in the conversation.' });
  });

  it('passes the first and newest requests to the thread, each one seen', async () => {
    const store = await openSession();
    store.requestThread('first');
    const a = store.threadCommand;
    store.requestThread('first');
    expect(store.threadCommand?.kind).toBe('first');
    expect(store.threadCommand?.n).not.toBe(a?.n);
  });
});
