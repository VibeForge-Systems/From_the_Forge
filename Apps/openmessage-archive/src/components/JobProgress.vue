<script setup lang="ts">
import { computed } from 'vue';
import type { JobState } from '@/stores/backups';
import type { JobPhase } from '@/types';

const props = defineProps<{ job: JobState }>();
const emit = defineEmits<{ cancel: [] }>();

const nf = new Intl.NumberFormat();
const LABEL: Record<JobPhase, string> = {
  copyingManifest: "Copying the backup's file index",
  derivingKey: 'Checking the password',
  unwrappingKeys: 'Unlocking encryption keys',
  decryptingManifest: 'Decrypting the file index',
  checkingDatabase: 'Checking the file index database',
  checkingFiles: 'Checking file records',
  checkingMessagesDb: 'Checking the Messages database',
  extracting: 'Extracting a file',
  // The archive import (M4); counted in bytes.
  copyingAttachments: 'Copying attachments',
  // The extraction check (M3 §17.3).
  stagingMessagesDb: 'Copying the Messages database',
  scanningSchema: 'Checking the database layout',
  checkingIntegrity: 'Checking database integrity',
  readingSchema: 'Reading the database layout',
  readingHandles: 'Reading addresses',
  readingChats: 'Reading chats',
  readingChatLinks: 'Reading chat links',
  readingMessages: 'Reading messages',
  checkingAttachments: 'Checking attachments',
  summarising: 'Summarising',
};

/** Phases whose `done` and `total` count items; the copy counts bytes. */
const COUNTED: ReadonlySet<JobPhase> = new Set<JobPhase>([
  'checkingFiles',
  'readingSchema',
  'readingHandles',
  'readingChats',
  'readingChatLinks',
  'readingMessages',
  'checkingAttachments',
  'summarising',
]);

const MIB = 1024 * 1024;
const mib = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const megabytes = (n: number) => `${mib.format(n / MIB)} MB`;

/** The phase alone: what the live region announces. */
const phaseLabel = computed(() => {
  const j = props.job;
  if (j.cancelling) return 'Cancelling…';
  if (j.phase === null) return 'Starting…';
  return LABEL[j.phase];
});

/** The phase with its count: shown, and exposed as the progressbar's value. */
const label = computed(() => {
  const j = props.job;
  if (j.cancelling || j.phase === null || j.total === null) return phaseLabel.value;
  if (COUNTED.has(j.phase)) {
    return `${LABEL[j.phase]} (${nf.format(j.done)} of ${nf.format(j.total)})`;
  }
  if (j.phase === 'stagingMessagesDb') {
    return `${LABEL.stagingMessagesDb} (${megabytes(j.done)} of ${megabytes(j.total)})`;
  }
  return phaseLabel.value;
});

const ariaLabel = computed(() =>
  props.job.kind === 'extractionCheck' ? 'Extraction check progress' : 'Validation progress',
);

const percent = computed(() => {
  const { done, total } = props.job;
  if (total === null || total === 0) return null;
  return Math.min(100, Math.round((done / total) * 100));
});
</script>

<template>
  <div class="card oma-job">
    <div class="card-body">
      <!-- Announces phase changes only, never the running count (progress
           arrives up to ten times a second). -->
      <span class="visually-hidden" aria-live="polite" aria-atomic="true">{{ phaseLabel }}</span>
      <div class="d-flex align-items-center gap-2 mb-2">
        <span class="spinner-border spinner-border-sm text-primary" aria-hidden="true"></span>
        <span class="fw-semibold me-auto oma-job-label" aria-hidden="true">{{ label }}</span>
        <button
          type="button"
          class="btn btn-sm btn-outline-secondary"
          :disabled="job.cancelling"
          @click="emit('cancel')"
        >
          Cancel
        </button>
      </div>
      <div
        class="progress"
        role="progressbar"
        :aria-label="ariaLabel"
        :aria-valuetext="label"
        :aria-valuenow="percent ?? undefined"
        aria-valuemin="0"
        aria-valuemax="100"
      >
        <div
          class="progress-bar"
          :class="{ 'progress-bar-striped progress-bar-animated w-100': percent === null }"
          :style="percent === null ? undefined : { width: `${percent}%` }"
        ></div>
      </div>
    </div>
  </div>
</template>
