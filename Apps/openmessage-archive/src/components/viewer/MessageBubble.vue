<script setup lang="ts">
// One message bubble (M5 §3.6, §8.4), slice 5h2: the reply quote, the
// body, edits and unsends per part, "N stored versions", withheld earlier
// text, labels as text badges, Recently Deleted lines, reaction chips and
// the reply count. Since slice 5h3 it lists the attachments (cards,
// thumbnails and Play cards), and the reply count and the reactions open
// the reply thread and the reaction rows in the right panel.
//
// Never shown (I5, D94): read, delivery or sent state; "not edited"; a
// whole-bubble "Unsent" unless every part is listed. Earlier text stays
// withheld in Rust until "Show earlier text" (D115) re-reads this one
// message with the reveal flag; the `revealEarlierText` setting reveals it
// on every page instead.
import { computed } from 'vue';
import type { HandleDto, MessageDto, RevealInput, SafeId } from '@/types';
import SafeText from './SafeText.vue';
import MessageBody from './MessageBody.vue';
import ReactionChips from './ReactionChips.vue';
import ReplyQuote from './ReplyQuote.vue';
import AttachmentList from './AttachmentList.vue';
import { editView, hasReactions, replyCountText } from '@/services/messageView';
import { formatInstant } from '@/services/time';
import {
  BUBBLE_TEXT,
  MESSAGE_LABEL_TEXT,
  REVEALED_TEXT,
  WITHHELD_TEXT,
  fill,
} from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = defineProps<{
  message: MessageDto;
  handles: ReadonlyMap<SafeId, HandleDto>;
  /** What this message was re-read with, when the user revealed it. */
  revealed: RevealInput | null;
  /** The last reveal failed. */
  error: string | null;
  /** The article's id prefix, for the attachments' captions. */
  idBase: string;
  /** In the reply panel, whose heading names the originator: no quote. */
  inPanel?: boolean;
}>();
const emit = defineEmits<{
  reveal: [what: RevealInput];
  hide: [what: RevealInput];
  jump: [originator: SafeId, chat: SafeId | null];
}>();

const settings = useSettings();
const viewer = useViewer();

const edits = computed(() => editView(props.message.edit));
const earlier = computed(() => props.message.edit.earlier);

function importDate(session: SafeId): string | null {
  const found = viewer.session?.imports.find((i) => i.session === session);
  return found ? formatInstant(found.startedMs, settings.viewer, 'date') : null;
}

const revealedFrom = computed(() => {
  const e = earlier.value;
  if (e.kind !== 'revealed') return null;
  const date = importDate(e.session);
  return date ? fill(BUBBLE_TEXT.revealedFrom, { date }) : null;
});

const labels = computed(() =>
  props.message.labels.map((l) =>
    l.kind === 'scheduleType' ? fill(MESSAGE_LABEL_TEXT.scheduleType, { value: l.value }) : MESSAGE_LABEL_TEXT[l.kind],
  ),
);

const deleted = computed(() =>
  props.message.recentlyDeleted.map((rd) => {
    const date = importDate(rd.session);
    const later = rd.notListedLater.map((s) => {
      const d = importDate(s);
      return d ? fill(BUBBLE_TEXT.notListedLater, { date: d }) : BUBBLE_TEXT.notListedLaterUnknown;
    });
    return {
      key: rd.session,
      text: date ? fill(BUBBLE_TEXT.recentlyDeleted, { date }) : BUBBLE_TEXT.recentlyDeletedUnknown,
      later,
    };
  }),
);

const recordedBy = computed(() => {
  const r = props.message.directionRecordedBy;
  return r ? fill(BUBBLE_TEXT.directionRecordedBy, { lineage: r.lineage }) : null;
});

const replies = computed(() => replyCountText(props.message.replyCount));
const reactions = computed(() => (hasReactions(props.message.reactions) ? props.message.reactions : null));
const canHideEarlier = computed(() => earlier.value.kind === 'revealed' && props.revealed?.earlierText === true);
</script>

<template>
  <div class="oma-bubble oma-contained">
    <ReplyQuote v-if="message.reply && !inPanel" :reply="message.reply" @jump="(o, c) => emit('jump', o, c)" />

    <MessageBody
      :message="message"
      :codes-revealed="revealed?.oneTimeCode === true"
      @reveal-code="emit('reveal', { oneTimeCode: true })"
      @hide-code="emit('hide', { oneTimeCode: true })"
    />

    <div v-if="earlier.kind === 'withheld'" class="oma-earlier">
      <span>{{ WITHHELD_TEXT[earlier.reason] }}</span>
      <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="emit('reveal', { earlierText: true })">
        {{ BUBBLE_TEXT.showEarlier }}
      </button>
    </div>
    <div v-else-if="earlier.kind === 'revealed'" class="oma-earlier oma-earlier-shown">
      <p class="oma-body-note">
        {{ REVEALED_TEXT[earlier.reason] }}<template v-if="revealedFrom"> ({{ revealedFrom }})</template>
      </p>
      <p class="oma-earlier-text">
        <SafeText :pieces="earlier.pieces" :show-hidden="settings.viewer.showHiddenCharacters" />
      </p>
      <button v-if="canHideEarlier" type="button" class="btn btn-link btn-sm oma-inline-btn" @click="emit('hide', { earlierText: true })">
        {{ BUBBLE_TEXT.hideEarlier }}
      </button>
    </div>
    <p v-if="error" class="oma-body-note" role="alert">{{ BUBBLE_TEXT.revealFailed }} {{ error }}</p>

    <p v-if="edits.edited || edits.unsent || edits.label || edits.versions" class="oma-bubble-badges">
      <span v-if="edits.edited" class="oma-badge-inline">{{ edits.edited }}</span>
      <span v-if="edits.unsent" class="oma-badge-inline">{{ edits.unsent }}</span>
      <span v-if="edits.label" class="oma-badge-inline">{{ edits.label }}</span>
      <span v-if="edits.versions" class="oma-badge-inline">{{ edits.versions }}</span>
    </p>
    <p v-if="labels.length" class="oma-bubble-badges">
      <span v-for="(l, i) in labels" :key="i" class="oma-badge-inline">{{ l }}</span>
    </p>
    <p v-for="rd in deleted" :key="rd.key" class="oma-body-note">
      {{ rd.text }}<template v-for="(l, i) in rd.later" :key="i">; {{ l }}</template>
    </p>
    <p v-if="recordedBy" class="oma-body-note">{{ recordedBy }}</p>
    <AttachmentList :message="message" :id-base="idBase" />

    <ReactionChips v-if="reactions" :reactions="reactions" :handles="handles" />
    <p v-if="reactions" class="mb-0">
      <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="viewer.openDetails(message.id, true)">
        {{ BUBBLE_TEXT.reactionRows }}
      </button>
    </p>
    <p v-if="replies" class="mb-0">
      <button type="button" class="btn btn-link btn-sm oma-inline-btn" @click="viewer.openReplies(message.id)">
        {{ replies }}
      </button>
    </p>
    <p v-if="!message.extras" class="oma-body-note">{{ BUBBLE_TEXT.extrasMissing }}</p>
  </div>
</template>
