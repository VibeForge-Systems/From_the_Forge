import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { IpcError, ipc } from '@/services/ipc';
import type {
  BackupDetails,
  BackupList,
  BackupSummary,
  ErrorCode,
  JobEvent,
  JobPhase,
} from '@/types';
import { useNotifications } from './notifications';

/** Apple's own limit is unverified; longer passwords are refused, never truncated. */
export const PASSWORD_MAX_BYTES = 1024;
export const PASSWORD_TOO_LONG_MESSAGE = 'Passwords longer than 1,024 bytes are not accepted.';
export const LEFTOVER_FILES_MESSAGE =
  'Some temporary files could not be removed yet. They will be removed the next time OpenMessage Archive starts.';
/** How often the store polls a job it cannot hear (after a reload). */
export const POLL_INTERVAL_MS = 2000;

export interface JobState {
  /** Null until the first event or the command response names it. */
  jobId: string | null;
  backupId: string;
  phase: JobPhase | null;
  done: number;
  total: number | null;
  cancelling: boolean;
  ended: boolean;
}

export interface PasswordPrompt {
  backupId: string;
  error: string | null;
}

/** An error as the store keeps it: code and sentence only. */
export interface StoredError {
  code: ErrorCode;
  message: string;
  /**
   * `details` when loading the backup's details failed: a later successful
   * load clears it. Job errors stay until the next check starts.
   */
  source?: 'details';
}

/**
 * The Backups page. The password is never stored here: it passes through
 * `submitPassword` as an argument straight into the IPC call.
 */
