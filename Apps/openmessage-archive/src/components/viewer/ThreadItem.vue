<script setup lang="ts">
// One item of the thread feed (M5 §8.4, §10), in its plain form: an
// `article` labelled by its sender and time and described by its text,
// with a local-day separator above the first item of each day. Slice 5h2
// replaces the body with MessageBubble and SystemRow (formatting, reaction
// chips, replies, edits, unsends and withheld text); attachments are
// counted here and shown in 5h3.
//
// `aria-setsize="-1"`: the number of visible items is not known (the
// chat's count includes folded reactions and goes stale during imports),
// and `aria-posinset` is omitted because keyset pages give no exact
// ordinal (a stated deviation from the APG feed pattern).
import { computed } from 'vue';
import type { HandleDto, MessageDto, SafeId } from '@/types';
import SafeText from './SafeText.vue';
import { timeText } from '@/services/time';
import {
  ASSOC_KIND_TEXT,
  BEFORE_2007_TEXT,
  DIRECTION_TEXT,
  DISPLAY_NONE_TEXT,
  ITEM_KIND_TEXT,
  MASK_TEXT,
  PREVIEW_BODY_TEXT,
  SEGMENT_TEXT,
  SENDER_TEXT,
  THREAD_TEXT,
  fill,
} from '@/services/viewerText';
import { useSettings } from '@/stores/settings';

const props = defineProps<{
  message: MessageDto;
  handles: ReadonlyMap<SafeId, HandleDto>;
  /** The day separator's text, when this item starts a new local day. */
  dayLabel: string | null;
}>();

const settings = useSettings();

const base = computed(() => `oma-m-${props.message.id}`);
const time = computed(() => timeText(props.message.date, settings.viewer));

const sender = computed<{ handle: HandleDto } | { text: string }>(() => {
  const s = props.message.sender;
  if (s.kind === 'resolved') {
    const handle = props.handles.get(s.handle);
    return handle ? { handle } : { text: THREAD_TEXT.senderNotLoaded };
  }
  return { text: SENDER_TEXT[s.kind] };
});

/** A conversation event or another non-message item. */
const eventText = computed(() => {
  const item = props.message.item;
  return item !== null && item !== 'message' ? ITEM_KIND_TEXT[item] : null;
});

const associationText = computed(() => {
  const a = props.message.association;
  if (!a) return null;
  return a.kind ? ASSOC_KIND_TEXT[a.kind] : PREVIEW_BODY_TEXT.reaction;
});

const attachmentText = computed(() => {
  const n = props.message.attachments.length + props.message.moreAttachments;
  if (n === 0) return null;
  return n === 1 ? THREAD_TEXT.oneAttachment : fill(THREAD_TEXT.attachments, { n });
});

const directionClass = computed(() => `oma-dir-${props.message.direction}`);
</script>

<template>
  <div v-if="dayLabel" class="oma-day-sep" aria-hidden="true">{{ dayLabel }}</div>
  <article
    :id="base"
    class="oma-thread-item"
    :class="directionClass"
    tabindex="0"
    aria-setsize="-1"
    :aria-labelledby="`${base}-h`"
    :aria-describedby="`${base}-b`"
  >
    <header :id="`${base}-h`" class="oma-item-head">
      <span class="oma-item-sender">
        <SafeText v-if="'handle' in sender" :value="sender.handle.address" mode="identifier" ltr quiet />
        <template v-else>{{ sender.text }}</template>
      </span>
      <span v-if="message.direction === 'notRecorded'" class="oma-badge-inline">{{ DIRECTION_TEXT.notRecorded }}</span>
      <span class="oma-item-time">{{ time.text }}</span>
      <span v-if="time.kind === 'instant' && time.before2007" class="oma-badge-inline">{{ BEFORE_2007_TEXT }}</span>
    </header>
    <div :id="`${base}-b`" class="oma-item-body oma-contained">
      <p v-if="eventText" class="oma-item-event">{{ eventText }}</p>
      <p v-if="associationText" class="oma-item-event">{{ associationText }}</p>
      <p v-if="message.body.kind === 'text'" class="oma-item-text">
        <template v-for="(seg, i) in message.body.segments" :key="i"
          ><SafeText
            v-if="seg.piece.kind === 'text'"
            :value="seg.piece.value"
            :show-hidden="settings.viewer.showHiddenCharacters"
          /><span v-else-if="seg.piece.kind === 'withheld'" class="oma-masked">{{ MASK_TEXT[seg.piece.mask] }}</span
          ><span v-else-if="seg.piece.kind === 'inlineEmoji'" class="oma-missing">{{ SEGMENT_TEXT.inlineEmoji }}</span
          ><span v-else-if="seg.piece.kind === 'attachment'" class="oma-missing">{{ THREAD_TEXT.attachmentInText }}</span
          ><span v-else class="oma-missing">{{ SEGMENT_TEXT.unmatchedPlaceholder }}</span></template
        >
      </p>
      <p v-else-if="message.body.kind === 'noText'" class="oma-missing">
        {{ message.body.detail.reason ? DISPLAY_NONE_TEXT[message.body.detail.reason] : PREVIEW_BODY_TEXT.noText }}
      </p>
      <p v-else-if="!eventText && !associationText" class="oma-missing">{{ PREVIEW_BODY_TEXT.noBody }}</p>
      <p v-if="attachmentText" class="oma-item-attachments">{{ attachmentText }}</p>
    </div>
  </article>
</template>
