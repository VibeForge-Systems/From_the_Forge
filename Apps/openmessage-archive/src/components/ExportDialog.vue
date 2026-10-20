<script setup lang="ts">
// The Export dialog (M6 spec §10), mounted once in `ArchiveDialogs.vue`.
// Its state is `stores/exports.ts`; its words are `services/exportText.ts`.
// Each step is a `fieldset` with a `legend`, and focus moves to the step's
// first control. No path is ever shown: the destination is the name Rust
// returns, rendered by `SafeText`. A format this version cannot write
// (XLSX) is shown disabled with its reason, never as a working control.
// "Save a copy" of one attachment (slice 6k2) uses the same dialog from
// its Review step: the attachment's facts and the disclosure, then where
// to save, progress and the result. While an export runs, "Continue in
// background" closes the dialog and the navbar's chip reopens it.
import { computed, nextTick, ref, watch } from 'vue';
import JobProgress from '@/components/JobProgress.vue';
import ModalDialog from '@/components/ModalDialog.vue';
import SafeText from '@/components/viewer/SafeText.vue';
import { untrustedContent } from '@/services/safeText';
import {
  ALL_MATCHES,
  ASK_CANCEL,
  BACKGROUND_NOTE,
  BACK,
  CANCEL_EXPORT,
  CANCEL_COUNT,
  CANCEL_NOTE,
  CHOOSE_ANOTHER,
  CHOOSE_ANOTHER_FOLDER,
  CHOOSE_DESTINATION,
  CLOCK_LABEL,
  CLOSE,
  CONTINUE_IN_BACKGROUND,
  COPY_CANCEL,
  COPY_DISCLOSURE,
  COPY_EARLIER_CONFIRM,
  COPY_FACTS_READY,
  COPY_LEGEND,
  COPY_RESULT_LEGEND,
  COPY_READING,
  COPY_RESULT,
  COPY_RISKY_PREFIX,
  COPY_RISKY_SUFFIX,
  COPY_SAVED,
  COPY_SAVES_AS,
  COPY_START,
  COPY_TITLE,
  COUNTING,
  COUNTS_READY,
  CSV_DATA_TOOLS_WARNING,
  CSV_PROFILE_LABEL,
  DATE_ALL,
  DATE_FROM,
  DATE_HELP,
  DATE_INVALID,
  DATE_THIS,
  DATE_THROUGH,
  DISCLOSURE,
  DIALOG_TITLE,
  DONE,
  EVENTS_NOTE,
  FORMAT_NEEDED,
  FORMAT_ORDER,
  FORMAT_TEXT,
  FORMAT_UNAVAILABLE,
  KEEP_EXPORTING,
  KEEP_EXTENSIONS_CONFIRM,
  LINE_ENDINGS_LABEL,
  NEXT,
  OPTION_GROUP,
  OPTION_TEXT,
  OUTCOME_TEXT,
  PACKAGING_TEXT,
  PAGE_SIZE_LABEL,
  PREPARING_ZONE,
  REPLACE,
  REPLACE_CONFIRMED,
  REPLACE_PROMPT_PREFIX,
  REPLACE_PROMPT_SUFFIX,
  RESULT_LEGEND,
  REVEAL_NONE,
  REVIEW_AGAIN,
  SAVES_AS,
  SCOPE_LABEL,
  SEARCH_CAPPED,
  SHOW_IN_FOLDER,
  START,
  STALE_NOTICE,
  STEP_LEGEND,
  USE_UTC,
  ZONE_CHOICE_LABEL,
  ZONE_FAILED,
  cancelledSentence,
  copySizeLine,
  failedNote,
  freeSpaceLine,
  leftoversLine,
  optionsSummary,
  resultNotes,
  resultSentence,
  revealCodesConfirm,
  revealEarlierConfirm,
  reviewLines,
  searchLines,
  searchScopeLine,
  warningText,
  zoneSentence,
  type ExportStep,
} from '@/services/exportText';
import { AVAILABLE_FORMATS, useExports } from '@/stores/exports';
import type { AttachmentModeInput, ExportFormatInput } from '@/types';

const x = useExports();
const body = ref<HTMLElement | null>(null);
const nf = new Intl.NumberFormat();

const f = computed(() => x.form);
const ctx = computed(() => x.context);
const legend = computed(() => {
  // The result's legend says how the job ended, not only that it did.
  if (x.step === 'result' && x.result) return (x.jobCopy ? COPY_RESULT_LEGEND : RESULT_LEGEND)[x.result.kind];
  return x.isCopy ? (COPY_LEGEND[x.step] ?? STEP_LEGEND[x.step]) : STEP_LEGEND[x.step];
});
const dialogTitle = computed(() => (x.isCopy ? COPY_TITLE : DIALOG_TITLE));

