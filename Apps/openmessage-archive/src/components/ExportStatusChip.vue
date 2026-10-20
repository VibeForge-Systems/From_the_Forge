<script setup lang="ts">
// The export's status chip in the navigation bar (M6 spec §10 step 6,
// slice 6k2). It shows while an export runs with its dialog closed
// ("Continue in background", or the privacy screen closed the dialog),
// and after such an export ended until its result is read. Activating it
// reopens the dialog at the progress or the result. Its words name the
// state only: never a conversation, a name or a count.
import { computed } from 'vue';
import { CHIP_TEXT } from '@/services/exportText';
import { useExports } from '@/stores/exports';

const x = useExports();

const state = computed<'running' | 'cancelling' | 'finished' | 'failed' | 'cancelled'>(() => {
  const job = x.liveJob;
  if (job) return job.cancelling ? 'cancelling' : 'running';
  const r = x.result;
  if (r?.kind === 'finished') return 'finished';
  if (r?.kind === 'failed') return 'failed';
  return 'cancelled';
});
const running = computed(() => x.liveJob !== null);
const label = computed(() => CHIP_TEXT[state.value]);
</script>

<template>
  <li v-if="x.chipVisible" class="nav-item d-flex align-items-center ms-2">
    <button
      type="button"
      class="btn btn-sm btn-outline-primary d-flex align-items-center gap-1 oma-export-chip"
      aria-describedby="oma-export-chip-hint"
      @click="x.reopen()"
    >
      <span v-if="running" class="spinner-border spinner-border-sm" aria-hidden="true"></span>
      <i v-else class="bi bi-box-arrow-up" aria-hidden="true"></i>
      <span role="status" aria-live="polite">{{ label }}</span>
    </button>
    <span id="oma-export-chip-hint" class="visually-hidden">{{ running ? CHIP_TEXT.show : CHIP_TEXT.showResult }}</span>
  </li>
</template>
