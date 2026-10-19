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
//
// Reveals (D115, slice 5h2): "Show code" and "Show earlier text" re-read
// one message with `get_message_detail` and the reveal flag, and keep the
// result as an overlay over the page's item (`shown`), never in the page
// itself; "Hide" drops the overlay. Page requests carry a reveal only when
// the `revealEarlierText` setting is on. The full text of a truncated body
// is read with `get_message_text`, chunk by chunk, on request only.
//
// Attachments and the side panel (slice 5h3): `inspect` runs at most
// `MAX_INSPECTING` inspections at once (cards ask when they come into
// view) and caches each answer for the session. Play is gated in three
// steps (§5.6, privacy.md): `inspect_attachment { forPlayback }` (the
// structural check and the full hash, in Rust), then, under `confirm`, the
// user's confirmation for that file in this session, then the media
// viewer. With `off` nothing is asked and Rust refuses anyway. The right
// panel shows the message details, the reply thread or the archive facts.
//
// Search and dates (slice 5i): the search form (query, scope and filters)
// lives here until the viewer closes and is never persisted (owner Q1c);
// the right panel shows the search panel. `dayCounts` and `seekDate` serve
// the date-jump dialog. Requests the global shortcuts make of the open
// thread (the date dialog, the first or newest item) go through
// `dateJumpOpen` and `threadCommand`, which the thread watches.
//
// Privacy (slice 5j): `menu` is the message menu (copy actions, D107) and
// `linkCopy` the "Copy link" confirmation; `closeDialogs` closes every
// viewer dialog for the privacy screen, and `hideContent` asks Rust to drop
// its cached renditions (`hide_viewer`, D120). A pending focus may ask only
// to show a message, not to focus it (the privacy screen's "Show").
import { defineStore } from 'pinia';
import { computed, markRaw, ref, shallowRef } from 'vue';
import { IpcError, ipc } from '@/services/ipc';
import { devNote } from '@/services/devlog';
import { playGate } from '@/services/media';
import { VIEWER_ERROR_TEXT } from '@/services/viewerText';
import type { LinkRun } from '@/services/copyText';
import type {
  AnchorInput,
  AttachmentDto,
  AttachmentInspectionDto,
  ConversationRowDto,
  ConversationSortInput,
  ErrorCode,
  HandleDto,
  MessageDetailDto,
  MessageDto,
  PageRangeDto,
  RevealInput,
  SafeId,
  ScanProgressDto,
  SearchModeInput,
  SenderFilterInput,
  SeekDirInput,
  SearchHitDto,
  SearchMessagesRequest,
  SearchStartedDto,
  TextPieceDto,
  ThreadPageDto,
  WithheldReasonDto,
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
/** Messages kept re-read with a reveal (D115). */
export const MAX_REVEALED = 100;
/** Bodies whose full text is kept loaded. */
export const MAX_FULL_TEXTS = 20;
/** Inspections in flight at once (§8.4). */
export const MAX_INSPECTING = 4;
/** Debounce of the search field (§2). */
export const SEARCH_DEBOUNCE_MS = 250;

/** What the right panel shows. */
export type SidePanel =
  | { kind: 'details'; message: SafeId; reactions: boolean }
  | { kind: 'replies'; message: SafeId }
  | { kind: 'search' };

/** The search form, kept until the viewer closes (never persisted). */
export interface SearchForm {
  query: string;
  mode: SearchModeInput;
  /** Only this conversation; `null` searches all of them. */
  chatId: SafeId | null;
  /** Typed `YYYY-MM-DD` dates, inclusive; empty for no bound. */
  from: string;
  to: string;
  hasAttachment: boolean;
  sender: SenderFilterInput;
  includeEarlier: boolean;
  includeEvents: boolean;
}

export const emptySearchForm = (): SearchForm => ({
  query: '',
  mode: 'auto',
  chatId: null,
  from: '',
  to: '',
  hasAttachment: false,
  sender: 'anyone',
  includeEarlier: false,
  includeEvents: false,
});

/** A request the global shortcuts make of the open thread. */
export interface ThreadCommand {
  kind: 'first' | 'last';
  /** Bumped per request, so the same request twice is seen twice. */
  n: number;
}

/** The attachment the media viewer shows, and how. */
export interface MediaOpen {
  attachment: AttachmentDto;
  mode: 'image' | 'play';
}

/** A message re-read with a reveal, shown over the page's copy. */
export interface RevealedMessage {
  reveal: RevealInput;
  message: MessageDto;
}

/** The full text of a truncated body, read chunk by chunk. */
export interface FullText {
  pieces: TextPieceDto[];
  /** UTF-16 units read so far, and the body's length. */
  end: number;
  full: number;
  /** The text is withheld from here on (D115), or there is none. */
  withheld: WithheldReasonDto | null;
  none: boolean;
  loading: boolean;
  error: string | null;
}

/** A message to show in a chat that is about to open (a reply's originator). */
export interface PendingFocus {
  chatId: SafeId;
  message: SafeId;
  /** Move keyboard focus to it as well (false: only show it). */
  focus: boolean;
}

/** The message menu, opened on a message (context menu, Shift+F10). */
export interface MessageMenuState {
  /** The message as its bubble shows it (the revealed copy when there is one). */
  message: MessageDto;
  handles: ReadonlyMap<SafeId, HandleDto>;
  /** Where to open it, in CSS px from the viewport's top left. */
  x: number;
  y: number;
  /** The article's element id (app-made), to return focus to. */
  returnTo: string;
}

/** The "Copy link" confirmation: the exact string, or a refusal. */
export interface LinkCopyState {
  link: LinkRun;
  returnTo: string;
}

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
  loadingMore: boolean;
  error: string | null;
  errorCode: ErrorCode | null;
}

