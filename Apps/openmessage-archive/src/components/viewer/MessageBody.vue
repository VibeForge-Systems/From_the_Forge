<script setup lang="ts">
// A message's current body (M5 §3.6, §8.1), slice 5h2.
//
// - Text: Rust's segments, each rendered by SafeText. Styles, mentions and
//   transcription marks come from closed booleans on the segment, and
//   Rust sets them only when the formatting could be trusted. Mentions are
//   styled spans and URL runs stay text: nothing is a link.
// - One-time codes arrive masked (D115). "Show code" re-reads the message
//   with the reveal flag; "Hide code" drops it again.
// - Text without a value says why: the display reason refined by the
//   attributed-body outcome and the plain column's state. It is never an
//   empty bubble.
// - A long body is collapsed with "Show more". A body cut at the page's
//   cap shows "Show the full text", which reads it with `get_message_text`
//   chunk by chunk, still masked.
import { computed, ref } from 'vue';
import type { MessageDto, SegmentDto } from '@/types';
import SafeText from './SafeText.vue';
import {
  ATTRIBUTED_OUTCOME_TEXT,
  BUBBLE_TEXT,
  CONTENT_BADGE_TEXT,
  DISPLAY_NONE_TEXT,
  DISPLAY_SOURCE_TEXT,
  FORMATTING_TEXT,
  MASK_TEXT,
  PREVIEW_BODY_TEXT,
  SEGMENT_TEXT,
  SOURCES_DIFFER_TEXT,
  THREAD_TEXT,
  TS_ERROR_TEXT,
  VALUE_STATE_TEXT,
  WITHHELD_TEXT,
  fill,
} from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{
  message: MessageDto;
  /** The message was re-read with its one-time codes revealed. */
  codesRevealed: boolean;
}>();
const emit = defineEmits<{ revealCode: []; hideCode: [] }>();

const settings = useSettings();
const viewer = useViewer();

/** Bodies longer than this many UTF-16 units start collapsed. */
const COLLAPSE_UTF16 = 1500;
const expanded = ref(false);

const body = computed(() => props.message.body);
const text = computed(() => (body.value.kind === 'text' ? body.value : null));
const segments = computed<SegmentDto[]>(() => text.value?.segments ?? []);
const masked = computed(() => segments.value.some((s) => s.piece.kind === 'withheld'));
const hasCode = computed(() => segments.value.some((s) => s.oneTimeCode));
const transcription = computed(() => segments.value.some((s) => s.transcription));
const hiddenLink = computed(() => segments.value.some((s) => s.url && s.urlHidden));

const full = computed(() => (text.value ? (viewer.fullTexts.get(text.value.body) ?? null) : null));
const showingFull = computed(() => full.value !== null && full.value.pieces.length > 0);
const long = computed(() => (text.value?.fullUtf16 ?? 0) > COLLAPSE_UTF16);
const collapsed = computed(() => long.value && !expanded.value && !showingFull.value);

/** The badges SafeText would repeat per segment, shown once per body. */
const badges = computed(() => {
  const t = text.value;
  if (!t) return [];
  const out: string[] = [];
  if (t.lossy) out.push(CONTENT_BADGE_TEXT.lossy);
  if (t.storedAsBlob) out.push(CONTENT_BADGE_TEXT.storedAsBlob);
  if (t.bidiControls || t.invisible) out.push(CONTENT_BADGE_TEXT.hidden);
  return out;
});

/** Notes about where the text came from and whether formatting applied. */
const notes = computed(() => {
  const t = text.value;
  if (!t) return [];
  const out: string[] = [];
  if (t.source === 'textLossy') out.push(DISPLAY_SOURCE_TEXT.textLossy);
  if (t.sourcesDiffer === true) out.push(SOURCES_DIFFER_TEXT);
  const f = t.formatting;
  if (f.kind === 'unreadable') out.push(f.error ? `${FORMATTING_TEXT.unreadable}: ${TS_ERROR_TEXT[f.error]}` : FORMATTING_TEXT.unreadable);
  else if (f.kind === 'tooManySegments') out.push(FORMATTING_TEXT.tooManySegments);
  if (hiddenLink.value) out.push(BUBBLE_TEXT.urlHidden);
  return out;
});

/** Why there is no text: each recorded reason, never an empty bubble. */
const reasons = computed(() => {
  const b = body.value;
  if (b.kind === 'noBody') return [PREVIEW_BODY_TEXT.noBody];
  if (b.kind !== 'noText') return [];
  const d = b.detail;
  const out: string[] = [d.reason ? DISPLAY_NONE_TEXT[d.reason] : PREVIEW_BODY_TEXT.noText];
  if (d.attributed === 'oversized' || d.attributed === 'unreadable') out.push(ATTRIBUTED_OUTCOME_TEXT[d.attributed]);
  if (d.attributed === 'decodeFailed') {
    out.push(d.attributedError ? TS_ERROR_TEXT[d.attributedError] : ATTRIBUTED_OUTCOME_TEXT.decodeFailed);
  }
  if (d.textColumn !== null) out.push(fill(BUBBLE_TEXT.textColumn, { state: VALUE_STATE_TEXT[d.textColumn] }));
  return out;
});