export const useBackups = defineStore('backups', () => {
  const list = ref<BackupList | null>(null);
  const loading = ref(false);
  const selectedId = ref<string | null>(null);
  const details = ref<Record<string, BackupDetails>>({});
  const loadingDetails = ref<string | null>(null);
  const job = ref<JobState | null>(null);
  const passwordPrompt = ref<PasswordPrompt | null>(null);
  const lastError = ref<Record<string, StoredError>>({});
  const polling = new Map<string, ReturnType<typeof setTimeout>>();

  const selectedSummary = computed<BackupSummary | null>(() => {
    const id = selectedId.value;
    if (!id) return null;
    return details.value[id]?.summary ?? list.value?.backups.find((b) => b.id === id) ?? null;
  });
  const selectedDetails = computed<BackupDetails | null>(() =>
    selectedId.value ? (details.value[selectedId.value] ?? null) : null,
  );
  const jobRunning = computed(
    () =>
      (job.value !== null && !job.value.ended) ||
      (list.value?.backups.some((b) => b.activeJobId !== null) ?? false) ||
      Object.values(details.value).some((d) => d.activeJob !== null),
  );

  function toast(level: 'info' | 'success' | 'warning' | 'danger', message: string): void {
    useNotifications().push(level, message);
  }

  function asStored(error: unknown): StoredError {
    if (error instanceof IpcError) return { code: error.code, message: error.message };
    return { code: 'internal', message: 'Something went wrong. Please report this as a bug.' };
  }

  function replaceSummary(summary: BackupSummary): void {
    if (!list.value) return;
    list.value = {
      ...list.value,
      backups: list.value.backups.map((b) => (b.id === summary.id ? summary : b)),
    };
  }

  function dropBackup(id: string): void {
    if (id in details.value) {
      const next = { ...details.value };
      delete next[id];
      details.value = next;
    }
    clearError(id);
    const t = polling.get(id);
    if (t !== undefined) {
      clearTimeout(t);
      polling.delete(id);
    }
    if (selectedId.value === id) selectedId.value = null;
  }

  /**
   * Make the store agree with a new list: a backup no longer listed is no
   * longer selected, and its cached details (which may claim a running job),
   * error and polling go too. The backup of a job still being heard is kept.
   */
  function applyList(next: BackupList): void {
    list.value = next;
    const listed = new Set(next.backups.map((b) => b.id));
    const live = job.value && !job.value.ended ? job.value.backupId : null;
    const known = new Set([...Object.keys(details.value), ...Object.keys(lastError.value), ...polling.keys()]);
    if (selectedId.value) known.add(selectedId.value);
    for (const id of known) {
      if (!listed.has(id) && id !== live) dropBackup(id);
    }
  }

  async function refresh(): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    try {
      applyList(await ipc.listBackups());
    } catch (error) {
      const e = asStored(error);
      toast(e.code === 'busy' ? 'info' : 'danger', e.message);
    } finally {
      loading.value = false;
    }
  }

  async function addFolder(): Promise<void> {
    try {
      const result = await ipc.addBackupFolder();
      if (result === null) return;
      applyList(result.list);
      switch (result.outcome) {
        case 'added':
          toast('success', 'Backup folder added.');
          break;
        case 'addedAncestor':
          toast('info', 'You chose a folder inside a backup. The backup folder itself was added.');
          break;
        case 'alreadyListed':
          toast('info', 'That folder is already listed.');
          break;
        case 'refused':
          toast('danger', result.message);
          break;
      }
      if (!result.saved && (result.outcome === 'added' || result.outcome === 'addedAncestor')) {
        toast('warning', 'Added for this session only. Settings are read-only.');
      }
    } catch (error) {
      toast('danger', asStored(error).message);
    }
  }

  async function forget(locationId: string): Promise<void> {
    try {
      const result = await ipc.forgetBackupFolder(locationId);
      applyList(result.list);
      toast('info', 'Folder removed from the list. Nothing was deleted.');
      if (!result.saved) toast('warning', 'Removed for this session only. Settings are read-only.');
    } catch (error) {
      const e = asStored(error);
      toast(e.code === 'busy' ? 'warning' : 'danger', e.message);
    }
  }

  /** Clear a cached `activeJob` that may be stale (the job is over). */
  function clearActiveJob(id: string): void {
    const d = details.value[id];
    if (d?.activeJob) details.value = { ...details.value, [id]: { ...d, activeJob: null } };
  }

  async function loadDetails(id: string): Promise<BackupDetails | null> {
    try {
      const d = await ipc.getBackupDetails(id);
      details.value = { ...details.value, [id]: d };
      replaceSummary(d.summary);
      // A details error is over once the details load; a job's error is not.
      if (lastError.value[id]?.source === 'details') clearError(id);
      return d;
    } catch (error) {
      const e = asStored(error);
      if (e.code === 'backupNotFound') {
        dropBackup(id);
        toast('warning', e.message);
        await refresh();
      } else {
        // The cached activeJob can no longer be confirmed; do not let it keep
        // every Validate button disabled.
        clearActiveJob(id);
        lastError.value = { ...lastError.value, [id]: { ...e, source: 'details' } };
      }
      return null;
    }
  }

  /** Select a backup and load its details. This inspects a `notInspected` one. */
  async function select(id: string): Promise<void> {
    selectedId.value = id;
    loadingDetails.value = id;
    try {
      const d = await loadDetails(id);
      if (d?.activeJob && !job.value) startPolling(id);
    } finally {
      if (loadingDetails.value === id) loadingDetails.value = null;
    }
  }

  /**
   * A job this page cannot hear (the webview reloaded and its channel is
   * gone): poll until it ends, then show its report and cleanup outcome.
   */
  function startPolling(id: string): void {
    if (polling.has(id)) return;
    const tick = async (): Promise<void> => {
      const d = await loadDetails(id);
      if (d?.activeJob) {
        polling.set(id, setTimeout(() => void tick(), POLL_INTERVAL_MS));
        return;
      }
      polling.delete(id);
      if (d?.lastCleanup && !d.lastCleanup.stagingRemoved) toast('warning', LEFTOVER_FILES_MESSAGE);
    };
    polling.set(id, setTimeout(() => void tick(), POLL_INTERVAL_MS));
  }

  function stopPolling(): void {
    for (const t of polling.values()) clearTimeout(t);
    polling.clear();
  }

  /** Poll again for every cached backup still marked as running a job. */
  function resumePolling(): void {
    for (const [id, d] of Object.entries(details.value)) {
      if (d.activeJob && !(job.value && !job.value.ended && job.value.backupId === id)) {
        startPolling(id);
      }
    }
  }

  function clearError(id: string): void {
    if (!(id in lastError.value)) return;
    const next = { ...lastError.value };
    delete next[id];
    lastError.value = next;
  }

  /** Name the job; a Cancel clicked before it had a name is sent now. */
  function adopt(current: JobState, jobId: string): void {
    if (current.jobId !== null) return;
    current.jobId = jobId;
    if (current.cancelling) void sendCancel(jobId);
  }

  async function sendCancel(jobId: string): Promise<void> {
    try {
      await ipc.cancelJob(jobId);
    } catch (error) {
      toast('danger', asStored(error).message);
    }
  }

  function handleEvent(backupId: string, event: JobEvent): void {
    const current = job.value;
    if (!current || current.backupId !== backupId || current.ended) return;
    if (current.jobId !== null && current.jobId !== event.jobId) return;
    adopt(current, event.jobId);

    switch (event.kind) {
      case 'progress':
        current.phase = event.phase;
        current.done = event.done;
        current.total = event.total;
        return;
      case 'finished': {
        const d = details.value[backupId];
        if (d) {
          details.value = {
            ...details.value,
            [backupId]: {
              ...d,
              lastReport: event.report,
              lastCleanup: { stagingRemoved: event.stagingRemoved },
              activeJob: null,
            },
          };
        }
        break;
      }
      case 'failed':
        if (event.error.code === 'wrongPassword') {
          passwordPrompt.value = { backupId, error: event.error.message };
        } else {
          lastError.value = {
            ...lastError.value,
            [backupId]: { code: event.error.code, message: event.error.message },
          };
        }
        break;
      case 'cancelled':
        toast(
          event.stagingRemoved ? 'info' : 'warning',
          event.stagingRemoved
            ? 'Check cancelled. Temporary files were removed.'
            : 'Check cancelled. Some temporary files could not be removed yet; they will be removed the next time OpenMessage Archive starts.',
        );
        break;
    }
    current.ended = true;
    job.value = null;
    clearActiveJob(backupId);
    if (event.kind !== 'cancelled' && !event.stagingRemoved) toast('warning', LEFTOVER_FILES_MESSAGE);
    void loadDetails(backupId);
  }

  async function startJob(backupId: string, password: string | null): Promise<void> {
    clearError(backupId);
    job.value = {
      jobId: null,
      backupId,
      phase: null,
      done: 0,
      total: null,
      cancelling: false,
      ended: false,
    };
    try {
      const started = await ipc.openBackup(backupId, password, (e) => handleEvent(backupId, e));
      const current = job.value;
      // Events and the response are not ordered: the job may already have
      // ended, or an event may already have named it.
      if (!current || current.backupId !== backupId || current.ended) return;
      if (current.jobId === null) adopt(current, started.jobId);
      else if (current.jobId !== started.jobId) {
        console.debug('job id mismatch between response and events; ignored');
      }
    } catch (error) {
      if (job.value?.backupId === backupId && job.value.jobId === null) job.value = null;
      const e = asStored(error);
      switch (e.code) {
        case 'passwordRequired':
          passwordPrompt.value = { backupId, error: null };
          break;
        case 'passwordTooLong':
        case 'wrongPassword':
          passwordPrompt.value = { backupId, error: e.message };
          break;
        case 'busy':
          toast('warning', e.message);
          break;
        default:
          lastError.value = { ...lastError.value, [backupId]: e };
      }
    }
  }

  /** Validate: unencrypted backups start at once; encrypted ones ask first. */
  async function check(id: string): Promise<void> {
    const summary = details.value[id]?.summary ?? list.value?.backups.find((b) => b.id === id);
    if (summary?.state === 'needsPassword') {
      passwordPrompt.value = { backupId: id, error: null };
      return;
    }
    await startJob(id, null);
  }

  async function submitPassword(id: string, password: string): Promise<void> {
    if (new TextEncoder().encode(password).length > PASSWORD_MAX_BYTES) {
      passwordPrompt.value = { backupId: id, error: PASSWORD_TOO_LONG_MESSAGE };
      return;
    }
    passwordPrompt.value = null;
    await startJob(id, password);
  }

  function closePrompt(): void {
    passwordPrompt.value = null;
  }

  async function cancel(): Promise<void> {
    const current = job.value;
    if (!current || current.ended) return;
    if (current.cancelling) return;
    current.cancelling = true;
    if (current.jobId !== null) await sendCancel(current.jobId);
  }

  return {
    list,
    loading,
    selectedId,
    details,
    loadingDetails,
    job,
    passwordPrompt,
    lastError,
    selectedSummary,
    selectedDetails,
    jobRunning,
    refresh,
    addFolder,
    forget,
    select,
    check,
    submitPassword,
    closePrompt,
    cancel,
    stopPolling,
    resumePolling,
  };
});
