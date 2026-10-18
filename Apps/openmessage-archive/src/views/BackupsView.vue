<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted } from 'vue';
import { RouterLink } from 'vue-router';
import BackupDetailsCard from '@/components/BackupDetailsCard.vue';
import BackupList from '@/components/BackupList.vue';
import BackupLocationsList from '@/components/BackupLocationsList.vue';
import BackupPasswordDialog from '@/components/BackupPasswordDialog.vue';
import BackupStateBadge from '@/components/BackupStateBadge.vue';
import EmptyState from '@/components/EmptyState.vue';
import JobProgress from '@/components/JobProgress.vue';
import LocationStatusList from '@/components/LocationStatusList.vue';
import PanelLayout from '@/components/PanelLayout.vue';
import PlannedBadge from '@/components/PlannedBadge.vue';
import ValidationReport from '@/components/ValidationReport.vue';
import { LEFTOVER_FILES_MESSAGE, useBackups } from '@/stores/backups';
import { useLayout } from '@/stores/layout';
import { SHORTCUTS, formatShortcut } from '@/services/shortcuts';
import { isDefaultLocation } from '@/services/backupText';

const backups = useBackups();
const layout = useLayout();

const keysFor = (id: string) => {
  const s = SHORTCUTS.find((x) => x.id === id);
  return s ? formatShortcut(s).join('+') : '';
};

onMounted(() => {
  // Polling stops when the page unmounts; pick up any job still marked
  // running in the cached details.
  backups.resumePolling();
  void backups.refresh();
});
onBeforeUnmount(() => backups.stopPolling());

const summary = computed(() => backups.selectedSummary);
const details = computed(() => backups.selectedDetails);
const hasDefaultLocations = computed(() => backups.list?.locations.some(isDefaultLocation) ?? false);
const thisJob = computed(() =>
  backups.job && summary.value && backups.job.backupId === summary.value.id ? backups.job : null,
);
const otherJobRunning = computed(
  () => backups.jobRunning && !thisJob.value && !details.value?.activeJob,
);
const blocked = computed(() => {
  const s = summary.value?.state;
  return s === 'incomplete' || s === 'unsupported' || s === 'unreadable';
});
const encrypted = computed(() => summary.value?.state === 'needsPassword');
const archiveConfigured = computed(() => details.value?.archiveConfigured ?? false);
const canValidate = computed(
  () =>
    !!details.value &&
    !blocked.value &&
    archiveConfigured.value &&
    !backups.jobRunning &&
    summary.value?.state !== 'notInspected',
);
const validateLabel = computed(() => {
  const again = !!details.value?.lastReport;
  if (encrypted.value) return again ? 'Unlock and validate again…' : 'Unlock and validate…';
  return again ? 'Validate again' : 'Validate';
});
const describedBy = computed(() => {
  if (blocked.value) return 'backup-state-reason';
  if (!archiveConfigured.value) return 'backup-no-archive';
  if (otherJobRunning.value) return 'backup-other-job';
  return 'backup-validate-note';
});
const error = computed(() => (summary.value ? backups.lastError[summary.value.id] : undefined));
const leftovers = computed(() => details.value?.lastCleanup?.stagingRemoved === false);
// The prompt names the backup it unlocks, which need not be the selected one:
// a wrong-password event can arrive after the user selected another backup.
const promptSummary = computed(() => {
  const id = backups.passwordPrompt?.backupId;
  if (!id) return null;
  return backups.details[id]?.summary ?? backups.list?.backups.find((b) => b.id === id) ?? null;
});
const deviceName = computed(() => promptSummary.value?.deviceName?.value ?? null);
const promptOpen = computed(() => backups.passwordPrompt !== null);

function validate(): void {
  if (summary.value) void backups.check(summary.value.id);
}

function onPasswordSubmit(password: string): void {
  const prompt = backups.passwordPrompt;
  if (prompt) void backups.submitPassword(prompt.backupId, password);
}
</script>

