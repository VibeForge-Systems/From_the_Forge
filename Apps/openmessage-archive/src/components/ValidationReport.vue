<script setup lang="ts">
import { computed } from 'vue';
import LocationStatusList from '@/components/LocationStatusList.vue';
import type { ValidationReportDto } from '@/types';
import { relativeTime } from '@/services/backupText';

const props = defineProps<{ report: ValidationReportDto }>();
const nf = new Intl.NumberFormat();

const n = (v: number, saturated = false) => (saturated ? `more than ${nf.format(v)}` : nf.format(v));

const stats = computed(() => {
  const s = props.report.stats;
  return [
    { label: 'Entries in the file index', value: n(s.totalRows, s.saturated) },
    { label: 'Files', value: n(s.files) },
    { label: 'Folders', value: n(s.directories) },
    { label: 'Links', value: n(s.symlinks) },
    { label: 'Missing files', value: n(s.missingBlobs) },
    { label: 'Files skipped (not plain files)', value: n(s.unsafeBlobs) },
    { label: 'Files that could not be examined', value: n(s.unreadableBlobs) },
    { label: 'Recorded sizes that differ', value: n(s.sizeMismatches) },
  ];
});

const yesNoUnknown = (v: boolean | null) => (v === null ? 'Not checked' : v ? 'Yes' : 'No');
const sms = computed(() => {
  const m = props.report.smsDb;
  const rows = [
    { label: 'Listed in the file index', value: m.listed ? 'Yes' : m.ambiguous ? 'More than once' : 'No' },
    { label: 'Present in the backup', value: yesNoUnknown(m.blobPresent) },
    { label: 'Starts like an SQLite database', value: yesNoUnknown(m.headerOk) },
  ];
  if (m.paddingOk !== null) rows.push({ label: 'Encryption padding valid', value: yesNoUnknown(m.paddingOk) });
  return rows;
});
</script>

<template>
  <section class="card mb-3" aria-labelledby="validation-title">
    <div class="card-header d-flex align-items-center gap-2">
      <h2 id="validation-title" class="h6 mb-0 me-auto">Validation</h2>
      <span class="small text-body-secondary">Checked {{ relativeTime(report.validatedAt) }}</span>
    </div>
    <div class="card-body">
      <div
        class="alert py-2"
        :class="report.importable ? 'alert-success' : 'alert-danger'"
        role="status"
      >
        {{
          report.importable
            ? 'This backup passed the checks for message import.'
            : 'This backup cannot be imported. See the failed checks below.'
        }}
      </div>
      <LocationStatusList :checks="report.checks" class="small mb-3" />
      <div class="row g-3">
        <div class="col-md-6">
          <h3 class="oma-detail-heading">File index</h3>
          <table class="table table-sm small mb-0">
            <tbody>
              <tr v-for="row in stats" :key="row.label">
                <th scope="row" class="fw-normal">{{ row.label }}</th>
                <td class="text-end">{{ row.value }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="col-md-6">
          <h3 class="oma-detail-heading">Messages database (sms.db)</h3>
          <table class="table table-sm small mb-0">
            <tbody>
              <tr v-for="row in sms" :key="row.label">
                <th scope="row" class="fw-normal">{{ row.label }}</th>
                <td class="text-end">{{ row.value }}</td>
              </tr>
            </tbody>
          </table>
          <p class="small text-body-secondary mt-2 mb-0">
            Validation checks the Messages database where it is and does not copy it. The
            extraction check reads a temporary copy.
          </p>
        </div>
      </div>
    </div>
  </section>
</template>
