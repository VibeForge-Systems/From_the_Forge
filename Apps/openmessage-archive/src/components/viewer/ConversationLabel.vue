<script setup lang="ts">
// A conversation's label, with the precedence of M5 §8.4. Nothing is
// invented and no contacts are used:
// 1. the recorded name;
// 2. "Name recorded but too large or unreadable" (it does not fall through);
// 3. the participants' addresses as recorded, "a, b, c +N (from participants)";
// 4. the chat identifier;
// 5. "Conversation without a name or participants (#id)".
// Rust has already chosen the case (`ChatLabelDto`); this only renders it.
import type { ChatLabelDto, SafeId } from '@/types';
import SafeText from './SafeText.vue';
import { CHAT_LABEL_TEXT, LIST_TEXT, fill } from '@/services/viewerText';

defineProps<{ label: ChatLabelDto; chatId: SafeId; quiet?: boolean }>();
</script>

<template>
  <span class="oma-chat-label">
    <SafeText v-if="label.kind === 'name'" :value="label.value" mode="identifier" :quiet="quiet" />
    <span v-else-if="label.kind === 'nameUnreadable'" class="oma-missing">{{ CHAT_LABEL_TEXT.nameUnreadable }}</span>
    <template v-else-if="label.kind === 'participants'">
      <template v-for="(h, i) in label.first" :key="h.id"
        ><template v-if="i > 0">, </template
        ><SafeText :value="h.address" mode="identifier" ltr :quiet="quiet"
      /></template
      ><template v-if="label.more > 0">{{ ' ' }}{{ fill(LIST_TEXT.moreParticipants, { n: label.more }) }}</template
      ><span class="oma-label-note">{{ ' ' }}{{ CHAT_LABEL_TEXT.participants }}</span>
    </template>
    <SafeText v-else-if="label.kind === 'identifier'" :value="label.value" mode="identifier" ltr :quiet="quiet" />
    <span v-else class="oma-missing">{{ fill(LIST_TEXT.unnamedId, { id: chatId }) }}</span>
  </span>
</template>
