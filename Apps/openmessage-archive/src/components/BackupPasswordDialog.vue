<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import type { JobKind } from '@/types';

// The password lives only in this component's ref, and only while the
// dialog is open. It is handed to the parent once, on submit, and the ref is
// cleared at once. No store field is bound to it.
// `resume` is the job the password starts, so the button says what the
// click does: the extraction check also copies and decrypts sms.db. A
// caller can name the action itself with `submitLabel` (the import does).
const props = withDefaults(
  defineProps<{
    open: boolean;
    deviceName: string | null;
    error: string | null;
    resume?: JobKind;
    submitLabel?: string | null;
  }>(),
  { resume: 'validate', submitLabel: null },
);
const RESUME_LABEL: Record<JobKind, string> = {
  validate: 'Unlock and validate',
  extractionCheck: 'Unlock and run extraction check',
  import: 'Unlock and prepare import',
  verify: 'Unlock',
  export: 'Unlock',
};
const buttonLabel = computed(() => props.submitLabel ?? RESUME_LABEL[props.resume]);
const emit = defineEmits<{ submit: [password: string]; close: [] }>();

const password = ref('');
const reveal = ref(false);
const capsLock = ref(false);
const input = ref<HTMLInputElement | null>(null);

function clear(): void {
  password.value = '';
  reveal.value = false;
  capsLock.value = false;
}

function submit(): void {
  if (password.value === '') return;
  const value = password.value;
  clear();
  emit('submit', value);
}

function close(): void {
  clear();
  emit('close');
}

function onKey(event: KeyboardEvent): void {
  capsLock.value = event.getModifierState?.('CapsLock') ?? false;
}

watch(
  () => [props.open, props.error] as const,
  async ([open]) => {
    if (!open) {
      clear();
      return;
    }
    clear();
    await nextTick();
    input.value?.focus();
  },
);

onBeforeUnmount(clear);
</script>

<template>
  <ModalDialog :open="open" title="Unlock encrypted backup" @close="close">
    <form id="backup-password-form" novalidate @submit.prevent="submit">
      <p id="backup-password-help">
        Enter the password that was set for encrypted backups of {{ deviceName ?? 'this device' }}.
        It is used once to unlock the backup and is never saved.
      </p>
      <p class="small text-body-secondary">Unlocking is deliberately slow and can take a while.</p>
      <label for="backup-password" class="form-label">Backup password</label>
      <input
        id="backup-password"
        ref="input"
        v-model="password"
        :type="reveal ? 'text' : 'password'"
        autocomplete="off"
        autocapitalize="off"
        autocorrect="off"
        spellcheck="false"
        name="oma-backup-password"
        class="form-control"
        :aria-invalid="!!error"
        aria-describedby="backup-password-help backup-password-error"
        @keydown="onKey"
        @keyup="onKey"
      />
      <div class="form-check mt-2">
        <input id="reveal" v-model="reveal" type="checkbox" class="form-check-input" />
        <label for="reveal" class="form-check-label">Show password</label>
      </div>
      <p v-if="capsLock" class="small text-warning mb-0">Caps Lock is on.</p>
      <p v-if="error" id="backup-password-error" class="text-danger small mt-2 mb-0" role="alert">
        {{ error }}
      </p>
    </form>
    <template #footer>
      <button type="button" class="btn btn-outline-secondary" @click="close">Cancel</button>
      <button
        type="submit"
        form="backup-password-form"
        class="btn btn-primary"
        :disabled="password === ''"
      >
        {{ buttonLabel }}
      </button>
    </template>
  </ModalDialog>
</template>
