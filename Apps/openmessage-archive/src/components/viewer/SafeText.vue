<script setup lang="ts">
// The only renderer of archive content (M5 §8.1, I7). It takes
// `ContentText`, `OptContent` or masked text pieces and renders them by
// interpolation only: no HTML, and no content in any attribute. Each value
// sits in its own `<bdi>` so its direction cannot leak into the
// surrounding text.
//
// - mode "identifier" (addresses, chat names, file names): bidi controls,
//   invisible and control characters become visible tokens such as
//   ⟨U+202E⟩; addresses are forced left-to-right (`ltr`).
// - mode "body": the characters stay inside the isolate (ZWJ and variation
//   selectors must stay, or emoji sequences break) unless `showHidden`
//   (the `showHiddenCharacters` setting) asks for tokens. The badge
//   "contains invisible or direction-control characters" shows either way.
// - `hits` (a search snippet, slice 5i): the pieces sit inside ONE `<bdi>`,
//   so a match never escapes the snippet's isolate, and each matched piece
//   is wrapped in `<mark>`. Masked pieces show their sentence.
import { computed } from 'vue';
import type { ContentTextDto, HitPieceDto, OptContentDto, TextPieceDto } from '@/types';
import { visibleRuns, type SafeRun } from '@/services/safeText';
import { CONTENT_BADGE_TEXT, MASK_TEXT, VALUE_STATE_TEXT } from '@/services/viewerText';

const props = withDefaults(
  defineProps<{
    value?: ContentTextDto | OptContentDto | null;
    pieces?: readonly TextPieceDto[] | null;
    /** A search snippet's pieces: rendered in one isolate, matches in `<mark>`. */
    hits?: readonly HitPieceDto[] | null;
    mode?: 'body' | 'identifier';
    ltr?: boolean;
    showHidden?: boolean;
    /** Hide the badges (for compact places that show them elsewhere). */
    quiet?: boolean;
  }>(),
  { value: null, pieces: null, hits: null, mode: 'body', ltr: false, showHidden: false, quiet: false },
);

type Part =
  | { kind: 'text'; key: number; runs: SafeRun[] | null; text: string; content: ContentTextDto; hit: boolean }
  | { kind: 'masked'; key: number; label: string }
  | { kind: 'missing'; key: number; label: string };

function isContent(v: ContentTextDto | OptContentDto): v is ContentTextDto {
  return !('kind' in v);
}

const tokens = computed(() => props.mode === 'identifier' || props.showHidden);

function textPart(key: number, content: ContentTextDto, hit = false): Part {
  const text: string = content.text;
  return {
    kind: 'text',
    key,
    text,
    content,
    hit,
    runs: tokens.value ? visibleRuns(text, props.mode === 'identifier' ? 'identifier' : 'body') : null,
  };
}

const parts = computed<Part[]>(() => {
  if (props.hits) {
    return props.hits.map((p, i) =>
      p.kind === 'text' ? textPart(i, p.value, p.hit) : { kind: 'masked', key: i, label: MASK_TEXT[p.mask] },
    );
  }
  if (props.pieces) {
    return props.pieces.map((p, i) =>
      p.kind === 'text' ? textPart(i, p.value) : { kind: 'masked', key: i, label: MASK_TEXT[p.mask] },
    );
  }
  const v = props.value;
  if (v === null) return [];
  if (isContent(v)) return [textPart(0, v)];
  if (v.kind === 'missing') return [{ kind: 'missing', key: 0, label: VALUE_STATE_TEXT[v.state] }];
  return [textPart(0, v.value)];
});

const contents = computed(() =>
  parts.value.flatMap((p) => (p.kind === 'text' ? [p.content] : [])),
);
const truncated = computed(() => contents.value.some((c) => c.truncated));
const lossy = computed(() => contents.value.some((c) => c.lossy));
const hidden = computed(() => contents.value.some((c) => c.bidiControls || c.invisible));
const dir = computed(() => (props.ltr ? 'ltr' : 'auto'));
</script>

<template>
  <span class="oma-safe-text" :class="mode === 'identifier' ? 'oma-safe-id' : 'oma-safe-body'">
    <bdi v-if="hits" :dir="dir"
      ><template v-for="part in parts" :key="part.key"
        ><template v-if="part.kind === 'text'"
          ><mark v-if="part.hit"
            ><template v-if="part.runs"
              ><template v-for="(run, i) in part.runs" :key="i"
                ><span v-if="run.kind === 'token'" class="oma-cp">{{ run.label }}</span
                ><template v-else>{{ run.text }}</template></template
              ></template
            ><template v-else>{{ part.text }}</template></mark
          ><template v-else-if="part.runs"
            ><template v-for="(run, i) in part.runs" :key="i"
              ><span v-if="run.kind === 'token'" class="oma-cp">{{ run.label }}</span
              ><template v-else>{{ run.text }}</template></template
            ></template
          ><template v-else>{{ part.text }}</template></template
        ><span v-else class="oma-masked">{{ part.label }}</span></template
      ></bdi
    >
    <template v-for="part in hits ? [] : parts" :key="part.key">
      <bdi v-if="part.kind === 'text'" :dir="dir"
        ><template v-if="part.runs"
          ><template v-for="(run, i) in part.runs" :key="i"
            ><span v-if="run.kind === 'token'" class="oma-cp">{{ run.label }}</span
            ><template v-else>{{ run.text }}</template></template
          ></template
        ><template v-else>{{ part.text }}</template></bdi
      >
      <span v-else-if="part.kind === 'masked'" class="oma-masked">{{ part.label }}</span>
      <span v-else class="oma-missing">{{ part.label }}</span>
    </template>
    <span v-if="truncated" class="oma-badge-inline">{{ CONTENT_BADGE_TEXT.truncated }}</span>
    <span v-if="!quiet && lossy" class="oma-badge-inline">{{ CONTENT_BADGE_TEXT.lossy }}</span>
    <span v-if="!quiet && hidden" class="oma-badge-inline">{{ CONTENT_BADGE_TEXT.hidden }}</span>
  </span>
</template>
