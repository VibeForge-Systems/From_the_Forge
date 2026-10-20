<script setup lang="ts">
// The thread (M5 §8.4, §10): a header with the conversation's recorded
// facts, then the APG feed of its items, virtualised with VirtualFeed and
// paged by anchor through `stores/viewer.ts`.
//
// - It opens at the newest **dated** item; "Jump to first" opens the
//   oldest, and "Undated items (N)" opens the undated section, which is
//   read in source order and never placed among dated items.
// - Older and newer pages load when the viewport comes within the feed's
//   edge distance of an end (well before scrollTop 0, and the feed
//   compensates prepends itself). A failed page stops loading until Retry.
// - Notices: an import running, an anchor an import moved, the focus
//   reason, a partial or size-cut page, unreadable extras, skipped rows.
// - Keyboard: Page Down/Up and Down/Up move focus between articles; the
//   focused article is kept mounted.
// - Items (5h2): sender labels above runs when the conversation has more
//   than one recorded address, service dividers, and a reply's jump to its
//   originator: here, by opening a page around it and focusing it; in
//   another conversation, by asking the view to open that conversation
//   (`openChat`) with the originator remembered in the store, never in
//   the URL.
// - Dates and search (5i): "Jump to date…" opens the date dialog (also
//   Ctrl+G, through the store); a picked day opens a page around its first
//   visible item and focuses it. "Search this conversation" opens the
//   search panel scoped here (also Ctrl+F). A search result opened in this
//   conversation arrives as a pending focus and is handled like a reply
//   jump. Ctrl+Home and Ctrl+End (through `threadCommand`) open the first
//   or newest page and focus its end. On a focused article, Enter shows
//   its details and R its reply thread (`scope: 'message'` shortcuts,
//   matched here only).
// - Privacy (5j): Shift+F10 on a focused article opens its message menu
//   (as the context menu does). While mounted, the thread tells the privacy
//   screen where it is (the focused message, or the first in view), so
//   "Show" returns there; an open asked with `focus: false` shows the
//   message without moving keyboard focus.
// - Review fixes: the header's first date is the first of 2007 or later,
//   with "and N earlier-dated items" (§3.6), and the time-zone note; a
//   reload around an anchor an import moved keeps focus on that message;
//   the conversation's row is loaded again if the row cache dropped it;
//   an open at an end never re-centres an earlier jump's focus.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { AnchorInput, MessageDto, SafeId } from '@/types';
import type { FeedHandle } from '@/services/feedModel';
import VirtualFeed from './VirtualFeed.vue';
import ThreadItem from './ThreadItem.vue';
import { runStarts, serviceChanges } from '@/services/messageView';
import ConversationLabel from './ConversationLabel.vue';
import DateJumpDialog from './DateJumpDialog.vue';
import { matchShortcut } from '@/services/shortcuts';
import { parseSafeId } from '@/services/ids';
import { dayKey, formatInstant, isBefore2007, timeText } from '@/services/time';
import {
  BEFORE_2007_TEXT,
  DATE_JUMP_TEXT,
  FOCUS_REASON_TEXT,
  LIST_TEXT,
  SERVICE_TEXT,
  THREAD_TEXT,
  TIME_ZONE_NOTE,
  TIME_ZONE_NOTE_UTC,
  VIEWER_STATE_TEXT,
  fill,
} from '@/services/viewerText';
import { ENTRY_TEXT } from '@/services/exportText';
import { useExports } from '@/stores/exports';
import { usePrivacy, type ThreadPlace } from '@/stores/privacy';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{ chatId: SafeId }>();
const emit = defineEmits<{ openChat: [chatId: SafeId] }>();
const viewer = useViewer();
const exports = useExports();

/** "Export conversation…" (M6 §10): the dialog with this conversation pre-selected. */
function exportConversation(): void {
  const archiveId = viewer.archiveId;
  if (archiveId !== null) exports.openExport({ archiveId, chatId: props.chatId, scope: 'conversation' });
}
const settings = useSettings();
const privacy = usePrivacy();

const ITEM_ESTIMATE = 72;
const ARTICLE_ID_PREFIX = 'oma-m-';

type Align = 'start' | 'end';

const feed = ref<FeedHandle<SafeId> | null>(null);
const focusedId = ref<SafeId | null>(null);
/**
 * The alignment an open still owes (an object, so a newer open is told
 * apart); `focus` moves keyboard focus to the focused message once shown.
 */
let pendingAlign: { align: Align; focus: boolean } | null = null;
let lastOpen: { anchor: AnchorInput; align: Align } = { anchor: { kind: 'newest' }, align: 'end' };
let lastEdge: 'older' | 'newer' | null = null;

