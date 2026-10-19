// The thread (M5 §8.4, §10, §13.8): the APG feed of articles over the
// store's window, opening at the newest dated item, paging at the edges,
// the undated section, notices, day separators and the retry row.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils';
import type { DefineComponent } from 'vue';

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
  return { ...actual, ipc: new Proxy({}, { get: (_t, name: string) => call(name) }) };
});
import { IpcError } from '@/services/ipc';
import ThreadView from '@/components/viewer/ThreadView.vue';
import VirtualFeed from '@/components/viewer/VirtualFeed.vue';
import DateJumpDialog from '@/components/viewer/DateJumpDialog.vue';
import { useViewer } from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import { dayKey } from '@/services/time';
import type { GetThreadPageRequest, MessageDto, ThreadPageDto } from '@/types';
import {
  ARCHIVE_ID,
  content,
  conversationRow,
  message,
  sid,
  snapshot,
  threadPage,
  viewerSession,
} from './helpers';

const CHAT = 4;
const range = (from: number, count: number) => Array.from({ length: count }, (_, i) => from + i);

/** A chat of message ids 1..total, one per hour from 2026-01-01T00:00Z. */
function textMessage(id: number): MessageDto {
  return message(id, {
    date: { ms: Date.UTC(2026, 0, 1) + id * 3_600_000, raw: null, unit: 'nanoseconds', state: null, before2007: false },
    body: {
      kind: 'text',
      body: sid(id),
      segments: [
        {
          piece: { kind: 'text', value: content(`hello ${id} <img src=x onerror=alert(1)>`) },
          bold: false,
          italic: false,
          underline: false,
          strikethrough: false,
          mention: false,
          transcription: false,
          oneTimeCode: false,
          url: false,
          urlHidden: false,
        },
      ],
      formatting: { kind: 'plain' },
      source: 'text',
      fullUtf16: 10,
      truncated: false,
      bidiControls: false,
      invisible: false,
      lossy: false,
      storedAsBlob: false,
      sourcesDiffer: null,
    },
  });
}

function page(ids: number[], total: number, overrides: Partial<ThreadPageDto> = {}): ThreadPageDto {
  return threadPage(ids, {
    items: ids.map(textMessage),
    hasOlder: (ids[0] ?? 1) > 1,
    hasNewer: (ids[ids.length - 1] ?? total) < total,
    ...overrides,
  });
}

function serveChat(total: number, overrides: Partial<ThreadPageDto> = {}): void {
  fake.handlers.getThreadPage = (arg) => {
    const r = arg as GetThreadPageRequest;
    let ids: number[];
    if (r.anchor.kind === 'newest') ids = range(Math.max(1, total - r.limit + 1), Math.min(r.limit, total));
    else if (r.anchor.kind === 'oldest') ids = range(1, Math.min(r.limit, total));
    else if (r.anchor.kind === 'undated') return Promise.resolve(page([900, 901], 0, { range: 'undated', hasOlder: false }));
    else if (r.direction === 'older') {
      const end = r.anchor.message - 1;
      ids = range(Math.max(1, end - r.limit + 1), Math.min(r.limit, end));
    } else {
      const start = r.anchor.message + 1;
      ids = range(start, Math.min(r.limit, total - start + 1));
    }
    return Promise.resolve(page(ids, total, overrides));
  };
}

async function mountThread() {
  const store = useViewer();
  await store.open(ARCHIVE_ID);
  const w = mount(ThreadView, { attachTo: document.body, props: { chatId: sid(CHAT) } });
  await flushPromises();
  return w;
}

beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = {
    openViewer: () => Promise.resolve(viewerSession({ viewer: 5 })),
    getConversations: (arg) => {
      const [, wanted] = arg as [number, number[]];
      return Promise.resolve({
        rows: wanted.map((id) => conversationRow(id, { undated: { n: 2, capped: false } })),
        skippedRows: 0,
      });
    },
  };
  useSettings().snapshot = snapshot();
  serveChat(200);
});

afterEach(() => {
  document.body.innerHTML = '';
});

/** What the feed emits when the viewport comes near its top. */
function nearStart(w: VueWrapper): void {
  w.findComponent(VirtualFeed as unknown as DefineComponent).vm.$emit('nearStart');
}

const pageRequests = () =>
  fake.calls.filter(([n]) => n === 'getThreadPage').map(([, a]) => a as GetThreadPageRequest);