<template>
  <PanelLayout left-label="Backups" right-label="Locations">
    <template #left>
      <BackupList />
    </template>

    <div class="oma-thread-toolbar">
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        :aria-pressed="layout.leftVisible"
        aria-controls="oma-left-panel"
        :title="`Show or hide the backup list (${keysFor('toggle-left-panel')})`"
        @click="layout.toggleLeft()"
      >
        <i class="bi bi-layout-sidebar" aria-hidden="true"></i>
        <span class="visually-hidden">Toggle backup list</span>
      </button>
      <span class="flex-grow-1"></span>
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        :aria-pressed="layout.rightVisible"
        aria-controls="oma-right-panel"
        :title="`Show or hide locations (${keysFor('toggle-right-panel')})`"
        @click="layout.toggleRight()"
      >
        <i class="bi bi-layout-sidebar-reverse" aria-hidden="true"></i>
        <span class="visually-hidden">Toggle locations panel</span>
      </button>
    </div>

    <div class="oma-thread-body">
      <div v-if="backups.loading && !backups.list" class="d-flex justify-content-center align-items-center gap-2 p-5" role="status">
        <div class="spinner-border text-secondary" aria-hidden="true"></div>
        <span>Looking for backups…</span>
      </div>

      <EmptyState
        v-else-if="backups.list && backups.list.backups.length === 0"
        icon="phone"
        title="No backups found"
      >
        <template v-if="hasDefaultLocations">
          <p>No iPhone backup was found in the standard locations:</p>
          <ul class="list-unstyled small text-start">
            <li v-for="loc in backups.list.locations" :key="loc.id">
              <code class="text-break">{{ loc.path }}</code>
            </li>
          </ul>
        </template>
        <p v-else>
          Linux has no standard backup location. Add the folder that holds your backup, for
          example the folder you gave to idevicebackup2.
        </p>
        <template #actions>
          <button type="button" class="btn btn-primary" @click="backups.addFolder()">
            <i class="bi bi-folder-plus me-1" aria-hidden="true"></i>Add backup folder…
          </button>
        </template>
      </EmptyState>

      <EmptyState v-else-if="!summary" icon="phone" title="Backups" compact>
        Select a backup to see its details.
      </EmptyState>

      <div v-else class="oma-page oma-backup-page">
        <div class="d-flex align-items-center gap-2 mb-3">
          <h1 class="h5 mb-0 me-auto text-truncate">
            {{ summary.deviceName?.value ?? 'Device name not recorded' }}
          </h1>
          <BackupStateBadge :state="summary.state" :last-report-importable="summary.lastReportImportable" />
        </div>

        <div v-if="backups.loadingDetails === summary.id && !details" class="d-flex align-items-center gap-2 mb-3" role="status">
          <span class="spinner-border spinner-border-sm" aria-hidden="true"></span>Inspecting…
        </div>

        <div v-if="blocked" id="backup-state-reason" class="alert alert-warning" role="status">
          {{ summary.stateReason }}
        </div>

        <BackupDetailsCard v-if="details" :details="details" />

        <section v-if="details && details.notes.length" class="card mb-3" aria-labelledby="backup-notes-title">
          <div class="card-header">
            <h2 id="backup-notes-title" class="h6 mb-0">Notes</h2>
          </div>
          <div class="card-body">
            <LocationStatusList :checks="details.notes" class="small" />
          </div>
        </section>

        <section v-if="details" class="card mb-3" aria-labelledby="backup-actions-title">
          <div class="card-header">
            <h2 id="backup-actions-title" class="h6 mb-0">Check this backup</h2>
          </div>
          <div class="card-body">
            <div v-if="!details.archiveConfigured" id="backup-no-archive" class="alert alert-warning small" role="status">
              Choose an archive folder first. Checking a backup copies its file index there
              temporarily.
              <RouterLink to="/settings">Open Settings</RouterLink>
            </div>
            <div v-else-if="details.archiveSyncProvider && !blocked" class="alert alert-warning small" role="status">
              Your archive folder appears to be synchronized by {{ details.archiveSyncProvider }}.
              While the check runs, a copy of the backup's file index (decrypted, if the backup is
              encrypted) is stored there, and {{ details.archiveSyncProvider }} may upload it
              before it is deleted. The file index lists the names of files in the backup,
              including attachment names.
            </div>
            <p v-if="otherJobRunning" id="backup-other-job" class="small text-body-secondary">
              Another backup is being checked.
            </p>

            <JobProgress v-if="thisJob" :job="thisJob" class="mb-3" @cancel="backups.cancel()" />
            <div v-else-if="details.activeJob" class="d-flex align-items-center gap-2 mb-3" role="status" aria-live="polite">
              <span class="spinner-border spinner-border-sm" aria-hidden="true"></span>
              A check is running for this backup.
            </div>

            <div class="d-flex flex-wrap gap-2 align-items-center">
              <button
                v-if="!thisJob"
                type="button"
                class="btn btn-primary"
                :disabled="!canValidate"
                :aria-describedby="describedBy"
                @click="validate()"
              >
                <i class="bi bi-shield-check me-1" aria-hidden="true"></i>{{ validateLabel }}
              </button>
              <button
                v-if="blocked"
                type="button"
                class="btn btn-outline-secondary"
                :disabled="backups.loading"
                @click="backups.refresh()"
              >
                Refresh
              </button>
              <button type="button" class="btn btn-outline-secondary" disabled aria-describedby="import-planned">
                <i class="bi bi-box-arrow-in-down me-1" aria-hidden="true"></i>Import messages
              </button>
              <span id="import-planned"><PlannedBadge capability="extraction" /></span>
            </div>
            <p id="backup-validate-note" class="small text-body-secondary mt-2 mb-0">
              Validation copies the backup's file index (Manifest.db) into your archive folder,
              checks it, and deletes the copy. For an encrypted backup, the copy is decrypted. The
              Messages database is checked where it is and is not copied. The backup itself is
              never changed.
            </p>
          </div>
        </section>

        <div v-if="error" class="alert alert-danger d-flex align-items-start gap-2" role="alert">
          <span class="me-auto">{{ error.message }}</span>
          <button v-if="canValidate" type="button" class="btn btn-sm btn-outline-danger" @click="validate()">
            Retry
          </button>
        </div>

        <div v-if="leftovers" class="alert alert-warning small" role="status">
          {{ LEFTOVER_FILES_MESSAGE }}
        </div>

        <ValidationReport v-if="details?.lastReport" :report="details.lastReport" />
      </div>
    </div>

    <template #right>
      <BackupLocationsList />
    </template>
  </PanelLayout>

  <BackupPasswordDialog
    :open="promptOpen"
    :device-name="deviceName"
    :error="backups.passwordPrompt?.error ?? null"
    @submit="onPasswordSubmit"
    @close="backups.closePrompt()"
  />
</template>
