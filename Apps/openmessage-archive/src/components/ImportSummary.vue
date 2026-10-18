<script setup lang="ts">
// The newest import's summary (M4 §16), after the ExtractionReport pattern:
// counts named after what they count, "not recorded" for null, each stop
// reason in its own sentence, and no content at all.
import { computed } from 'vue';
import type { ImportResult } from '@/stores/archives';
import type { EntityCountsDto, LastImportDto, LinkCountsDto } from '@/types';
import {
  NOT_RECORDED,
  PROVISIONAL,
  SESSION_STATE,
  STOP_REASON,
  bytes,
  formatMs,
  formatMsOrNotRecorded,
  num,
} from '@/services/importText';

const props = defineProps<{ last: LastImportDto; result?: ImportResult | null }>();

const l = computed(() => props.last);
/** The event's facts apply only to the session it reported. */
const event = computed(() =>
  props.result && props.result.session === l.value.session ? props.result : null,
);
const resumable = computed(() => ['stopped', 'cancelled', 'interrupted'].includes(l.value.state));
const alertClass = computed(() => {
  switch (l.value.state) {
    case 'completed':
      return 'alert-success';
    case 'failed':
      return 'alert-danger';
    case 'running':
      return 'alert-info';
    default:
      return 'alert-warning';
  }
});

interface Row {
  label: string;
  value: string;
  indent?: boolean;
}
interface Group {
  title: string;
  rows: Row[];
}

function entity(c: EntityCountsDto, sat: boolean): Row[] {
  const s = sat || c.saturated;
  return [
    { label: 'Read from the backup', value: num(c.seen, s) },
    { label: 'New in the archive', value: num(c.inserted, s), indent: true },
    { label: 'Already in the archive', value: num(c.alreadyPresent, s), indent: true },
    { label: 'Imported without merging (no usable or unique identifier)', value: num(c.unkeyed, s), indent: true },
    { label: 'Stored values updated from this backup', value: num(c.cellsReplaced, s) },
    { label: 'Missing, oversized or unreadable values that left the stored value as it was', value: num(c.notApplied, s) },
    { label: 'Earlier values kept as history', value: num(c.historyAdded, s) },
    { label: 'Values stored with a note about how they were recorded', value: num(c.valueIssueCells, s) },
  ];
}

function link(c: LinkCountsDto, sat: boolean): Row[] {
  const s = sat || c.saturated;
  return [
    { label: 'Read from the backup', value: num(c.seen, s) },
    { label: 'New in the archive', value: num(c.inserted, s), indent: true },
    { label: 'Already in the archive', value: num(c.alreadyPresent, s), indent: true },
    { label: 'Not linked: one side was not imported or not recorded', value: num(c.unresolved, s), indent: true },
    { label: 'Not linked: Recently Deleted was left out', value: num(c.skippedRecentlyDeleted, s), indent: true },
  ];
}

