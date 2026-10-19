// The viewer's state (M5 §8.3): the session, the conversation snapshot,
// the page windows per chat, the search state and the media inspection
// cache. It holds content (`UntrustedText`) in memory only and never
// persists anything: no localStorage, no settings field.
//
// Heap bound: each chat keeps at most `MAX_WINDOW_ITEMS` loaded items.
// When a load goes over, whole pages furthest from the page the user is
// looking at are evicted, and the window's edge keeps that page's anchor
// so the same rows are fetched again by anchor when scrolled back. Only
// the open chat and the last two visited keep windows.
import { defineStore } from 'pinia';
import { computed, markRaw, ref, shallowRef } from 'vue';
import { IpcError, ipc } from '@/services/ipc';
import { devNote } from '@/services/devlog';
import { VIEWER_ERROR_TEXT } from '@/services/viewerText';
import type {
  AnchorInput,
  AttachmentInspectionDto,
  ConversationRowDto,
  ConversationSortInput,
  ErrorCode,
  HandleDto,
  MessageDto,
  PageRangeDto,
  RevealInput,
  SafeId,
  ScanProgressDto,
  SearchHitDto,
  SearchMessagesRequest,
  SearchStartedDto,
  ThreadPageDto,
  ViewerSessionDto,
} from '@/types';
import { useSettings } from './settings';

/** Loaded items per chat (§8.3). */
export const MAX_WINDOW_ITEMS = 1500;
/** Chats that keep a window: the open one and the last two visited. */
export const MAX_WINDOWS = 3;
/** Items per page request. */
export const PAGE_LIMIT = 50;
/** `get_conversations` takes at most 200 ids. */
export const ROWS_PER_REQUEST = 200;
/** Conversation rows kept in memory. */
export const MAX_ROWS = 2000;
/** Attachment inspections kept in memory. */
export const MAX_INSPECTIONS = 500;
/** Search hits per `get_search_results`. */
export const HITS_PER_REQUEST = 50;

/** One loaded page of a chat, with the anchors to load beyond it. */
export interface LoadedPage {
  items: MessageDto[];
  olderAnchor: SafeId | null;
  newerAnchor: SafeId | null;
  hasOlder: boolean;
  hasNewer: boolean;
}

/** The contiguous run of pages loaded for one chat, oldest first. */
export interface ChatWindow {
  chatId: SafeId;
  range: PageRangeDto;
  pages: LoadedPage[];
  handles: Map<SafeId, HandleDto>;
  /** Bumped whenever the window is replaced, so late responses are dropped. */
  epoch: number;
  loading: 'older' | 'newer' | 'initial' | null;
  /** The last page reply's notices. */
  notices: Pick<
    ThreadPageDto,
    'partial' | 'extrasIncomplete' | 'anchorMoved' | 'focus' | 'cutForSize' | 'skippedRows' | 'undatedCount'
  > | null;
  error: string | null;
}

export function windowItemCount(w: Pick<ChatWindow, 'pages'>): number {
  return w.pages.reduce((n, p) => n + p.items.length, 0);
}

function pageOf(dto: ThreadPageDto): LoadedPage {
  return {
    // Items are immutable DTOs: kept raw, so 1,500 of them are not proxied.
    items: markRaw(dto.items),
    olderAnchor: dto.olderAnchor,
    newerAnchor: dto.newerAnchor,
    hasOlder: dto.hasOlder,
    hasNewer: dto.hasNewer,
  };
}

/**
 * Evict whole pages until the window holds at most `max` items. The page
 * at `keep` (the one in view, or the one just loaded) is never evicted;
 * of the two ends, the one further from it goes first. An evicted end
 * leaves its neighbour's anchor as the window's edge, with "more" set, so
 * the rows come back by anchor. Returns the number of pages evicted.
 */
