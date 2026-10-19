<script setup lang="ts">
import { onMounted, ref, useId, watch } from 'vue';

// Built on the native <dialog> element: showModal() provides the focus
// trap, Escape handling, and inert background that a hand-rolled modal
// would have to reimplement. showModal() moves focus to the first control;
// on close, focus returns to what had it before (M5 §10, slice 5j), also on
// engines that do not restore it themselves.
const props = defineProps<{ open: boolean; title: string }>();
const emit = defineEmits<{ close: [] }>();

const dialog = ref<HTMLDialogElement | null>(null);
const titleId = useId();
let opener: HTMLElement | null = null;

/** Give focus back to the opener, unless the user already moved it elsewhere. */
function restoreFocus(): void {
  const back = opener;
  opener = null;
  if (!back || !back.isConnected) return;
  const now = document.activeElement;
  if (now === null || now === document.body || (dialog.value?.contains(now) ?? false)) {
    back.focus({ preventScroll: true });
  }
}

function sync(open: boolean): void {
  const el = dialog.value;
  if (!el || typeof el.showModal !== 'function') return;
  if (open && !el.open) {
    opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    el.showModal();
  }
  if (!open && el.open) {
    el.close();
    restoreFocus();
  }
}

onMounted(() => sync(props.open));
watch(() => props.open, sync);

/**
 * The native `close` event is dispatched asynchronously. Report it only when
 * the user closed the dialog (Escape) while it was meant to be open; a stale
 * event from a programmatic close that arrives after the dialog was reopened
 * must not close it again.
 */
function onNativeClose(): void {
  if (props.open && !dialog.value?.open) {
    restoreFocus();
    emit('close');
  }
}

function onBackdropClick(event: MouseEvent): void {
  if (event.target === dialog.value) emit('close');
}
</script>

<template>
  <dialog
    ref="dialog"
    class="oma-dialog p-0"
    :aria-labelledby="titleId"
    @close="onNativeClose"
    @click="onBackdropClick"
  >
    <div class="modal-content">
      <div class="modal-header">
        <h2 :id="titleId" class="modal-title fs-5">{{ title }}</h2>
        <button type="button" class="btn-close" aria-label="Close" @click="emit('close')"></button>
      </div>
      <div class="modal-body"><slot /></div>
      <div v-if="$slots.footer" class="modal-footer"><slot name="footer" /></div>
    </div>
  </dialog>
</template>
