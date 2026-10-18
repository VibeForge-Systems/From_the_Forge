<script setup lang="ts">
import { computed } from 'vue';
import type { BackupDetails, SourcedText } from '@/types';
import { formatDate, sourceHint } from '@/services/backupText';

const props = defineProps<{ details: BackupDetails }>();

interface Row {
  label: string;
  value: string | null;
  hint: string | null;
}

const sourced = (label: string, s: SourcedText | null): Row => ({
  label,
  value: s?.value ?? null,
  hint: sourceHint(s),
});

const yesNo = (v: boolean | null): string | null => (v === null ? null : v ? 'Yes' : 'No');

const rows = computed<Row[]>(() => {
  const d = props.details;
  const model = d.device.productName ?? d.device.productType;
  const formatLabel = { v3: 'Format 3 (Manifest.db)', legacyMbdb: 'Format 2.4 (Manifest.mbdb)', unknown: null }[
    d.layout.format
  ];
  return [
    sourced('Device name', d.device.deviceName),
    sourced('Model', model),
    sourced('iOS version', d.device.iosVersion),
    sourced('Build', d.device.buildVersion),
    {
      label: 'Last backup',
      value: d.summary.lastBackupDate ? formatDate(d.summary.lastBackupDate) : null,
      hint: d.summary.lastBackupDate ? 'from Info.plist' : null,
    },
    {
      label: 'Encrypted',
      value: yesNo(d.encryption.isEncrypted),
      hint: d.encryption.isEncrypted === null ? null : 'from Manifest.plist',
    },
    {
      label: 'Full backup',
      value: yesNo(d.status.isFullBackup),
      hint: d.status.isFullBackup === null ? null : 'from Status.plist',
    },
    {
      label: 'Backup status',
      value: d.status.snapshotState,
      hint: d.status.snapshotState === null ? null : 'from Status.plist',
    },
    {
      label: 'Format',
      value: formatLabel
        ? d.status.formatVersion
          ? `${formatLabel}, version ${d.status.formatVersion}`
          : formatLabel
        : null,
      hint: d.status.formatVersion ? 'version from Status.plist' : null,
    },
  ];
});
</script>

<template>
  <section class="card mb-3" aria-labelledby="backup-details-title">
    <div class="card-header">
      <h2 id="backup-details-title" class="h6 mb-0">Backup details</h2>
    </div>
    <div class="card-body">
      <dl class="row mb-0 oma-facts">
        <template v-for="row in rows" :key="row.label">
          <dt class="col-sm-4">{{ row.label }}</dt>
          <dd class="col-sm-8">
            <template v-if="row.value !== null">
              <span>{{ row.value }}</span>
              <span v-if="row.hint" class="small text-body-secondary ms-2">{{ row.hint }}</span>
            </template>
            <span v-else class="text-body-secondary">Not recorded</span>
          </dd>
        </template>
        <dt class="col-sm-4">Folder</dt>
        <dd class="col-sm-8">
          <code class="text-break">{{ details.path }}</code>
          <div class="small text-body-secondary">The folder name is usually your device's identifier.</div>
        </dd>
      </dl>
    </div>
  </section>
</template>
