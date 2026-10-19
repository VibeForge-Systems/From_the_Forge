<script setup lang="ts">
// A centred system row (M5 §3.6, §8.4), slice 5h2: conversation events,
// unrecognised items, and association rows that stay timeline items
// (legacy stickers, poll votes, and reactions that do not fold: their
// target is in another conversation, unreadable, not unique, itself a
// reaction, or whether they fold is undeterminable).
//
// - Events come from closed sentences. The addresses recorded with an
//   event are listed, and **no actor or target role is asserted**, so the
//   row's sender is not shown either. A renamed conversation shows the
//   recorded name as isolated text.
// - An unrecognised item names its raw `item_type` and
//   `group_action_type`, followed by any text.
// - Text recorded with the item (an SMS fallback such as a quoted
//   reaction) is shown as recorded and never parsed into a reaction.
import { computed } from 'vue';
import type { HandleDto, MessageDto, RevealInput, SafeId } from '@/types';
import SafeText from './SafeText.vue';
import MessageBody from './MessageBody.vue';
import { associationEmoji, associationSentences, eventSentence, itemShape } from '@/services/messageView';
import { BUBBLE_TEXT, SENDER_TEXT, THREAD_TEXT, fill } from '@/services/viewerText';

const props = defineProps<{
  message: MessageDto;
  handles: ReadonlyMap<SafeId, HandleDto>;
  revealed: RevealInput | null;
}>();
const emit = defineEmits<{ reveal: [what: RevealInput]; hide: [what: RevealInput] }>();

const shape = computed(() => itemShape(props.message));
const association = computed(() => props.message.association);
const sentences = computed(() =>
  association.value ? associationSentences(association.value) : [eventSentence(props.message)],
);
const emoji = computed(() => (association.value && associationEmoji(association.value) ? association.value.emoji : null));

/** For an association row, the row's own sender (who reacted). */
const sender = computed<{ handle: HandleDto } | { text: string } | null>(() => {
  if (shape.value !== 'association') return null;
  const s = props.message.sender;
  if (s.kind === 'resolved') {
    const handle = props.handles.get(s.handle);
    return handle ? { handle } : { text: BUBBLE_TEXT.addressNotLoaded };
  }
  return { text: SENDER_TEXT[s.kind] };
});

const event = computed(() => (shape.value === 'event' || shape.value === 'unknown' ? props.message.event : null));
const eventHandles = computed(() =>
  (event.value?.handles ?? []).map((id) => ({ id, handle: props.handles.get(id) ?? null })),
);
const showBody = computed(() => props.message.body.kind === 'text');

const attachments = computed(() => {
  const n = props.message.attachments.length + props.message.moreAttachments;
  if (n === 0) return null;
  return n === 1 ? THREAD_TEXT.oneAttachment : fill(THREAD_TEXT.attachments, { n });
});
</script>

<template>
  <div class="oma-system oma-contained">
    <p class="oma-system-text">
      <template v-for="(s, i) in sentences" :key="i"
        ><template v-if="i > 0"> · </template>{{ s }}</template
      >
      <template v-if="emoji">: <SafeText :value="emoji" quiet /></template>
      <template v-if="sender">
        {{ BUBBLE_TEXT.reactionBy }}
        <SafeText v-if="'handle' in sender" :value="sender.handle.address" mode="identifier" ltr quiet />
        <template v-else>{{ sender.text }}</template>
      </template>
    </p>

    <p v-if="event && message.item === 'groupRenamed' && event.title" class="oma-system-text">
      {{ BUBBLE_TEXT.newTitle }} <SafeText :value="event.title" mode="identifier" />
    </p>
    <p v-if="event && shape === 'event'" class="oma-system-text">
      <template v-if="eventHandles.length">
        {{ BUBBLE_TEXT.eventAddresses }}
        <template v-for="(h, i) in eventHandles" :key="h.id"
          ><template v-if="i > 0">, </template
          ><SafeText v-if="h.handle" :value="h.handle.address" mode="identifier" ltr quiet /><span
            v-else
            class="oma-missing"
            >{{ BUBBLE_TEXT.addressNotLoaded }}</span
          ></template
        >
      </template>
      <template v-else>{{ BUBBLE_TEXT.eventNoAddresses }}</template>
    </p>

    <div v-if="showBody" class="oma-system-body">
      <p class="oma-body-note">{{ BUBBLE_TEXT.recordedText }}</p>
      <MessageBody
        :message="message"
        :codes-revealed="revealed?.oneTimeCode === true"
        @reveal-code="emit('reveal', { oneTimeCode: true })"
        @hide-code="emit('hide', { oneTimeCode: true })"
      />
    </div>
    <p v-if="attachments" class="oma-body-note">{{ attachments }}</p>
  </div>
</template>
