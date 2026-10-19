<script setup lang="ts">
// One item of the thread feed (M5 §8.4, §10): an `article` labelled by its
// sender and time and described by its content, with a local-day
// separator and a service divider above it when they change. Since slice
// 5h2 its content is a MessageBubble (aligned by direction) or a centred
// SystemRow (events, unrecognised items, association rows that stay
// items). Since slice 5h3 each item has a Details button that shows the
// "As recorded" panel on the right, and its attachments are drawn.
//
// - A sender label shows above each run of messages when the conversation
//   has more than one recorded address (`showSender`); otherwise it stays
//   in the article's accessible name only. System rows name no sender in
//   their heading, so an event never asserts who acted.
// - Reveals (D115) re-read this one message through the store; the page's
//   own copy stays masked, and "Hide" drops the revealed copy.
//
// Since slice 5j a right click, the context-menu key or Shift+F10 on the
// article opens the app's message menu (copy actions, details) instead of
// the engine's, and the article cannot be dragged out.
//
// `aria-setsize="-1"`: the number of visible items is not known (the
// chat's count includes folded reactions and goes stale during imports),
// and `aria-posinset` is omitted because keyset pages give no exact
// ordinal (a stated deviation from the APG feed pattern).
import { computed, ref } from 'vue';
import type { HandleDto, MessageDto, RevealInput, SafeId, ServiceClassDto } from '@/types';
import SafeText from './SafeText.vue';
import MessageBubble from './MessageBubble.vue';
import SystemRow from './SystemRow.vue';
import { itemShape } from '@/services/messageView';
import { timeText } from '@/services/time';
import { BEFORE_2007_TEXT, BUBBLE_TEXT, DIRECTION_TEXT, SENDER_TEXT, SERVICE_TEXT, THREAD_TEXT, fill } from '@/services/viewerText';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';

const props = withDefaults(
  defineProps<{
    message: MessageDto;
    handles: ReadonlyMap<SafeId, HandleDto>;
    /** The day separator's text, when this item starts a new local day. */
    dayLabel: string | null;
    /** Show the sender above this bubble (the first of a run in a conversation with several addresses). */
    showSender?: boolean;
    /** The service this item changes to; `undefined` when it does not change. */
    serviceChange?: ServiceClassDto | null | undefined;
    /** The id prefix of the article (the reply panel uses its own, so ids stay unique). */
    idPrefix?: string;
    /** Shown in the reply panel: the quote of the originator is left out. */
    inPanel?: boolean;
  }>(),
  { showSender: false, idPrefix: 'oma-m-', inPanel: false },
);
const emit = defineEmits<{ jump: [originator: SafeId, chat: SafeId | null] }>();

const settings = useSettings();
const viewer = useViewer();

const shown = computed(() => viewer.shown(props.message));
const revealed = computed(() => viewer.revealed.get(props.message.id)?.reveal ?? null);
const error = ref<string | null>(null);

const shape = computed(() => itemShape(shown.value));
const system = computed(() => shape.value !== 'bubble');
const base = computed(() => `${props.idPrefix}${props.message.id}`);
const time = computed(() => timeText(shown.value.date, settings.viewer));

const sender = computed<{ handle: HandleDto } | { text: string }>(() => {
  const s = shown.value.sender;
  if (s.kind === 'resolved') {
    const handle = props.handles.get(s.handle);
    return handle ? { handle } : { text: THREAD_TEXT.senderNotLoaded };
  }
  return { text: SENDER_TEXT[s.kind] };
});

/** A sender that is not a resolved address is a qualifier: always shown. */
const senderVisible = computed(() => props.showSender || 'text' in sender.value);

const itemClass = computed(() =>
  system.value ? 'oma-thread-item oma-system-item' : `oma-thread-item oma-dir-${shown.value.direction}`,
);
const senderClass = computed(() => (senderVisible.value ? 'oma-item-sender' : 'oma-item-sender visually-hidden'));

const serviceText = computed(() => {
  if (props.serviceChange === undefined) return null;
  // `null`: the service cell was NULL, absent or unreadable (Rust stores
  // unrecognised text as `other` and empty text as `empty`).
  return props.serviceChange === null
    ? BUBBLE_TEXT.serviceNotRecorded
    : fill(BUBBLE_TEXT.serviceChanged, { service: SERVICE_TEXT[props.serviceChange] });
});

function openDetails(): void {
  // Close gives focus back to this article.
  viewer.openDetails(props.message.id, false, base.value);
}

async function reveal(what: RevealInput): Promise<void> {
  error.value = await viewer.revealMessage(props.message.id, what);
}

async function hide(what: RevealInput): Promise<void> {
  error.value = await viewer.unreveal(props.message.id, what);
}

/**
 * The message menu, at the pointer; from the keyboard (no pointer
 * position), at the article's top left.
 */
function onContextMenu(event: MouseEvent): void {
  event.preventDefault();
  const article = event.currentTarget as HTMLElement;
  let x = event.clientX;
  let y = event.clientY;
  if (x === 0 && y === 0) {
    const r = article.getBoundingClientRect();
    x = r.left + 16;
    y = r.top + 16;
  }
  viewer.openMenu({ message: shown.value, handles: props.handles, x, y, returnTo: base.value });
}
</script>

<template>
  <div v-if="dayLabel" class="oma-day-sep" aria-hidden="true">{{ dayLabel }}</div>
  <div v-if="serviceText" class="oma-service-sep" aria-hidden="true">{{ serviceText }}</div>
  <article
    :id="base"
    :class="itemClass"
    tabindex="0"
    draggable="false"
    aria-setsize="-1"
    :aria-labelledby="`${base}-h`"
    :aria-describedby="`${base}-b`"
    @contextmenu="onContextMenu"
  >
    <header :id="`${base}-h`" class="oma-item-head">
      <span v-if="!system" :class="senderClass">
        <SafeText v-if="'handle' in sender" :value="sender.handle.address" mode="identifier" ltr quiet />
        <template v-else>{{ sender.text }}</template>
      </span>
      <span v-if="!system && shown.direction === 'received' && 'handle' in sender" class="visually-hidden">{{
        BUBBLE_TEXT.received
      }}</span>
      <span v-if="shown.direction === 'notRecorded'" class="oma-badge-inline">{{ DIRECTION_TEXT.notRecorded }}</span>
      {{ ' ' }}<span class="oma-item-time">{{ time.text }}</span>
      <span v-if="time.kind === 'instant' && time.before2007" class="oma-badge-inline">{{ BEFORE_2007_TEXT }}</span>
    </header>
    <div :id="`${base}-b`" class="oma-item-body">
      <span v-if="serviceText" class="visually-hidden">{{ serviceText }}</span>
      <SystemRow
        v-if="system"
        :message="shown"
        :handles="handles"
        :revealed="revealed"
        :id-base="base"
        @reveal="reveal"
        @hide="hide"
      />
      <MessageBubble
        v-else
        :message="shown"
        :handles="handles"
        :revealed="revealed"
        :error="error"
        :id-base="base"
        :in-panel="inPanel"
        @reveal="reveal"
        @hide="hide"
        @jump="(o, c) => emit('jump', o, c)"
      />
      <p v-if="system && error" class="oma-body-note" role="alert">{{ BUBBLE_TEXT.revealFailed }} {{ error }}</p>
    </div>
    <p class="oma-item-actions">
      <button
        type="button"
        class="btn btn-link btn-sm oma-inline-btn"
        :aria-describedby="`${base}-h`"
        @click="openDetails"
      >
        {{ BUBBLE_TEXT.details }}
      </button>
    </p>
  </article>
</template>
