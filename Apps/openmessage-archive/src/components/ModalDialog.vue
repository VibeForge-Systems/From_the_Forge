<script setup lang="ts">
import { onMounted, ref, useId, watch } from 'vue';

// Built on the native <dialog> element: showModal() provides the focus
// trap, Escape handling, and inert background that a hand-rolled modal
// would have to reimplement.
const props = defineProps<{ open: boolean; title: string }>();
const emit = defineEmits<{ close: [] }>();

const dialog = ref<HTMLDialogElement | null>(null);
const titleId = useId();

function sync(open: boolean): void {
  const el = dialog.value;
  if (!el || typeof el.showModal !== 'function') return;
  if (open && !el.open) el.showModal();
  if (!open && el.open) el.close();
}

onMounted(() => sync(props.open));
watch(() => props.open, sync);

function onBackdropClick(event: MouseEvent): void {
  if (event.target === dialog.value) emit('close');
}
</script>

<template>
  <dialog
    ref="dialog"
    class="oma-dialog p-0"
    :aria-labelledby="titleId"
    @close="emit('close')"
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