describe('ThreadView', () => {
  it('opens at the newest dated item and renders a feed of articles', async () => {
    const w = await mountThread();
    expect(pageRequests()[0]).toMatchObject({ chatId: CHAT, anchor: { kind: 'newest' }, direction: 'older' });
    const feed = w.get('[role="feed"]');
    expect(feed.attributes('aria-labelledby')).toBe('oma-thread-title');
    expect(feed.attributes('aria-busy')).toBe('false');
    expect(w.get('#oma-thread-title').text()).toContain('Synthetic chat 4');
    const articles = w.findAll('article');
    expect(articles.length).toBeGreaterThan(0);
    for (const a of articles) {
      expect(a.attributes('aria-setsize')).toBe('-1');
      expect(a.attributes('aria-posinset')).toBeUndefined();
      expect(a.attributes('tabindex')).toBe('0');
      expect(w.find(`#${a.attributes('aria-labelledby')}`).exists()).toBe(true);
      expect(w.find(`#${a.attributes('aria-describedby')}`).exists()).toBe(true);
    }
    // The newest item is rendered (the feed sits at its end).
    expect(w.find('#oma-m-200').exists()).toBe(true);
  });

  it('renders content as text only', async () => {
    const w = await mountThread();
    expect(w.text()).toContain('hello 200 <img src=x onerror=alert(1)>');
    expect(w.find('.oma-thread-item img').exists()).toBe(false);
  });

  it('shows the header facts and no read, delivery or sent words', async () => {
    const w = await mountThread();
    const head = w.get('.oma-thread-head').text();
    expect(head).toContain('2 addresses recorded in this conversation');
    expect(head).toContain('12 items recorded');
    expect(head).toContain('Undated items (2)');
    expect(w.text()).not.toMatch(/\b(?:Read|Delivered|Seen)\b/);
  });

  it('marks the feed busy while a page loads', async () => {
    let release: () => void = () => undefined;
    const serve = fake.handlers.getThreadPage!;
    fake.handlers.getThreadPage = (arg) => new Promise((resolve) => (release = () => resolve(serve(arg))));
    const store = useViewer();
    await store.open(ARCHIVE_ID);
    const w = mount(ThreadView, { attachTo: document.body, props: { chatId: sid(CHAT) } });
    await flushPromises();
    expect(w.get('[role="feed"]').attributes('aria-busy')).toBe('true');
    release();
    await flushPromises();
    expect(w.get('[role="feed"]').attributes('aria-busy')).toBe('false');
  });

  it('loads older items near the top, after the anchor', async () => {
    const w = await mountThread();
    fake.calls.length = 0;
    nearStart(w);
    await flushPromises();
    const [req] = pageRequests();
    expect(req).toMatchObject({ anchor: { kind: 'message', message: 151 }, direction: 'older' });
    expect(w.find('#oma-m-200').exists()).toBe(true);
    expect(useViewer().openWindow!.pages).toHaveLength(2);
  });

  it('shows a kept window again without a request, and still pages from it', async () => {
    const first = await mountThread();
    first.unmount();
    fake.calls.length = 0;
    const w = mount(ThreadView, { attachTo: document.body, props: { chatId: sid(CHAT) } });
    await flushPromises();
    expect(pageRequests()).toHaveLength(0);
    expect(w.find('#oma-m-200').exists()).toBe(true);
    nearStart(w);
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'message', message: 151 }, direction: 'older' });
  });

  it('stops loading after a failed page until Retry', async () => {
    const w = await mountThread();
    const serve = fake.handlers.getThreadPage!;
    fake.handlers.getThreadPage = () => Promise.reject(new IpcError('get_thread_page', 'queryTooBroad', 'x'));
    nearStart(w);
    await flushPromises();
    expect(w.get('[role="alert"]').text()).toContain('This page could not be loaded.');
    fake.calls.length = 0;
    nearStart(w);
    await flushPromises();
    expect(pageRequests()).toHaveLength(0);
    fake.handlers.getThreadPage = serve;
    await w.get('[role="alert"] button').trigger('click');
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'message', message: 151 }, direction: 'older' });
    expect(w.find('[role="alert"]').exists()).toBe(false);
  });

  it('opens the first items and the undated section on request', async () => {
    const w = await mountThread();
    fake.calls.length = 0;
    await w.findAll('button').find((b) => b.text() === 'Jump to first')!.trigger('click');
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'oldest' }, direction: 'newer' });
    expect(w.find('#oma-m-1').exists()).toBe(true);
    await w.findAll('button').find((b) => b.text() === 'Undated items (2)')!.trigger('click');
    await flushPromises();
    expect(pageRequests()[1]).toMatchObject({ anchor: { kind: 'undated' }, direction: 'newer' });
    expect(w.text()).toContain('Undated items, in source order');
    expect(w.findAll('button').some((b) => b.text() === 'Back to dated items')).toBe(true);
  });

  it('shows one separator per local day', async () => {
    const at = (id: number) => Date.UTC(2026, 0, 1) + id * 20 * 3_600_000;
    fake.handlers.getThreadPage = () =>
      Promise.resolve(
        threadPage(range(1, 6), {
          hasOlder: false,
          items: range(1, 6).map((id) => ({ ...textMessage(id), date: { ...textMessage(id).date, ms: at(id) } })),
        }),
      );
    const w = await mountThread();
    expect(w.findAll('article')).toHaveLength(6);
    const days = new Set(range(1, 6).map((id) => dayKey(at(id), 'system')));
    const seps = w.findAll('.oma-day-sep');
    expect(seps).toHaveLength(days.size);
    expect(new Set(seps.map((s) => s.text())).size).toBe(days.size);
    for (const s of seps) expect(s.attributes('aria-hidden')).toBe('true');
  });

  it('shows the notices a page carries', async () => {
    serveChat(200, { anchorMoved: true, extrasIncomplete: true, skippedRows: 2 });
    fake.handlers.openViewer = () => Promise.resolve(viewerSession({ viewer: 5, importRunning: true }));
    const w = await mountThread();
    const text = w.get('.oma-thread-notices').text();
    expect(text).toContain('An import is running.');
    expect(text).toContain('An import moved this position.');
    expect(text).toContain('Reactions and replies could not all be read');
    expect(text).toContain('run Verify');
  });

  it('says when a conversation has no messages in the archive', async () => {
    fake.handlers.getThreadPage = () => Promise.resolve(threadPage([], { hasOlder: false }));
    const w = await mountThread();
    expect(w.text()).toContain('No messages for this conversation are in the archive');
  });

  it('moves focus between articles with Page Down and Page Up', async () => {
    const w = await mountThread();
    const last = w.get('#oma-m-200').element as HTMLElement;
    last.focus();
    await w.get('#oma-m-200').trigger('focusin');
    await w.get('#oma-m-200').trigger('keydown', { key: 'PageUp' });
    await flushPromises();
    expect(document.activeElement?.id).toBe('oma-m-199');
    await w.get('#oma-m-199').trigger('keydown', { key: 'PageDown' });
    await flushPromises();
    expect(document.activeElement?.id).toBe('oma-m-200');
  });

  it('jumps to a reply\'s originator in this conversation and focuses it', async () => {
    const serve = fake.handlers.getThreadPage!;
    fake.handlers.getThreadPage = (arg) => {
      const r = arg as GetThreadPageRequest;
      if (r.anchor.kind === 'message' && r.direction === 'around') {
        const at = r.anchor.message;
        return Promise.resolve(page(range(at - 5, 11), 200, { focus: { id: sid(at), reason: 'anchor' } }));
      }
      return (serve(arg) as Promise<ThreadPageDto>).then((p) => ({
        ...p,
        items: p.items.map((m) =>
          m.id === 200 ? { ...m, reply: { kind: 'resolved' as const, originator: sid(20), quote: { kind: 'noBody' as const } } } : m,
        ),
      }));
    };
    const w = await mountThread();
    fake.calls.length = 0;
    const button = w.get('#oma-m-200').findAll('button').find((b) => b.text() === 'Show the message this replies to')!;
    await button.trigger('click');
    await flushPromises();
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'message', message: 20 }, direction: 'around' });
    expect(w.text()).toContain('Showing the message you chose');
    expect(document.activeElement?.id).toBe('oma-m-20');
  });

  it('opens another conversation at a reply\'s originator without putting the id in a URL', async () => {
    const serve = fake.handlers.getThreadPage!;
    fake.handlers.getThreadPage = (arg) =>
      (serve(arg) as Promise<ThreadPageDto>).then((p) => ({
        ...p,
        items: p.items.map((m) =>
          m.id === 200
            ? {
                ...m,
                reply: {
                  kind: 'inAnotherConversation' as const,
                  originator: sid(8),
                  chat: sid(9),
                  quote: { kind: 'noBody' as const },
                },
              }
            : m,
        ),
      }));
    const w = await mountThread();
    const button = w.get('#oma-m-200').findAll('button').find((b) => b.text() === 'Open that conversation at the message')!;
    await button.trigger('click');
    expect(w.emitted('openChat')).toEqual([[9]]);
    expect(useViewer().pendingFocus).toEqual({ chatId: 9, message: 8 });
    fake.calls.length = 0;
    const other = mount(ThreadView, { attachTo: document.body, props: { chatId: sid(9) } });
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ chatId: 9, anchor: { kind: 'message', message: 8 }, direction: 'around' });
    expect(useViewer().pendingFocus).toBeNull();
    other.unmount();
  });

  it('labels runs of messages only when the conversation has several addresses', async () => {
    const w = await mountThread();
    const visible = () => w.findAll('.oma-item-sender').filter((s) => !s.classes().includes('visually-hidden'));
    // One sender throughout: a label at the start of each local day only.
    const days = w.findAll('.oma-day-sep').length;
    expect(days).toBeGreaterThan(0);
    expect(visible()).toHaveLength(days);
    w.unmount();

    fake.handlers.getConversations = (arg) => {
      const [, wanted] = arg as [number, number[]];
      return Promise.resolve({ rows: wanted.map((id) => conversationRow(id, { addresses: 1 })), skippedRows: 0 });
    };
    setActivePinia(createPinia());
    useSettings().snapshot = snapshot();
    const single = await mountThread();
    expect(single.findAll('.oma-item-sender').every((s) => s.classes().includes('visually-hidden'))).toBe(true);
  });
});

