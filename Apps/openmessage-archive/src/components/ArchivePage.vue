<script setup lang="ts">
// One archive's details (centre of the Archive page, M4 §16): its status,
// the import and verify actions, progress, and the last import and verify.
// Counts and states only; the label is the one free text (I7).
import { computed, ref, watch } from 'vue';
import ArchiveStatusBadge from '@/components/ArchiveStatusBadge.vue';
import ImportSummary from '@/components/ImportSummary.vue';
import JobProgress from '@/components/JobProgress.vue';
import PlannedBadge from '@/components/PlannedBadge.vue';
import VerifySummary from '@/components/VerifySummary.vue';
import { useArchives } from '@/stores/archives';
import { useBackups } from '@/stores/backups';
import {
  GATE_ENDING,
  NOT_IMPORTABLE,
  NOT_RECORDED,
  SESSION_STATE,
  UNENCRYPTED_NOTE,
  archiveName,
  bytes,
  formatMs,
  formatMsOrNotRecorded,
  num,
  statusSentence,
} from '@/services/importText';
import type { ProgressJob } from '@/types/progress';

const archives = useArchives();
const backups = useBackups();

const id = computed(() => archives.selectedId as string);
const entry = computed(() => archives.selectedEntry);
const details = computed(() => archives.selectedDetails);
const summary = computed(() => archives.selectedSummary);
const error = computed(() => archives.lastError[id.value]);
const result = computed(() => archives.importResults[id.value] ?? null);
const verifyResult = computed(() => archives.verifyResults[id.value] ?? null);

const thisJob = computed(() => (archives.liveJob?.archiveId === id.value ? archives.liveJob : null));
/** A job on this archive this page cannot hear (after a reload, or elsewhere). */
const unheard = computed(() => (thisJob.value ? null : (details.value?.activeJob ?? null)));
const unheardCancelling = ref(false);
watch(
  () => unheard.value?.jobId,
  () => (unheardCancelling.value = false),
);
const unheardProgress = computed<ProgressJob | null>(() => {
  const a = unheard.value;
  if (!a) return null;
  return {
    kind: a.kind,
    phase: a.waitingAtGate ? 'awaitingConfirmation' : a.phase,
    done: 0,
    total: null,
    cancelling: unheardCancelling.value,
  };
});

const busy = computed(() => archives.jobRunning || backups.jobRunning);
const ready = computed(() => entry.value?.status === 'ready' || (!entry.value && !!details.value));
const newerRules = computed(() => summary.value?.newerRules ?? false);
const canImport = computed(() => ready.value && !newerRules.value && !busy.value);
const canVerify = computed(() => ready.value && !busy.value);
const resumable = computed(() => details.value?.resumable ?? false);
const describedBy = computed(() => {
  if (!ready.value) return 'archive-status-reason';
  if (newerRules.value) return 'archive-newer-rules';
  if (busy.value) return 'archive-busy';
  return undefined;
});

/** The event's ending, for the endings the summary does not show. */
const endingSentence = computed(() => {
  const r = result.value;
  if (!r) return null;
  if (r.outcome === 'cancelledAtGate') return GATE_ENDING[r.gate ?? 'declined'];
  if (r.outcome === 'notImportable') {
    return `${r.notice?.message ?? NOT_IMPORTABLE[r.notImportable ?? 'other']}`;
  }
  return null;
});

const stats = computed(() => {
  const s = summary.value?.stats;
  if (!s) return null;
  const sat = s.saturated;
  return [
    { label: 'Messages', value: num(s.messages, sat) },
    { label: 'Message text versions', value: num(s.bodyVersions, sat) },
    { label: 'Chats', value: num(s.chats, sat) },
    { label: 'Addresses', value: num(s.handles, sat) },
    { label: 'Attachments', value: num(s.attachments, sat) },
    { label: 'Attachment files stored', value: num(s.blobs, sat) },
    { label: 'Size of attachment files', value: bytes(s.blobBytes, sat) },
  ];
});

const sessions = computed(() => (details.value?.sessions ?? []).slice(0, 20));

function startImport(): void {
  archives.openImport({ archiveId: id.value });
}

function continueImport(): void {
  archives.openImport({ archiveId: id.value, continuing: true });
}

function cancelUnheard(): void {
  unheardCancelling.value = true;
  void archives.cancel(id.value);
}
</script>

