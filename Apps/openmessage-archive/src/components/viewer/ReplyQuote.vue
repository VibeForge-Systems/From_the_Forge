<script setup lang="ts">
// A reply's quote chip (M5 §3.6), slice 5h2: two lines of the
// originator's current body, built and masked in Rust (a quote never
// reveals), that jump to the originator. An originator in another
// conversation says so before the jump, and a reference Rust could not
// resolve to one keyed message says that instead; it is not a control.
import { computed } from 'vue';
import type { ReplyRefDto, SafeId } from '@/types';
import SafeText from './SafeText.vue';
import { previewSentence } from '@/services/messageView';
import { BUBBLE_TEXT, REPLY_TEXT } from '@/services/viewerText';

const props = defineProps<{ reply: ReplyRefDto }>();
const emit = defineEmits<{ jump: [originator: SafeId, chat: SafeId | null] }>();

const quote = computed(() => (props.reply.kind === 'notUniquelyIdentified' ? null : props.reply.quote));
const canJump = computed(
  () => props.reply.kind === 'resolved' || (props.reply.kind === 'inAnotherConversation' && props.reply.chat !== null),
);

function jump(): void {
  const r = props.reply;
  if (r.kind === 'resolved') emit('jump', r.originator, null);
  else if (r.kind === 'inAnotherConversation' && r.chat !== null) emit('jump', r.originator, r.chat);
}
</script>

<template>
  <div class="oma-reply">
    <p class="oma-reply-head">{{ REPLY_TEXT[reply.kind] }}</p>
    <p v-if="quote" class="oma-reply-quote oma-contained">
      <SafeText v-if="quote.kind === 'text'" :pieces="quote.pieces" quiet />
      <span v-else class="oma-missing">{{ previewSentence(quote, null) }}</span>
    </p>
    <button v-if="canJump" type="button" class="btn btn-link btn-sm oma-inline-btn" @click="jump">
      {{ reply.kind === 'resolved' ? BUBBLE_TEXT.replyJump : BUBBLE_TEXT.replyOtherChat }}
    </button>
  </div>
</template>