describe('ThreadView: dates, search and shortcuts (5i)', () => {
  /** Serve pages around a message with a focus notice, and the rest as usual. */
  function serveAround(): void {
    const serve = fake.handlers.getThreadPage!;
    fake.handlers.getThreadPage = (arg) => {
      const r = arg as GetThreadPageRequest;
      if (r.anchor.kind === 'message' && r.direction === 'around') {
        const at = r.anchor.message;
        return Promise.resolve(page(range(at - 5, 11), 200, { focus: { id: sid(at), reason: 'anchor' } }));
      }
      return serve(arg);
    };
  }

  it('opens a search result in this conversation around it, and focuses it', async () => {
    serveAround();
    const w = await mountThread();
    fake.calls.length = 0;
    useViewer().focusOnOpen(sid(CHAT), sid(20));
    await flushPromises();
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({
      anchor: { kind: 'message', message: 20, include: true },
      direction: 'around',
    });
    expect(useViewer().pendingFocus).toBeNull();
    expect(document.activeElement?.id).toBe('oma-m-20');
    w.unmount();
  });

  it('jumps to the day the date dialog picked, without animation, and focuses its first item', async () => {
    serveAround();
    const w = await mountThread();
    const open = w.findAll('button').find((b) => b.text() === 'Jump to date…')!;
    await open.trigger('click');
    expect(useViewer().dateJumpOpen).toBe(true);
    expect((w.findComponent(DateJumpDialog as unknown as DefineComponent).props() as { open: boolean }).open).toBe(true);
    fake.calls.length = 0;
    w.findComponent(DateJumpDialog as unknown as DefineComponent).vm.$emit('jump', sid(30));
    await flushPromises();
    await flushPromises();
    expect(useViewer().dateJumpOpen).toBe(false);
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'message', message: 30, include: true }, direction: 'around' });
    expect(document.activeElement?.id).toBe('oma-m-30');
  });

  it('opens and focuses the first or newest item when the global shortcuts ask (Ctrl+Home, Ctrl+End)', async () => {
    const w = await mountThread();
    fake.calls.length = 0;
    useViewer().requestThread('first');
    await flushPromises();
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'oldest' }, direction: 'newer' });
    expect(document.activeElement?.id).toBe('oma-m-1');
    fake.calls.length = 0;
    useViewer().requestThread('last');
    await flushPromises();
    await flushPromises();
    expect(pageRequests()[0]).toMatchObject({ anchor: { kind: 'newest' } });
    expect(document.activeElement?.id).toBe('oma-m-200');
    w.unmount();
  });

  it('shows a focused message’s details on Enter and its replies on R, only from the article itself', async () => {
    const w = await mountThread();
    const store = useViewer();
    const article = w.get('#oma-m-200');
    (article.element as HTMLElement).focus();
    await article.trigger('keydown', { key: 'Enter' });
    expect(store.panel).toEqual({ kind: 'details', message: 200, reactions: false });
    await article.trigger('keydown', { key: 'r' });
    expect(store.panel).toEqual({ kind: 'replies', message: 200 });
    store.closePanel();
    // A key typed on a control inside the article is that control's.
    const inner = article.findAll('button').find((b) => b.text() === 'Details')!;
    await inner.trigger('keydown', { key: 'r' });
    expect(store.panel).toBeNull();
    // With a modifier it is not the shortcut.
    await article.trigger('keydown', { key: 'r', ctrlKey: true });
    expect(store.panel).toBeNull();
  });

  it('opens the search panel scoped to this conversation', async () => {
    const w = await mountThread();
    const store = useViewer();
    await w.findAll('button').find((b) => b.text() === 'Search this conversation')!.trigger('click');
    expect(store.panel).toEqual({ kind: 'search' });
    expect(store.searchForm.chatId).toBe(CHAT);
  });
});
