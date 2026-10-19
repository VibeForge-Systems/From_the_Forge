// The search panel (M5 §4, §8.4, §13.8, slice 5i): inputs that teach no
// engine service the text, the 250 ms debounce, the request Rust gets
// (scope, local date range through the injected builder, filters, the
// explicit reveal), superseded searches dropped silently, the polite
// count and its capped sentences, scan progress and Cancel, results with
// `<mark>` and masked text, and opening a result without a URL.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount } from '@vue/test-utils';

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
import SearchPanel from '@/components/viewer/SearchPanel.vue';
import { SEARCH_TEXT, fill } from '@/services/viewerText';
import type { DayBoundaryBuilder } from '@/services/time';
import { SEARCH_DEBOUNCE_MS, useViewer } from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import type { ScanProgressDto, SearchHitDto, SearchMessagesRequest, SearchStartedDto } from '@/types';
import {
  ARCHIVE_ID,
  content,
  conversationRow,
  searchHit,
  searchStarted,
  sid,
  snapshot,
  viewerSession,
} from './helpers';

/** A fake zone where day N of 2026 starts at N × 1,000 ms. */
const build: DayBoundaryBuilder = (y, m, d) =>
  Math.round((Date.UTC(y, m, d) - Date.UTC(2026, 0, 1)) / 86_400_000) * 1000;

const QUERY = 'secret words';

let started: SearchStartedDto;
let hits: SearchHitDto[];
let progressSink: ((p: ScanProgressDto) => void) | null;

function searchCalls(): SearchMessagesRequest[] {
  return fake.calls.filter(([n]) => n === 'searchMessages').map(([, a]) => (a as [SearchMessagesRequest])[0]);
}

async function mountPanel() {
  const store = useViewer();
  await store.open(ARCHIVE_ID);
  const w = mount(SearchPanel, { attachTo: document.body, props: { build } });
  await flushPromises();
  return w;
}

async function typeQuery(w: Awaited<ReturnType<typeof mountPanel>>, value: string) {
  await w.find('#oma-search-q').setValue(value);
  vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
  await flushPromises();
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
  setActivePinia(createPinia());
  fake.calls.length = 0;
  started = searchStarted(3);
  hits = [searchHit(1), searchHit(2), searchHit(3)];
  progressSink = null;
  fake.handlers = {
    openViewer: () => Promise.resolve(viewerSession({ viewer: 5, unsearchable: 7 })),
    searchMessages: (arg) => {
      progressSink = (arg as [unknown, (p: ScanProgressDto) => void])[1];
      return Promise.resolve(started);
    },
    getSearchResults: (arg) => {
      const r = arg as { offset: number; limit: number };
      return Promise.resolve({ hits: hits.slice(r.offset, r.offset + r.limit), skippedRows: 0 });
    },
    getConversations: (arg) => {
      const [, wanted] = arg as [number, number[]];
      return Promise.resolve({ rows: wanted.map((id) => conversationRow(id)), skippedRows: 0 });
    },
  };
  useSettings().snapshot = snapshot();
});

afterEach(() => {
  vi.useRealTimers();
  document.body.innerHTML = '';
});

