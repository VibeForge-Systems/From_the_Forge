<script setup lang="ts">
// The details panel, "As recorded" (M5 §3.6, §3.7, §7.3, §8.4), slice
// 5h3, in the viewer's right panel. It reads `get_message_detail` for one
// message and lists what the archive recorded, headed by the source
// column names and never paraphrased into a state:
//
// - direction, sender, service and item kind; the raw time values of
//   `date`, `date_edited` and `date_retracted` with their unit and a UTC
//   ISO form at the unit's precision;
// - the versions this archive captured (by the backups that first
//   contained them; a stored version is not necessarily an edit) and, per
//   version, the edit entries recorded on the phone with their text
//   decoded at read time (owner Q3a). Earlier text stays withheld until
//   "Show earlier text" (D115, Q3b), which re-reads the message with the
//   reveal flag through the store, as the bubble does;
// - the account, the subject, values other backups recorded, the
//   conversations' raw flags (meaning unverified);
// - each attachment's declared facts, stored file, card, full-verify
//   result and observations by backup rank, as codes with their raw
//   values;
// - the identifier checks, and the reaction rows, paged by 200, with the
//   rule that derives the chips.
//
// No read, delivery or sent value is shown (D94); there is none in the DTO.
//
// It takes focus on its heading when it opens (the store gives focus back
// to the opener on Close). Text revealed here carries the same caution as
// in the bubble (REVEALED_TEXT), and dates before 2007 their mark.
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import type { HandleDto, MessageDetailDto, OptContentDto, RawValueDto, ReactionRecordDto, SafeId, TimeDto } from '@/types';
import SafeText from './SafeText.vue';
import { bytesText, cardSentence, sizeLine } from '@/services/attachmentView';
import { formatInstant, isoAtPrecision, timeText } from '@/services/time';
import {
  ASSOC_KIND_TEXT,
  BEFORE_2007_TEXT,
  BLOB_VERIFY_TEXT,
  CHIP_SENDER_TEXT,
  COPY_TEXT,
  DETAILS_TEXT,
  DIRECTION_DETAIL_TEXT,
  DISPLAY_NONE_TEXT,
  EDIT_ENTRY_TEXT,
  EDIT_LABEL_TEXT,
  EDIT_TEXTS_OUTCOME_TEXT,
  EDIT_UI_TEXT,
  HISTORY_COLUMN_TEXT,
  IDENTIFIER_PROBE_TEXT,
  ITEM_KIND_TEXT,
  LOOKUP_TEXT,
  PATH_CLASS_TEXT,
  PREVIEW_BODY_TEXT,
  REACTION_RULE_TEXT,
  REVEALED_TEXT,
  SENDER_TEXT,
  SERVICE_TEXT,
  SUMMARY_TIME_KIND_TEXT,
  TS_ERROR_TEXT,
  VALUE_STATE_TEXT,
  VERIFY_STALE_TEXT,
  WITHHELD_TEXT,
  fill,
} from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{
  messageId: SafeId;
  /** Load the first page of reaction rows straight away. */
  reactions?: boolean;
  handles: ReadonlyMap<SafeId, HandleDto>;
}>();
const emit = defineEmits<{ close: [] }>();

const viewer = useViewer();
const settings = useSettings();

const detail = ref<MessageDetailDto | null>(null);
const loading = ref(false);
const error = ref<string | null>(null);
const rows = ref<ReactionRecordDto[]>([]);
const rowsShown = ref(false);
const rowsMore = ref(false);
const rowsLoading = ref(false);
const heading = ref<HTMLElement | null>(null);
let seq = 0;

function focusHeading(): void {
  void nextTick(() => heading.value?.focus({ preventScroll: true }));
}
onMounted(focusHeading);
watch(() => viewer.panelFocus, focusHeading);

async function load(withReactions: boolean): Promise<void> {
  const mine = (seq += 1);
  loading.value = true;
  rowsLoading.value = false;
  error.value = null;
  const got = await viewer.loadDetail(props.messageId, withReactions ? null : undefined);
  if (mine !== seq) return;
  loading.value = false;
  if (got === null) return;
  if ('error' in got) {
    error.value = got.error;
    return;
  }
  detail.value = got.detail;
  rows.value = got.detail.reactions?.rows ?? [];
  rowsMore.value = got.detail.reactions?.hasMore ?? false;
  rowsShown.value = got.detail.reactions !== null;
}