const attachmentsModel = computed<AttachmentModeInput>({
  get: () => x.attachments,
  set: (v) => {
    if (x.form) x.form.attachments = v;
  },
});
const copies = computed(() => x.attachments === 'copyOriginals');
const dateInvalid = computed(
  () => f.value?.scope === 'dateRange' && f.value.fromDate !== '' && f.value.throughDate !== '' && x.dateRange === null,
);

function formatAvailable(fmt: ExportFormatInput): boolean {
  return AVAILABLE_FORMATS.includes(fmt);
}

function toggleFormat(fmt: ExportFormatInput, on: boolean): void {
  const form = x.form;
  if (!form || !formatAvailable(fmt)) return;
  const rest = form.formats.filter((v) => v !== fmt);
  form.formats = on ? [...rest, fmt] : rest;
}

const summaryLine = computed(() => {
  const form = f.value;
  if (!form) return '';
  return optionsSummary({
    revealCodes: form.revealOneTimeCodes,
    revealEarlier: form.revealEarlierContent,
    copies: copies.value,
    zone: form.zone,
    clock: form.clock,
  });
});

const displayName = computed(() => (x.destination ? untrustedContent(x.destination.displayName) : null));
const informational = computed(() => (x.destination?.warnings ?? []).filter((w) => !x.warningsToConfirm.includes(w)));

const countStatus = computed(() => {
  const c = x.count;
  if (x.isCopy && (c.kind === 'counting' || c.kind === 'idle')) return COPY_READING;
  if (c.kind === 'counting') return c.rows > 0 ? `${COUNTING} (${nf.format(c.rows)} rows so far)` : COUNTING;
  if (c.kind === 'zone') return PREPARING_ZONE;
  // Said once the counts arrive, so the consent-critical lines are
  // announced, not met in silence (review fix).
  if (c.kind === 'ready') return COUNTS_READY;
  if (c.kind === 'copyReady') return COPY_FACTS_READY;
  return '';
});

const showsZoneChoice = computed(() => f.value?.zone !== 'utc');

const NEXT_STEP: Partial<Record<ExportStep, ExportStep>> = { what: 'format', format: 'options', options: 'review' };
const PREV_STEP: Partial<Record<ExportStep, ExportStep>> = {
  format: 'what',
  options: 'format',
  review: 'options',
  destination: 'review',
};
const canNext = computed(() => {
  switch (x.step) {
    case 'what':
      return x.whatValid;
    case 'format':
      return x.formatValid;
    case 'options':
      return x.optionsValid;
    default:
      return false;
  }
});

function next(): void {
  const to = NEXT_STEP[x.step];
  if (to && canNext.value) x.goTo(to);
}

/** "Save a copy" starts at Review: there is nothing before it. */
const canBack = computed(() => x.step !== 'what' && !(x.isCopy && x.step === 'review'));

function back(): void {
  const to = PREV_STEP[x.step];
  if (to && canBack.value) x.goTo(to);
}

/** Cancel while counting: back to Options, or, for a copy, close. */
function cancelCount(): void {
  if (x.isCopy) x.close();
  else x.goTo('options');
}

/** Move focus to the step's first enabled control. */
async function focusStep(): Promise<void> {
  await nextTick();
  const set = body.value?.querySelector('fieldset');
  const first = set?.querySelector<HTMLElement>(
    'input:not([disabled]), select:not([disabled]), button:not([disabled]), textarea:not([disabled])',
  );
  first?.focus({ preventScroll: true });
}

watch(
  () => [x.open, x.step] as const,
  ([open]) => {
    if (open) void focusStep();
  },
);

// The counting block (and the Cancel button that had focus) goes away
// when the count ends: focus moves to the Review step's first control
// rather than falling to <body> (review fix).
watch(
  () => x.count.kind,
  (kind) => {
    if (x.open && x.step === 'review' && kind !== 'counting' && kind !== 'zone' && kind !== 'idle') void focusStep();
  },
);

/** "Keep exporting": the question goes away with its focused button. */
function keepExporting(): void {
  x.askCancel = false;
  void focusStep();
}

/** "Replace": its buttons give way to a sentence; focus goes to Start. */
async function confirmReplace(): Promise<void> {
  x.replaceConfirmed = true;
  await nextTick();
  const start = body.value?.querySelector<HTMLButtonElement>('#export-start');
  if (start && !start.disabled) start.focus({ preventScroll: true });
  else void focusStep();
}
</script>