const win = computed(() => {
  const w = viewer.openWindow;
  return w && w.chatId === props.chatId ? w : null;
});
const items = computed<MessageDto[]>(() => (win.value ? win.value.pages.flatMap((p) => p.items) : []));
const row = computed(() => viewer.rows.get(props.chatId) ?? null);
const notices = computed(() => win.value?.notices ?? null);
const loading = computed(() => win.value?.loading ?? null);
const keep = computed(() => (focusedId.value === null ? [] : [focusedId.value]));
const keyOf = (m: MessageDto) => m.id;

const addresses = computed(() => {
  const n = row.value?.addresses;
  if (n === undefined) return null;
  return n === 1 ? THREAD_TEXT.oneAddress : fill(THREAD_TEXT.addresses, { n });
});

const recorded = computed(() => {
  const s = row.value?.stats;
  return s ? fill(THREAD_TEXT.itemsRecorded, { n: s.itemsRecorded }) : null;
});

/**
 * The dated range (§3.6): from the first item dated 2007 or later to the
 * last dated item (both live), and "and N earlier-dated items" for dates
 * before 2007, which are never used silently as the first date.
 */
const dates = computed(() => {
  const r = row.value;
  if (!r) return null;
  const date = (ms: number) => formatInstant(ms, settings.viewer, 'date');
  const parts: string[] = [];
  const first = r.firstDated2007Ms;
  const last = r.lastDatedMs;
  if (first !== null && last !== null) {
    parts.push(fill(THREAD_TEXT.dateRange, { first: date(first), last: date(last) }));
  } else if (last !== null) {
    const text = fill(THREAD_TEXT.lastDatedOnly, { last: date(last) });
    parts.push(isBefore2007(last) ? `${text} (${BEFORE_2007_TEXT})` : text);
  }
  const e = r.earlierDated;
  if (e.n > 0) {
    parts.push(
      fill(e.capped ? THREAD_TEXT.earlierDatedCapped : e.n === 1 ? THREAD_TEXT.earlierDatedOne : THREAD_TEXT.earlierDated, {
        n: e.n,
      }),
    );
  }
  return parts.length > 0 ? parts.join(' ') : null;
});

/** No zone is recorded in the archive: say which one the times are shown in (§8.5). */
const zoneNote = computed(() => (settings.viewer.timeZone === 'utc' ? TIME_ZONE_NOTE_UTC : TIME_ZONE_NOTE));

const undated = computed(() => {
  const u = notices.value?.undatedCount ?? row.value?.undated ?? null;
  if (!u || u.n === 0) return null;
  return fill(u.capped ? THREAD_TEXT.undatedButtonCapped : THREAD_TEXT.undatedButton, { n: u.n });
});

const inUndated = computed(() => win.value?.range === 'undated');

/** "Showing items around D1 – D2": the loaded window's dated ends. */
const showing = computed(() => {
  const dated = items.value.filter((m) => timeText(m.date).kind === 'instant');
  const first = dated[0]?.date.ms;
  const last = dated[dated.length - 1]?.date.ms;
  if (first == null || last == null) return null;
  return fill(VIEWER_STATE_TEXT.showingWindow, {
    from: formatInstant(first, settings.viewer, 'date'),
    to: formatInstant(last, settings.viewer, 'date'),
  });
});

/** The separator text for each item that starts a new local day. */
const dayLabels = computed(() => {
  const out = new Map<SafeId, string>();
  let previous = '';
  for (const m of items.value) {
    const t = timeText(m.date, settings.viewer);
    if (t.kind !== 'instant') continue;
    const key = dayKey(t.ms, settings.viewer.timeZone);
    if (key !== previous) {
      const day = formatInstant(t.ms, settings.viewer, 'date');
      out.set(m.id, t.before2007 ? `${day} (${BEFORE_2007_TEXT})` : day);
      previous = key;
    }
  }
  return out;
});

/** Sender labels show above runs only with more than one recorded address. */
const several = computed(() => (row.value?.addresses ?? 0) > 1);
const starts = computed(() => runStarts(items.value, dayLabels.value));
const changes = computed(() => serviceChanges(items.value));

/** Where the date dialog starts: the focused item, the window's newest dated item, or the chat's last. */
const dateStart = computed<number | null>(() => {
  const focused = focusedId.value === null ? null : items.value.find((m) => m.id === focusedId.value);
  if (focused) {
    const t = timeText(focused.date);
    if (t.kind === 'instant') return t.ms;
  }
  for (let i = items.value.length - 1; i >= 0; i -= 1) {
    const t = timeText(items.value[i]!.date);
    if (t.kind === 'instant') return t.ms;
  }
  return row.value?.lastDatedMs ?? row.value?.stats?.lastMs ?? null;
});