describe('SearchPanel', () => {
  it('teaches no engine service the text: every text field has spellcheck, autocomplete and autocapitalize off', async () => {
    const w = await mountPanel();
    const fields = w.findAll('input[type="text"]');
    expect(fields.length).toBe(3);
    for (const f of fields) {
      expect(f.attributes('spellcheck')).toBe('false');
      expect(f.attributes('autocomplete')).toBe('off');
      expect(f.attributes('autocapitalize')).toBe('off');
    }
    expect(document.activeElement?.id).toBe('oma-search-q');
  });

  it('debounces typing by 250 ms and sends one request with the defaults', async () => {
    const w = await mountPanel();
    const input = w.find('#oma-search-q');
    for (const partial of ['s', 'se', 'sec', QUERY]) {
      await input.setValue(partial);
      vi.advanceTimersByTime(100);
    }
    expect(searchCalls()).toHaveLength(0);
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
    await flushPromises();
    expect(searchCalls()).toEqual([
      { viewer: 5, query: QUERY, mode: 'auto', filters: {}, includeEarlier: false, includeEvents: false },
    ]);
    expect(fake.calls.find(([n]) => n === 'getSearchResults')?.[1]).toEqual({ viewer: 5, searchId: 1, offset: 0, limit: 50 });
  });

  it('sends the scope, the local date range, the filters and the explicit reveal', async () => {
    const store = useViewer();
    await store.open(ARCHIVE_ID);
    store.openChatId = sid(4);
    store.openSearch(sid(4));
    const w = mount(SearchPanel, { attachTo: document.body, props: { build } });
    await w.find('#oma-search-from').setValue('2026-01-02');
    await w.find('#oma-search-to').setValue('2026-01-04');
    await w.find('#oma-search-att').setValue(true);
    await w.find('#oma-search-sender').setValue('others');
    await w.find('#oma-search-earlier').setValue(true);
    await w.find('#oma-search-mode').setValue('exact');
    await typeQuery(w, QUERY);
    const last = searchCalls().at(-1)!;
    expect(last).toEqual({
      viewer: 5,
      query: QUERY,
      mode: 'exact',
      // From the start of 2 January to the start of 5 January: whole local days, the end inclusive.
      filters: { chatId: 4, fromUtcMs: 1000, untilUtcMs: 4000, hasAttachment: true, sender: 'others' },
      includeEarlier: true,
      includeEvents: false,
    });
    // Unticking the scope searches every conversation.
    await w.find('#oma-search-scope').setValue(false);
    vi.advanceTimersByTime(0);
    await flushPromises();
    expect(searchCalls().at(-1)!.filters.chatId).toBeUndefined();
  });

  it('refuses a malformed or reversed date range in the panel, without searching', async () => {
    const w = await mountPanel();
    await w.find('#oma-search-from').setValue('2026-02-30');
    await typeQuery(w, QUERY);
    expect(searchCalls()).toHaveLength(0);
    expect(w.find('[role="alert"]').text()).toBe(SEARCH_TEXT.badFrom);
    expect(w.find('#oma-search-from').attributes('aria-invalid')).toBe('true');
    await w.find('#oma-search-from').setValue('2026-03-02');
    await w.find('#oma-search-to').setValue('2026-03-01');
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
    await flushPromises();
    expect(searchCalls()).toHaveLength(0);
    expect(w.find('[role="alert"]').text()).toBe(SEARCH_TEXT.badOrder);
  });

  it('announces the count politely and never repeats the query', async () => {
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    const status = w.find('.oma-search-status');
    expect(status.attributes('role')).toBe('status');
    expect(status.text()).toBe(fill(SEARCH_TEXT.many, { n: 3 }));
    expect(w.text()).toContain(fill(SEARCH_TEXT.unsearchable, { n: 7 }));
    expect(w.text()).toContain(fill(SEARCH_TEXT.ranMode, { mode: 'Words' }));
    expect(w.find('.oma-search-status').text()).not.toContain('secret');
  });

  it('says a capped result is incomplete, and how', async () => {
    started = searchStarted(10_000, { capped: true, newestKept: false });
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    expect(w.find('.oma-search-status').text()).toBe(fill(SEARCH_TEXT.capped, { n: 10_000 }));
    expect(w.find('.oma-search-status').text()).toContain('not necessarily the newest');
  });

  it('shows hints when nothing matches', async () => {
    started = searchStarted(0);
    hits = [];
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    expect(w.find('.oma-search-status').text()).toBe(SEARCH_TEXT.none);
    expect(w.findAll('.oma-search-hints li').map((l) => l.text())).toEqual([
      SEARCH_TEXT.hintPrefix,
      SEARCH_TEXT.hintScripts,
      SEARCH_TEXT.hintFilters,
    ]);
  });

  it('drops a superseded search silently, and words a too-broad one', async () => {
    fake.handlers.searchMessages = () => Promise.reject(new IpcError('search_messages', 'searchSuperseded', ''));
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    expect(w.find('[role="alert"]').exists()).toBe(false);
    fake.handlers.searchMessages = () =>
      Promise.reject(new IpcError('search_messages', 'queryTooBroad', 'The request covers too much.'));
    await typeQuery(w, `${QUERY} more`);
    expect(w.find('[role="alert"]').text()).toBe(SEARCH_TEXT.tooBroad);
  });

  it('renders snippets as text, with matches in <mark>, masked codes as words, and undated results apart', async () => {
    started = searchStarted(3, { dated: 2 });
    hits = [
      searchHit(1, {}, '<img src=x onerror=alert(1)>'),
      searchHit(2, {
        version: 'earlier',
        text: {
          pieces: [
            { kind: 'text', value: content('code '), hit: false },
            { kind: 'masked', mask: 'oneTimeCode' },
          ],
          cutStart: true,
          cutEnd: false,
          highlight: { kind: 'notShown', why: 'budget' },
          matchedHidden: true,
          fullUtf16: 400,
        },
        otherChats: { n: 2, capped: false },
      }),
      searchHit(3, { ms: null }),
    ];
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    const lists = w.findAll('ol.oma-hits');
    expect(lists).toHaveLength(2);
    expect(lists[0]!.findAll('li')).toHaveLength(2);
    expect(w.find('.oma-hits-heading').text()).toBe(SEARCH_TEXT.undatedHeading);
    const first = lists[0]!.findAll('li')[0]!;
    expect(first.find('mark').text()).toBe('<img src=x onerror=alert(1)>');
    expect(first.find('img').exists()).toBe(false);
    // One isolate per snippet.
    expect(first.findAll('.oma-hit-text bdi')).toHaveLength(1);
    const second = lists[0]!.findAll('li')[1]!;
    expect(second.find('.oma-masked').text()).toBe('One-time code hidden');
    expect(second.text()).toContain(SEARCH_TEXT.matchedHidden);
    expect(second.text()).toContain('Earlier version');
    expect(second.text()).toContain(fill(SEARCH_TEXT.otherChats, { n: 2 }));
    expect(second.find('.oma-hit-text').text().startsWith('…')).toBe(true);
    expect(lists[1]!.text()).toContain(SEARCH_TEXT.undated);
    // The conversations named are loaded as rows (labels only).
    expect(w.text()).toContain('Synthetic chat 4');
  });

  it('opens a result in its conversation through the store, never the URL', async () => {
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    const open = w.findAll('button').find((b) => b.text() === SEARCH_TEXT.open)!;
    expect(open.attributes('aria-describedby')).toBe('oma-hit-0-h oma-hit-0-t');
    await open.trigger('click');
    expect(w.emitted('openChat')).toEqual([[sid(4)]]);
    expect(useViewer().pendingFocus).toEqual({ chatId: sid(4), message: sid(1), focus: true });
  });

  it('loads more results on request', async () => {
    started = searchStarted(60);
    hits = Array.from({ length: 60 }, (_, i) => searchHit(i + 1));
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    expect(w.findAll('ol.oma-hits li')).toHaveLength(50);
    await w.findAll('button').find((b) => b.text() === SEARCH_TEXT.more)!.trigger('click');
    await flushPromises();
    expect(fake.calls.filter(([n]) => n === 'getSearchResults').at(-1)?.[1]).toEqual({
      viewer: 5,
      searchId: 1,
      offset: 50,
      limit: 50,
    });
    expect(w.findAll('ol.oma-hits li')).toHaveLength(60);
    expect(w.findAll('button').some((b) => b.text() === SEARCH_TEXT.more)).toBe(false);
  });

  it('shows a scan’s progress and cancels it by the id its progress names', async () => {
    let finish: (v: SearchStartedDto) => void = () => {};
    fake.handlers.searchMessages = (arg) => {
      progressSink = (arg as [unknown, (p: ScanProgressDto) => void])[1];
      return new Promise((resolve) => (finish = resolve));
    };
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    expect(w.find('.oma-search-status').text()).toBe(SEARCH_TEXT.searching);
    progressSink!({ searchId: 9, examined: 5000, matched: 2 });
    await flushPromises();
    expect(w.find('.oma-search-status').text()).toBe(fill(SEARCH_TEXT.scanning, { examined: 5000, matched: 2 }));
    await w.findAll('button').find((b) => b.text() === SEARCH_TEXT.cancel)!.trigger('click');
    await flushPromises();
    expect(fake.calls.find(([n]) => n === 'cancelSearch')?.[1]).toEqual({ viewer: 5, searchId: 9 });
    expect(w.find('.oma-search-status').text()).toBe(SEARCH_TEXT.cancelled);
    // A late answer from the cancelled scan is dropped.
    finish(searchStarted(1, { searchId: 9 }));
    await flushPromises();
    expect(w.find('.oma-search-status').text()).toBe(SEARCH_TEXT.cancelled);
    expect(w.findAll('ol.oma-hits')).toHaveLength(0);
  });

  it('clears the results when the query is emptied, and keeps the form until the viewer closes', async () => {
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    expect(w.findAll('ol.oma-hits li')).toHaveLength(3);
    await typeQuery(w, '   ');
    expect(fake.calls.find(([n]) => n === 'cancelSearch')?.[1]).toEqual({ viewer: 5, searchId: 1 });
    expect(w.findAll('ol.oma-hits')).toHaveLength(0);
    const store = useViewer();
    expect(store.searchForm.query).toBe('   ');
    await store.close();
    expect(store.searchForm.query).toBe('');
  });
});

