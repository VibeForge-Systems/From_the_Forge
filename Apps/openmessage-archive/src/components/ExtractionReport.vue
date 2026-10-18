<script setup lang="ts">
// The extraction check's report (M3 §17.4). Text only, never v-html: the
// report holds counts and closed kinds, and every sentence comes from
// services/extractionText.ts. A null counter is "not recorded by this
// database", never 0.
import { computed } from 'vue';
import type { ExtractionReportDto, FeaturesDto } from '@/types';
import { LEFTOVER_FILES_MESSAGE } from '@/stores/backups';
import { formatDate, relativeTime } from '@/services/backupText';
import {
  FAILURE_LABEL,
  INTEGRITY_LABEL,
  NEWEST_MAY_BE_MISSING,
  OPEN_MODE_LABEL,
  SOURCE_CHANGED_CAVEAT,
  WAL_LABEL,
  count,
  newestMayBeMissing,
  outcomeSentence,
  qualifierSentence,
  unsupportedSentence,
} from '@/services/extractionText';

const props = defineProps<{ report: ExtractionReportDto }>();

const r = computed(() => props.report);
const ran = computed(() => r.value.outcome !== 'skippedNotImportable');
const alertClass = computed(() => {
  switch (r.value.outcome) {
    case 'readable':
      return 'alert-success';
    case 'readableWithQualifiers':
    case 'incomplete':
      return 'alert-warning';
    default:
      return 'alert-danger';
  }
});
const sat = computed(() => r.value.saturated);
// A readable result carries the sourceChanged qualifier instead.
const sourceChangedCaveat = computed(
  () => r.value.sourceChanged && ran.value && r.value.outcome !== 'readable' && r.value.outcome !== 'readableWithQualifiers',
);

interface Row {
  label: string;
  value: string;
  indent?: boolean;
}

const counts = computed<Row[]>(() => {
  const c = r.value.counts;
  if (!c) return [];
  const s = sat.value;
  return [
    { label: 'Message rows', value: count(c.messageRows, s) },
    { label: 'Reactions, stickers and votes (associations)', value: count(c.associationRows, s), indent: true },
    // Every row with item_type ≠ 0 and no association: group events, but
    // also shared locations, SharePlay and kinds this version does not name.
    { label: 'Non-message rows (item_type ≠ 0)', value: count(c.groupActionRows, s), indent: true },
    { label: 'In Recently Deleted', value: count(c.recentlyDeletedRows, s), indent: true },
    { label: 'Other rows', value: count(c.otherRows, s), indent: true },
    { label: 'Chat rows', value: count(c.chatRows, s) },
    { label: 'Addresses (handle rows: address and service pairs)', value: count(c.handleRows, s) },
    { label: 'Attachment rows', value: count(c.attachmentRows, s) },
    { label: 'Messages in no chat', value: count(c.messagesInNoChat, s) },
  ];
});

const dates = computed<Row[]>(() => {
  const d = r.value.dates;
  if (!d) return [];
  const s = sat.value;
  return [
    { label: 'Earliest message', value: d.earliestMessageAt ? formatDate(d.earliestMessageAt) : 'none' },
    { label: 'Latest message', value: d.latestMessageAt ? formatDate(d.latestMessageAt) : 'none' },
    { label: 'Left out: scheduled', value: count(d.excludedScheduled, s) },
    { label: 'Left out: before 2007', value: count(d.excludedBeforeFloor, s) },
    { label: 'Left out: after the check', value: count(d.excludedAfterCheck, s) },
    { label: 'Left out: unusable dates', value: count(d.excludedUnusable, s) },
  ];
});

