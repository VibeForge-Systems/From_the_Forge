<script setup lang="ts">
// One search result (M5 §4.3, §4.5, slice 5i): when and in which
// conversation it was recorded, the direction, the version it matched,
// and Rust's snippet with its matches in `<mark>` (one isolate per
// snippet, through SafeText). Masked text stays masked: a match inside a
// one-time code says so and shows nothing of it. "Open in conversation"
// opens the thread around the message and focuses it; the message id
// never enters the URL. Ids are built from the result's position only.
import { computed } from 'vue';
import type { ConversationRowDto, SearchHitDto } from '@/types';
import SafeText from './SafeText.vue';
import ConversationLabel from './ConversationLabel.vue';
import { formatInstant } from '@/services/time';
import {
  ASSOC_KIND_TEXT,
  DIRECTION_TEXT,
  HIT_VERSION_TEXT,
  ITEM_KIND_TEXT,
  LIST_TEXT,
  NO_HIGHLIGHT_TEXT,
  SEARCH_TEXT,
  fill,
} from '@/services/viewerText';
import { useSettings } from '@/stores/settings';

const props = defineProps<{ hit: SearchHitDto; index: number; row: ConversationRowDto | null }>();
const emit = defineEmits<{ open: [hit: SearchHitDto] }>();
const settings = useSettings();

const base = computed(() => `oma-hit-${props.index}`);
const when = computed(() =>
  props.hit.ms === null ? SEARCH_TEXT.undated : formatInstant(props.hit.ms, settings.viewer),
);
const kind = computed(() => {
  if (props.hit.association !== null && props.hit.association !== 'noAssociation') {
    return ASSOC_KIND_TEXT[props.hit.association];
  }
  if (props.hit.item !== null && props.hit.item !== 'message') return ITEM_KIND_TEXT[props.hit.item];
  return null;
});
const others = computed(() => {
  const o = props.hit.otherChats;
  if (o.n === 0) return null;
  return fill(o.capped ? SEARCH_TEXT.otherChatsCapped : SEARCH_TEXT.otherChats, { n: o.n });
});
</script>

<template>
  <div class="oma-hit">
    <p :id="`${base}-h`" class="oma-hit-head">
      <span class="oma-hit-time">{{ when }}</span>
      {{ ' ' }}<span class="oma-hit-dir">{{ DIRECTION_TEXT[hit.direction] }}</span>
      {{ ' ' }}<span class="oma-hit-chat">
        <ConversationLabel v-if="row && hit.chat !== null" :label="row.label" :chat-id="hit.chat" quiet />
        <template v-else-if="hit.chat !== null">{{ LIST_TEXT.loadingRow }}</template>
        <template v-else>{{ SEARCH_TEXT.noChat }}</template>
      </span>
      <span v-if="others" class="oma-hit-note">{{ ' ' }}{{ others }}</span>
    </p>
    <p v-if="kind || hit.version !== 'current'" class="oma-hit-badges">
      <span v-if="kind" class="oma-badge-inline">{{ kind }}</span>
      <span v-if="hit.version !== 'current'" class="oma-badge-inline">{{ HIT_VERSION_TEXT[hit.version] }}</span>
    </p>
    <p :id="`${base}-t`" class="oma-hit-text">
      <template v-if="hit.text">
        <span v-if="hit.text.cutStart" aria-hidden="true">…</span
        ><SafeText :hits="hit.text.pieces" /><span v-if="hit.text.cutEnd" aria-hidden="true">…</span>
      </template>
      <span v-else class="oma-missing">{{ SEARCH_TEXT.noText }}</span>
    </p>
    <p v-if="hit.text && hit.text.highlight.kind === 'notShown'" class="oma-hit-note">
      {{ NO_HIGHLIGHT_TEXT[hit.text.highlight.why] }}
    </p>
    <p v-if="hit.text?.matchedHidden" class="oma-hit-note">{{ SEARCH_TEXT.matchedHidden }}</p>
    <button
      v-if="hit.chat !== null"
      type="button"
      class="btn btn-sm btn-outline-secondary"
      :aria-describedby="`${base}-h ${base}-t`"
      @click="emit('open', hit)"
    >
      {{ SEARCH_TEXT.open }}
    </button>
  </div>
</template>