export function evictPages(w: Pick<ChatWindow, 'pages'>, keep: number, max = MAX_WINDOW_ITEMS): number {
  let evicted = 0;
  let k = Math.min(Math.max(keep, 0), w.pages.length - 1);
  while (windowItemCount(w) > max && w.pages.length > 1) {
    const fromStart = k;
    const fromEnd = w.pages.length - 1 - k;
    if (fromStart >= fromEnd && fromStart > 0) {
      w.pages.shift();
      k -= 1;
      w.pages[0]!.hasOlder = true;
    } else if (fromEnd > 0) {
      w.pages.pop();
      w.pages[w.pages.length - 1]!.hasNewer = true;
    } else {
      break;
    }
    evicted += 1;
  }
  return evicted;
}

/** The index of the page that holds a message, or -1. */
export function pageIndexOf(w: Pick<ChatWindow, 'pages'>, id: SafeId): number {
  return w.pages.findIndex((p) => p.items.some((m) => m.id === id));
}

export interface SearchState {
  searchId: number | null;
  started: SearchStartedDto | null;
  hits: SearchHitDto[];
  progress: ScanProgressDto | null;
  running: boolean;
  error: string | null;
}

const emptySearch = (): SearchState => ({
  searchId: null,
  started: null,
  hits: [],
  progress: null,
  running: false,
  error: null,
});

function message(error: unknown): string {
  if (error instanceof IpcError) {
    const words = (VIEWER_ERROR_TEXT as Partial<Record<ErrorCode, string>>)[error.code];
    return words || error.message;
  }
  return error instanceof Error ? error.message : 'The viewer request failed.';
}