const emptySearch = (): SearchState => ({
  searchId: null,
  started: null,
  hits: [],
  progress: null,
  running: false,
  loadingMore: false,
  error: null,
  errorCode: null,
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
  const searchForm = ref<SearchForm>(emptySearchForm());
  /** Bumped to ask the search panel to focus its query field. */
  const searchFocus = ref(0);
  /** Open the search panel once the next session opens (the Archive page's button). */
  let searchWhenOpened = false;
  const dateJumpOpen = ref(false);
  const threadCommand = ref<ThreadCommand | null>(null);
  let commands = 0;
  const inspections = shallowRef(new Map<SafeId, AttachmentInspectionDto>());
  const revealed = shallowRef(new Map<SafeId, RevealedMessage>());
  const fullTexts = shallowRef(new Map<SafeId, FullText>());
  const pendingFocus = ref<PendingFocus | null>(null);
  const menu = shallowRef<MessageMenuState | null>(null);
  const linkCopy = shallowRef<LinkCopyState | null>(null);

  const panel = shallowRef<SidePanel | null>(null);
  const media = shallowRef<MediaOpen | null>(null);
  /** The attachment waiting for the play confirmation (`confirm`). */
  const pendingPlay = shallowRef<AttachmentDto | null>(null);
  /** Attachments whose Play check is running. */
  const checking = shallowRef(new Set<SafeId>());
  /** Attachments the user confirmed for playback in this session. */
  let confirmed = new Set<SafeId>();
  let inspecting = 0;
  const waiting: (() => void)[] = [];

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
    searchForm.value = emptySearchForm();
    dateJumpOpen.value = false;
    threadCommand.value = null;
    inspections.value = new Map();
    revealed.value = new Map();
    fullTexts.value = new Map();
    pendingFocus.value = null;
    menu.value = null;
    linkCopy.value = null;
    panel.value = null;
    media.value = null;
    pendingPlay.value = null;
    checking.value = new Set();
    confirmed = new Set();
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
      if (searchWhenOpened) openSearch(null);
      return true;
    } catch (e) {
      error.value = failed(e);
      return false;
    } finally {
      opening.value = false;
      searchWhenOpened = false;
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
    // Details and replies describe a message of the conversation that was
    // open, with its addresses; they close when another one opens (seen in
    // the slice 5k smoke test). The search panel stays.
    if (openChatId.value !== chatId && panel.value !== null && panel.value.kind !== 'search') {
      panel.value = null;
    }
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
      search.value.errorCode = e instanceof IpcError ? e.code : null;
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
    if (offset > 0) search.value.loadingMore = true;
    try {
      const got = await ipc.getSearchResults({ viewer: v, searchId: id, offset, limit: HITS_PER_REQUEST });
      if (seq !== searchSeq) return;
      // Hits are immutable DTOs: kept raw, like page items.
      search.value.hits = markRaw(offset === 0 ? got.hits : [...search.value.hits, ...got.hits]);
    } catch (e) {
      if (seq !== searchSeq) return;
      if (e instanceof IpcError && e.code === 'searchSuperseded') return;
      search.value.errorCode = e instanceof IpcError ? e.code : null;
      search.value.error = failed(e);
    } finally {
      if (seq === searchSeq) search.value.loadingMore = false;
    }
  }

  /** The next page of hits, when more are kept than loaded. */
  async function loadMoreHits(): Promise<void> {
    const s = search.value;
    if (s.running || s.loadingMore || !s.started || s.hits.length >= s.started.total) return;
    await loadHits(s.hits.length);
  }

  /** Clear the results (an empty query), cancelling a running search. */
  async function clearSearch(): Promise<void> {
    if (search.value.running || search.value.searchId !== null) await cancelSearch();
    else search.value = emptySearch();
  }

  /** Show the search panel, scoped to one conversation or to all. */
  function openSearch(scope: SafeId | null): void {
    searchForm.value = { ...searchForm.value, chatId: scope };
    panel.value = { kind: 'search' };
    searchFocus.value += 1;
  }

  /** Open the search panel in archive `id`'s viewer, now or once its session opens. */
  function searchOnOpen(id: string): void {
    if (session.value && archiveId.value === id) openSearch(null);
    else searchWhenOpened = true;
  }

  /** Ask the open thread to jump to its first or newest item. */
  function requestThread(kind: ThreadCommand['kind']): void {
    threadCommand.value = { kind, n: (commands += 1) };
  }

  /**
   * Visible items per interval of `bounds` (2 to 43 UTC instants, local
   * day boundaries built in the webview, D104).
   */
  async function dayCounts(chatId: SafeId, bounds: number[]): Promise<{ counts: number[] } | { error: string } | null> {
    const v = viewer.value;
    if (v === null) return null;
    try {
      const got = await ipc.getDayCounts({ viewer: v, chatId, boundsUtcMs: bounds });
      if (viewer.value !== v) return null;
      return { counts: got.counts };
    } catch (e) {
      return { error: failed(e) };
    }
  }

  /** The first visible dated item at or after an instant, or the last before it. */
  async function seekDate(
    chatId: SafeId,
    utcMs: number,
    dir: SeekDirInput,
  ): Promise<{ id: SafeId | null } | { error: string } | null> {
    const v = viewer.value;
    if (v === null) return null;
    try {
      const got = await ipc.seekDate({ viewer: v, chatId, utcMs, dir });
      if (viewer.value !== v) return null;
      return { id: got.messageId };
    } catch (e) {
      return { error: failed(e) };
    }
  }

  /**
   * Stop the search: a scan still running is named by its progress
   * (`searchId` arrives with the first batch); a finished one is forgotten
   * in Rust. A words search that has not answered yet cannot be named, so
   * it runs out (bounded by its 5 s deadline) and its answer is dropped.
   */
  async function cancelSearch(): Promise<void> {
    const v = viewer.value;
    const id = search.value.searchId ?? search.value.progress?.searchId ?? null;
    searchSeq += 1;
    search.value = emptySearch();
    if (v === null || id === null) return;
    try {
      await ipc.cancelSearch({ viewer: v, searchId: id });
    } catch {
      // Cancelling a finished or superseded search is not an error.
    }
  }

  async function slot(): Promise<void> {
    if (inspecting < MAX_INSPECTING) {
      inspecting += 1;
      return;
    }
    await new Promise<void>((resolve) => waiting.push(resolve));
  }

  function release(): void {
    const next = waiting.shift();
    if (next) next();
    else inspecting = Math.max(0, inspecting - 1);
  }

  /**
   * Inspect a surfaced attachment, cached per session. `fresh` asks again
   * (the original size is known only after a preview was made). At most
   * `MAX_INSPECTING` run at once.
   */
  async function inspect(
    attachmentId: SafeId,
    forPlayback = false,
    fresh = false,
  ): Promise<AttachmentInspectionDto | null> {
    const v = viewer.value;
    if (v === null) return null;
    const cached = inspections.value.get(attachmentId);
    if (!fresh && cached && (!forPlayback || cached.playback !== null)) return cached;
    await slot();
    try {
      if (viewer.value !== v) return null;
      const got = await ipc.inspectAttachment({ viewer: v, attachmentId, forPlayback });
      if (viewer.value !== v) return null;
      const next = new Map(inspections.value);
      // A plain inspection never forgets a playback verdict already given.
      const before = next.get(attachmentId);
      const kept = !forPlayback && before?.playback ? { ...got, playback: before.playback } : got;
      next.delete(attachmentId);
      next.set(attachmentId, kept);
      while (next.size > MAX_INSPECTIONS) next.delete(next.keys().next().value!);
      inspections.value = next;
      return kept;
    } catch (e) {
      error.value = failed(e);
      return null;
    } finally {
      release();
    }
  }

  /** Open the media viewer on an image's preview. */
  function openImage(attachment: AttachmentDto): void {
    media.value = { attachment, mode: 'image' };
  }

  function closeMedia(): void {
    media.value = null;
  }

  /**
   * Play: Rust's checks first (the structural check and the full hash),
   * then the confirmation under `confirm`, then the media viewer.
   */
  async function requestPlay(attachment: AttachmentDto): Promise<void> {
    const setting = useSettings().viewer.mediaPlayback;
    if (setting === 'off' || checking.value.has(attachment.id)) return;
    const busy = new Set(checking.value);
    busy.add(attachment.id);
    checking.value = busy;
    let got: AttachmentInspectionDto | null;
    try {
      got = await inspect(attachment.id, true);
    } finally {
      const done = new Set(checking.value);
      done.delete(attachment.id);
      checking.value = done;
    }
    if (!got || got.playback?.kind !== 'allowed') return;
    const gate = playGate(useSettings().viewer.mediaPlayback, confirmed.has(attachment.id));
    if (gate === 'none') return;
    if (gate === 'confirm') {
      pendingPlay.value = attachment;
      return;
    }
    media.value = { attachment, mode: 'play' };
  }

  /** The answer to the play confirmation. */
  async function answerPlay(answer: 'play' | 'cancel' | 'always'): Promise<void> {
    const attachment = pendingPlay.value;
    pendingPlay.value = null;
    if (!attachment || answer === 'cancel') return;
    if (answer === 'always') await useSettings().setViewer({ mediaPlayback: 'clickToPlay' });
    confirmed.add(attachment.id);
    media.value = { attachment, mode: 'play' };
  }

  /** Show a message's details (and its reaction rows) in the right panel. */
  function openDetails(message: SafeId, reactions = false): void {
    panel.value = { kind: 'details', message, reactions };
  }

  function openReplies(message: SafeId): void {
    panel.value = { kind: 'replies', message };
  }

  function closePanel(): void {
    panel.value = null;
  }

  /**
   * Read one message's details. The reveal the user already asked for that
   * message is repeated, never added; `reactionsAfter` adds a page of the
   * reaction history (`null`: the first).
   */
  async function loadDetail(
    messageId: SafeId,
    reactionsAfter?: SafeId | null,
  ): Promise<{ detail: MessageDetailDto } | { error: string } | null> {
    const v = viewer.value;
    if (v === null) return null;
    const reveal = revealed.value.get(messageId)?.reveal;
    try {
      const detail = await ipc.getMessageDetail({
        viewer: v,
        messageId,
        ...(reveal && (reveal.earlierText || reveal.oneTimeCode) ? { reveal } : {}),
        ...(reactionsAfter === undefined ? {} : { reactionPage: reactionsAfter === null ? {} : { after: reactionsAfter } }),
      });
      if (viewer.value !== v) return null;
      return { detail: markRaw(detail) };
    } catch (e) {
      return { error: failed(e) };
    }
  }

  /** Read the reply thread of an originator. */
  async function loadReplies(messageId: SafeId): Promise<{ page: ThreadPageDto } | { error: string } | null> {
    const v = viewer.value;
    if (v === null) return null;
    const r = reveal();
    try {
      const page = await ipc.getReplyThread({ viewer: v, messageId, ...(r ? { reveal: r } : {}) });
      if (viewer.value !== v) return null;
      return { page: markRaw(page) };
    } catch (e) {
      return { error: failed(e) };
    }
  }

  /** The message as shown: the revealed copy when there is one. */
  function shown(m: MessageDto): MessageDto {
    return revealed.value.get(m.id)?.message ?? m;
  }

  /**
   * Re-read one message with a reveal added (D115): "Show code" or "Show
   * earlier text". Nothing is revealed until the user asks. Returns the
   * error sentence, or null.
   */
  async function revealMessage(id: SafeId, add: RevealInput): Promise<string | null> {
    const v = viewer.value;
    if (v === null) return null;
    const before = revealed.value.get(id)?.reveal ?? {};
    const reveal: RevealInput = {
      ...(before.earlierText || add.earlierText ? { earlierText: true } : {}),
      ...(before.oneTimeCode || add.oneTimeCode ? { oneTimeCode: true } : {}),
    };
    try {
      const detail = await ipc.getMessageDetail({ viewer: v, messageId: id, reveal });
      if (viewer.value !== v) return null;
      const next = new Map(revealed.value);
      next.delete(id);
      next.set(id, { reveal, message: markRaw(detail.message) });
      while (next.size > MAX_REVEALED) next.delete(next.keys().next().value!);
      revealed.value = next;
      return null;
    } catch (e) {
      return failed(e);
    }
  }

  /** "Hide": drop the revealed copy, or only one of its reveals. */
  async function unreveal(id: SafeId, drop: RevealInput): Promise<string | null> {
    const kept = revealed.value.get(id);
    if (!kept) return null;
    const rest: RevealInput = {
      ...(kept.reveal.earlierText && !drop.earlierText ? { earlierText: true } : {}),
      ...(kept.reveal.oneTimeCode && !drop.oneTimeCode ? { oneTimeCode: true } : {}),
    };
    const next = new Map(revealed.value);
    next.delete(id);
    revealed.value = next;
    // Drop the full text read under the old reveal as well.
    const body = kept.message.body;
    if (body.kind === 'text' && fullTexts.value.has(body.body)) {
      const texts = new Map(fullTexts.value);
      texts.delete(body.body);
      fullTexts.value = texts;
    }
    if (rest.earlierText || rest.oneTimeCode) return revealMessage(id, rest);
    return null;
  }

  function setFullText(bodyId: SafeId, value: FullText): void {
    const next = new Map(fullTexts.value);
    next.delete(bodyId);
    next.set(bodyId, value);
    while (next.size > MAX_FULL_TEXTS) next.delete(next.keys().next().value!);
    fullTexts.value = next;
  }

  /**
   * Read the next chunk of a truncated body's text (from the start, then
   * from where the last chunk ended). Codes stay masked unless the
   * message's code was revealed.
   */
  async function loadFullText(bodyId: SafeId, messageId: SafeId): Promise<void> {
    const v = viewer.value;
    if (v === null) return;
    const have = fullTexts.value.get(bodyId);
    if (have && (have.loading || have.withheld !== null || have.none || have.end >= have.full)) return;
    const from = have?.end ?? 0;
    const base: FullText = have ?? { pieces: [], end: 0, full: 0, withheld: null, none: false, loading: false, error: null };
    setFullText(bodyId, { ...base, loading: true, error: null });
    const codes = revealed.value.get(messageId)?.reveal.oneTimeCode === true;
    try {
      const chunk = await ipc.getMessageText({
        viewer: v,
        bodyId,
        fromUtf16: from,
        ...(codes ? { reveal: { oneTimeCode: true } } : {}),
      });
      if (viewer.value !== v) return;
      const now = fullTexts.value.get(bodyId) ?? base;
      const next: FullText = { ...now, loading: false, error: null, full: chunk.fullUtf16 };
      if (chunk.text.kind === 'text') {
        next.pieces = markRaw([...now.pieces, ...chunk.text.pieces]);
        next.end = Math.max(chunk.endUtf16, now.end);
        // A chunk that ends where it started has nothing more to give.
        if (chunk.endUtf16 <= from) next.none = true;
      } else if (chunk.text.kind === 'withheld') {
        next.withheld = chunk.text.reason;
      } else {
        next.none = true;
      }
      setFullText(bodyId, next);
    } catch (e) {
      const now = fullTexts.value.get(bodyId) ?? base;
      setFullText(bodyId, { ...now, loading: false, error: failed(e) });
    }
  }

  /** Remember a message to show (and focus) when `chatId` opens; taken once. */
  function focusOnOpen(chatId: SafeId, message: SafeId, focus = true): void {
    pendingFocus.value = { chatId, message, focus };
  }

  /** The pending message of `chatId`, taken once. */
  function takePending(chatId: SafeId): PendingFocus | null {
    const p = pendingFocus.value;
    if (!p || p.chatId !== chatId) return null;
    pendingFocus.value = null;
    return p;
  }

  function takePendingFocus(chatId: SafeId): SafeId | null {
    return takePending(chatId)?.message ?? null;
  }

  function openMenu(state: MessageMenuState): void {
    linkCopy.value = null;
    menu.value = state;
  }

  function closeMenu(): void {
    menu.value = null;
  }

  function openLinkCopy(state: LinkCopyState): void {
    menu.value = null;
    linkCopy.value = state;
  }

  function closeLinkCopy(): void {
    linkCopy.value = null;
  }

  /**
   * Close every viewer dialog (the privacy screen): the media viewer (its
   * element unmounts, which stops it), the play confirmation (nothing
   * plays), the date dialog, the message menu and the link confirmation.
   */
  function closeDialogs(): void {
    media.value = null;
    pendingPlay.value = null;
    dateJumpOpen.value = false;
    menu.value = null;
    linkCopy.value = null;
  }

  /** Ask Rust to drop the cached renditions (`hide_viewer`, D120). */
  async function hideContent(): Promise<void> {
    const v = viewer.value;
    if (v === null) return;
    try {
      await ipc.hideViewer(v);
    } catch {
      // A closed session has no renditions left either way.
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
    searchForm,
    searchFocus,
    dateJumpOpen,
    threadCommand,
    inspections,
    revealed,
    fullTexts,
    pendingFocus,
    menu,
    linkCopy,
    panel,
    media,
    pendingPlay,
    checking,
    open,
    close,
    listConversations,
    loadRows,
    openChat,
    loadMore,
    startSearch,
    loadHits,
    loadMoreHits,
    clearSearch,
    cancelSearch,
    openSearch,
    searchOnOpen,
    requestThread,
    dayCounts,
    seekDate,
    inspect,
    openImage,
    closeMedia,
    requestPlay,
    answerPlay,
    openDetails,
    openReplies,
    closePanel,
    loadDetail,
    loadReplies,
    shown,
    revealMessage,
    unreveal,
    loadFullText,
    focusOnOpen,
    takePending,
    takePendingFocus,
    openMenu,
    closeMenu,
    openLinkCopy,
    closeLinkCopy,
    closeDialogs,
    hideContent,
  };
});