<template>
  <ModalDialog :open="x.open" :title="dialogTitle" guard-escape @close="x.requestClose()">
    <div v-if="x.open && f && ctx" ref="body" class="oma-export">
      <div v-if="x.askCancel" class="alert alert-warning" role="alert">
        <p class="mb-2">{{ ASK_CANCEL }}</p>
        <div class="d-flex flex-wrap gap-2">
          <button type="button" class="btn btn-sm btn-danger" @click="x.cancelExport()">{{ CANCEL_EXPORT }}</button>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="keepExporting()">
            {{ KEEP_EXPORTING }}
          </button>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="x.continueInBackground()">
            {{ CONTINUE_IN_BACKGROUND }}
          </button>
        </div>
      </div>

      <!-- 1. What -->
      <fieldset v-if="x.step === 'what'">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <div v-if="ctx.chatId" class="form-check">
          <input id="export-scope-conversation" v-model="f.scope" class="form-check-input" type="radio" value="conversation" />
          <label class="form-check-label" for="export-scope-conversation">{{ SCOPE_LABEL.conversation }}</label>
        </div>
        <div v-if="ctx.search" class="form-check">
          <input id="export-scope-search" v-model="f.scope" class="form-check-input" type="radio" value="searchResults" />
          <label class="form-check-label" for="export-scope-search">{{
            searchScopeLine(ctx.search.matches, ctx.search.conversations)
          }}</label>
          <div v-if="ctx.search.capped" class="ms-1 mt-1">
            <p class="small text-body-secondary mb-1">{{ SEARCH_CAPPED }}</p>
            <div class="form-check">
              <input
                id="export-all-matches"
                v-model="f.allMatches"
                class="form-check-input"
                type="checkbox"
                :disabled="f.scope !== 'searchResults'"
              />
              <label class="form-check-label" for="export-all-matches">{{ ALL_MATCHES }}</label>
            </div>
          </div>
        </div>
        <div class="form-check">
          <input id="export-scope-dates" v-model="f.scope" class="form-check-input" type="radio" value="dateRange" />
          <label class="form-check-label" for="export-scope-dates">{{ SCOPE_LABEL.dateRange }}</label>
        </div>
        <div v-if="f.scope === 'dateRange'" class="ms-4 mb-2">
          <p id="export-date-help" class="small text-body-secondary mb-1">{{ DATE_HELP }}</p>
          <div class="d-flex flex-wrap gap-2 align-items-end">
            <div>
              <label class="form-label small mb-0" for="export-date-from">{{ DATE_FROM }}</label>
              <input
                id="export-date-from"
                v-model="f.fromDate"
                class="form-control form-control-sm"
                type="date"
                min="1900-01-01"
                max="9999-12-31"
                spellcheck="false"
                autocomplete="off"
                aria-describedby="export-date-help"
                :aria-invalid="dateInvalid"
              />
            </div>
            <div>
              <label class="form-label small mb-0" for="export-date-through">{{ DATE_THROUGH }}</label>
              <input
                id="export-date-through"
                v-model="f.throughDate"
                class="form-control form-control-sm"
                type="date"
                min="1900-01-01"
                max="9999-12-31"
                spellcheck="false"
                autocomplete="off"
                aria-describedby="export-date-help"
                :aria-invalid="dateInvalid"
              />
            </div>
          </div>
          <p v-if="dateInvalid" class="small text-danger mb-1" role="alert">{{ DATE_INVALID }}</p>
          <div v-if="ctx.chatId" class="mt-1">
            <div class="form-check form-check-inline">
              <input
                id="export-dates-all"
                v-model="f.dateThisConversation"
                class="form-check-input"
                type="radio"
                :value="false"
              />
              <label class="form-check-label" for="export-dates-all">{{ DATE_ALL }}</label>
            </div>
            <div class="form-check form-check-inline">
              <input
                id="export-dates-this"
                v-model="f.dateThisConversation"
                class="form-check-input"
                type="radio"
                :value="true"
              />
              <label class="form-check-label" for="export-dates-this">{{ DATE_THIS }}</label>
            </div>
          </div>
        </div>
        <div class="form-check">
          <input id="export-scope-archive" v-model="f.scope" class="form-check-input" type="radio" value="archive" />
          <label class="form-check-label" for="export-scope-archive">{{ SCOPE_LABEL.archive }}</label>
        </div>
      </fieldset>

      <!-- 2. Formats -->
      <fieldset v-else-if="x.step === 'format'">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <div v-for="fmt in FORMAT_ORDER" :key="fmt" class="form-check mb-1">
          <input
            :id="`export-format-${fmt}`"
            class="form-check-input"
            type="checkbox"
            :checked="f.formats.includes(fmt)"
            :disabled="!formatAvailable(fmt)"
            :aria-describedby="`export-format-${fmt}-note`"
            @change="toggleFormat(fmt, ($event.target as HTMLInputElement).checked)"
          />
          <label class="form-check-label" :for="`export-format-${fmt}`">{{ FORMAT_TEXT[fmt].name }}</label>
          <div :id="`export-format-${fmt}-note`" class="small text-body-secondary">
            {{ FORMAT_UNAVAILABLE[fmt] ?? FORMAT_TEXT[fmt].bestFor }}
          </div>
        </div>
        <p v-if="!x.formatValid" class="small text-body-secondary mb-0">{{ FORMAT_NEEDED }}</p>
      </fieldset>

      <!-- 3. Options -->
      <fieldset v-else-if="x.step === 'options'">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <p class="mb-2">{{ summaryLine }}</p>
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary mb-2"
          aria-controls="export-options"
          :aria-expanded="x.optionsOpen"
          @click="x.optionsOpen = !x.optionsOpen"
        >
          {{ x.optionsOpen ? OPTION_TEXT.hideOptions : OPTION_TEXT.change }}
        </button>
        <div v-show="x.optionsOpen" id="export-options">
          <h3 class="fs-6 mt-2">{{ OPTION_GROUP.attachments }}</h3>
          <div class="form-check">
            <input id="export-att-none" v-model="attachmentsModel" class="form-check-input" type="radio" value="none" />
            <label class="form-check-label" for="export-att-none">{{ OPTION_TEXT.namesOnly }}</label>
          </div>
          <div class="form-check">
            <input
              id="export-att-copy"
              v-model="attachmentsModel"
              class="form-check-input"
              type="radio"
              value="copyOriginals"
            />
            <label class="form-check-label" for="export-att-copy">{{ OPTION_TEXT.copyOriginals }}</label>
          </div>
          <div class="form-check">
            <input
              id="export-previews"
              v-model="f.previews"
              class="form-check-input"
              type="checkbox"
              :disabled="!x.previewsApply"
            />
            <label class="form-check-label" for="export-previews">{{ OPTION_TEXT.previews }}</label>
            <span v-if="!x.previewsApply" class="small text-body-secondary">{{ ' ' }}{{ OPTION_TEXT.previewsOnlyFor }}</span>
          </div>
          <div class="form-check ms-3">
            <input
              id="export-larger-previews"
              v-model="f.largerPreviews"
              class="form-check-input"
              type="checkbox"
              :disabled="!x.previewsApply || !f.previews"
            />
            <label class="form-check-label" for="export-larger-previews">{{ OPTION_TEXT.largerPreviews }}</label>
          </div>
          <div class="form-check">
            <input
              id="export-keep-extensions"
              v-model="f.keepExtensions"
              class="form-check-input"
              type="checkbox"
              :disabled="!copies"
              aria-describedby="export-keep-extensions-note"
            />
            <label class="form-check-label" for="export-keep-extensions">{{ OPTION_TEXT.keepExtensions }}</label>
            <div id="export-keep-extensions-note" class="small text-body-secondary">
              {{ OPTION_TEXT.keepExtensionsNote }}
              <template v-if="!copies">{{ ' ' }}{{ OPTION_TEXT.keepExtensionsOnlyWithCopies }}</template>
            </div>
          </div>

          <h3 class="fs-6 mt-3">{{ OPTION_GROUP.content }}</h3>
          <div class="form-check">
            <input id="export-history" v-model="f.history" class="form-check-input" type="checkbox" />
            <label class="form-check-label" for="export-history">{{ OPTION_TEXT.history }}</label>
          </div>
          <div class="form-check">
            <input id="export-reactions" v-model="f.reactions" class="form-check-input" type="checkbox" />
            <label class="form-check-label" for="export-reactions">{{ OPTION_TEXT.reactions }}</label>
          </div>
          <div class="form-check">
            <input
              id="export-events"
              v-model="f.events"
              class="form-check-input"
              type="checkbox"
              aria-describedby="export-events-note"
            />
            <label class="form-check-label" for="export-events">{{ OPTION_TEXT.events }}</label>
            <div id="export-events-note" class="small text-body-secondary">{{ EVENTS_NOTE }}</div>
          </div>

          <h3 class="fs-6 mt-3">{{ OPTION_GROUP.sensitive }}</h3>
          <p id="export-reveal-note" class="small text-body-secondary mb-1">{{ OPTION_TEXT.revealNote }}</p>
          <div class="form-check">
            <input
              id="export-reveal-codes"
              v-model="f.revealOneTimeCodes"
              class="form-check-input"
              type="checkbox"
              aria-describedby="export-reveal-note"
            />
            <label class="form-check-label" for="export-reveal-codes">{{ OPTION_TEXT.revealCodes }}</label>
          </div>
          <div class="form-check">
            <input
              id="export-reveal-earlier"
              v-model="f.revealEarlierContent"
              class="form-check-input"
              type="checkbox"
              aria-describedby="export-reveal-note"
            />
            <label class="form-check-label" for="export-reveal-earlier">{{ OPTION_TEXT.revealEarlier }}</label>
          </div>

          <h3 class="fs-6 mt-3">{{ OPTION_GROUP.time }}</h3>
          <div class="form-check">
            <input
              id="export-zone-system"
              v-model="f.zone"
              class="form-check-input"
              type="radio"
              value="system"
              :disabled="x.systemZone === null"
            />
            <label class="form-check-label" for="export-zone-system">
              {{ ZONE_CHOICE_LABEL.system }}<template v-if="x.systemZone !== null">{{ ' ' }}({{ x.systemZone }})</template>
            </label>
          </div>
          <p v-if="x.systemZone === null" class="small text-body-secondary mb-1">{{ OPTION_TEXT.noSystemZone }}</p>
          <div class="form-check">
            <input id="export-zone-utc" v-model="f.zone" class="form-check-input" type="radio" value="utc" />
            <label class="form-check-label" for="export-zone-utc">{{ ZONE_CHOICE_LABEL.utc }}</label>
          </div>
          <div class="form-check">
            <input
              id="export-zone-named"
              v-model="f.zone"
              class="form-check-input"
              type="radio"
              value="named"
              :disabled="x.zones === null"
            />
            <label class="form-check-label" for="export-zone-named">{{ ZONE_CHOICE_LABEL.named }}</label>
          </div>
          <p v-if="x.zones === null" class="small text-body-secondary mb-1">{{ OPTION_TEXT.noZoneList }}</p>
          <div v-if="f.zone === 'named' && x.zones" class="ms-4 mb-2">
            <label class="form-label small mb-0" for="export-zone-name">{{ OPTION_TEXT.namedZone }}</label>
            <select id="export-zone-name" v-model="f.namedZone" class="form-select form-select-sm">
              <option value="" disabled>—</option>
              <option v-for="z in x.zones" :key="z" :value="z">{{ z }}</option>
            </select>
          </div>
          <div class="d-flex flex-wrap gap-3 align-items-end">
            <div>
              <label class="form-label small mb-0" for="export-clock">{{ OPTION_TEXT.clock }}</label>
              <select id="export-clock" v-model="f.clock" class="form-select form-select-sm">
                <option value="h23">{{ CLOCK_LABEL.h23 }}</option>
                <option value="h12">{{ CLOCK_LABEL.h12 }}</option>
              </select>
            </div>
            <div class="form-check mb-1">
              <input id="export-seconds" v-model="f.seconds" class="form-check-input" type="checkbox" />
              <label class="form-check-label" for="export-seconds">{{ OPTION_TEXT.seconds }}</label>
            </div>
          </div>

          <h3 class="fs-6 mt-3">{{ OPTION_GROUP.text }}</h3>
          <div class="form-check">
            <input id="export-markers" v-model="f.hiddenCharacterMarkers" class="form-check-input" type="checkbox" />
            <label class="form-check-label" for="export-markers">{{ OPTION_TEXT.hiddenMarkers }}</label>
          </div>
          <div v-if="f.formats.includes('txt')" class="mt-1">
            <label class="form-label small mb-0" for="export-line-endings">{{ OPTION_TEXT.lineEndings }}</label>
            <select id="export-line-endings" v-model="f.lineEndings" class="form-select form-select-sm">
              <option value="lf">{{ LINE_ENDINGS_LABEL.lf }}</option>
              <option value="crlf">{{ LINE_ENDINGS_LABEL.crlf }}</option>
            </select>
          </div>

          <template v-if="f.formats.includes('csv')">
            <h3 class="fs-6 mt-3">{{ OPTION_GROUP.spreadsheet }}</h3>
            <div class="form-check">
              <input
                id="export-csv-spreadsheet"
                v-model="f.csvProfile"
                class="form-check-input"
                type="radio"
                value="spreadsheet"
              />
              <label class="form-check-label" for="export-csv-spreadsheet">{{ CSV_PROFILE_LABEL.spreadsheet }}</label>
            </div>
            <div class="form-check">
              <input id="export-csv-data" v-model="f.csvProfile" class="form-check-input" type="radio" value="dataTools" />
              <label class="form-check-label" for="export-csv-data">{{ CSV_PROFILE_LABEL.dataTools }}</label>
            </div>
            <p v-if="f.csvProfile === 'dataTools'" class="alert alert-warning small py-1 px-2 mb-1">
              {{ CSV_DATA_TOOLS_WARNING }}
            </p>
          </template>

          <template v-if="f.formats.includes('pdf')">
            <h3 class="fs-6 mt-3">{{ OPTION_GROUP.pdf }}</h3>
            <label class="form-label small mb-0" for="export-page-size">{{ OPTION_TEXT.pageSize }}</label>
            <select id="export-page-size" v-model="f.pageSize" class="form-select form-select-sm">
              <option value="a4">{{ PAGE_SIZE_LABEL.a4 }}</option>
              <option value="letter">{{ PAGE_SIZE_LABEL.letter }}</option>
            </select>
          </template>

          <h3 class="fs-6 mt-3">{{ OPTION_GROUP.identifiers }}</h3>
          <div class="form-check">
            <input
              id="export-identifiers"
              v-model="f.includeIdentifiers"
              class="form-check-input"
              type="checkbox"
              :disabled="!x.identifiersApply"
              aria-describedby="export-identifiers-note"
            />
            <label class="form-check-label" for="export-identifiers">{{ OPTION_TEXT.includeIdentifiers }}</label>
            <div id="export-identifiers-note" class="small text-body-secondary">
              {{ OPTION_TEXT.identifiersNote }}
              <template v-if="!x.identifiersApply">{{ ' ' }}{{ OPTION_TEXT.identifiersOnlyFor }}</template>
            </div>
          </div>
        </div>
      </fieldset>

      <!-- 4. Review -->
      <fieldset v-else-if="x.step === 'review'">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <p class="visually-hidden" aria-live="polite" aria-atomic="true">{{ countStatus }}</p>
        <div v-if="x.count.kind === 'counting' || x.count.kind === 'zone' || x.count.kind === 'idle'">
          <p class="mb-2" aria-hidden="true">
            <span class="spinner-border spinner-border-sm text-primary me-2"></span>{{ countStatus || COUNTING }}
          </p>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="cancelCount()">{{ CANCEL_COUNT }}</button>
        </div>
        <div v-else-if="x.count.kind === 'zoneFailed'" class="alert alert-warning" role="alert">
          <p class="mb-2">{{ ZONE_FAILED[x.count.reason] }}</p>
          <button type="button" class="btn btn-sm btn-primary" @click="x.useUtc()">{{ USE_UTC }}</button>
        </div>
        <div v-else-if="x.count.kind === 'failed'" class="alert alert-danger" role="alert">
          <p class="mb-2">{{ x.count.error.message }}</p>
          <button type="button" class="btn btn-sm btn-outline-secondary" @click="x.reviewAgain()">{{ REVIEW_AGAIN }}</button>
        </div>
        <template v-else-if="x.copyFacts">
          <p v-if="x.notice" class="alert alert-info py-2" role="status">{{ x.notice }}</p>
          <p class="mb-2">{{ copySizeLine(x.copyFacts.fileBytes) }}</p>
          <p class="alert alert-secondary small mb-2">{{ COPY_DISCLOSURE }}</p>
          <div v-if="x.copyFacts.earlierContent" class="form-check mb-2">
            <input
              id="export-confirm-copy-earlier"
              v-model="x.confirmCopyEarlier"
              class="form-check-input"
              type="checkbox"
              required
            />
            <label class="form-check-label" for="export-confirm-copy-earlier">{{ COPY_EARLIER_CONFIRM }}</label>
          </div>
          <p v-if="x.destinationError" class="alert alert-danger py-2 mb-2" role="alert">
            {{ x.destinationError.message }}
          </p>
          <button
            type="button"
            class="btn btn-primary"
            :disabled="!x.reviewConfirmed || x.choosing"
            @click="x.chooseDestination()"
          >
            {{ CHOOSE_DESTINATION }}
          </button>
        </template>
        <template v-else-if="x.preview">
          <p v-if="x.notice" class="alert alert-info py-2" role="status">{{ x.notice }}</p>
          <ul class="mb-2">
            <li>{{ PACKAGING_TEXT[x.preview.packaging] }}</li>
            <li v-for="(line, i) in reviewLines(x.preview, copies)" :key="`c${i}`">{{ line }}</li>
            <li v-for="(line, i) in searchLines(x.preview)" :key="`s${i}`">{{ line }}</li>
          </ul>
          <div class="d-flex flex-wrap align-items-center gap-2 mb-2">
            <span id="export-zone-sentence">{{ zoneSentence(f.zone, x.zoneName) }}</span>
            <button v-if="showsZoneChoice" type="button" class="btn btn-sm btn-outline-secondary" @click="x.useUtc()">
              {{ USE_UTC }}
            </button>
          </div>
          <p class="alert alert-secondary small mb-2">{{ DISCLOSURE }}</p>
          <div v-if="f.revealOneTimeCodes" class="mb-2">
            <div v-if="x.codesCount > 0" class="form-check">
              <input id="export-confirm-codes" v-model="x.confirmCodes" class="form-check-input" type="checkbox" required />
              <label class="form-check-label" for="export-confirm-codes">{{ revealCodesConfirm(x.codesCount) }}</label>
            </div>
            <p v-else class="small text-body-secondary mb-0">{{ OPTION_TEXT.revealCodes }}: {{ REVEAL_NONE }}</p>
          </div>
          <div v-if="f.revealEarlierContent" class="mb-2">
            <div v-if="x.earlierCount > 0" class="form-check">
              <input
                id="export-confirm-earlier"
                v-model="x.confirmEarlier"
                class="form-check-input"
                type="checkbox"
                required
              />
              <label class="form-check-label" for="export-confirm-earlier">{{ revealEarlierConfirm(x.earlierCount) }}</label>
            </div>
            <p v-else class="small text-body-secondary mb-0">{{ OPTION_TEXT.revealEarlier }}: {{ REVEAL_NONE }}</p>
          </div>
          <div v-if="f.keepExtensions && copies" class="form-check mb-2">
            <input
              id="export-confirm-extensions"
              v-model="x.confirmKeepExtensions"
              class="form-check-input"
              type="checkbox"
              required
            />
            <label class="form-check-label" for="export-confirm-extensions">{{ KEEP_EXTENSIONS_CONFIRM }}</label>
          </div>
          <p v-if="x.destinationError" class="alert alert-danger py-2 mb-2" role="alert">
            {{ x.destinationError.message }}
          </p>
          <button
            type="button"
            class="btn btn-primary"
            :disabled="!x.reviewConfirmed || x.choosing"
            @click="x.chooseDestination()"
          >
            {{ CHOOSE_DESTINATION }}
          </button>
        </template>
      </fieldset>

      <!-- 5. Destination -->
      <fieldset v-else-if="x.step === 'destination' && x.destination">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <p class="mb-2">
          {{ x.isCopy ? COPY_SAVES_AS : SAVES_AS[x.destination.packaging] }}{{ ' '
          }}<SafeText v-if="displayName" :value="displayName" mode="identifier" />.
        </p>
        <div v-if="x.destination.riskyName" class="form-check mb-2">
          <input id="export-confirm-risky" v-model="x.confirmRiskyName" class="form-check-input" type="checkbox" required />
          <label class="form-check-label" for="export-confirm-risky">
            {{ COPY_RISKY_PREFIX }}<SafeText v-if="displayName" :value="displayName" mode="identifier" quiet />{{
              COPY_RISKY_SUFFIX
            }}
          </label>
        </div>
        <div v-if="x.destination.targetExists" class="alert alert-warning py-2" role="alert">
          <p class="mb-2">
            {{ REPLACE_PROMPT_PREFIX }}<SafeText v-if="displayName" :value="displayName" mode="identifier" quiet />{{
              REPLACE_PROMPT_SUFFIX
            }}
          </p>
          <p v-if="x.replaceConfirmed" class="mb-0">{{ REPLACE_CONFIRMED }}</p>
          <div v-else class="d-flex gap-2">
            <button type="button" class="btn btn-sm btn-danger" :disabled="x.starting" @click="confirmReplace()">
              {{ REPLACE }}
            </button>
            <button
              type="button"
              class="btn btn-sm btn-outline-secondary"
              :disabled="x.choosing || x.starting"
              @click="x.chooseDestination()"
            >
              {{ CHOOSE_ANOTHER }}
            </button>
          </div>
        </div>
        <div v-for="w in x.warningsToConfirm" :key="w" class="form-check mb-1">
          <input
            :id="`export-warning-${w}`"
            class="form-check-input"
            type="checkbox"
            :checked="x.confirmedWarnings.includes(w)"
            @change="x.setWarningConfirmed(w, ($event.target as HTMLInputElement).checked)"
          />
          <label class="form-check-label" :for="`export-warning-${w}`">{{
            warningText(w, x.destination.syncProvider)
          }}</label>
        </div>
        <ul v-if="informational.length > 0" class="small mb-1">
          <li v-for="w in informational" :key="w">{{ warningText(w, x.destination.syncProvider) }}</li>
        </ul>
        <p class="small text-body-secondary mb-1">{{ freeSpaceLine(x.destination.freeSpace, x.destination.enough) }}</p>
        <div v-if="x.destination.leftovers > 0" class="form-check mb-1">
          <input id="export-leftovers" v-model="x.removeLeftovers" class="form-check-input" type="checkbox" />
          <label class="form-check-label" for="export-leftovers">{{ leftoversLine(x.destination.leftovers) }}</label>
        </div>
        <p v-if="x.destinationError" class="alert alert-danger py-2 my-2" role="alert">{{ x.destinationError.message }}</p>
        <div class="d-flex flex-wrap gap-2 mt-2">
          <button v-if="x.needsReview" type="button" class="btn btn-outline-primary" @click="x.reviewAgain()">
            {{ REVIEW_AGAIN }}
          </button>
          <button
            v-else-if="!x.destination.targetExists"
            type="button"
            class="btn btn-outline-secondary"
            :disabled="x.choosing || x.starting"
            @click="x.chooseDestination()"
          >
            {{ x.destination.packaging === 'singleFile' ? CHOOSE_ANOTHER : CHOOSE_ANOTHER_FOLDER }}
          </button>
          <button id="export-start" type="button" class="btn btn-primary" :disabled="!x.canStart" @click="x.start()">
            {{ x.isCopy ? COPY_START : START }}
          </button>
        </div>
      </fieldset>

      <!-- 6. Progress -->
      <fieldset v-else-if="x.step === 'progress'">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <JobProgress
          v-if="x.job"
          :job="x.job"
          :cancel-label="x.isCopy ? COPY_CANCEL : CANCEL_EXPORT"
          @cancel="x.cancelExport()"
        />
        <p class="small text-body-secondary mt-2 mb-2">{{ CANCEL_NOTE }}</p>
        <button
          v-if="x.liveJob"
          type="button"
          class="btn btn-sm btn-outline-secondary"
          aria-describedby="export-background-note"
          @click="x.continueInBackground()"
        >
          {{ CONTINUE_IN_BACKGROUND }}
        </button>
        <p id="export-background-note" class="small text-body-secondary mt-1 mb-0">{{ BACKGROUND_NOTE }}</p>
      </fieldset>

      <!-- 7. Result -->
      <fieldset v-else-if="x.step === 'result' && x.result">
        <legend class="fs-6 fw-semibold">{{ legend }}</legend>
        <div role="status">
          <template v-if="x.result.kind === 'finished' && x.result.copy">
            <p class="fw-semibold mb-1">{{ COPY_SAVED }}</p>
            <p class="mb-2">{{ COPY_RESULT }}</p>
            <ul class="small mb-2">
              <li v-for="(line, i) in resultNotes(x.result.summary)" :key="i">{{ line }}</li>
            </ul>
          </template>
          <template v-else-if="x.result.kind === 'finished'">
            <p class="fw-semibold mb-1">{{ OUTCOME_TEXT[x.result.outcome] }}</p>
            <p class="mb-2">{{ resultSentence(x.result.summary, x.result.formats) }}</p>
            <ul class="small mb-2">
              <li v-for="(line, i) in resultNotes(x.result.summary)" :key="i">{{ line }}</li>
            </ul>
          </template>
          <template v-else-if="x.result.kind === 'failed'">
            <p class="alert alert-danger mb-2">
              {{ x.result.error.message }} {{ failedNote(x.result.stagingRemoved, x.jobCopy) }}
            </p>
          </template>
          <p v-else class="mb-2">{{ cancelledSentence(x.result.stagingRemoved, x.jobCopy) }}</p>
        </div>
        <div class="d-flex flex-wrap gap-2">
          <button v-if="x.result.kind === 'finished'" type="button" class="btn btn-outline-secondary" @click="x.showInFolder()">
            {{ SHOW_IN_FOLDER }}
          </button>
          <button
            v-if="x.result.kind === 'failed' && x.result.error.code === 'exportConfirmationStale'"
            type="button"
            class="btn btn-outline-primary"
            @click="x.reviewAgain(STALE_NOTICE)"
          >
            {{ REVIEW_AGAIN }}
          </button>
          <button type="button" class="btn btn-primary" @click="x.close()">{{ DONE }}</button>
        </div>
      </fieldset>
    </div>
    <template #footer>
      <template v-if="x.step === 'what' || x.step === 'format' || x.step === 'options' || x.step === 'review' || x.step === 'destination'">
        <button
          v-if="canBack"
          type="button"
          class="btn btn-outline-secondary me-auto"
          :disabled="x.starting"
          @click="back()"
        >
          {{ BACK }}
        </button>
        <!-- Through requestClose, so a starting or running job asks first. -->
        <button type="button" class="btn btn-outline-secondary" :disabled="x.starting" @click="x.requestClose()">
          {{ CLOSE }}
        </button>
        <button
          v-if="x.step === 'what' || x.step === 'format' || x.step === 'options'"
          type="button"
          class="btn btn-primary"
          :disabled="!canNext"
          @click="next()"
        >
          {{ NEXT }}
        </button>
      </template>
    </template>
  </ModalDialog>
</template>