watch(
  () => [props.messageId, props.reactions] as const,
  ([, reactions]) => {
    // A reply still on its way (`load` or `moreRows`) is for the old message.
    seq += 1;
    rowsLoading.value = false;
    detail.value = null;
    rows.value = [];
    rowsShown.value = false;
    void load(reactions === true);
  },
  { immediate: true },
);

/** Reload after a reveal, keeping the rows already shown. */
watch(
  () => viewer.revealed.get(props.messageId)?.reveal,
  () => void load(rowsShown.value),
);

async function moreRows(): Promise<void> {
  const last = rows.value[rows.value.length - 1];
  // The panel is reused for another message: a late answer for this one
  // must not land there.
  const id = props.messageId;
  const mine = seq;
  rowsLoading.value = true;
  const got = await viewer.loadDetail(id, rowsShown.value && last ? last.id : null);
  if (mine !== seq || id !== props.messageId) return;
  rowsLoading.value = false;
  if (got === null) return;
  if ('error' in got) {
    error.value = got.error;
    return;
  }
  const page = got.detail.reactions;
  if (!page) return;
  rows.value = rowsShown.value ? [...rows.value, ...page.rows] : page.rows;
  rowsMore.value = page.hasMore;
  rowsShown.value = true;
  if (detail.value) detail.value = { ...got.detail, reactions: page };
}

const m = computed(() => detail.value?.message ?? null);

function importDate(session: SafeId): string | null {
  const found = viewer.session?.imports.find((i) => i.session === session);
  return found ? formatInstant(found.startedMs, settings.viewer, 'date') : null;
}

function sessionLine(template: string, unknown: string, session: SafeId): string {
  const date = importDate(session);
  return date ? fill(template, { date }) : unknown;
}

function raw(v: RawValueDto): string {
  if (v.value !== null) return v.value;
  return v.state !== null ? VALUE_STATE_TEXT[v.state] : VALUE_STATE_TEXT.null;
}

interface TimeRow {
  label: string;
  text: string;
  /** The date is before 2007: shown as recorded, marked (§3.6). */
  before2007: boolean;
  raw: string | null;
  iso: string | null;
}

/** A time's text, with the before-2007 mark when it applies. */
function markedTime(t: TimeDto): string {
  const shown = timeText(t, settings.viewer);
  return shown.kind === 'instant' && shown.before2007 ? `${shown.text} (${BEFORE_2007_TEXT})` : shown.text;
}

function timeRow(label: string, t: TimeDto): TimeRow {
  const shown = timeText(t, settings.viewer);
  const iso = isoAtPrecision(t.raw, t.unit);
  return {
    label,
    text: shown.text,
    before2007: shown.kind === 'instant' && shown.before2007,
    raw: t.raw === null ? null : fill(DETAILS_TEXT.raw, { raw: t.raw }),
    iso: iso === null ? null : fill(DETAILS_TEXT.utc, { iso }),
  };
}

const times = computed<TimeRow[]>(() => {
  const d = detail.value;
  if (!d) return [];
  return [
    timeRow(DETAILS_TEXT.date, d.times.date),
    timeRow(DETAILS_TEXT.dateEdited, d.times.edited),
    timeRow(DETAILS_TEXT.dateRetracted, d.times.retracted),
  ];
});

const sender = computed<{ handle: HandleDto } | { text: string } | null>(() => {
  const s = m.value?.sender;
  if (!s) return null;
  if (s.kind === 'resolved') {
    const handle = props.handles.get(s.handle);
    return handle ? { handle } : { text: SENDER_TEXT.resolved };
  }
  return { text: SENDER_TEXT[s.kind] };
});

/** A reaction row's sender: the recorded address when loaded, else words. */
function rowSender(r: ReactionRecordDto): { address: OptContentDto | null; text: string } {
  if (r.sender.kind === 'handle') {
    const handle = props.handles.get(r.sender.handle);
    return { address: handle?.address ?? null, text: CHIP_SENDER_TEXT.handle };
  }
  return { address: null, text: CHIP_SENDER_TEXT[r.sender.kind] };
}

/** "12 KB, from the import of D": the stored file and the import that copied it. */
function storedLine(file: { storedBytes: number; heldBy: SafeId }): string {
  const size = bytesText(file.storedBytes);
  const date = importDate(file.heldBy);
  return date ? fill(DETAILS_TEXT.storedBy, { size, date }) : fill(DETAILS_TEXT.storedByUnknown, { size });
}

