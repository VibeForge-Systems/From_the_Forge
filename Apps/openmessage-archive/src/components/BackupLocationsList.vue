<script setup lang="ts">
import { useBackups } from '@/stores/backups';
import type { BackupLocationState } from '@/types';
import { locationKindLabel } from '@/services/backupText';

const backups = useBackups();

const STATE: Record<BackupLocationState, string> = {
  present: 'Found',
  absent: 'Not found',
  unreadable: 'Could not be read',
  tooManyEntries: 'Partly read',
};
</script>

<template>
  <div class="oma-panel-header">
    <h2 class="oma-panel-title">Locations</h2>
  </div>
  <div class="oma-panel-body px-3">
    <p v-if="!backups.list" class="small text-body-secondary">Not scanned yet.</p>
    <p v-else-if="backups.list.locations.length === 0" class="small text-body-secondary">
      No locations are scanned. Linux has no standard backup location; add the folder that holds
      your backups.
    </p>
    <ul v-else class="list-unstyled small mb-0">
      <li v-for="loc in backups.list.locations" :key="loc.id" class="oma-detail-section">
        <div class="fw-semibold">{{ locationKindLabel(loc) }}</div>
        <code class="text-break d-block">{{ loc.path }}</code>
        <div class="text-body-secondary">
          {{ STATE[loc.state] }} · {{ loc.backupCount }} backup{{ loc.backupCount === 1 ? '' : 's' }}
        </div>
        <div v-if="loc.message" class="text-body-secondary">{{ loc.message }}</div>
        <button
          v-if="loc.kind === 'addedParent' || loc.kind === 'addedBackup'"
          type="button"
          class="btn btn-link btn-sm p-0"
          title="Removes it from this list. Nothing is deleted."
          @click="backups.forget(loc.id)"
        >
          Forget this folder
        </button>
      </li>
    </ul>
    <p v-if="backups.list?.budgetExhausted" class="small text-body-secondary mt-2">
      Some backups were not inspected yet because there are many. Select one to inspect it.
    </p>
  </div>
</template>
