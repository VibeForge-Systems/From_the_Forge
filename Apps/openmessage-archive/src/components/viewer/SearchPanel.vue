<script setup lang="ts">
// The search panel (M5 §4, §8.4, slice 5i), in the viewer's right panel.
//
// - The query field and the filter inputs have `spellcheck="false"`,
//   `autocomplete="off"` and `autocapitalize="off"`, so no engine service
//   learns the text. Typing is debounced 250 ms; Rust builds the FTS5
//   query (user text never reaches `MATCH`), cancels the previous search
//   before waiting, and answers a superseded one with `searchSuperseded`,
//   which the store drops silently.
// - Mode: words (Rust switches to exact text for CJK, Thai and emoji by
//   itself) or exact text, which is case-, accent- and
//   normalisation-sensitive; the panel says which mode ran.
// - Filters: this conversation or all, a local date range (typed
//   `YYYY-MM-DD`, turned into UTC instants by the injected day-boundary
//   builder: Rust stays zone-free, D104), has an attachment, the sender as
//   recorded, and the two toggles. "Include earlier versions and unsent
//   text" is the explicit reveal for search (D115); it is off by default.
// - The count is announced politely (WCAG 4.1.3), with the scan's progress
//   and Cancel. A capped result says it is not complete, and how.
// - The form stays in the store until the viewer closes; nothing is
//   persisted (owner Q1c). The query is never echoed in a sentence.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { SafeId, SearchFiltersInput, SearchHitDto } from '@/types';
import SearchHit from './SearchHit.vue';
import ConversationLabel from './ConversationLabel.vue';
import { dateFilter } from '@/services/calendar';
import { dayBoundaryFor, type DayBoundaryBuilder } from '@/services/time';
import { SEARCH_RAN_TEXT, SEARCH_TEXT, SENDER_FILTER_TEXT, SCAN_STOP_TEXT, fill } from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { SEARCH_DEBOUNCE_MS, useViewer } from '@/stores/viewer';

const props = defineProps<{ build?: DayBoundaryBuilder }>();
const emit = defineEmits<{ close: []; openChat: [chatId: SafeId] }>();
const viewer = useViewer();
const settings = useSettings();

const query = ref<HTMLInputElement | null>(null);
const dateProblem = ref<'from' | 'to' | 'order' | null>(null);
const cancelled = ref(false);
let timer: ReturnType<typeof setTimeout> | null = null;

const form = computed(() => viewer.searchForm);
const state = computed(() => viewer.search);
const started = computed(() => state.value.started);
const build = computed(() => props.build ?? dayBoundaryFor(settings.viewer.timeZone));

const scopeAvailable = computed(() => form.value.chatId !== null || viewer.openChatId !== null);
const scopeRow = computed(() => (form.value.chatId === null ? null : (viewer.rows.get(form.value.chatId) ?? null)));

function setScope(event: Event): void {
  const on = (event.target as HTMLInputElement).checked;
  viewer.searchForm.chatId = on ? (form.value.chatId ?? viewer.openChatId) : null;
}

function schedule(delay: number): void {
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    run();
  }, delay);
}

function run(): void {
  const f = viewer.searchForm;
  cancelled.value = false;
  if (f.query.trim() === '') {
    dateProblem.value = null;
    void viewer.clearSearch();
    return;
  }
  const range = dateFilter(build.value, f.from, f.to);
  if (!range.ok) {
    dateProblem.value = range.problem;
    return;
  }
  dateProblem.value = null;
  const filters: SearchFiltersInput = {};
  if (f.chatId !== null) filters.chatId = f.chatId;
  if (range.fromUtcMs !== undefined) filters.fromUtcMs = range.fromUtcMs;
  if (range.untilUtcMs !== undefined) filters.untilUtcMs = range.untilUtcMs;
  if (f.hasAttachment) filters.hasAttachment = true;
  if (f.sender !== 'anyone') filters.sender = f.sender;
  void viewer.startSearch({
    query: f.query,
    mode: f.mode,
    filters,
    includeEarlier: f.includeEarlier,
    includeEvents: f.includeEvents,
  });
}

watch(() => [form.value.query, form.value.from, form.value.to], () => schedule(SEARCH_DEBOUNCE_MS));
watch(
  () => [
    form.value.mode,
    form.value.chatId,
    form.value.hasAttachment,
    form.value.sender,
    form.value.includeEarlier,
    form.value.includeEvents,
  ],
  () => schedule(0),
);

function onQueryEnter(): void {
  schedule(0);
}

async function cancel(): Promise<void> {
  if (timer !== null) clearTimeout(timer);
  timer = null;
  await viewer.cancelSearch();
  cancelled.value = true;
}

function focusQuery(): void {
  void nextTick(() => query.value?.focus());
}

onMounted(focusQuery);
watch(() => viewer.searchFocus, focusQuery);
onBeforeUnmount(() => {
  if (timer !== null) clearTimeout(timer);
});

