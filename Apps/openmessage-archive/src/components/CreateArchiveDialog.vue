<script setup lang="ts">
import { nextTick, ref, watch } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import { LABEL_MAX_BYTES, checkLabel, useArchives } from '@/stores/archives';
import { UNENCRYPTED_NOTE } from '@/services/importText';

const archives = useArchives();
const label = ref('');
const input = ref<HTMLInputElement | null>(null);
const localError = ref<string | null>(null);

watch(
  () => archives.createOpen,
  async (open) => {
    label.value = '';
    localError.value = null;
    if (open) {
      await nextTick();
      input.value?.focus();
    }
  },
);

function onInput(): void {
  const c = checkLabel(label.value);
  localError.value = c.ok ? null : c.error;
}

async function submit(): Promise<void> {
  if (archives.creating) return;
  const c = checkLabel(label.value);
  if (!c.ok) {
    localError.value = c.error;
    return;
  }
  await archives.create(label.value);
}
</script>

<template>
  <ModalDialog :open="archives.createOpen" title="Create archive" @close="archives.closeCreate()">
    <form id="create-archive-form" novalidate @submit.prevent="submit">
      <label for="archive-label" class="form-label">Name (optional)</label>
      <input
        id="archive-label"
        ref="input"
        v-model="label"
        type="text"
        class="form-control"
        autocomplete="off"
        spellcheck="false"
        :aria-invalid="!!(localError ?? archives.createError)"
        aria-describedby="archive-label-help archive-label-error"
        @input="onInput"
      />
      <p id="archive-label-help" class="small text-body-secondary mt-1">
        Up to {{ LABEL_MAX_BYTES }} bytes. The name is stored inside the archive, not as a folder name.
      </p>
      <p class="small alert alert-warning mb-0" role="note">
        <i class="bi bi-unlock me-1" aria-hidden="true"></i>{{ UNENCRYPTED_NOTE }} Archive
        encryption is planned for a later milestone.
      </p>
      <p
        v-if="localError ?? archives.createError"
        id="archive-label-error"
        class="text-danger small mt-2 mb-0"
        role="alert"
      >
        {{ localError ?? archives.createError }}
      </p>
    </form>
    <template #footer>
      <button type="button" class="btn btn-outline-secondary" @click="archives.closeCreate()">Cancel</button>
      <button
        type="submit"
        form="create-archive-form"
        class="btn btn-primary"
        :disabled="archives.creating || localError !== null"
      >
        Create archive
      </button>
    </template>
  </ModalDialog>
</template>