describe('SearchPanel (review fixes)', () => {
  it('clears revealed snippets when "Include earlier versions" is unticked while a date is invalid', async () => {
    hits = [searchHit(1, { version: 'earlier' })];
    started = searchStarted(1);
    const w = await mountPanel();
    await w.find('#oma-search-earlier').setValue(true);
    await typeQuery(w, QUERY);
    expect(w.findAll('.oma-hit')).toHaveLength(1);
    // A half-typed date, then the reveal is withdrawn.
    await w.find('#oma-search-from').setValue('2026-0');
    await w.find('#oma-search-earlier').setValue(false);
    vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS);
    await flushPromises();
    expect(w.findAll('.oma-hit')).toHaveLength(0);
    expect(useViewer().search.hits).toEqual([]);
  });

  it('names no direction for a conversation event, and marks a date before 2007', async () => {
    hits = [
      searchHit(1, { item: 'participantAdded', direction: 'you' }),
      searchHit(2, { ms: Date.UTC(2001, 0, 1) }),
    ];
    started = searchStarted(2);
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    const [event, early] = w.findAll('.oma-hit');
    expect(event!.find('.oma-hit-dir').exists()).toBe(false);
    expect(early!.find('.oma-hit-dir').exists()).toBe(true);
    expect(early!.find('.oma-hit-head').text()).toContain('Before 2007');
    expect(event!.find('.oma-hit-head').text()).not.toContain('Before 2007');
  });

  it('keeps one "more results" button while more load, so focus stays on it', async () => {
    started = searchStarted(80);
    hits = Array.from({ length: 80 }, (_, i) => searchHit(i + 1));
    let release: () => void = () => undefined;
    const w = await mountPanel();
    await typeQuery(w, QUERY);
    fake.handlers.getSearchResults = (arg) => {
      const r = arg as { offset: number; limit: number };
      return new Promise((resolve) => {
        release = () => resolve({ hits: hits.slice(r.offset, r.offset + r.limit), skippedRows: 0 });
      });
    };
    const button = w.findAll('button').find((b) => b.text() === SEARCH_TEXT.more)!;
    (button.element as HTMLButtonElement).focus();
    await button.trigger('click');
    await flushPromises();
    expect(button.element.isConnected).toBe(true);
    expect(button.attributes('aria-disabled')).toBe('true');
    expect(button.text()).toBe(SEARCH_TEXT.loadingMore);
    expect(document.activeElement).toBe(button.element);
    release();
    await flushPromises();
    expect(w.findAll('.oma-hit')).toHaveLength(80);
  });
});