const groups = computed<Group[]>(() => {
  const c = l.value.counts;
  if (!c) return [];
  const s = c.saturated;
  const b = c.bodies;
  const bs = s || b.saturated;
  const f = c.attachmentFiles;
  const fs = s || f.saturated;
  const r = c.recentlyDeleted;
  const rs = s || r.saturated;
  return [
    { title: 'Messages', rows: entity(c.messages, s) },
    {
      title: 'Message text versions',
      rows: [
        { label: 'New versions', value: num(b.added, bs) },
        { label: 'Unchanged', value: num(b.unchanged, bs) },
        { label: 'Re-derived by a newer decoder', value: num(b.rederived, bs) },
        { label: 'Formatted body could not be decoded', value: num(b.decodeFailed, bs) },
        { label: 'Plain and formatted text differ', value: num(b.sourcesDiffer, bs) },
        { label: 'No text recorded', value: num(b.noneNoSource + b.noneTextEmpty + b.noneAttributedBodyUnusable, bs) },
      ],
    },
    { title: 'Chats', rows: entity(c.chats, s) },
    { title: 'Addresses', rows: entity(c.handles, s) },
    { title: 'Chat members', rows: link(c.chatHandles, s) },
    { title: 'Messages in chats', rows: link(c.chatMessages, s) },
    { title: 'Attachments', rows: entity(c.attachments, s) },
    {
      title: 'Attachment files',
      rows: [
        { label: 'Copied', value: num(f.copied, fs) },
        { label: 'Same file already stored (not copied again)', value: num(f.deduplicated, fs) },
        { label: 'Already in the archive', value: num(f.alreadyArchived, fs) },
        { label: 'Missing from the archive, copied again', value: num(f.replaced, fs) },
        { label: 'Bytes copied', value: bytes(f.bytesCopied, fs) },
        { label: 'Not copied: you chose not to copy attachments', value: num(f.notRequested, fs) },
        { label: 'Not in this backup', value: num(f.notInBackup, fs) },
        { label: 'Not looked up', value: num(f.notLookedUp, fs) },
        { label: 'File unavailable in the backup', value: num(f.blobUnavailable, fs) },
        { label: 'Too large to copy', value: num(f.overLimit + f.tooLargeForVolume, fs) },
        { label: "File size differs from the backup's record", value: num(f.sizeMismatch, fs) },
        { label: 'Could not be read or written', value: num(f.readFailed + f.writeFailed, fs) },
        { label: 'Conflicting files left as they were', value: num(f.storeConflicts, fs) },
      ],
    },
    { title: 'Attachments in messages', rows: link(c.messageAttachments, s) },
    {
      title: 'Recently Deleted',
      rows: [
        { label: 'Messages in Recently Deleted', value: num(r.messages, rs) },
        { label: 'Left out', value: num(r.excluded, rs), indent: true },
        { label: 'Imported', value: num(r.imported, rs), indent: true },
        { label: 'Already archived: not changed', value: num(r.notUpdated, rs), indent: true },
        { label: 'Attachments left out', value: num(r.attachmentsExcluded, rs) },
        { label: 'Marked as Recently Deleted', value: num(r.marked, rs) },
      ],
    },
  ];
});

const plaintextLine = computed(() => {
  const e = event.value;
  if (!e) return null;
  return `The temporary copy of the Messages database was deleted: ${e.stagingRemoved ? 'yes' : 'no'}.`;
});
</script>

<template>
  <section class="card mb-3 oma-import-summary" aria-labelledby="import-summary-title">
    <div class="card-header">
      <h2 id="import-summary-title" class="h6 mb-0">Last import (session {{ last.session }})</h2>
    </div>
    <div class="card-body">
      <div class="alert small" :class="alertClass" role="status">
        <p class="mb-1 fw-semibold">{{ SESSION_STATE[last.state] }}</p>
        <p v-if="last.state === 'stopped'" class="mb-1">{{ STOP_REASON[last.stopReason ?? 'other'] }}</p>
        <p v-if="resumable" class="mb-1">Use “Continue importing…” to pick up where it left off.</p>
        <p v-if="last.provisional" class="mb-1">{{ PROVISIONAL }}</p>
        <p v-if="plaintextLine" class="mb-0">{{ plaintextLine }}</p>
      </div>
      <dl class="row small mb-3">
        <dt class="col-sm-4">Started</dt>
        <dd class="col-sm-8">{{ formatMs(last.startedAtMs) }}</dd>
        <dt class="col-sm-4">Ended</dt>
        <dd class="col-sm-8">{{ formatMsOrNotRecorded(last.endedAtMs) }}</dd>
        <dt class="col-sm-4">Times continued</dt>
        <dd class="col-sm-8">{{ num(last.resumes) }}</dd>
        <dt class="col-sm-4">Import set</dt>
        <dd class="col-sm-8">{{ last.lineageId }}</dd>
      </dl>
      <p v-if="!last.counts" class="small mb-0">The counts of this import: {{ NOT_RECORDED }}.</p>
      <div v-for="g in groups" :key="g.title" class="mb-3">
        <h3 class="oma-detail-heading">{{ g.title }}</h3>
        <table class="table table-sm small mb-0">
          <tbody>
            <tr v-for="row in g.rows" :key="row.label">
              <th scope="row" class="fw-normal" :class="{ 'ps-4': row.indent }">{{ row.label }}</th>
              <td class="text-end font-monospace">{{ row.value }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</template>
