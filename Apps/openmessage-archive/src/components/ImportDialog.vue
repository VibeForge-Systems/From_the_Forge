<script setup lang="ts">
// Choose a backup, an archive and the two options (M4 §16). The import
// itself first stops at the gate (ImportPreflightDialog), so nothing is
// written to the archive from here.
import { computed, ref, watch } from 'vue';
import ModalDialog from '@/components/ModalDialog.vue';
import { DEFAULT_IMPORT_OPTIONS, useArchives } from '@/stores/archives';
import { useBackups } from '@/stores/backups';
import { formatDate } from '@/services/backupText';
import { RD_WARNING, UNENCRYPTED_NOTE, archiveName } from '@/services/importText';
import type { BackupState, BackupSummary } from '@/types';

const archives = useArchives();
const backups = useBackups();

const archiveId = ref<string | null>(null);
const backupId = ref<string | null>(null);
const copyAttachments = ref(DEFAULT_IMPORT_OPTIONS.copyAttachments);
const includeRecentlyDeleted = ref(DEFAULT_IMPORT_OPTIONS.includeRecentlyDeleted);

const open = computed(() => archives.importDialog !== null);
const continuing = computed(() => archives.importDialog?.continuing ?? false);

// A continue must use the unfinished import's options (D64), so "Continue
// importing…" starts from them; any other import starts from the defaults.
watch(
  () => archives.importDialog,
  (d) => {
    archiveId.value = d?.archiveId ?? null;
    backupId.value = d?.backupId ?? null;
    const resume = d?.continuing && d.archiveId ? archives.details[d.archiveId]?.resumeOptions : null;
    const start = resume ?? DEFAULT_IMPORT_OPTIONS;
    copyAttachments.value = start.copyAttachments;
    includeRecentlyDeleted.value = start.includeRecentlyDeleted;
  },
);

// Default to the only archive once the list arrives.
watch(
  () => archives.importable,
  (list) => {
    const only = list.length === 1 ? list[0] : undefined;
    if (open.value && archiveId.value === null && only) archiveId.value = only.archiveId;
  },
);

/** States that cannot be imported: the backup itself is not usable. */
const BLOCKED: ReadonlySet<BackupState> = new Set<BackupState>(['incomplete', 'unsupported', 'unreadable']);

const backupList = computed<BackupSummary[]>(() => backups.list?.backups ?? []);
const selectedBackup = computed(() => backupList.value.find((b) => b.id === backupId.value) ?? null);
const matching = computed(() => {
  const id = archives.importDialog?.archiveId;
  return new Set(id ? (archives.details[id]?.matchingBackupIds ?? []) : []);
});
const encrypted = computed(() => selectedBackup.value?.state === 'needsPassword');
const canStart = computed(
  () =>
    archiveId.value !== null &&
    archives.importable.some((e) => e.archiveId === archiveId.value) &&
    selectedBackup.value !== null &&
    !BLOCKED.has(selectedBackup.value.state) &&
    !archives.jobRunning &&
    !backups.jobRunning,
);
const submitLabel = computed(() => (encrypted.value ? 'Unlock and prepare import…' : 'Prepare import'));

function backupTitle(b: BackupSummary): string {
  return b.deviceName?.value ?? 'Device name not recorded';
}

function backupSubtitle(b: BackupSummary): string {
  const parts: string[] = [];
  const model = b.productName?.value ?? b.productType?.value;
  if (model) parts.push(model);
  if (b.iosVersion) parts.push(`iOS ${b.iosVersion.value}`);
  parts.push(b.lastBackupDate ? `backed up ${formatDate(b.lastBackupDate)}` : 'backup date not recorded');
  if (b.state === 'needsPassword') parts.push('encrypted');
  if (BLOCKED.has(b.state)) parts.push('cannot be imported');
  return parts.join(' · ');
}

function createFirst(): void {
  archives.openCreate(backupId.value);
}