const empty = computed(
  () => !!win.value && win.value.loading === null && !win.value.error && win.value.pages.length > 0 && items.value.length === 0,
);

function open(anchor: AnchorInput, align: Align, focus = false): void {
  pendingAlign = { align, focus };
  lastOpen = { anchor, align };
  lastEdge = null;
  // A window kept from an earlier visit may change nothing the watcher
  // below sees, so align once the open has settled as well.
  void viewer.openChat(props.chatId, anchor).then(alignOpened);
}

// The row cache is bounded: when it drops this conversation's row, load
// it again (the header, the sender labels and the undated count use it).
watch(
  () => viewer.rows.has(props.chatId),
  (has) => {
    if (!has && viewer.session) void viewer.loadRows([props.chatId]);
  },
);

watch(
  () => props.chatId,
  (id) => {
    focusedId.value = null;
    if (!viewer.rows.has(id)) void viewer.loadRows([id]);
    const target = viewer.takePending(id);
    if (target !== null) open({ kind: 'message', message: target.message, include: true }, 'end', target.focus);
    else open({ kind: 'newest' }, 'end');
  },
  { immediate: true },
);

// Once the page an open asked for is in, show its anchor: the focused
// message, or the requested end. Edge loads wait until this is done, so
// the feed's report from before the alignment (at the top) cannot load a
// page nobody asked for.
let aligning = false;
watch([loading, items], () => alignOpened());

async function alignOpened(): Promise<void> {
  const w = win.value;
  if (pendingAlign === null || aligning || !w || w.loading !== null) return;
  const pending = pendingAlign;
  aligning = true;
  await nextTick();
  const f = feed.value;
  // A page's focus reason belongs to an open at a message; an open at an
  // end (Jump to newest, Ctrl+End, a kept window) never re-centres it.
  const focus = lastOpen.anchor.kind === 'message' ? w.notices?.focus : null;
  const shown = !!focus && items.value.some((m) => m.id === focus.id);
  if (f && focus && shown) f.scrollToKey(focus.id, 'center');
  else if (f && pending.align === 'start') f.scrollToStart();
  else if (f) f.scrollToEnd();
  // An open at a message focuses that message only; an open at an end
  // (Ctrl+Home, Ctrl+End) focuses the end's item.
  const target =
    focus && shown
      ? focus.id
      : lastOpen.anchor.kind === 'message'
        ? null
        : pending.align === 'start'
        ? (items.value[0]?.id ?? null)
        : (items.value[items.value.length - 1]?.id ?? null);
  if (pending.focus && target !== null) {
    focusedId.value = target;
    await nextTick();
    document.getElementById(`${ARTICLE_ID_PREFIX}${target}`)?.focus({ preventScroll: true });
  }
  if (pendingAlign === pending) pendingAlign = null;
  aligning = false;
}

// An import moved an anchor and the store reloaded the window around it:
// show that message and keep keyboard focus there (§3.5).
watch(
  () => win.value?.reloaded?.n,
  async () => {
    const r = win.value?.reloaded;
    if (!r) return;
    const had = focusedId.value;
    const target = had !== null && items.value.some((m) => m.id === had) ? had : r.around;
    if (!items.value.some((m) => m.id === target)) return;
    focusedId.value = target;
    await nextTick();
    feed.value?.scrollToKey(target, 'center');
    const active = document.activeElement;
    const lost = active === null || active === document.body;
    const inFeed = active instanceof Element && active.closest('.oma-thread-feed') !== null;
    if (had !== null && (lost || inFeed)) {
      await nextTick();
      document.getElementById(`${ARTICLE_ID_PREFIX}${target}`)?.focus({ preventScroll: true });
    }
  },
);

function canLoad(): boolean {
  const w = win.value;
  return pendingAlign === null && !!w && w.loading === null && w.error === null && w.pages.length > 0;
}

function onNearStart(): void {
  if (!canLoad()) return;
  lastEdge = 'older';
  void viewer.loadMore('older', feed.value?.firstVisibleKey() ?? null);
}

function onNearEnd(): void {
  if (!canLoad()) return;
  lastEdge = 'newer';
  void viewer.loadMore('newer', feed.value?.firstVisibleKey() ?? null);
}

/** Retry goes away with the error: keep focus in the thread, not on `<body>`. */
function keepFocusAfterRetry(): void {
  void nextTick(() => {
    const active = document.activeElement;
    if (active !== null && active !== document.body) return;
    const at = focusedId.value === null ? null : document.getElementById(`${ARTICLE_ID_PREFIX}${focusedId.value}`);
    (at ?? document.getElementById('oma-thread-title'))?.focus({ preventScroll: true });
  });
}

