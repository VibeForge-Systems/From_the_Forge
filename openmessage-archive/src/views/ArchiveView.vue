<script setup lang="ts">
import { computed } from 'vue';
import { RouterLink } from 'vue-router';
import EmptyState from '@/components/EmptyState.vue';
import LocationStateBadge from '@/components/LocationStateBadge.vue';
import LocationStatusList from '@/components/LocationStatusList.vue';
import PanelLayout from '@/components/PanelLayout.vue';
import PlannedBadge from '@/components/PlannedBadge.vue';
import { useLayout } from '@/stores/layout';
import { useSettings } from '@/stores/settings';
import { SHORTCUTS, formatShortcut } from '@/services/shortcuts';

const settings = useSettings();
const layout = useLayout();

const keysFor = (id: string) => {
  const s = SHORTCUTS.find((x) => x.id === id);
  return s ? formatShortcut(s).join('+') : '';
};

const warnings = computed(() => settings.location.checks.filter((c) => c.level === 'warning'));
</script>

<template>
  <PanelLayout left-label="Conversations" right-label="Details">
    <template #left>
      <div class="oma-panel-header">
        <h2 class="oma-panel-title">Conversations</h2>
        <span class="badge text-bg-light border" title="Conversations in this archive">0</span>
      </div>
      <div class="px-3 pb-2">
        <label for="conversation-search" class="form-label small mb-1 d-flex justify-content-between">
          <span>Search conversations</span>
          <PlannedBadge capability="search" />
        </label>
        <input
          id="conversation-search"
          type="search"
          class="form-control form-control-sm"
          placeholder="Available after the first import"
          disabled
        />
      </div>
      <div class="oma-panel-body">
        <EmptyState icon="chat-square-text" title="No conversations" compact :heading-level="3">
          Conversations appear here once a backup has been imported.
        </EmptyState>
      </div>
    </template>

    <div class="oma-thread-toolbar">
      <button
        type="button"
        class="btn btn-sm btn-outline-secondary border-0"
        :aria-pressed="layout.leftVisible"
        aria-controls="oma-left-panel"
        :title="`Show or hide conversations (${keysFor('toggle-left-panel')})`"
        @click="layout.toggleLeft()"
      >
        <i class="bi bi-layout-sidebar" aria-hidden="true"></i>
        <span class="visually-hidden">Toggle conversation list</span>
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
            <strong>Import an iPhone backup.</strong> <PlannedBadge capability="backup-import" />
          </li>
          <li>
            <strong>Browse, search, and export.</strong> <PlannedBadge capability="viewer" />
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

      <EmptyState v-else icon="inboxes" title="Your archive library is ready">
        <p>
          Archives will be stored in
          <span class="font-monospace text-break">{{ settings.location.path }}</span>.
        </p>
        <div v-if="warnings.length" class="alert alert-warning text-start small">
          <LocationStatusList :checks="warnings" />
        </div>
        <p class="text-body-secondary small">
          Importing reads a local iPhone backup and copies Messages data into the archive. The
          backup itself is never modified.
        </p>
        <template #actions>
          <button type="button" class="btn btn-outline-secondary" disabled aria-describedby="import-planned">
            <i class="bi bi-box-arrow-in-down me-1" aria-hidden="true"></i>Import backup…
          </button>
          <span id="import-planned"><PlannedBadge capability="backup-import" /></span>
        </template>
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
          </ul>
        </section>
      </div>
    </template>
  </PanelLayout>
</template>