const reactionTotal = computed(() => {
  const r = detail.value?.reactions;
  if (!r) return null;
  return fill(r.total.capped ? DETAILS_TEXT.reactionsTotalCapped : DETAILS_TEXT.reactionsTotal, { n: r.total.n });
});
const reactionsUnkeyed = computed(() => detail.value?.reactions?.keyed === false);

/** Earlier text is shown here because the user revealed it: why it was withheld. */
const revealedReason = computed(() => {
  const e = m.value?.edit.earlier;
  return e && e.kind === 'revealed' ? e.reason : null;
});

const hasEarlierWithheld = computed(() =>
  (detail.value?.versions ?? []).some(
    (v) => v.text.kind === 'withheld' || v.edits.some((e) => e.text.kind === 'withheld'),
  ),
);

function noTextReason(v: MessageDetailDto['versions'][number]): string {
  if (v.text.kind !== 'noText') return '';
  const r = v.text.detail.reason;
  return r ? DISPLAY_NONE_TEXT[r] : PREVIEW_BODY_TEXT.noText;
}

async function showEarlier(): Promise<void> {
  const failed = await viewer.revealMessage(props.messageId, { earlierText: true });
  if (failed) error.value = failed;
}
</script>

<template>
  <div class="oma-details">
    <div class="oma-panel-header">
      <h2 id="oma-details-title" ref="heading" class="oma-panel-title" tabindex="-1">{{ DETAILS_TEXT.title }}</h2>
      <button type="button" class="btn-close" :aria-label="DETAILS_TEXT.close" @click="emit('close')"></button>
    </div>
    <div class="oma-panel-body px-3 small" aria-labelledby="oma-details-title" role="region">
      <p v-if="loading && !detail" role="status">{{ DETAILS_TEXT.loading }}</p>
      <div v-if="error" class="alert alert-warning py-2" role="alert">
        {{ DETAILS_TEXT.failed }} {{ error }}
        <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="load(rowsShown)">
          {{ DETAILS_TEXT.retry }}
        </button>
      </div>

      <template v-if="detail && m">
        <h3 class="oma-details-heading">{{ DETAILS_TEXT.message }}</h3>
        <dl class="oma-details-list">
          <dt>{{ DETAILS_TEXT.item }}</dt>
          <dd>{{ m.item ? ITEM_KIND_TEXT[m.item] : ITEM_KIND_TEXT.unknown }}</dd>
          <dt>{{ DETAILS_TEXT.direction }}</dt>
          <dd>{{ DIRECTION_DETAIL_TEXT[m.direction] }}</dd>
          <dt>{{ DETAILS_TEXT.sender }}</dt>
          <dd>
            <SafeText v-if="sender && 'handle' in sender" :value="sender.handle.address" mode="identifier" ltr />
            <template v-else-if="sender">{{ sender.text }}</template>
          </dd>
          <dt>{{ DETAILS_TEXT.service }}</dt>
          <dd>{{ m.service ? SERVICE_TEXT[m.service] : DETAILS_TEXT.serviceNone }}</dd>
          <dt>{{ DETAILS_TEXT.account }}</dt>
          <dd><SafeText :value="detail.account" mode="identifier" /></dd>
          <dt>{{ DETAILS_TEXT.subject }}</dt>
          <dd><SafeText :value="detail.subject" /></dd>
        </dl>

        <h3 class="oma-details-heading">{{ DETAILS_TEXT.times }}</h3>
        <dl class="oma-details-list">
          <template v-for="t in times" :key="t.label">
            <dt>{{ t.label }}</dt>
            <dd>
              {{ t.text }}
              <span v-if="t.before2007" class="oma-badge-inline">{{ BEFORE_2007_TEXT }}</span>
              <span v-if="t.iso" class="d-block">{{ t.iso }}</span>
              <span v-if="t.raw" class="d-block oma-raw">{{ t.raw }}</span>
            </dd>
          </template>
        </dl>

        <h3 class="oma-details-heading">{{ DETAILS_TEXT.versions }}</h3>
        <p class="oma-body-note">{{ DETAILS_TEXT.versionsNote }} {{ EDIT_UI_TEXT.searchCovers }}</p>
        <p v-if="revealedReason" class="oma-body-note oma-revealed-note" role="note">{{ REVEALED_TEXT[revealedReason] }}</p>
        <ol class="oma-details-versions">
          <li v-for="v in detail.versions" :key="v.body">
            <p class="mb-1">
              <strong>{{ v.current ? DETAILS_TEXT.current : DETAILS_TEXT.earlierVersion }}</strong>
              · {{ sessionLine(DETAILS_TEXT.firstKept, DETAILS_TEXT.firstKeptUnknown, v.firstSession) }}
            </p>
            <p v-if="v.textUnchanged" class="oma-body-note">{{ EDIT_UI_TEXT.textUnchanged }}</p>
            <p v-else-if="v.text.kind === 'text'" class="oma-details-text">
              <SafeText :pieces="v.text.pieces" :show-hidden="settings.viewer.showHiddenCharacters" />
            </p>
            <p v-else-if="v.text.kind === 'withheld'" class="oma-body-note">{{ WITHHELD_TEXT[v.text.reason] }}</p>
            <p v-else class="oma-missing">{{ noTextReason(v) }}</p>
            <p v-if="v.editLabel" class="oma-body-note">{{ DETAILS_TEXT.editState }}: {{ EDIT_LABEL_TEXT[v.editLabel] }}</p>
            <template v-if="v.edits.length || v.moreEdits">
              <p class="mb-1">
                <strong>{{ EDIT_UI_TEXT.versionsOnPhone }}</strong>
                · {{ EDIT_TEXTS_OUTCOME_TEXT[v.editTexts] }}
              </p>
              <p class="oma-body-note">{{ DETAILS_TEXT.editsNote }}</p>
              <ol class="oma-details-edits">
                <li v-for="e in v.edits" :key="`${e.part}:${e.ordinal}`">
                  <span>{{ fill(DETAILS_TEXT.editEntry, { part: e.part, ordinal: e.ordinal }) }}</span>
                  <span v-if="e.at.kind" class="d-block oma-raw">
                    {{ SUMMARY_TIME_KIND_TEXT[e.at.kind] }}<template v-if="e.at.raw"
                      >: {{ e.at.raw.kind === 'integer' ? e.at.raw.value : (e.at.raw.value ?? VALUE_STATE_TEXT.null) }}</template
                    ><template v-if="e.at.ms !== null"> ({{ formatInstant(e.at.ms, settings.viewer) }})</template>
                  </span>
                  <span v-if="e.text.kind === 'text'" class="d-block">
                    <SafeText :pieces="e.text.pieces" :show-hidden="settings.viewer.showHiddenCharacters" />
                  </span>
                  <span v-else-if="e.text.kind === 'withheld'" class="d-block oma-body-note">{{ WITHHELD_TEXT[e.text.reason] }}</span>
                  <span v-else-if="e.text.kind === 'failed'" class="d-block oma-missing"
                    >{{ EDIT_ENTRY_TEXT.failed }}: {{ TS_ERROR_TEXT[e.text.error] }}</span
                  >
                  <span v-else class="d-block oma-missing">{{ EDIT_ENTRY_TEXT[e.text.kind] }}</span>
                </li>
              </ol>
              <p v-if="v.moreEdits" class="oma-body-note">{{ fill(DETAILS_TEXT.moreEdits, { n: v.moreEdits }) }}</p>
            </template>
          </li>
        </ol>
        <p v-if="detail.moreVersions" class="oma-body-note">{{ fill(DETAILS_TEXT.moreVersions, { n: detail.moreVersions }) }}</p>
        <p v-if="hasEarlierWithheld">
          <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="showEarlier">
            {{ DETAILS_TEXT.showEarlier }}
          </button>
        </p>

        <template v-if="detail.history.length">
          <h3 class="oma-details-heading">{{ DETAILS_TEXT.history }}</h3>
          <p class="oma-body-note">{{ DETAILS_TEXT.historyNote }}</p>
          <dl class="oma-details-list">
            <template v-for="(h, i) in detail.history" :key="i">
              <dt>{{ HISTORY_COLUMN_TEXT[h.column] }}</dt>
              <dd>
                <SafeText :value="h.value" mode="identifier" />
                <span class="d-block oma-body-note">{{ sessionLine(DETAILS_TEXT.historyFirst, DETAILS_TEXT.firstKeptUnknown, h.firstSession) }}</span>
              </dd>
            </template>
          </dl>
        </template>

        <template v-if="detail.chats.length">
          <h3 class="oma-details-heading">{{ DETAILS_TEXT.chats }}</h3>
          <dl v-for="c in detail.chats" :key="c.chat" class="oma-details-list">
            <dt>{{ fill(DETAILS_TEXT.chat, { chat: c.chat }) }}</dt>
            <dd>
              <span class="d-block">{{ DETAILS_TEXT.style }}: {{ raw(c.style) }}</span>
              <span class="d-block">{{ DETAILS_TEXT.isFiltered }}: {{ raw(c.isFiltered) }}</span>
              <span class="d-block">{{ DETAILS_TEXT.isArchived }}: {{ raw(c.isArchived) }}</span>
              <span v-for="(h, i) in c.history" :key="i" class="d-block">
                {{ HISTORY_COLUMN_TEXT[h.column] }}: <SafeText :value="h.value" mode="identifier" />
                ({{ sessionLine(DETAILS_TEXT.historyFirst, DETAILS_TEXT.firstKeptUnknown, h.firstSession) }})
              </span>
            </dd>
          </dl>
        </template>

        <template v-if="detail.attachments.length">
          <h3 class="oma-details-heading">{{ DETAILS_TEXT.attachments }}</h3>
          <div v-for="a in detail.attachments" :key="a.id" class="oma-details-attachment">
            <dl class="oma-details-list">
              <dt>{{ DETAILS_TEXT.name }}</dt>
              <dd><SafeText :value="a.name" mode="identifier" /></dd>
              <dt>{{ DETAILS_TEXT.mimeType }}</dt>
              <dd><SafeText :value="a.declaredType" mode="identifier" /></dd>
              <dt>{{ DETAILS_TEXT.uti }}</dt>
              <dd><SafeText :value="a.uti" mode="identifier" /></dd>
              <dt>{{ DETAILS_TEXT.totalBytes }}</dt>
              <dd>{{ raw(a.declaredBytes) }}</dd>
              <dt>{{ DETAILS_TEXT.isSticker }}</dt>
              <dd>{{ raw(a.sticker) }}</dd>
              <dt>{{ DETAILS_TEXT.hideAttachment }}</dt>
              <dd>{{ raw(a.hideAttachment) }}</dd>
              <dt>{{ DETAILS_TEXT.emojiDescription }}</dt>
              <dd><SafeText :value="a.emojiDescription" /></dd>
              <dt>{{ DETAILS_TEXT.storedFile }}</dt>
              <dd>
                <template v-if="a.file">{{ storedLine(a.file) }}</template>
                <template v-else>{{ DETAILS_TEXT.noStoredFile }}</template>
                <span class="d-block oma-body-note">{{ sizeLine(a.file?.storedBytes ?? null, a.declaredBytes.value) }}</span>
              </dd>
              <dt>{{ DETAILS_TEXT.card }}</dt>
              <dd>
                {{ cardSentence(a.card) }}
                <span v-if="a.card.kind === 'file' && a.card.olderBackup" class="d-block">{{
                  fill(DETAILS_TEXT.olderBackup, {
                    date: importDate(a.card.olderBackup.heldBy) ?? '?',
                    newest: importDate(a.card.olderBackup.newest) ?? '?',
                  })
                }}</span>
              </dd>
              <dt>{{ DETAILS_TEXT.verify }}</dt>
              <dd>
                <template v-if="a.verify">
                  {{ a.verify.state ? BLOB_VERIFY_TEXT[a.verify.state] : fill(DETAILS_TEXT.rawCode, { raw: a.verify.raw }) }}
                  <template v-if="a.verify.verifiedMs !== null">
                    {{ fill(DETAILS_TEXT.verifyWhen, { date: formatInstant(a.verify.verifiedMs, settings.viewer) }) }}</template
                  >
                  <span v-if="a.verify.mayBeStale" class="d-block">{{ VERIFY_STALE_TEXT }}</span>
                </template>
                <template v-else>{{ DETAILS_TEXT.verifyNone }}</template>
              </dd>
            </dl>
            <p class="mb-1"><strong>{{ DETAILS_TEXT.observations }}</strong></p>
            <ol class="oma-details-observations">
              <li v-for="(o, i) in a.observations" :key="i">
                <span class="d-block">{{ sessionLine(DETAILS_TEXT.observation, DETAILS_TEXT.observationUnknown, o.session) }}</span>
                <span class="d-block">
                  {{ DETAILS_TEXT.lookup }}: {{ o.lookup ? LOOKUP_TEXT[o.lookup] : fill(DETAILS_TEXT.rawCode, { raw: o.lookupRaw }) }}
                  <span class="oma-raw">({{ fill(DETAILS_TEXT.rawCode, { raw: o.lookupRaw }) }})</span>
                </span>
                <span class="d-block">
                  {{ DETAILS_TEXT.copy }}: {{ o.copy ? COPY_TEXT[o.copy] : fill(DETAILS_TEXT.rawCode, { raw: o.copyRaw }) }}
                  <span class="oma-raw">({{ fill(DETAILS_TEXT.rawCode, { raw: o.copyRaw }) }})</span>
                </span>
                <span v-if="o.pathClass" class="d-block">{{ DETAILS_TEXT.pathClass }}: {{ PATH_CLASS_TEXT[o.pathClass] }}</span>
                <span class="d-block">
                  {{ o.stored ? DETAILS_TEXT.storedYes : DETAILS_TEXT.storedNo
                  }}<template v-if="o.sizeMatchedManifest !== null"
                    >; {{ o.sizeMatchedManifest ? DETAILS_TEXT.sizeMatched : DETAILS_TEXT.sizeDiffered }}</template
                  >
                </span>
              </li>
            </ol>
            <template v-if="a.history.length">
              <p v-for="(h, i) in a.history" :key="i" class="oma-body-note">
                {{ HISTORY_COLUMN_TEXT[h.column] }}: <SafeText :value="h.value" mode="identifier" />
                ({{ sessionLine(DETAILS_TEXT.historyFirst, DETAILS_TEXT.firstKeptUnknown, h.firstSession) }})
              </p>
            </template>
          </div>
        </template>

        <template v-if="detail.replyProbe || detail.reactionTargetProbe">
          <h3 class="oma-details-heading">{{ DETAILS_TEXT.probes }}</h3>
          <dl class="oma-details-list">
            <template v-if="detail.replyProbe">
              <dt>{{ DETAILS_TEXT.replyProbe }}</dt>
              <dd>{{ IDENTIFIER_PROBE_TEXT[detail.replyProbe] }}</dd>
            </template>
            <template v-if="detail.reactionTargetProbe">
              <dt>{{ DETAILS_TEXT.reactionTargetProbe }}</dt>
              <dd>{{ IDENTIFIER_PROBE_TEXT[detail.reactionTargetProbe] }}</dd>
            </template>
          </dl>
        </template>

        <h3 class="oma-details-heading">{{ DETAILS_TEXT.reactions }}</h3>
        <p class="oma-body-note">{{ REACTION_RULE_TEXT }}</p>
        <template v-if="rowsShown">
          <p v-if="reactionsUnkeyed" class="oma-body-note">{{ DETAILS_TEXT.reactionsUnkeyed }}</p>
          <p v-if="reactionTotal">{{ reactionTotal }}</p>
          <p v-if="rows.length === 0 && !reactionsUnkeyed">{{ DETAILS_TEXT.reactionsNone }}</p>
          <ol v-else class="oma-details-reactions">
            <li v-for="r in rows" :key="r.id">
              <span>{{ r.kind ? ASSOC_KIND_TEXT[r.kind] : ASSOC_KIND_TEXT.unknown }}</span>
              ({{ r.adds ? DETAILS_TEXT.reactionAdds : DETAILS_TEXT.reactionRemoves }})
              <template v-if="r.emoji">: <SafeText :value="r.emoji" quiet /></template>
              ·
              <SafeText v-if="rowSender(r).address" :value="rowSender(r).address" mode="identifier" ltr quiet />
              <template v-else>{{ rowSender(r).text }}</template>
              · {{ markedTime(r.time) }}
              <template v-if="r.targetPart !== null"> · {{ fill(DETAILS_TEXT.targetPart, { part: r.targetPart }) }}</template>
            </li>
          </ol>
          <button v-if="rowsMore" type="button" class="btn btn-sm btn-outline-secondary" :disabled="rowsLoading" @click="moreRows">
            {{ DETAILS_TEXT.reactionsMore }}
          </button>
        </template>
        <button v-else type="button" class="btn btn-sm btn-outline-secondary" :disabled="rowsLoading" @click="moreRows">
          {{ DETAILS_TEXT.reactionsShow }}
        </button>

        <p v-if="detail.cutForSize" class="oma-body-note">{{ DETAILS_TEXT.cutForSize }}</p>
        <p v-if="detail.skippedRows > 0" class="oma-body-note">{{ DETAILS_TEXT.skippedRows }}</p>
      </template>
    </div>
  </div>
</template>