function submit(): void {
  if (!canStart.value || archiveId.value === null || backupId.value === null) return;
  void archives.startImport(backupId.value, archiveId.value, {
    copyAttachments: copyAttachments.value,
    includeRecentlyDeleted: includeRecentlyDeleted.value,
  });
}
</script>

<template>
  <ModalDialog :open="open" :title="continuing ? 'Continue importing' : 'Import from a backup'" @close="archives.closeImport()">
    <form id="import-form" novalidate @submit.prevent="submit">
      <p v-if="continuing" class="small">
        To continue an unfinished import, choose the same backup again and keep its options, which
        are set as they were. If the backup has changed since, the app was updated or the options
        differ, the import starts over; the next step says which.
      </p>

      <fieldset class="mb-3">
        <legend class="h6">Archive</legend>
        <div v-if="archives.listing && archives.importable.length === 0" class="oma-import-no-archive">
          <p class="small mb-2">There is no archive to import into yet.</p>
          <button type="button" class="btn btn-sm btn-primary" @click="createFirst()">
            <i class="bi bi-plus-lg me-1" aria-hidden="true"></i>Create archive first…
          </button>
        </div>
        <select
          v-else
          id="import-archive"
          v-model="archiveId"
          class="form-select"
          aria-label="Archive to import into"
        >
          <option :value="null" disabled>Choose an archive</option>
          <option v-for="e in archives.importable" :key="e.archiveId" :value="e.archiveId">
            {{ archiveName(e.summary) }}
          </option>
        </select>
      </fieldset>

      <fieldset class="mb-3">
        <legend class="h6">Backup</legend>
        <p v-if="backups.loading && !backups.list" class="small" role="status">Looking for backups…</p>
        <p v-else-if="backupList.length === 0" class="small mb-0">
          No backups found. Find one on the Backups page first.
        </p>
        <div v-for="b in backupList" :key="b.id" class="form-check">
          <input
            :id="`import-backup-${b.id}`"
            v-model="backupId"
            class="form-check-input"
            type="radio"
            name="import-backup"
            :value="b.id"
            :disabled="BLOCKED.has(b.state)"
          />
          <label class="form-check-label" :for="`import-backup-${b.id}`">
            <span class="fw-semibold">{{ backupTitle(b) }}</span>
            <span v-if="matching.has(b.id)" class="badge text-bg-info ms-1">Looks like the unfinished import's backup</span>
            <span class="d-block small text-body-secondary">{{ backupSubtitle(b) }}</span>
          </label>
        </div>
      </fieldset>

      <fieldset class="mb-3">
        <legend class="h6">Options</legend>
        <div class="form-check">
          <input id="import-copy-attachments" v-model="copyAttachments" class="form-check-input" type="checkbox" />
          <label class="form-check-label" for="import-copy-attachments">Copy attachment files</label>
        </div>
        <div class="form-check">
          <input id="import-include-rd" v-model="includeRecentlyDeleted" class="form-check-input" type="checkbox" aria-describedby="import-rd-note" />
          <label class="form-check-label" for="import-include-rd">Include Recently Deleted messages</label>
        </div>
        <p id="import-rd-note" class="small mt-1 mb-0" :class="includeRecentlyDeleted ? 'alert alert-warning' : 'text-body-secondary'">
          {{ RD_WARNING }}
        </p>
      </fieldset>

      <p class="small text-body-secondary mb-0">
        {{ UNENCRYPTED_NOTE }} The backup itself is never changed. Before anything is written, the
        next step shows what this import will do and asks you to confirm.
      </p>
    </form>
    <template #footer>
      <button type="button" class="btn btn-outline-secondary" @click="archives.closeImport()">Cancel</button>
      <button type="submit" form="import-form" class="btn btn-primary" :disabled="!canStart">
        {{ submitLabel }}
      </button>
    </template>
  </ModalDialog>
</template>