// The conversations the results name, loaded as rows (labels only).
watch(
  () => state.value.hits,
  (hits) => {
    const chats = [...new Set(hits.flatMap((h) => (h.chat === null ? [] : [h.chat])))];
    const missing = chats.filter((c) => !viewer.rows.has(c));
    if (missing.length > 0) void viewer.loadRows(missing);
  },
);

const status = computed(() => {
  const s = state.value;
  if (s.running) {
    return s.progress
      ? fill(SEARCH_TEXT.scanning, { examined: s.progress.examined, matched: s.progress.matched })
      : SEARCH_TEXT.searching;
  }
  if (cancelled.value) return SEARCH_TEXT.cancelled;
  const st = s.started;
  if (!st) return '';
  if (st.capped) return fill(st.newestKept ? SEARCH_TEXT.cappedNewest : SEARCH_TEXT.capped, { n: st.total });
  if (st.total === 0) return SEARCH_TEXT.none;
  return st.total === 1 ? SEARCH_TEXT.one : fill(SEARCH_TEXT.many, { n: st.total });
});

const errorText = computed(() => {
  const s = state.value;
  if (!s.error) return null;
  return s.errorCode === 'queryTooBroad' ? SEARCH_TEXT.tooBroad : s.error;
});

const coverage = computed(() => {
  const c = started.value?.coverage;
  if (!c || c.complete) return null;
  const parts = [fill(SEARCH_TEXT.examinedPartial, { examined: c.examined })];
  if (c.stop) parts.push(SCAN_STOP_TEXT[c.stop]);
  return parts.join(' ');
});

const dated = computed(() => state.value.hits.slice(0, started.value?.dated ?? state.value.hits.length));
const undated = computed(() => state.value.hits.slice(dated.value.length));
const more = computed(() => !!started.value && state.value.hits.length < started.value.total);
const unsearchable = computed(() => viewer.session?.unsearchable ?? 0);
const fromInvalid = computed(() => dateProblem.value === 'from' || dateProblem.value === 'order');
const toInvalid = computed(() => dateProblem.value === 'to' || dateProblem.value === 'order');
const dateMessage = computed(() =>
  dateProblem.value === 'from'
    ? SEARCH_TEXT.badFrom
    : dateProblem.value === 'to'
      ? SEARCH_TEXT.badTo
      : dateProblem.value === 'order'
        ? SEARCH_TEXT.badOrder
        : null,
);

function open(hit: SearchHitDto): void {
  if (hit.chat === null) return;
  viewer.focusOnOpen(hit.chat, hit.message);
  emit('openChat', hit.chat);
}
</script>