const attachments = computed<Row[]>(() => {
  const a = r.value.attachments;
  if (!a) return [];
  const s = sat.value;
  // The indented rows are every probe outcome, so they add up to "Found".
  return [
    { label: 'Found in the file index', value: count(a.found, s) },
    { label: 'File present', value: count(a.filePresent, s), indent: true },
    { label: 'File missing', value: count(a.fileMissing, s), indent: true },
    { label: "File size differs from the backup's record", value: count(a.fileSizeMismatch, s), indent: true },
    { label: 'File could not be examined', value: count(a.fileNotReadable, s), indent: true },
    { label: 'The backup has no key for this file', value: count(a.noKey, s), indent: true },
    { label: "This file's key could not be unwrapped", value: count(a.keyDoesNotUnwrap, s), indent: true },
    { label: 'The key class this file needs is unavailable', value: count(a.classKeyUnavailable, s), indent: true },
    { label: 'Other file outcomes', value: count(a.probeOther, s), indent: true },
    { label: 'Not examined', value: count(a.notProbed, s), indent: true },
    { label: 'Listed more than once in the file index', value: count(a.ambiguous, s) },
    { label: 'Invalid file-index entry', value: count(a.invalidEntry, s) },
    { label: 'Not in this backup', value: count(a.notListedAtSourcedKey, s) },
    { label: 'Not found at an assumed location', value: count(a.notFoundAtAssumedKey, s) },
    { label: 'Not looked up', value: count(a.notLookedUp, s) },
  ];
});

const FEATURE_LABEL: Record<keyof FeaturesDto, string> = {
  attributedBody: 'Formatted message bodies',
  tapbacks: 'Reactions',
  emojiTapbacks: 'Emoji reactions',
  replies: 'Replies',
  editsAndUnsends: 'Edits and unsends',
  recentlyDeleted: 'Recently Deleted',
  scheduled: 'Scheduled messages',
  filtering: 'Message filtering',
  attachments: 'Attachments',
  genmoji: 'Genmoji',
};
const features = computed(() => {
  const f = r.value.schema?.features;
  if (!f) return [];
  return (Object.keys(FEATURE_LABEL) as (keyof FeaturesDto)[])
    .filter((k) => f[k])
    .map((k) => FEATURE_LABEL[k]);
});
const missingColumns = computed(() =>
  (r.value.schema?.tables ?? []).flatMap((t) => t.missingColumns),
);

const database = computed<Row[]>(() => {
  const d = r.value.database;
  const s = r.value.schema;
  if (!d) return [];
  const rows: Row[] = [
    { label: 'Opened', value: d.openMode ? OPEN_MODE_LABEL[d.openMode] : 'Not opened' },
    { label: 'Pending changes', value: WAL_LABEL[d.walOutcome] },
    { label: 'Integrity', value: d.integrity ? INTEGRITY_LABEL[d.integrity] : 'Not run' },
  ];
  if (d.sizeMatches === false) {
    rows.push({ label: 'Size of the copy', value: 'Differs from the size the backup records' });
  }
  if (s) {
    rows.push(
      { label: 'Expected columns not present', value: count(s.missingColumns) },
      { label: 'Columns this version does not know', value: count(s.unknownColumns) },
      { label: 'Tables this version does not know', value: count(s.unknownTables) },
    );
  }
  return rows;
});

const problems = computed(() =>
  (r.value.problems ?? []).map((p) => ({ kind: p.kind, label: FAILURE_LABEL[p.kind], value: count(p.count, sat.value) })),
);
</script>

