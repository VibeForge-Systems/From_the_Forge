<script setup lang="ts">
// The play confirmation (M5 §5.6; owner Q4: `mediaPlayback` is `confirm`
// by default). It opens on the first Play of each file in a viewer
// session, after Rust's structural check and full hash have passed, and
// names the risk: the system's codecs will parse bytes a sender chose.
// "Always play without asking" switches the setting to `clickToPlay`;
// Settings explains it and can switch it back. Escape or Cancel plays
// nothing. Mounted once, in the viewer page.
import { computed } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import SafeText from './SafeText.vue';
import { ATTACHMENT_TEXT, MEDIA_UI_TEXT, PLAY_CONFIRM_TEXT } from '@/services/viewerText';
import { useViewer } from '@/stores/viewer';

const viewer = useViewer();
const attachment = computed(() => viewer.pendingPlay);
</script>

<template>
  <ModalDialog
    :open="attachment !== null"
    :title="PLAY_CONFIRM_TEXT.title"
    :return-to="viewer.playReturnTo()"
    @close="viewer.answerPlay('cancel')"
  >
    <template v-if="attachment">
      <p class="oma-play-file">
        <SafeText v-if="attachment.name.kind === 'text'" :value="attachment.name" mode="identifier" />
        <span v-else class="oma-missing">{{ ATTACHMENT_TEXT.noName }}</span>
      </p>
      <p>{{ MEDIA_UI_TEXT.confirm }}</p>
      <p class="form-text mb-0">{{ PLAY_CONFIRM_TEXT.alwaysHint }}</p>
    </template>
    <template #footer>
      <button type="button" class="btn btn-outline-secondary" @click="viewer.answerPlay('cancel')">
        {{ PLAY_CONFIRM_TEXT.cancel }}
      </button>
      <button type="button" class="btn btn-outline-secondary" @click="viewer.answerPlay('always')">
        {{ PLAY_CONFIRM_TEXT.always }}
      </button>
      <button type="button" class="btn btn-primary" @click="viewer.answerPlay('play')">
        {{ PLAY_CONFIRM_TEXT.play }}
      </button>
    </template>
  </ModalDialog>
</template>
