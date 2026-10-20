<script setup lang="ts">
import { computed } from 'vue';
import type { JobKind, JobPhase } from '@/types';
import type { ProgressJob } from '@/types/progress';

const props = withDefaults(defineProps<{ job: ProgressJob; cancelLabel?: string }>(), { cancelLabel: 'Cancel' });
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
  // The archive import (M4); counted in attachment rows, not bytes (the
  // crate reports rows; see docs/architecture.md, slice 4j).
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
  // The import (M4 §8.12).
  scanningRecentlyDeleted: 'Checking Recently Deleted',
  scanningKeys: 'Checking message identifiers',
  estimatingAttachments: 'Estimating the space needed',
  awaitingConfirmation: 'Waiting for your confirmation',
  preparingArchive: 'Preparing the archive',
  importingHandles: 'Importing addresses',
  importingChats: 'Importing chats',
  importingMessages: 'Importing messages',
  importingChatLinks: 'Importing chat links',
  importingAttachmentLinks: 'Linking attachments',
  importingRecentlyDeleted: 'Marking Recently Deleted messages',
  finalizing: 'Finishing the import',
  optimizingIndex: 'Optimising the search index',
  checkpointing: 'Saving the archive',
  // The verify (M4 §11).
  verifyingDatabase: 'Checking the archive database',
  verifyingIndex: 'Checking the search index',
  verifyingFiles: 'Checking attachment files',
  // The export (M6 §9.4).
  preparingExport: 'Counting what to export',
  writingMessages: 'Writing messages',
  renderingPreviews: 'Making image previews',
  writingSpreadsheet: 'Writing the spreadsheet',
  layingOutPdf: 'Laying out the PDF',
  finishingExport: 'Finishing the export',
  removingPartial: 'Removing the unfinished export',
};

/**
 * Phases whose `done` and `total` count items (rows or files). The staging
 * copy counts bytes, and so does `verifyingFiles` in a full verify. The
 * import's scans, the estimate, the gate and finalize report no total.
 */
const COUNTED: ReadonlySet<JobPhase> = new Set<JobPhase>([
  'checkingFiles',
  'readingSchema',
  'readingHandles',
  'readingChats',
  'readingChatLinks',
  'readingMessages',
  'checkingAttachments',
  'summarising',
  'importingHandles',
  'importingChats',
  'importingMessages',
  'importingChatLinks',
  'copyingAttachments',
  'importingAttachmentLinks',
  'importingRecentlyDeleted',
  'verifyingFiles',
  'writingMessages',
  'writingSpreadsheet',
  'layingOutPdf',
]);

/** Whether this job's phase counts bytes. */
function countsBytes(j: ProgressJob, phase: JobPhase): boolean {
  return phase === 'stagingMessagesDb' || (phase === 'verifyingFiles' && j.depth === 'full');
}

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
  if (countsBytes(j, j.phase)) {
    return `${LABEL[j.phase]} (${megabytes(j.done)} of ${megabytes(j.total)})`;
  }
  if (COUNTED.has(j.phase)) {
    return `${LABEL[j.phase]} (${nf.format(j.done)} of ${nf.format(j.total)})`;
  }
  return phaseLabel.value;
});

const ARIA: Record<JobKind, string> = {
  validate: 'Validation progress',
  extractionCheck: 'Extraction check progress',
  import: 'Import progress',
  verify: 'Verification progress',
  export: 'Export progress',
};
const ariaLabel = computed(() => ARIA[props.job.kind]);

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
          {{ cancelLabel }}
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