<template>
  <section class="card mb-3" aria-labelledby="extraction-title">
    <div class="card-header d-flex align-items-center gap-2">
      <h2 id="extraction-title" class="h6 mb-0 me-auto">Extraction check</h2>
      <span v-if="report.checkedAt" class="small text-body-secondary">Checked {{ relativeTime(report.checkedAt) }}</span>
    </div>
    <div class="card-body">
      <!-- 1. Outcome -->
      <div class="alert py-2 oma-extraction-outcome" :class="alertClass" role="status">
        <p class="mb-0">{{ outcomeSentence(report) }}</p>
        <ul v-if="report.outcome === 'readableWithQualifiers'" class="mb-0 mt-1 oma-qualifiers">
          <li v-for="(q, i) in report.qualifiers" :key="i">{{ qualifierSentence(q) }}</li>
        </ul>
        <ul v-if="report.outcome === 'unsupported'" class="mb-0 mt-1 oma-unsupported">
          <li v-for="(u, i) in report.unsupportedReasons" :key="i">{{ unsupportedSentence(u) }}</li>
        </ul>
      </div>
      <p v-if="sourceChangedCaveat" class="alert alert-warning py-2 small oma-source-changed">{{ SOURCE_CHANGED_CAVEAT }}</p>
      <ul v-if="ran" class="list-unstyled small mb-3 oma-copy-status">
        <li>
          {{
            report.plaintextRemovedAfterRowPass
              ? 'The temporary copy of the Messages database was deleted straight after reading.'
              : 'The temporary copy of the Messages database could not be deleted straight after reading.'
          }}
        </li>
        <li>
          {{
            report.stagingRemoved
              ? 'All temporary files were removed at the end of the check.'
              : LEFTOVER_FILES_MESSAGE
          }}
        </li>
      </ul>

      <div class="row g-3">
        <!-- 2. Counts -->
        <div v-if="counts.length" class="col-md-6 oma-extraction-counts">
          <h3 class="oma-detail-heading">Counts</h3>
          <table class="table table-sm small mb-0">
            <tbody>
              <tr v-for="row in counts" :key="row.label">
                <th scope="row" class="fw-normal" :class="{ 'ps-3': row.indent }">{{ row.label }}</th>
                <td class="text-end">{{ row.value }}</td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- 3. Dates -->
        <div v-if="dates.length" class="col-md-6 oma-extraction-dates">
          <h3 class="oma-detail-heading">Dates</h3>
          <table class="table table-sm small mb-0">
            <tbody>
              <tr v-for="row in dates" :key="row.label">
                <th scope="row" class="fw-normal">{{ row.label }}</th>
                <td class="text-end">{{ row.value }}</td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- 4. Attachments -->
        <div v-if="attachments.length" class="col-md-6 oma-extraction-attachments">
          <h3 class="oma-detail-heading">Attachments</h3>
          <table class="table table-sm small mb-0">
            <tbody>
              <tr v-for="row in attachments" :key="row.label">
                <th scope="row" class="fw-normal" :class="{ 'ps-3': row.indent }">{{ row.label }}</th>
                <td class="text-end">{{ row.value }}</td>
              </tr>
            </tbody>
          </table>
          <p class="small text-body-secondary mt-2 mb-0">
            Attachments were looked up, not copied, decrypted or opened.
          </p>
        </div>

        <!-- 5. Database and schema -->
        <div v-if="database.length" class="col-md-6 oma-extraction-database">
          <h3 class="oma-detail-heading">Database and layout</h3>
          <table class="table table-sm small mb-0">
            <tbody>
              <tr v-for="row in database" :key="row.label">
                <th scope="row" class="fw-normal">{{ row.label }}</th>
                <td class="text-end">{{ row.value }}</td>
              </tr>
            </tbody>
          </table>
          <p v-if="newestMayBeMissing(report)" class="small text-warning-emphasis mt-2 mb-0">
            {{ NEWEST_MAY_BE_MISSING }}
          </p>
          <p v-if="features.length" class="small mt-2 mb-0">
            Detected features (derived from the columns present, not from an iOS version):
            {{ features.join(', ') }}.
          </p>
          <details v-if="missingColumns.length" class="small mt-2">
            <summary>{{ missingColumns.length }} expected {{ missingColumns.length === 1 ? 'column' : 'columns' }} not present</summary>
            <ul class="mb-0 oma-missing-columns">
              <li v-for="c in missingColumns" :key="c"><code>{{ c }}</code></li>
            </ul>
          </details>
        </div>

        <!-- 6. Problems -->
        <div v-if="report.problems" class="col-12 oma-extraction-problems">
          <h3 class="oma-detail-heading">Problems</h3>
          <p v-if="problems.length === 0" class="small mb-0">No problems were counted.</p>
          <table v-else class="table table-sm small mb-0">
            <tbody>
              <tr v-for="p in problems" :key="p.kind">
                <th scope="row" class="fw-normal">{{ p.label }}</th>
                <td class="text-end">{{ p.value }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <p v-if="ran && !report.counts" class="small text-body-secondary mt-3 mb-0">
        Messages, chats and attachments were not read.
      </p>
      <p class="small text-body-secondary mt-3 mb-0 oma-extraction-footer">
        Nothing from this check was saved. Importing into an archive arrives in milestone 4.
      </p>
    </div>
  </section>
</template>
