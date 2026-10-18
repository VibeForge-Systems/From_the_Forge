<script setup lang="ts">
import { computed } from 'vue';
import BackupStateBadge from '@/components/BackupStateBadge.vue';
import { useBackups } from '@/stores/backups';
import type { BackupLocation, BackupSummary } from '@/types';
import { formatDate, locationKindLabel } from '@/services/backupText';

const backups = useBackups();

const groups = computed(() => {
  const list = backups.list;
  if (!list) return [] as { location: BackupLocation; items: BackupSummary[] }[];
  return list.locations
    .map((location) => ({
      location,
      items: list.backups.filter((b) => b.locationId === location.id),
    }))
    .filter((g) => g.items.length > 0);
});

function subtitle(b: BackupSummary): string {
  const parts: string[] = [];
  const model = b.productName?.value ?? b.productType?.value;
  if (model) parts.push(model);
  if (b.iosVersion) parts.push(`iOS ${b.iosVersion.value}`);
  if (b.lastBackupDate) parts.push(formatDate(b.lastBackupDate));
  return parts.join(' · ');
}
</script>

<template>
  <div class="oma-panel-header">
    <h2 class="oma-panel-title">Backups</h2>
    <span v-if="backups.list" class="badge text-bg-light border" title="Backups found">
      {{ backups.list.backups.length }}
    </span>
  </div>
  <div class="px-3 pb-2 d-flex gap-2 flex-wrap">
    <button type="button" class="btn btn-sm btn-primary" @click="backups.addFolder()">
      <i class="bi bi-folder-plus me-1" aria-hidden="true"></i>Add backup folder…
    </button>
    <button
      type="button"
      class="btn btn-sm btn-outline-secondary"
      :disabled="backups.loading"
      @click="backups.refresh()"
    >
      <i class="bi bi-arrow-clockwise me-1" aria-hidden="true"></i>Refresh
    </button>
  </div>
  <div class="oma-panel-body">
    <div v-if="backups.loading && !backups.list" class="d-flex align-items-center gap-2 px-3 small" role="status">
      <span class="spinner-border spinner-border-sm" aria-hidden="true"></span>Looking for backups…
    </div>
    <p v-else-if="backups.list && backups.list.backups.length === 0" class="px-3 small text-body-secondary">
      No backups found yet.
    </p>
    <section v-for="g in groups" :key="g.location.id" class="mb-2" :aria-label="locationKindLabel(g.location)">
      <h3 class="oma-detail-heading px-3 mb-1 mt-2">{{ locationKindLabel(g.location) }}</h3>
      <ul class="list-unstyled mb-0 oma-backup-list">
        <li v-for="b in g.items" :key="b.id">
          <button
            type="button"
            class="oma-backup-item"
            :class="{ active: backups.selectedId === b.id }"
            :aria-current="backups.selectedId === b.id ? 'true' : undefined"
            @click="backups.select(b.id)"
          >
            <span class="d-flex align-items-center gap-2">
              <span class="fw-semibold text-truncate me-auto">
                {{ b.deviceName?.value ?? 'Device name not recorded' }}
              </span>
              <span
                v-if="b.activeJobId"
                class="spinner-border spinner-border-sm text-primary"
                role="status"
                aria-label="Being checked"
              ></span>
              <BackupStateBadge :state="b.state" :last-report-importable="b.lastReportImportable" />
            </span>
            <span class="small text-body-secondary text-truncate d-block">{{ subtitle(b) || '—' }}</span>
          </button>
        </li>
      </ul>
    </section>
  </div>
</template>