<template>
  <div class="oma-page oma-archive-page">
    <div class="d-flex align-items-center gap-2 mb-3">
      <h1 class="h5 mb-0 me-auto text-truncate">{{ archiveName(summary) }}</h1>
      <ArchiveStatusBadge v-if="entry" :status="entry.status" />
    </div>

    <div v-if="archives.loadingDetails === id && !details" class="d-flex align-items-center gap-2 mb-3" role="status">
      <span class="spinner-border spinner-border-sm" aria-hidden="true"></span>Reading the archive…
    </div>

    <div v-if="entry && entry.status !== 'ready'" id="archive-status-reason" class="alert alert-warning small" role="status">
      {{ statusSentence(entry) }}
    </div>
    <div v-else-if="newerRules" id="archive-newer-rules" class="alert alert-info small" role="status">
      {{ entry ? statusSentence(entry) : '' }}
    </div>

    <div v-if="error" class="alert alert-danger small" role="alert">{{ error.message }}</div>

    <section class="card mb-3" aria-labelledby="archive-actions-title">
      <div class="card-header">
        <h2 id="archive-actions-title" class="h6 mb-0">Import and check</h2>
      </div>
      <div class="card-body">
        <JobProgress v-if="thisJob" :job="thisJob" class="mb-3" @cancel="archives.cancel(id)" />
        <JobProgress v-else-if="unheardProgress" :job="unheardProgress" class="mb-3" @cancel="cancelUnheard()" />
        <p v-if="busy && !thisJob && !unheard" id="archive-busy" class="small text-body-secondary">
          Another import or check is running. Wait for it to finish or cancel it.
        </p>

        <div v-if="!thisJob && !unheard" class="d-flex flex-wrap gap-2 align-items-center">
          <button type="button" class="btn btn-primary" :disabled="!canImport" :aria-describedby="describedBy" @click="startImport()">
            <i class="bi bi-box-arrow-in-down me-1" aria-hidden="true"></i>Import from a backup…
          </button>
          <button
            v-if="resumable"
            type="button"
            class="btn btn-outline-primary"
            :disabled="!canImport"
            :aria-describedby="describedBy"
            @click="continueImport()"
          >
            <i class="bi bi-arrow-repeat me-1" aria-hidden="true"></i>Continue importing…
          </button>
          <button type="button" class="btn btn-outline-secondary" :disabled="!canVerify" :aria-describedby="describedBy" @click="archives.verify(id, 'quick')">
            <i class="bi bi-shield-check me-1" aria-hidden="true"></i>Verify archive (quick)
          </button>
          <button type="button" class="btn btn-outline-secondary" :disabled="!canVerify" :aria-describedby="describedBy" @click="archives.verify(id, 'full')">
            Verify archive (full)
          </button>
        </div>
        <p class="small text-body-secondary mt-2 mb-0">
          An import reads a backup and adds what is new; it never removes anything from the
          archive, and the backup itself is never changed. A quick verify checks the database and
          that every attachment file is present with the right size; a full verify also reads every
          file back. Verifying reports problems and changes nothing.
        </p>
        <p class="small text-body-secondary mt-2 mb-0">{{ UNENCRYPTED_NOTE }}</p>
      </div>
    </section>

    <div v-if="endingSentence" class="alert alert-info small oma-import-ending" role="status">{{ endingSentence }}</div>
    <div v-if="verifyResult && !verifyResult.recorded" class="alert alert-warning small" role="status">
      The result of the last check could not be recorded in the archive.
    </div>

    <ImportSummary v-if="details?.lastImport" :last="details.lastImport" :result="result" />
    <VerifySummary v-if="details?.lastVerify" :summary="details.lastVerify" />

    <section v-if="summary" class="card mb-3" aria-labelledby="archive-contents-title">
      <div class="card-header">
        <h2 id="archive-contents-title" class="h6 mb-0">In this archive</h2>
      </div>
      <div class="card-body">
        <p v-if="!stats" class="small mb-0">Counts: {{ NOT_RECORDED }} until the first import finishes.</p>
        <table v-else class="table table-sm small mb-2">
          <tbody>
            <tr v-for="row in stats" :key="row.label">
              <th scope="row" class="fw-normal">{{ row.label }}</th>
              <td class="text-end font-monospace">{{ row.value }}</td>
            </tr>
          </tbody>
        </table>
        <p class="small text-body-secondary mb-0">
          Created {{ formatMs(summary.createdAtMs) }}. Schema version {{ summary.schemaVersion }}.
        </p>
      </div>
    </section>

    <section v-if="sessions.length" class="card mb-3" aria-labelledby="archive-sessions-title">
      <div class="card-header">
        <h2 id="archive-sessions-title" class="h6 mb-0">Imports</h2>
      </div>
      <div class="card-body p-0">
        <table class="table table-sm small mb-0 align-middle">
          <thead>
            <tr>
              <th scope="col" class="ps-3">Session</th>
              <th scope="col">State</th>
              <th scope="col">Started</th>
              <th scope="col">Ended</th>
              <th scope="col" class="text-end pe-3">Continued</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in sessions" :key="s.session">
              <td class="ps-3">{{ s.session }}</td>
              <td>{{ SESSION_STATE[s.state] }}</td>
              <td>{{ formatMs(s.startedAtMs) }}</td>
              <td>{{ formatMsOrNotRecorded(s.endedAtMs) }}</td>
              <td class="text-end pe-3">{{ num(s.resumes) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="card mb-3" aria-labelledby="archive-browse-title">
      <div class="card-header">
        <h2 id="archive-browse-title" class="h6 mb-0">Conversations</h2>
      </div>
      <div class="card-body">
        <p class="small">This version shows counts only. Reading and searching messages comes next.</p>
        <div class="d-flex flex-wrap gap-2 align-items-center mb-2">
          <button type="button" class="btn btn-outline-secondary" disabled aria-describedby="viewer-planned">
            <i class="bi bi-chat-square-text me-1" aria-hidden="true"></i>Browse conversations
          </button>
          <span id="viewer-planned"><PlannedBadge capability="viewer" /></span>
        </div>
        <label for="conversation-search" class="form-label small mb-1 d-flex justify-content-between">
          <span>Search messages</span>
          <PlannedBadge capability="search" />
        </label>
        <input id="conversation-search" type="search" class="form-control form-control-sm" disabled />
      </div>
    </section>
  </div>
</template>
