<script setup lang="ts">
// The archive list (left panel of the Archive page). The label is the one
// free text, rendered with interpolation only (I7).
import ArchiveStatusBadge from '@/components/ArchiveStatusBadge.vue';
import { useArchives } from '@/stores/archives';
import { archiveName, formatMs } from '@/services/importText';
import type { ArchiveEntryDto } from '@/types';

const archives = useArchives();

function name(e: ArchiveEntryDto): string {
  const s = e.summary ?? archives.summaries[e.archiveId];
  return s ? archiveName(s) : 'Archive';
}

function subtitle(e: ArchiveEntryDto): string {
  const s = e.summary ?? archives.summaries[e.archiveId];
  if (!s) return '—';
  const parts = [`Created ${formatMs(s.createdAtMs)}`];
  if (s.stats) parts.push(`${new Intl.NumberFormat().format(s.stats.messages)} messages`);
  return parts.join(' · ');
}
</script>

<template>
  <div class="oma-panel-header">
    <h2 class="oma-panel-title">Archives</h2>
    <span v-if="archives.listing" class="badge text-bg-light border" title="Archives in your archive folder">
      {{ archives.entries.length }}
    </span>
  </div>
  <div class="px-3 pb-2 d-flex gap-2 flex-wrap">
    <button type="button" class="btn btn-sm btn-primary" :disabled="archives.listing?.libraryState !== 'ready'" @click="archives.openCreate()">
      <i class="bi bi-plus-lg me-1" aria-hidden="true"></i>Create archive…
    </button>
    <button
      type="button"
      class="btn btn-sm btn-outline-secondary"
      :disabled="archives.loading"
      @click="archives.refresh()"
    >
      <i class="bi bi-arrow-clockwise me-1" aria-hidden="true"></i>Refresh
    </button>
  </div>
  <div class="oma-panel-body">
    <div v-if="archives.loading && !archives.listing" class="d-flex align-items-center gap-2 px-3 small" role="status">
      <span class="spinner-border spinner-border-sm" aria-hidden="true"></span>Looking for archives…
    </div>
    <p v-else-if="archives.listing && archives.entries.length === 0" class="px-3 small text-body-secondary">
      No archives yet.
    </p>
    <ul v-else class="list-unstyled mb-0 oma-backup-list oma-archive-list">
      <li v-for="e in archives.entries" :key="e.archiveId">
        <button
          type="button"
          class="oma-backup-item"
          :class="{ active: archives.selectedId === e.archiveId }"
          :aria-current="archives.selectedId === e.archiveId ? 'true' : undefined"
          @click="archives.select(e.archiveId)"
        >
          <span class="d-flex align-items-center gap-2">
            <span class="fw-semibold text-truncate me-auto">{{ name(e) }}</span>
            <span
              v-if="e.status === 'importing' || e.status === 'verifying'"
              class="spinner-border spinner-border-sm text-primary"
              role="status"
              :aria-label="e.status === 'importing' ? 'Importing' : 'Verifying'"
            ></span>
            <ArchiveStatusBadge :status="e.status" />
          </span>
          <span class="small text-body-secondary text-truncate d-block">{{ subtitle(e) }}</span>
        </button>
      </li>
    </ul>
  </div>
</template>