<template>
  <div class="oma-search">
    <div class="oma-panel-header">
      <h2 id="oma-search-title" class="oma-panel-title">{{ SEARCH_TEXT.title }}</h2>
      <button type="button" class="btn-close" :aria-label="SEARCH_TEXT.close" @click="emit('close')"></button>
    </div>
    <div class="oma-panel-body px-3 small" role="search" aria-labelledby="oma-search-title">
      <label for="oma-search-q" class="form-label mb-1">{{ SEARCH_TEXT.queryLabel }}</label>
      <input
        id="oma-search-q"
        ref="query"
        v-model="viewer.searchForm.query"
        type="text"
        class="form-control form-control-sm"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        autocorrect="off"
        aria-describedby="oma-search-q-hint"
        @keydown.enter.prevent="onQueryEnter"
      />
      <p id="oma-search-q-hint" class="form-text mt-1">{{ SEARCH_TEXT.queryHint }}</p>

      <div class="oma-search-row">
        <label for="oma-search-mode" class="form-label mb-0">{{ SEARCH_TEXT.modeLabel }}</label>
        <select id="oma-search-mode" v-model="viewer.searchForm.mode" class="form-select form-select-sm">
          <option value="auto">{{ SEARCH_TEXT.modeAuto }}</option>
          <option value="exact">{{ SEARCH_TEXT.modeExact }}</option>
        </select>
      </div>
      <p v-if="viewer.searchForm.mode === 'exact'" class="form-text mt-0">{{ SEARCH_TEXT.exactNote }}</p>

      <fieldset class="oma-search-filters">
        <legend class="fs-6 mb-1">{{ SEARCH_TEXT.filtersLegend }}</legend>
        <div v-if="scopeAvailable" class="form-check">
          <input
            id="oma-search-scope"
            class="form-check-input"
            type="checkbox"
            :checked="viewer.searchForm.chatId !== null"
            @change="setScope"
          />
          <label for="oma-search-scope" class="form-check-label">
            {{ SEARCH_TEXT.scopeLabel }}
            <template v-if="scopeRow && viewer.searchForm.chatId !== null">
              (<ConversationLabel :label="scopeRow.label" :chat-id="viewer.searchForm.chatId" quiet />)
            </template>
          </label>
        </div>
        <div class="oma-search-dates">
          <div>
            <label for="oma-search-from" class="form-label mb-0">{{ SEARCH_TEXT.fromLabel }}</label>
            <input
              id="oma-search-from"
              v-model="viewer.searchForm.from"
              type="text"
              inputmode="numeric"
              class="form-control form-control-sm"
              spellcheck="false"
              autocomplete="off"
              autocapitalize="off"
              autocorrect="off"
              :aria-invalid="fromInvalid"
              aria-describedby="oma-search-date-note"
            />
          </div>
          <div>
            <label for="oma-search-to" class="form-label mb-0">{{ SEARCH_TEXT.toLabel }}</label>
            <input
              id="oma-search-to"
              v-model="viewer.searchForm.to"
              type="text"
              inputmode="numeric"
              class="form-control form-control-sm"
              spellcheck="false"
              autocomplete="off"
              autocapitalize="off"
              autocorrect="off"
              :aria-invalid="toInvalid"
              aria-describedby="oma-search-date-note"
            />
          </div>
        </div>
        <p id="oma-search-date-note" class="form-text mt-1">{{ SEARCH_TEXT.dateNote }}</p>
        <p v-if="dateMessage" class="text-danger mb-1" role="alert">{{ dateMessage }}</p>
        <div class="form-check">
          <input id="oma-search-att" v-model="viewer.searchForm.hasAttachment" class="form-check-input" type="checkbox" />
          <label for="oma-search-att" class="form-check-label">{{ SEARCH_TEXT.attachmentLabel }}</label>
        </div>
        <div class="oma-search-row">
          <label for="oma-search-sender" class="form-label mb-0">{{ SEARCH_TEXT.senderLabel }}</label>
          <select id="oma-search-sender" v-model="viewer.searchForm.sender" class="form-select form-select-sm">
            <option value="anyone">{{ SENDER_FILTER_TEXT.anyone }}</option>
            <option value="me">{{ SENDER_FILTER_TEXT.me }}</option>
            <option value="others">{{ SENDER_FILTER_TEXT.others }}</option>
          </select>
        </div>
        <div class="form-check">
          <input
            id="oma-search-earlier"
            v-model="viewer.searchForm.includeEarlier"
            class="form-check-input"
            type="checkbox"
            aria-describedby="oma-search-earlier-note"
          />
          <label for="oma-search-earlier" class="form-check-label">{{ SEARCH_TEXT.earlierLabel }}</label>
          <p id="oma-search-earlier-note" class="form-text mt-0 mb-1">{{ SEARCH_TEXT.earlierNote }}</p>
        </div>
        <div class="form-check">
          <input id="oma-search-events" v-model="viewer.searchForm.includeEvents" class="form-check-input" type="checkbox" />
          <label for="oma-search-events" class="form-check-label">{{ SEARCH_TEXT.eventsLabel }}</label>
        </div>
      </fieldset>

      <div class="oma-search-status-row">
        <p class="oma-search-status mb-0" role="status">{{ status }}</p>
        <button v-if="state.running" type="button" class="btn btn-sm btn-outline-secondary" @click="cancel">
          {{ SEARCH_TEXT.cancel }}
        </button>
      </div>
      <p v-if="errorText" class="alert alert-warning py-2 my-2" role="alert">{{ errorText }}</p>
      <template v-if="started && !state.running">
        <p class="oma-search-note">{{ fill(SEARCH_TEXT.ranMode, { mode: SEARCH_RAN_TEXT[started.mode] }) }}</p>
        <p v-if="started.mode === 'exact'" class="oma-search-note">{{ SEARCH_TEXT.exactNote }}</p>
        <p v-if="coverage" class="oma-search-note">{{ coverage }}</p>
        <ul v-if="started.total === 0" class="oma-search-hints">
          <li>{{ SEARCH_TEXT.hintPrefix }}</li>
          <li>{{ SEARCH_TEXT.hintScripts }}</li>
          <li>{{ SEARCH_TEXT.hintFilters }}</li>
        </ul>
      </template>
      <p v-if="unsearchable > 0" class="oma-search-note">{{ fill(SEARCH_TEXT.unsearchable, { n: unsearchable }) }}</p>

      <ol v-if="dated.length > 0" class="oma-hits" aria-label="Search results">
        <li v-for="(hit, i) in dated" :key="i">
          <SearchHit :hit="hit" :index="i" :row="hit.chat === null ? null : (viewer.rows.get(hit.chat) ?? null)" @open="open" />
        </li>
      </ol>
      <template v-if="undated.length > 0">
        <h3 id="oma-search-undated" class="oma-hits-heading">{{ SEARCH_TEXT.undatedHeading }}</h3>
        <ol class="oma-hits" aria-labelledby="oma-search-undated">
          <li v-for="(hit, i) in undated" :key="i">
            <SearchHit
              :hit="hit"
              :index="dated.length + i"
              :row="hit.chat === null ? null : (viewer.rows.get(hit.chat) ?? null)"
              @open="open"
            />
          </li>
        </ol>
      </template>
      <p v-if="state.loadingMore" role="status">{{ SEARCH_TEXT.loadingMore }}</p>
      <button
        v-else-if="more && !state.running"
        type="button"
        class="btn btn-sm btn-outline-secondary mb-3"
        @click="viewer.loadMoreHits()"
      >
        {{ SEARCH_TEXT.more }}
      </button>
    </div>
  </div>
</template>
