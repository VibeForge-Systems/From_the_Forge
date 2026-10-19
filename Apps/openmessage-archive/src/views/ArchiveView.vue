<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, watch } from 'vue';
import { RouterLink } from 'vue-router';
import ArchiveList from '@/components/ArchiveList.vue';
import ArchivePage from '@/components/ArchivePage.vue';
import EmptyState from '@/components/EmptyState.vue';
import LocationStateBadge from '@/components/LocationStateBadge.vue';
import LocationStatusList from '@/components/LocationStatusList.vue';
import PanelLayout from '@/components/PanelLayout.vue';
import PlannedBadge from '@/components/PlannedBadge.vue';
import { useArchives } from '@/stores/archives';
import { useLayout } from '@/stores/layout';
import { useSettings } from '@/stores/settings';
import { SHORTCUTS, formatShortcut } from '@/services/shortcuts';
import { UNENCRYPTED_NOTE } from '@/services/importText';

const settings = useSettings();
const layout = useLayout();
const archives = useArchives();

/** List the archives once the location is usable; select the first. */
async function load(): Promise<void> {
  if (!settings.locationUsable) return;
  await archives.refresh();
  const first = archives.entries[0];
  if (!archives.selectedId && first) await archives.select(first.archiveId);
}

onMounted(() => {
  archives.resumePolling();
  void load();
});
onBeforeUnmount(() => archives.stopPolling());
watch(
  () => [settings.locationUsable, settings.location.path] as const,
  () => void load(),
);

const keysFor = (id: string) => {
  const s = SHORTCUTS.find((x) => x.id === id);
  return s ? formatShortcut(s).join('+') : '';
};

const warnings = computed(() => settings.location.checks.filter((c) => c.level === 'warning'));
const libraryError = computed(() =>
  archives.listing?.libraryState === 'unusable' ? (archives.listing.libraryError?.message ?? null) : null,
);
</script>