function retry(): void {
  keepFocusAfterRetry();
  const w = win.value;
  if (!w || w.pages.length === 0 || lastEdge === null) {
    open(lastOpen.anchor, lastOpen.align);
    return;
  }
  void viewer.loadMore(lastEdge, feed.value?.firstVisibleKey() ?? null);
}

function articleId(target: EventTarget | null): SafeId | null {
  if (!(target instanceof Element)) return null;
  const article = target.closest('article.oma-thread-item');
  if (!article || !article.id.startsWith(ARTICLE_ID_PREFIX)) return null;
  const id = parseSafeId(article.id.slice(ARTICLE_ID_PREFIX.length));
  return id !== null && items.value.some((m) => m.id === id) ? id : null;
}

function onFocusin(event: FocusEvent): void {
  const id = articleId(event.target);
  if (id !== null) focusedId.value = id;
}

// A search result opened in this conversation (another conversation's
// remounts this view, and the watch on `chatId` takes it).
watch(
  () => viewer.pendingFocus,
  (p) => {
    if (!p || p.chatId !== props.chatId) return;
    const target = viewer.takePending(props.chatId);
    if (target !== null) open({ kind: 'message', message: target.message, include: true }, 'end', target.focus);
  },
);

/** Where the privacy screen returns to: the focused article, or the first in view. */
function place(): ThreadPlace | null {
  const active = document.activeElement;
  const inFeed = active instanceof Element && active.closest('.oma-thread-feed') !== null;
  const focused = inFeed ? articleId(active) : null;
  if (focused !== null) return { chatId: props.chatId, message: focused, focus: true };
  return { chatId: props.chatId, message: feed.value?.firstVisibleKey() ?? focusedId.value, focus: false };
}
onMounted(() => privacy.setPlaceProvider(place));
onBeforeUnmount(() => privacy.setPlaceProvider(null, place));

// Ctrl+Home and Ctrl+End, asked through the store by the global handler.
watch(
  () => viewer.threadCommand,
  (c) => {
    if (!c || viewer.openChatId !== props.chatId) return;
    if (c.kind === 'first') open({ kind: 'oldest' }, 'start', true);
    else open({ kind: 'newest' }, 'end', true);
  },
);

function onDateJump(messageId: SafeId): void {
  viewer.dateJumpOpen = false;
  open({ kind: 'message', message: messageId, include: true }, 'end', true);
}

/** A reply's quote: open around its originator, here or in its conversation. */
function onJump(originator: SafeId, chat: SafeId | null): void {
  if (chat === null || chat === props.chatId) {
    open({ kind: 'message', message: originator }, 'end', true);
    return;
  }
  viewer.focusOnOpen(chat, originator);
  emit('openChat', chat);
}

const NEXT_KEYS: Record<string, number> = { PageDown: 1, ArrowDown: 1, PageUp: -1, ArrowUp: -1 };

function onFeedKeydown(event: KeyboardEvent): void {
  if (event.target instanceof Element && event.target.matches('article.oma-thread-item')) {
    const shortcut = matchShortcut(event, undefined, 'message');
    const id = shortcut ? articleId(event.target) : null;
    if (shortcut && id !== null) {
      event.preventDefault();
      const back = `${ARTICLE_ID_PREFIX}${id}`;
      if (shortcut.id === 'viewer-details') viewer.openDetails(id, false, back);
      else if (shortcut.id === 'viewer-reply-thread') viewer.openReplies(id, back);
      // The same path as the context menu: the article opens its menu.
      else event.target.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
      return;
    }
  }
  if (event.ctrlKey || event.altKey || event.metaKey) return;
  const delta = NEXT_KEYS[event.key];
  if (delta === undefined) return;
  const id = articleId(event.target);
  if (id === null) return;
  const index = items.value.findIndex((m) => m.id === id);
  const next = items.value[index + delta];
  event.preventDefault();
  if (!next) return;
  focusedId.value = next.id;
  feed.value?.scrollToIndex(index + delta, 'nearest');
  void nextTick(() => document.getElementById(`${ARTICLE_ID_PREFIX}${next.id}`)?.focus({ preventScroll: true }));
}
</script>