const fullButton = computed(() => {
  const t = text.value;
  if (!t || !t.truncated) return null;
  const f = full.value;
  if (f && (f.withheld !== null || f.none || (f.pieces.length > 0 && f.end >= f.full))) return null;
  return f && f.pieces.length > 0 ? BUBBLE_TEXT.showMoreText : BUBBLE_TEXT.showFullText;
});

function segmentClass(seg: SegmentDto): string {
  const c: string[] = [];
  if (seg.bold) c.push('oma-seg-bold');
  if (seg.italic) c.push('oma-seg-italic');
  if (seg.underline) c.push('oma-seg-underline');
  if (seg.strikethrough) c.push('oma-seg-strike');
  if (seg.mention) c.push('oma-seg-mention');
  if (seg.oneTimeCode) c.push('oma-seg-code');
  if (seg.url) c.push('oma-seg-url');
  return c.join(' ');
}

function readMore(): void {
  const t = text.value;
  if (t) void viewer.loadFullText(t.body, props.message.id);
}
</script>

<template>
  <div class="oma-body">
    <p v-if="transcription" class="oma-body-note">{{ SEGMENT_TEXT.transcription }}</p>
    <p v-if="text" class="oma-body-text" :class="collapsed ? 'oma-collapsed' : ''">
      <template v-if="showingFull && full">
        <SafeText :pieces="full.pieces" :show-hidden="settings.viewer.showHiddenCharacters" quiet />
      </template>
      <template v-else>
        <template v-for="(seg, i) in segments" :key="i"
          ><span v-if="seg.piece.kind === 'text'" :class="segmentClass(seg)"
            ><SafeText :value="seg.piece.value" :show-hidden="settings.viewer.showHiddenCharacters" quiet /></span
          ><span v-else-if="seg.piece.kind === 'withheld'" class="oma-masked">{{ MASK_TEXT[seg.piece.mask] }}</span
          ><span v-else-if="seg.piece.kind === 'inlineEmoji'" class="oma-missing">{{ SEGMENT_TEXT.inlineEmoji }}</span
          ><span v-else-if="seg.piece.kind === 'attachment'" class="oma-placeholder">{{ THREAD_TEXT.attachmentInText }}</span
          ><span v-else class="oma-placeholder">{{ SEGMENT_TEXT.unmatchedPlaceholder }}</span></template
        >
      </template>
    </p>
    <template v-else>
      <p v-for="(r, i) in reasons" :key="i" class="oma-missing">{{ r }}</p>
    </template>

    <p v-if="badges.length" class="oma-body-badges">
      <span v-for="b in badges" :key="b" class="oma-badge-inline">{{ b }}</span>
    </p>
    <p v-for="n in notes" :key="n" class="oma-body-note">{{ n }}</p>

    <div v-if="text && (long || fullButton || masked || (codesRevealed && hasCode))" class="oma-body-actions">
      <button v-if="long && !showingFull" type="button" class="btn btn-link btn-sm oma-inline-btn" @click="expanded = !expanded">
        {{ expanded ? BUBBLE_TEXT.showLess : BUBBLE_TEXT.showMore }}
      </button>
      <button v-if="fullButton" type="button" class="btn btn-link btn-sm oma-inline-btn" :disabled="full?.loading === true" @click="readMore">
        {{ fullButton }}
      </button>
      <button v-if="masked" type="button" class="btn btn-link btn-sm oma-inline-btn" @click="emit('revealCode')">
        {{ BUBBLE_TEXT.showCode }}
      </button>
      <button v-else-if="codesRevealed && hasCode" type="button" class="btn btn-link btn-sm oma-inline-btn" @click="emit('hideCode')">
        {{ BUBBLE_TEXT.hideCode }}
      </button>
    </div>
    <p v-if="full?.loading" class="oma-body-note" role="status">{{ BUBBLE_TEXT.loadingText }}</p>
    <p v-if="full?.error" class="oma-body-note" role="alert">{{ full.error }}</p>
    <p v-if="full?.withheld" class="oma-body-note">{{ BUBBLE_TEXT.fullTextWithheld }} {{ WITHHELD_TEXT[full.withheld] }}</p>
    <p v-if="full?.none && full.end < full.full" class="oma-body-note">{{ BUBBLE_TEXT.fullTextNone }}</p>
  </div>
</template>
