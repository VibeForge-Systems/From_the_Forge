<script setup lang="ts">
// The privacy curtain (M5 §9.2, D108), slice 5j: an opaque modal
// `<dialog>`, opened after every viewer dialog has closed, so nothing sits
// above it in the top layer; its backdrop covers the whole window. The
// viewer's content is already unmounted behind it (`stores/privacy.ts`).
// Focus moves to "Show"; Escape does not dismiss it (an engine that closes
// it anyway gets it reopened); there is no hover reveal. It names itself
// honestly: it hides the screen and is not a lock. Mounted once, in App.vue.
import { computed, nextTick, ref, watch } from 'vue';
import { PRIVACY_TEXT } from '@/services/viewerText';
import { usePrivacy } from '@/stores/privacy';

const privacy = usePrivacy();
const dialog = ref<HTMLDialogElement | null>(null);
const showButton = ref<HTMLButtonElement | null>(null);

const hidden = computed(() => privacy.hidden);

async function sync(open: boolean): Promise<void> {
  await nextTick();
  const el = dialog.value;
  if (!el) return;
  if (open) {
    if (!el.open && typeof el.showModal === 'function') el.showModal();
    showButton.value?.focus();
  } else if (el.open) {
    el.close();
  }
}

watch(hidden, (open) => void sync(open), { immediate: true, flush: 'post' });

/** Escape is not a way out: only "Show" is. */
function onCancel(event: Event): void {
  event.preventDefault();
}

/** An engine that closed it anyway (repeated Escape) gets it back. */
function onClose(): void {
  if (privacy.hidden) void sync(true);
}
</script>

<template>
  <dialog
    ref="dialog"
    class="oma-curtain"
    aria-labelledby="oma-curtain-title"
    aria-describedby="oma-curtain-text"
    @cancel="onCancel"
    @close="onClose"
  >
    <div v-if="hidden" class="oma-curtain-body">
      <h2 id="oma-curtain-title" class="h4">{{ PRIVACY_TEXT.title }}</h2>
      <p id="oma-curtain-text">{{ PRIVACY_TEXT.notALock }}</p>
      <p v-if="privacy.reason === 'focusLoss'">{{ PRIVACY_TEXT.focusLoss }}</p>
      <p v-if="privacy.closed">{{ PRIVACY_TEXT.idle }}</p>
      <button
        ref="showButton"
        type="button"
        class="btn btn-primary"
        :disabled="privacy.showing"
        @click="privacy.show()"
      >
        {{ PRIVACY_TEXT.show }}
      </button>
      <p v-if="privacy.showing" class="mt-2 mb-0" role="status">{{ PRIVACY_TEXT.showing }}</p>
    </div>
  </dialog>
</template>