<template>
  <div class="oma-thread">
    <header class="oma-thread-head">
      <h2 id="oma-thread-title" class="oma-thread-title" tabindex="-1">
        <ConversationLabel v-if="row" :label="row.label" :chat-id="chatId" />
        <template v-else>{{ LIST_TEXT.loadingRow }}</template>
      </h2>
      <p class="oma-thread-facts">
        <span v-if="addresses">{{ addresses }}</span>
        <span v-if="row?.service">{{ SERVICE_TEXT[row.service] }}</span>
        <span v-if="recorded">{{ recorded }}</span>
        <span v-if="dates">{{ dates }}</span>
        <span class="oma-thread-zone">{{ zoneNote }}</span>
      </p>
      <div class="oma-thread-actions">
        <template v-if="inUndated">
          <span class="oma-thread-range">{{ THREAD_TEXT.undatedHeading }}</span>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="open({ kind: 'newest' }, 'end')">
            {{ THREAD_TEXT.backToDated }}
          </button>
        </template>
        <template v-else>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="open({ kind: 'oldest' }, 'start')">
            {{ THREAD_TEXT.jumpFirst }}
          </button>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="open({ kind: 'newest' }, 'end')">
            {{ THREAD_TEXT.jumpNewest }}
          </button>
          <button v-if="undated" type="button" class="btn btn-sm btn-outline-secondary" @click="open({ kind: 'undated' }, 'start')">
            {{ undated }}
          </button>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="viewer.dateJumpOpen = true">
            {{ DATE_JUMP_TEXT.open }}
          </button>
          <span v-if="showing" class="oma-thread-range">{{ showing }}</span>
        </template>
        <button type="button" class="btn btn-sm btn-outline-secondary" @click="viewer.openSearch(chatId)">
          {{ THREAD_TEXT.searchConversation }}
        </button>
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary"
          :disabled="viewer.archiveId === null"
          @click="exportConversation"
        >
          {{ ENTRY_TEXT.conversation }}
        </button>
      </div>
    </header>

    <div class="oma-thread-notices">
      <p v-if="viewer.session?.importRunning" class="oma-notice">{{ VIEWER_STATE_TEXT.importRunning }}</p>
      <p v-if="notices?.anchorMoved" class="oma-notice" role="status">{{ VIEWER_STATE_TEXT.anchorMoved }}</p>
      <p v-if="notices?.focus" class="oma-notice">{{ FOCUS_REASON_TEXT[notices.focus.reason] }}</p>
      <p v-if="notices?.partial" class="oma-notice">{{ VIEWER_STATE_TEXT.partial }}</p>
      <p v-if="notices?.cutForSize" class="oma-notice">{{ THREAD_TEXT.cutForSize }}</p>
      <p v-if="notices?.extrasIncomplete" class="oma-notice">{{ VIEWER_STATE_TEXT.extrasIncomplete }}</p>
      <p v-if="(notices?.skippedRows ?? 0) > 0" class="oma-notice">{{ VIEWER_STATE_TEXT.skippedRows }}</p>
    </div>

    <p v-if="loading === 'older' || loading === 'initial'" class="oma-thread-loading" role="status">
      {{ loading === 'initial' ? THREAD_TEXT.loadingFirst : THREAD_TEXT.loadingOlder }}
    </p>
    <div v-if="win?.error" class="oma-thread-error" role="alert">
      <span>{{ VIEWER_STATE_TEXT.pageFailed }} {{ win.error }}</span>
      <button type="button" class="btn btn-sm btn-outline-secondary" @click="retry">{{ THREAD_TEXT.retry }}</button>
    </div>
    <p v-if="empty" class="oma-thread-empty">{{ VIEWER_STATE_TEXT.emptyChat }}</p>

    <VirtualFeed
      ref="feed"
      class="oma-thread-feed"
      :items="items"
      :item-key="keyOf"
      :estimate="ITEM_ESTIMATE"
      :keep-mounted="keep"
      role="feed"
      aria-labelledby="oma-thread-title"
      :aria-busy="loading !== null"
      @near-start="onNearStart"
      @near-end="onNearEnd"
      @keydown="onFeedKeydown"
      @focusin="onFocusin"
    >
      <template #default="{ item }">
        <ThreadItem
          :message="item"
          :handles="win?.handles ?? new Map()"
          :day-label="dayLabels.get(item.id) ?? null"
          :show-sender="several && starts.has(item.id)"
          :service-change="changes.get(item.id)"
          @jump="onJump"
        />
      </template>
    </VirtualFeed>
    <p v-if="loading === 'newer'" class="oma-thread-loading" role="status">{{ THREAD_TEXT.loadingNewer }}</p>
    <DateJumpDialog
      :open="viewer.dateJumpOpen"
      :chat-id="chatId"
      :initial="dateStart"
      @close="viewer.dateJumpOpen = false"
      @jump="onDateJump"
    />
  </div>
</template>
