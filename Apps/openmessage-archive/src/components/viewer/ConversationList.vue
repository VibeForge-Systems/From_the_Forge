<script setup lang="ts">
// The conversation list (M5 §8.4, §10): a `listbox` over the id snapshot
// from `list_conversations`, virtualised with VirtualFeed, that loads the
// rows of the visible window with `get_conversations`. Focus stays on the
// listbox; the active option is named by `aria-activedescendant` (not a
// roving tabindex) and is kept mounted so the reference never dangles.
// Up and Down, Page Up and Page Down, Home and End move; Enter or Space
// opens. Option ids are built from the position in the snapshot, never
// from content.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import type { ConversationSortInput, SafeId } from '@/types';
import type { FeedHandle, FeedRange } from '@/services/feedModel';
import VirtualFeed from './VirtualFeed.vue';
import ConversationRow from './ConversationRow.vue';
import { useViewer } from '@/stores/viewer';
import { LIST_TEXT, SORT_TEXT, VIEWER_STATE_TEXT, fill } from '@/services/viewerText';

const props = defineProps<{ selected: SafeId | null }>();
const emit = defineEmits<{ open: [chatId: SafeId] }>();
const viewer = useViewer();

/** Typing in the filter waits this long before asking Rust. */
const FILTER_DEBOUNCE_MS = 250;
const ROW_LOAD_DELAY_MS = 50;
/** Rows loaded beyond each edge of the visible window. */
const ROW_MARGIN = 20;
const PAGE_STEP = 10;
const ROW_ESTIMATE = 76;

const feed = ref<FeedHandle<SafeId> | null>(null);
const active = ref(0);
const filter = ref('');
const sort = ref<ConversationSortInput>('recent');
const listed = ref(false);

const ids = computed(() => viewer.conversationIds);
const activeId = computed(() => ids.value[active.value] ?? null);
const activeOptionId = computed(() => (activeId.value === null ? undefined : optionId(active.value)));
const keep = computed(() => (activeId.value === null ? [] : [activeId.value]));
const sorts = Object.keys(SORT_TEXT) as ConversationSortInput[];

function optionId(index: number): string {
  return `oma-conv-opt-${index}`;
}

const keyOf = (id: SafeId) => id;

async function reload(): Promise<void> {
  const text = filter.value.trim();
  await viewer.listConversations(sort.value, text === '' ? null : text);
  listed.value = true;
}

watch(
  () => viewer.viewer,
  (v) => {
    listed.value = false;
    if (v !== null) void reload();
  },
  { immediate: true },
);

let filterTimer: ReturnType<typeof setTimeout> | null = null;
watch(filter, () => {
  if (filterTimer !== null) clearTimeout(filterTimer);
  filterTimer = setTimeout(() => {
    filterTimer = null;
    void reload();
  }, FILTER_DEBOUNCE_MS);
});
watch(sort, () => void reload());

watch(ids, (list) => {
  const i = props.selected === null ? -1 : list.indexOf(props.selected);
  active.value = i >= 0 ? i : 0;
  void nextTick(() => feed.value?.scrollToIndex(active.value, i >= 0 ? 'center' : 'start'));
});

watch(
  () => props.selected,
  (sel) => {
    if (sel === null) return;
    const i = ids.value.indexOf(sel);
    if (i >= 0) active.value = i;
  },
);

let rowTimer: ReturnType<typeof setTimeout> | null = null;
let wanted: FeedRange<SafeId> | null = null;

function onVisible(range: FeedRange<SafeId>): void {
  wanted = range;
  if (rowTimer !== null) return;
  rowTimer = setTimeout(() => {
    rowTimer = null;
    const r = wanted;
    if (!r) return;
    const from = Math.max(0, r.firstIndex - ROW_MARGIN);
    const to = Math.min(ids.value.length, r.lastIndex + 1 + ROW_MARGIN);
    const chunk = ids.value.slice(from, to);
    if (activeId.value !== null && !chunk.includes(activeId.value)) chunk.push(activeId.value);
    void viewer.loadRows(chunk);
  }, ROW_LOAD_DELAY_MS);
}

onBeforeUnmount(() => {
  if (filterTimer !== null) clearTimeout(filterTimer);
  if (rowTimer !== null) clearTimeout(rowTimer);
});

function move(index: number): void {
  const n = ids.value.length;
  if (n === 0) return;
  active.value = Math.min(Math.max(index, 0), n - 1);
  void nextTick(() => feed.value?.scrollToIndex(active.value, 'nearest'));
}

function openActive(): void {
  const id = activeId.value;
  if (id !== null) emit('open', id);
}

function choose(index: number): void {
  move(index);
  openActive();
}

function onKeydown(event: KeyboardEvent): void {
  if (event.ctrlKey || event.altKey || event.metaKey) return;
  const n = ids.value.length;
  if (n === 0) return;
  switch (event.key) {
    case 'ArrowDown':
      move(active.value + 1);
      break;
    case 'ArrowUp':
      move(active.value - 1);
      break;
    case 'PageDown':
      move(active.value + PAGE_STEP);
      break;
    case 'PageUp':
      move(active.value - PAGE_STEP);
      break;
    case 'Home':
      move(0);
      break;
    case 'End':
      move(n - 1);
      break;
    case 'Enter':
    case ' ':
      openActive();
      break;
    default:
      return;
  }
  event.preventDefault();
}
</script>

<template>
  <div class="oma-conv-list">
    <div class="oma-conv-tools">
      <label for="oma-conv-filter" class="visually-hidden">{{ LIST_TEXT.filterLabel }}</label>
      <input
        id="oma-conv-filter"
        v-model="filter"
        type="search"
        class="form-control form-control-sm"
        placeholder="Filter"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        aria-describedby="oma-conv-filter-hint"
      />
      <span id="oma-conv-filter-hint" class="visually-hidden">{{ LIST_TEXT.filterHint }}</span>
      <label for="oma-conv-sort" class="visually-hidden">{{ LIST_TEXT.sortLabel }}</label>
      <select id="oma-conv-sort" v-model="sort" class="form-select form-select-sm">
        <option v-for="s in sorts" :key="s" :value="s">{{ SORT_TEXT[s] }}</option>
      </select>
    </div>
    <p v-if="viewer.conversationsTruncated" class="oma-conv-note">
      {{ fill(LIST_TEXT.truncated, { n: ids.length, total: viewer.conversationTotal }) }}
    </p>
    <p v-if="listed && ids.length === 0" class="oma-conv-note" role="status">
      {{ filter.trim() === '' ? VIEWER_STATE_TEXT.noConversations : LIST_TEXT.noMatch }}
    </p>
    <VirtualFeed
      ref="feed"
      class="oma-conv-feed"
      :items="ids"
      :item-key="keyOf"
      :estimate="ROW_ESTIMATE"
      :keep-mounted="keep"
      role="listbox"
      tabindex="0"
      aria-label="Conversations"
      :aria-activedescendant="activeOptionId"
      @keydown="onKeydown"
      @visible="onVisible"
    >
      <template #default="{ item, index }">
        <div
          :id="optionId(index)"
          role="option"
          class="oma-conv-option"
          :class="{ 'oma-active': index === active, 'oma-selected': item === selected }"
          :aria-selected="item === selected"
          :aria-posinset="index + 1"
          :aria-setsize="ids.length"
          @click="choose(index)"
        >
          <ConversationRow v-if="viewer.rows.get(item)" :row="viewer.rows.get(item)!" />
          <span v-else class="oma-missing">{{ LIST_TEXT.loadingRow }}</span>
        </div>
      </template>
    </VirtualFeed>
  </div>
</template>