<template>
  <PanelLayout left-label="Archives" right-label="Details">
    <template #left>
      <ArchiveList v-if="settings.locationUsable" />
      <template v-else>
        <div class="oma-panel-header">
          <h2 class="oma-panel-title">Archives</h2>
        </div>
        <p class="px-3 small text-body-secondary">Choose an archive folder to see its archives.</p>
      </template>
    </template>

    <div class="oma-thread-toolbar">
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        :aria-pressed="layout.leftVisible"
        aria-controls="oma-left-panel"
        :title="`Show or hide archives (${keysFor('toggle-left-panel')})`"
        @click="layout.toggleLeft()"
      >
        <i class="bi bi-layout-sidebar" aria-hidden="true"></i>
        <span class="visually-hidden">Toggle archive list</span>
      </button>
      <span class="flex-grow-1"></span>
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        :aria-pressed="layout.rightVisible"
        aria-controls="oma-right-panel"
        :title="`Show or hide details (${keysFor('toggle-right-panel')})`"
        @click="layout.toggleRight()"
      >
        <i class="bi bi-layout-sidebar-reverse" aria-hidden="true"></i>
        <span class="visually-hidden">Toggle details panel</span>
      </button>
    </div>

    <div class="oma-thread-body">
      <div v-if="!settings.loaded" class="d-flex justify-content-center p-5" role="status">
        <div class="spinner-border text-secondary" aria-hidden="true"></div>
        <span class="visually-hidden">Loading settings…</span>
      </div>

      <div v-else-if="settings.loadError" class="alert alert-danger m-4" role="alert">
        <h2 class="h6">Settings could not be loaded</h2>
        <p class="mb-0">{{ settings.loadError }}</p>
      </div>

      <EmptyState
        v-else-if="settings.location.state === 'unset'"
        icon="archive"
        title="Welcome to OpenMessage Archive"
      >
        <p>
          Keep your iPhone Messages history on your own computer — searchable, exportable, and
          never uploaded anywhere.
        </p>
        <ol class="oma-steps text-start">
          <li class="oma-step-current">
            <strong>Choose where archives are stored.</strong>
            Pick a folder on this computer. Archives are kept separate from the original backups.
          </li>
          <li>
            <strong>Find your iPhone backup.</strong> Check it, and check that its messages can be
            read, before importing.
            <RouterLink to="/backups">Go to Backups</RouterLink>
          </li>
          <li>
            <strong>Import messages.</strong> Create an archive and import a backup into it. Counts
            are shown; the backup itself is never modified.
          </li>
          <li>
            <strong>Browse, search, and export.</strong> Read and search your conversations in the
            viewer. Exports <PlannedBadge capability="exports" /> come later.
          </li>
        </ol>
        <template #actions>
          <button
            type="button"
            class="btn btn-primary"
            :disabled="settings.busy"
            @click="settings.chooseLocation()"
          >
            <i class="bi bi-folder2-open me-1" aria-hidden="true"></i>Choose archive folder…
          </button>
        </template>
      </EmptyState>

      <div v-else-if="settings.location.state === 'error'" class="m-4">
        <div class="alert alert-danger" role="alert">
          <h2 class="h6 d-flex align-items-center gap-2">
            <i class="bi bi-x-octagon-fill" aria-hidden="true"></i>The archive location is unavailable
          </h2>
          <p class="font-monospace small text-break mb-2">{{ settings.location.path }}</p>
          <LocationStatusList :checks="settings.location.checks" />
        </div>
        <div class="d-flex gap-2 flex-wrap">
          <button
            type="button"
            class="btn btn-outline-secondary"
            :disabled="settings.busy"
            @click="settings.refreshLocation()"
          >
            Check again
          </button>
          <button
            type="button"
            class="btn btn-primary"
            :disabled="settings.busy"
            @click="settings.chooseLocation()"
          >
            Choose another folder…
          </button>
        </div>
      </div>

      <div v-else-if="archives.loading && !archives.listing" class="d-flex justify-content-center align-items-center gap-2 p-5" role="status">
        <div class="spinner-border text-secondary" aria-hidden="true"></div>
        <span>Looking for archives…</span>
      </div>

      <div v-else-if="libraryError" class="alert alert-danger m-4" role="alert">
        <h2 class="h6">The archive folder cannot be used</h2>
        <p class="mb-0">{{ libraryError }}</p>
      </div>

      <EmptyState v-else-if="archives.listing && archives.entries.length === 0" icon="inboxes" title="Your archive library is ready">
        <p>
          Archives will be stored in
          <span class="font-monospace text-break">{{ settings.location.path }}</span>.
        </p>
        <div v-if="warnings.length" class="alert alert-warning text-start small">
          <LocationStatusList :checks="warnings" />
        </div>
        <p class="text-body-secondary small">
          Create an archive, then import a backup into it. Check the backup first on the Backups
          page if you like. The backup itself is never modified. {{ UNENCRYPTED_NOTE }}
        </p>
        <template #actions>
          <button type="button" class="btn btn-primary" @click="archives.openCreate()">
            <i class="bi bi-plus-lg me-1" aria-hidden="true"></i>Create archive…
          </button>
          <RouterLink to="/backups" class="btn btn-outline-secondary">
            <i class="bi bi-phone me-1" aria-hidden="true"></i>Find backups…
          </RouterLink>
        </template>
      </EmptyState>

      <ArchivePage v-else-if="archives.selectedId" :key="archives.selectedId" />

      <EmptyState v-else icon="archive" title="Archives" compact>
        Select an archive to see its details.
      </EmptyState>
    </div>

    <template #right>
      <div class="oma-panel-header">
        <h2 class="oma-panel-title">Details</h2>
      </div>
      <div class="oma-panel-body px-3">
        <section class="oma-detail-section" aria-labelledby="detail-location">
          <h3 id="detail-location" class="oma-detail-heading">Archive location</h3>
          <div class="d-flex align-items-center gap-2 mb-1">
            <LocationStateBadge :state="settings.location.state" />
          </div>
          <p v-if="settings.location.path" class="font-monospace small text-break mb-1">
            {{ settings.location.path }}
          </p>
          <RouterLink to="/settings" class="small">Manage in Settings</RouterLink>
        </section>
        <section class="oma-detail-section" aria-labelledby="detail-privacy">
          <h3 id="detail-privacy" class="oma-detail-heading">Privacy</h3>
          <ul class="list-unstyled small mb-0 oma-privacy-list">
            <li><i class="bi bi-wifi-off" aria-hidden="true"></i>Makes no network requests</li>
            <li><i class="bi bi-person-slash" aria-hidden="true"></i>No accounts or telemetry</li>
            <li><i class="bi bi-lock" aria-hidden="true"></i>Original backups are read-only</li>
            <li><i class="bi bi-key" aria-hidden="true"></i>Backup passwords are used once and never saved.</li>
            <li><i class="bi bi-unlock" aria-hidden="true"></i>Archives are stored unencrypted</li>
          </ul>
        </section>
      </div>
    </template>
  </PanelLayout>
</template>
