<script setup lang="ts">
// A message's attachments (slice 5h3), in the order Rust listed them
// (M4 §6.5 order). Attachments placed in the text are listed here too;
// the text names them "Attachment N (shown below)". Attachments over the
// page's cap are counted, never invented.
import { computed } from 'vue';
import type { MessageDto } from '@/types';
import AttachmentView from './AttachmentView.vue';
import { ATTACHMENT_TEXT, fill } from '@/services/viewerText';

const props = defineProps<{ message: MessageDto; idBase: string }>();

const more = computed(() => {
  const n = props.message.moreAttachments;
  if (n <= 0) return null;
  return n === 1 ? ATTACHMENT_TEXT.oneMore : fill(ATTACHMENT_TEXT.more, { n });
});
</script>

<template>
  <div v-if="message.attachments.length || more" class="oma-attachments">
    <AttachmentView
      v-for="(a, i) in message.attachments"
      :key="a.id"
      :attachment="a"
      :index="i + 1"
      :id-base="idBase"
    />
    <p v-if="more" class="oma-body-note">{{ more }}</p>
  </div>
</template>