export const useViewer = defineStore('viewer', () => {
  const session = shallowRef<ViewerSessionDto | null>(null);
  const archiveId = ref<string | null>(null);
  const opening = ref(false);
  const error = ref<string | null>(null);

  const conversationIds = shallowRef<SafeId[]>([]);
  const conversationTotal = ref(0);
  const conversationsTruncated = ref(false);
  const sort = ref<ConversationSortInput>('recent');
  const rows = shallowRef(new Map<SafeId, ConversationRowDto>());

  /** Windows in visit order: the last entry is the open chat. */
  const windows = ref<ChatWindow[]>([]);
  const openChatId = ref<SafeId | null>(null);

  const search = ref<SearchState>(emptySearch());
  const inspections = shallowRef(new Map<SafeId, AttachmentInspectionDto>());

  let epochs = 0;
  let searchSeq = 0;

  const viewer = computed(() => session.value?.viewer ?? null);
  const openWindow = computed(
    () => windows.value.find((w) => w.chatId === openChatId.value) ?? null,
  );

  function reveal(): RevealInput | undefined {
    const prefs = useSettings().settings?.viewer;
    return prefs?.revealEarlierText ? { earlierText: true } : undefined;
  }

  function clear(): void {
    session.value = null;
    archiveId.value = null;
    conversationIds.value = [];
    conversationTotal.value = 0;
    conversationsTruncated.value = false;
    rows.value = new Map();
    windows.value = [];
    openChatId.value = null;
    search.value = emptySearch();
    inspections.value = new Map();
  }

  /** A `viewerClosed` rejection means the session is gone: forget it all. */
  function failed(e: unknown): string {
    if (e instanceof IpcError && e.code === 'viewerClosed') {
      clear();
    }
    return message(e);
  }

  async function open(id: string): Promise<boolean> {
    opening.value = true;
    error.value = null;
    try {
      if (session.value) await close();
      session.value = await ipc.openViewer(id);
      archiveId.value = id;
      return true;
    } catch (e) {
      error.value = failed(e);
      return false;
    } finally {
      opening.value = false;
    }
  }

  /** Close the session (D114: leaving the viewer routes, idle hide). */
  async function close(): Promise<void> {
    const v = viewer.value;
    clear();
    if (v === null) return;
    try {
      await ipc.closeViewer(v);
    } catch {
      // The session is gone either way; a stale generation is not an error.
    }
  }

  async function listConversations(next: ConversationSortInput, filter: string | null): Promise<void> {
    const v = viewer.value;
    if (v === null) return;
    try {
      const index = await ipc.listConversations(v, next, filter);
      if (viewer.value !== v) return;
      sort.value = next;
      conversationIds.value = index.ids;
      conversationTotal.value = index.total;
      conversationsTruncated.value = index.truncated;
    } catch (e) {
      error.value = failed(e);
    }
  }

  /** Load the rows of `ids` not yet cached, 200 per request. */
  async function loadRows(ids: readonly SafeId[]): Promise<void> {
    const v = viewer.value;
    if (v === null) return;
    const missing = ids.filter((id) => !rows.value.has(id));
    for (let i = 0; i < missing.length; i += ROWS_PER_REQUEST) {
      const chunk = missing.slice(i, i + ROWS_PER_REQUEST);
      try {
        const got = await ipc.getConversations(v, chunk);
        if (viewer.value !== v) return;
        const next = new Map(rows.value);
        for (const row of got.rows) {
          next.delete(row.id);
          next.set(row.id, row);
        }
        while (next.size > MAX_ROWS) next.delete(next.keys().next().value!);
        rows.value = next;
      } catch (e) {
        error.value = failed(e);
        return;
      }
    }
  }

  function touchWindow(chatId: SafeId): ChatWindow | null {
    const i = windows.value.findIndex((w) => w.chatId === chatId);
    if (i < 0) return null;
    const [w] = windows.value.splice(i, 1);
    windows.value.push(w!);
    return windows.value[windows.value.length - 1]!;
  }

  function addHandles(w: ChatWindow, handles: readonly HandleDto[]): void {
    for (const h of handles) w.handles.set(h.id, h);
  }

  /**
   * Open a chat at an anchor (newest dated message by default). A chat that
   * still has a dated window reaching its newest item is shown from that
   * window without a request.
   */
  async function openChat(chatId: SafeId, anchor: AnchorInput = { kind: 'newest' }): Promise<void> {
    const v = viewer.value;
    if (v === null) return;
    openChatId.value = chatId;
    const kept = touchWindow(chatId);
    if (
      kept &&
      anchor.kind === 'newest' &&
      kept.range === 'dated' &&
      kept.pages.length > 0 &&
      !kept.error &&
      !kept.pages[kept.pages.length - 1]!.hasNewer
    ) {
      return;
    }
    // The undated section is read in source order from its start.
    const direction =
      anchor.kind === 'message'
        ? 'around'
        : anchor.kind === 'oldest' || anchor.kind === 'undated'
          ? 'newer'
          : 'older';
    const w: ChatWindow = {
      chatId,
      range: anchor.kind === 'undated' ? 'undated' : 'dated',
      pages: [],
      handles: new Map(),
      epoch: (epochs += 1),
      loading: 'initial',
      notices: null,
      error: null,
    };
    windows.value = windows.value.filter((x) => x.chatId !== chatId);
    windows.value.push(w);
    while (windows.value.length > MAX_WINDOWS) windows.value.shift();
    const live = windows.value[windows.value.length - 1]!;
    try {
      const r = reveal();
      const page = await ipc.getThreadPage({
        viewer: v,
        chatId,
        anchor,
        direction,
        limit: PAGE_LIMIT,
        ...(r ? { reveal: r } : {}),
      });
      if (viewer.value !== v || live.epoch !== w.epoch || !windows.value.includes(live)) return;
      live.range = page.range;
      live.pages = [pageOf(page)];
      addHandles(live, page.handles);
      live.notices = page;
    } catch (e) {
      live.error = failed(e);
    } finally {
      live.loading = null;
    }
  }

  /** Load the page beyond one edge of the open chat's window. */
  async function loadMore(edge: 'older' | 'newer', inView: SafeId | null = null): Promise<void> {
    const v = viewer.value;
    const w = openWindow.value;
    if (v === null || !w || w.loading || w.pages.length === 0) return;
    const end = edge === 'older' ? w.pages[0]! : w.pages[w.pages.length - 1]!;
    const more = edge === 'older' ? end.hasOlder : end.hasNewer;
    const anchor = edge === 'older' ? end.olderAnchor : end.newerAnchor;
    if (!more || anchor === null) return;
    const epoch = w.epoch;
    w.loading = edge;
    w.error = null;
    try {
      const r = reveal();
      const page = await ipc.getThreadPage({
        viewer: v,
        chatId: w.chatId,
        anchor: { kind: 'message', message: anchor },
        direction: edge,
        limit: PAGE_LIMIT,
        ...(r ? { reveal: r } : {}),
      });
      if (viewer.value !== v || w.epoch !== epoch || !windows.value.includes(w)) return;
      addHandles(w, page.handles);
      w.notices = page;
      if (edge === 'older') w.pages.unshift(pageOf(page));
      else w.pages.push(pageOf(page));
      const viewed = inView === null ? -1 : pageIndexOf(w, inView);
      const keep = viewed >= 0 ? viewed : edge === 'older' ? 0 : w.pages.length - 1;
      const evicted = evictPages(w, keep);
      if (evicted > 0) devNote('viewerWindowEvicted', evicted);
    } catch (e) {
      w.error = failed(e);
    } finally {
      w.loading = null;
    }
  }

  /** Start a search; a newer one supersedes it (dropped silently). */
  async function startSearch(
    request: Omit<SearchMessagesRequest, 'viewer'>,
  ): Promise<void> {
    const v = viewer.value;
    if (v === null) return;
    const seq = (searchSeq += 1);
    search.value = { ...emptySearch(), running: true };
    try {
      const started = await ipc.searchMessages({ ...request, viewer: v }, (p) => {
        if (seq === searchSeq) search.value.progress = p;
      });
      if (seq !== searchSeq || viewer.value !== v) return;
      search.value.searchId = started.searchId;
      search.value.started = started;
      await loadHits(0);
    } catch (e) {
      if (seq !== searchSeq) return;
      if (e instanceof IpcError && e.code === 'searchSuperseded') return;
      search.value.error = failed(e);
    } finally {
      if (seq === searchSeq) search.value.running = false;
    }
  }

  async function loadHits(offset: number): Promise<void> {
    const v = viewer.value;
    const id = search.value.searchId;
    if (v === null || id === null) return;
    const seq = searchSeq;
    try {
      const got = await ipc.getSearchResults({ viewer: v, searchId: id, offset, limit: HITS_PER_REQUEST });
      if (seq !== searchSeq) return;
      search.value.hits = offset === 0 ? got.hits : [...search.value.hits, ...got.hits];
    } catch (e) {
      if (seq !== searchSeq) return;
      if (e instanceof IpcError && e.code === 'searchSuperseded') return;
      search.value.error = failed(e);
    }
  }

  async function cancelSearch(): Promise<void> {
    const v = viewer.value;
    const id = search.value.searchId;
    searchSeq += 1;
    search.value = emptySearch();
    if (v === null || id === null) return;
    try {
      await ipc.cancelSearch({ viewer: v, searchId: id });
    } catch {
      // Cancelling a finished or superseded search is not an error.
    }
  }

  /** Inspect a surfaced attachment, cached per session. */
  async function inspect(attachmentId: SafeId, forPlayback = false): Promise<AttachmentInspectionDto | null> {
    const v = viewer.value;
    if (v === null) return null;
    const cached = inspections.value.get(attachmentId);
    if (cached && (!forPlayback || cached.playback !== null)) return cached;
    try {
      const got = await ipc.inspectAttachment({ viewer: v, attachmentId, forPlayback });
      if (viewer.value !== v) return null;
      const next = new Map(inspections.value);
      next.delete(attachmentId);
      next.set(attachmentId, got);
      while (next.size > MAX_INSPECTIONS) next.delete(next.keys().next().value!);
      inspections.value = next;
      return got;
    } catch (e) {
      error.value = failed(e);
      return null;
    }
  }

  return {
    session,
    archiveId,
    opening,
    error,
    viewer,
    conversationIds,
    conversationTotal,
    conversationsTruncated,
    sort,
    rows,
    windows,
    openChatId,
    openWindow,
    search,
    inspections,
    open,
    close,
    listConversations,
    loadRows,
    openChat,
    loadMore,
    startSearch,
    loadHits,
    cancelSearch,
    inspect,
  };
});
