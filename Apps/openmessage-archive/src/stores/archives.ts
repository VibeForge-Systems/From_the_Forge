import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { IpcError, ipc } from '@/services/ipc';
import { GATE_ENDING, NOT_IMPORTABLE, OUTCOME_LABEL, STOP_REASON, VERIFY_OUTCOME } from '@/services/importText';
import type {
  ArchiveDetailsDto,
  ArchiveEntryDto,
  ArchiveListingDto,
  ArchiveSummaryDto,
  GateEndingDto,
  ImportOptionsInput,
  ImportOutcomeDto,
  ImportPreflightDto,
  IpcErrorPayload,
  JobEvent,
  JobPhase,
  LineageInput,
  NotImportableDto,
  StopReasonDto,
  VerifyDepthDto,
  VerifyOutcomeDto,
} from '@/types';
import {
  LEFTOVER_FILES_MESSAGE,
  PASSWORD_MAX_BYTES,
  PASSWORD_TOO_LONG_MESSAGE,
  POLL_INTERVAL_MS,
  useBackups,
  type StoredError,
} from './backups';
import { useNotifications } from './notifications';

/** At most 200 UTF-8 bytes, as `oma_archive::Label::parse` checks. */
export const LABEL_MAX_BYTES = 200;
export const LABEL_INVALID_MESSAGE =
  'Archive names can be up to 200 bytes long and cannot contain control or text-direction characters.';
// C0 and C1 controls, and the bidirectional formatting characters.
const LABEL_FORBIDDEN = /[\u0000-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]/u;

/** The label as the backend will accept it: `null` for none, or an error sentence. */
export function checkLabel(label: string): { ok: true; label: string | null } | { ok: false; error: string } {
  if (label === '') return { ok: true, label: null };
  if (new TextEncoder().encode(label).length > LABEL_MAX_BYTES || LABEL_FORBIDDEN.test(label)) {
    return { ok: false, error: LABEL_INVALID_MESSAGE };
  }
  return { ok: true, label };
}

export const DEFAULT_IMPORT_OPTIONS: Readonly<ImportOptionsInput> = {
  copyAttachments: true,
  includeRecentlyDeleted: false,
};

/** An import or a verify this page started and hears over its channel. */
export interface ArchiveJobState {
  jobId: string | null;
  archiveId: string;
  /** The backup an import reads. */
  backupId: string | null;
  kind: 'import' | 'verify';
  /** For a verify: a full one counts its files in bytes. */
  depth: VerifyDepthDto | null;
  options: ImportOptionsInput | null;
  phase: JobPhase | null;
  done: number;
  total: number | null;
  cancelling: boolean;
  ended: boolean;
  /** An `importPreflight` arrived and the gate has not been answered. */
  waitingAtGate: boolean;
}

/** What an `importFinished` event said. */
export interface ImportResult {
  jobId: string;
  outcome: ImportOutcomeDto;
  stopReason: StopReasonDto | null;
  notImportable: NotImportableDto | null;
  gate: GateEndingDto | null;
  session: number | null;
  notice: IpcErrorPayload | null;
  stagingRemoved: boolean;
  plaintextRemoved: boolean | null;
}

/** What a `verifyFinished` event said. */
export interface VerifyResult {
  jobId: string;
  outcome: VerifyOutcomeDto;
  recorded: boolean;
}

export interface ImportDialogState {
  archiveId: string | null;
  backupId: string | null;
  /** Opened with "Continue importing…": the matching backup is pre-selected. */
  continuing: boolean;
}

export interface ImportPasswordPrompt {
  backupId: string;
  archiveId: string;
  options: ImportOptionsInput;
  error: string | null;
}

/** The gate as the dialog shows it, live or re-shown after a reload. */
export interface GateView {
  jobId: string;
  archiveId: string;
  preflight: ImportPreflightDto;
  /** The options the import was started with, when this page started it. */
  options: ImportOptionsInput | null;
}

/**
 * The Archive page and its dialogs (M4 §16). The password is never stored
 * here: it passes through `submitPassword` as an argument straight into the
 * IPC call. Archive DTOs hold counts, kinds and the label only.
 */
export const useArchives = defineStore('archives', () => {
  const listing = ref<ArchiveListingDto | null>(null);
  const loading = ref(false);
  const selectedId = ref<string | null>(null);
  const details = ref<Record<string, ArchiveDetailsDto>>({});
  /** The last summary seen for each archive, to name it while it is busy. */
  const summaries = ref<Record<string, ArchiveSummaryDto>>({});
  const loadingDetails = ref<string | null>(null);
  const lastError = ref<Record<string, StoredError>>({});
  const job = ref<ArchiveJobState | null>(null);
  const importResults = ref<Record<string, ImportResult>>({});
  const verifyResults = ref<Record<string, VerifyResult>>({});

  const createOpen = ref(false);
  const creating = ref(false);
  const createError = ref<string | null>(null);
  /** Create first, then open the import dialog with this backup. */
  const createThenImport = ref<{ backupId: string | null } | null>(null);

  const importDialog = ref<ImportDialogState | null>(null);
  const passwordPrompt = ref<ImportPasswordPrompt | null>(null);
  const confirming = ref(false);
  const gateError = ref<string | null>(null);
  /** Gates answered from this page, so a stale poll cannot re-show one. */
  const answered = ref<string[]>([]);
  const polling = new Map<string, ReturnType<typeof setTimeout>>();

  const entries = computed<ArchiveEntryDto[]>(() => listing.value?.entries ?? []);
  /** Archives an import can go into. */
  const importable = computed(() =>
    entries.value.filter((e) => e.status === 'ready' && e.summary && !e.summary.newerRules),
  );
  const selectedEntry = computed(() => entries.value.find((e) => e.archiveId === selectedId.value) ?? null);
  const selectedDetails = computed(() => (selectedId.value ? (details.value[selectedId.value] ?? null) : null));
  const selectedSummary = computed<ArchiveSummaryDto | null>(() => {
    const id = selectedId.value;
    if (!id) return null;
    return details.value[id]?.summary ?? selectedEntry.value?.summary ?? summaries.value[id] ?? null;
  });
  const liveJob = computed(() => (job.value && !job.value.ended ? job.value : null));
  const jobRunning = computed(
    () =>
      liveJob.value !== null ||
      Object.values(details.value).some((d) => d.activeJob !== null) ||
      entries.value.some((e) => e.status === 'importing' || e.status === 'verifying'),
  );

  /** The gate to show: this page's job first, else one re-shown after a reload. */
  const gate = computed<GateView | null>(() => {
    const j = liveJob.value;
    if (j && j.kind === 'import' && j.waitingAtGate && j.jobId && !answered.value.includes(j.jobId)) {
      const d = details.value[j.archiveId];
      if (d?.preflight && d.activeJob?.jobId === j.jobId) {
        return { jobId: j.jobId, archiveId: j.archiveId, preflight: d.preflight, options: j.options };
      }
    }
    for (const [archiveId, d] of Object.entries(details.value)) {
      const a = d.activeJob;
      if (!a?.waitingAtGate || !d.preflight || answered.value.includes(a.jobId)) continue;
      if (j && j.jobId === a.jobId && !j.waitingAtGate) continue;
      return { jobId: a.jobId, archiveId, preflight: d.preflight, options: j?.jobId === a.jobId ? j.options : null };
    }
    return null;
  });

  function toast(level: 'info' | 'success' | 'warning' | 'danger', message: string): void {
    useNotifications().push(level, message);
  }

  function asStored(error: unknown): StoredError {
    if (error instanceof IpcError) return { code: error.code, message: error.message };
    return { code: 'internal', message: 'Something went wrong. Please report this as a bug.' };
  }

  function setError(id: string, e: StoredError | null): void {
    const next = { ...lastError.value };
    if (e) next[id] = e;
    else delete next[id];
    lastError.value = next;
  }

  function remember(s: ArchiveSummaryDto): void {
    summaries.value = { ...summaries.value, [s.archiveId]: s };
  }

  function stopPollingFor(id: string): void {
    const t = polling.get(id);
    if (t !== undefined) {
      clearTimeout(t);
      polling.delete(id);
    }
  }

  function dropArchive(id: string): void {
    if (id in details.value) {
      const next = { ...details.value };
      delete next[id];
      details.value = next;
    }
    setError(id, null);
    stopPollingFor(id);
    if (selectedId.value === id) selectedId.value = null;
  }

  function applyListing(next: ArchiveListingDto): void {
    listing.value = next;
    for (const e of next.entries) if (e.summary) remember(e.summary);
    const listed = new Set(next.entries.map((e) => e.archiveId));
    const live = liveJob.value?.archiveId ?? null;
    const known = new Set([...Object.keys(details.value), ...polling.keys()]);
    if (selectedId.value) known.add(selectedId.value);
    for (const id of known) if (!listed.has(id) && id !== live) dropArchive(id);
  }

  async function refresh(): Promise<void> {
    if (loading.value) return;
    loading.value = true;
    try {
      applyListing(await ipc.listArchives());
    } catch (error) {
      const e = asStored(error);
      toast(e.code === 'busy' ? 'info' : 'danger', e.message);
    } finally {
      loading.value = false;
    }
  }

  function clearActiveJob(id: string): void {
    const d = details.value[id];
    if (d?.activeJob || d?.preflight) {
      details.value = { ...details.value, [id]: { ...d, activeJob: null, preflight: null } };
    }
  }

  async function loadDetails(id: string): Promise<ArchiveDetailsDto | null> {
    try {
      const d = await ipc.getArchiveDetails(id);
      details.value = { ...details.value, [id]: d };
      remember(d.summary);
      if (lastError.value[id]?.source === 'details') setError(id, null);
      return d;
    } catch (error) {
      const e = asStored(error);
      if (e.code === 'archiveNotFound') {
        dropArchive(id);
        toast('warning', e.message);
        await refresh();
      } else {
        clearActiveJob(id);
        setError(id, { ...e, source: 'details' });
      }
      return null;
    }
  }

  async function select(id: string): Promise<void> {
    selectedId.value = id;
    loadingDetails.value = id;
    try {
      const d = await loadDetails(id);
      if (d?.activeJob) startPolling(id);
    } finally {
      if (loadingDetails.value === id) loadingDetails.value = null;
    }
  }

  /**
   * Poll an archive's details every 2 s while a job runs on it (§16): the
   * phase of a job this page cannot hear, and the gate after a reload.
   */
  function startPolling(id: string): void {
    if (polling.has(id)) return;
    const tick = async (): Promise<void> => {
      const d = await loadDetails(id);
      const live = liveJob.value?.archiveId === id;
      if (d?.activeJob || (live && d !== null)) {
        polling.set(id, setTimeout(() => void tick(), POLL_INTERVAL_MS));
        return;
      }
      polling.delete(id);
      void refresh();
    };
    polling.set(id, setTimeout(() => void tick(), POLL_INTERVAL_MS));
  }

  function stopPolling(): void {
    for (const t of polling.values()) clearTimeout(t);
    polling.clear();
  }

  /** Poll again for every cached archive still marked as running a job. */
  function resumePolling(): void {
    for (const [id, d] of Object.entries(details.value)) if (d.activeJob) startPolling(id);
  }

  // --- Create ---------------------------------------------------------------

  function openCreate(thenImportBackupId: string | null | undefined = undefined): void {
    createError.value = null;
    createThenImport.value = thenImportBackupId === undefined ? null : { backupId: thenImportBackupId };
    importDialog.value = null;
    createOpen.value = true;
  }

  function closeCreate(): void {
    createOpen.value = false;
    createError.value = null;
    createThenImport.value = null;
  }

  async function create(rawLabel: string): Promise<boolean> {
    const checked = checkLabel(rawLabel);
    if (!checked.ok) {
      createError.value = checked.error;
      return false;
    }
    creating.value = true;
    createError.value = null;
    try {
      const s = await ipc.createArchive(checked.label);
      remember(s);
      const then = createThenImport.value;
      createOpen.value = false;
      createThenImport.value = null;
      toast('success', 'Archive created.');
      await refresh();
      await select(s.archiveId);
      if (then) openImport({ archiveId: s.archiveId, backupId: then.backupId });
      return true;
    } catch (error) {
      const e = asStored(error);
      createError.value = e.message;
      return false;
    } finally {
      creating.value = false;
    }
  }

  // --- Import ---------------------------------------------------------------

  /**
   * Open the import dialog. With `continuing`, the backups the unfinished
   * session's source matches (computed in Rust) are pre-selected.
   */
  function openImport(opts: { archiveId?: string | null; backupId?: string | null; continuing?: boolean } = {}): void {
    const only = importable.value.length === 1 ? importable.value[0] : undefined;
    const archiveId = opts.archiveId ?? only?.archiveId ?? null;
    let backupId = opts.backupId ?? null;
    if (opts.continuing && archiveId && backupId === null) {
      const listed = new Set(useBackups().list?.backups.map((b) => b.id) ?? []);
      backupId = details.value[archiveId]?.matchingBackupIds.find((id) => listed.has(id)) ?? null;
    }
    importDialog.value = { archiveId, backupId, continuing: opts.continuing ?? false };
    const backups = useBackups();
    if (!backups.list) void backups.refresh();
    if (!listing.value) void refresh();
  }

  function closeImport(): void {
    importDialog.value = null;
  }

  function backupNeedsPassword(backupId: string): boolean {
    const b = useBackups();
    const s = b.details[backupId]?.summary ?? b.list?.backups.find((x) => x.id === backupId);
    return s?.state === 'needsPassword';
  }

  /** Start an import: an encrypted backup asks for its password first. */
  async function startImport(backupId: string, archiveId: string, options: ImportOptionsInput): Promise<void> {
    importDialog.value = null;
    if (backupNeedsPassword(backupId)) {
      passwordPrompt.value = { backupId, archiveId, options: { ...options }, error: null };
      return;
    }
    await runImport(backupId, archiveId, { ...options }, null);
  }

  async function submitPassword(password: string): Promise<void> {
    const prompt = passwordPrompt.value;
    if (!prompt) return;
    if (new TextEncoder().encode(password).length > PASSWORD_MAX_BYTES) {
      passwordPrompt.value = { ...prompt, error: PASSWORD_TOO_LONG_MESSAGE };
      return;
    }
    passwordPrompt.value = null;
    await runImport(prompt.backupId, prompt.archiveId, prompt.options, password);
  }

  function closePrompt(): void {
    passwordPrompt.value = null;
  }

  function adopt(current: ArchiveJobState, jobId: string): void {
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

  function newJob(
    archiveId: string,
    kind: 'import' | 'verify',
    extra: Partial<ArchiveJobState> = {},
  ): ArchiveJobState {
    return {
      jobId: null,
      archiveId,
      backupId: null,
      kind,
      depth: null,
      options: null,
      phase: null,
      done: 0,
      total: null,
      cancelling: false,
      ended: false,
      waitingAtGate: false,
      ...extra,
    };
  }

  /** Name the job from the command's response, unless it already ended. */
  function started(archiveId: string, jobId: string): void {
    const current = job.value;
    if (!current || current.archiveId !== archiveId || current.ended) return;
    if (current.jobId === null) adopt(current, jobId);
    startPolling(archiveId);
  }

  async function runImport(
    backupId: string,
    archiveId: string,
    options: ImportOptionsInput,
    password: string | null,
  ): Promise<void> {
    setError(archiveId, null);
    const next = { ...importResults.value };
    delete next[archiveId];
    importResults.value = next;
    job.value = newJob(archiveId, 'import', { backupId, options });
    try {
      const r = await ipc.startImport(backupId, archiveId, password, options, (e) => handleEvent(archiveId, e));
      started(archiveId, r.jobId);
    } catch (error) {
      if (job.value?.archiveId === archiveId && job.value.jobId === null) job.value = null;
      const e = asStored(error);
      switch (e.code) {
        case 'passwordRequired':
          passwordPrompt.value = { backupId, archiveId, options, error: null };
          break;
        case 'passwordTooLong':
        case 'wrongPassword':
          passwordPrompt.value = { backupId, archiveId, options, error: e.message };
          break;
        case 'busy':
          toast('warning', e.message);
          break;
        default:
          setError(archiveId, { ...e, kind: 'import' });
      }
    }
  }

  /** Answer the gate: Import with a lineage, or Cancel (nothing is written). */
  async function answerGate(proceed: boolean, lineage: LineageInput | null): Promise<void> {
    const view = gate.value;
    if (!view || confirming.value) return;
    confirming.value = true;
    gateError.value = null;
    try {
      await ipc.confirmImport(view.jobId, proceed, proceed ? lineage : null);
      answered.value = [...answered.value, view.jobId];
      if (job.value?.jobId === view.jobId) job.value.waitingAtGate = false;
      startPolling(view.archiveId);
    } catch (error) {
      const e = asStored(error);
      if (e.code === 'invalidInput') {
        gateError.value = e.message;
      } else {
        answered.value = [...answered.value, view.jobId];
        if (job.value?.jobId === view.jobId) job.value.waitingAtGate = false;
        toast(e.code === 'jobNotWaiting' ? 'info' : 'danger', e.message);
        void loadDetails(view.archiveId);
      }
    } finally {
      confirming.value = false;
    }
  }

  // --- Verify ---------------------------------------------------------------

  async function verify(archiveId: string, depth: VerifyDepthDto): Promise<void> {
    setError(archiveId, null);
    const next = { ...verifyResults.value };
    delete next[archiveId];
    verifyResults.value = next;
    job.value = newJob(archiveId, 'verify', { depth });
    try {
      const r = await ipc.verifyArchive(archiveId, depth, (e) => handleEvent(archiveId, e));
      started(archiveId, r.jobId);
    } catch (error) {
      if (job.value?.archiveId === archiveId && job.value.jobId === null) job.value = null;
      const e = asStored(error);
      if (e.code === 'busy') toast('warning', e.message);
      else setError(archiveId, { ...e, kind: 'verify' });
    }
  }

  /** Cancel this page's job, or a job it cannot hear on the selected archive. */
  async function cancel(archiveId: string | null = null): Promise<void> {
    const current = liveJob.value;
    if (current && (archiveId === null || current.archiveId === archiveId)) {
      if (current.cancelling) return;
      current.cancelling = true;
      if (current.jobId !== null) await sendCancel(current.jobId);
      return;
    }
    const id = archiveId ?? selectedId.value;
    const active = id ? details.value[id]?.activeJob : null;
    if (active) await sendCancel(active.jobId);
  }

  // --- Events ---------------------------------------------------------------

  function handleEvent(archiveId: string, event: JobEvent): void {
    const current = job.value;
    if (!current || current.archiveId !== archiveId || current.ended) return;
    if (current.jobId !== null && current.jobId !== event.jobId) return;
    adopt(current, event.jobId);

    let stagingRemoved = true;
    switch (event.kind) {
      case 'progress':
        current.phase = event.phase;
        current.done = event.done;
        current.total = event.total;
        if (event.phase !== 'awaitingConfirmation') current.waitingAtGate = false;
        return;
      case 'importPreflight':
        // The preflight itself comes with the details (D47).
        current.waitingAtGate = true;
        current.phase = 'awaitingConfirmation';
        void loadDetails(archiveId);
        return;
      // The backup jobs' events: never sent to an archive job.
      case 'finished':
      case 'extractionFinished':
        return;
      case 'importFinished':
        stagingRemoved = event.stagingRemoved;
        importResults.value = {
          ...importResults.value,
          [archiveId]: {
            jobId: event.jobId,
            outcome: event.outcome,
            stopReason: event.stopReason,
            notImportable: event.notImportable,
            gate: event.gate,
            session: event.session,
            notice: event.notice,
            stagingRemoved: event.stagingRemoved,
            plaintextRemoved: event.plaintextRemoved,
          },
        };
        toast(...importToast(event.outcome, event.stopReason, event.notImportable, event.gate));
        break;
      case 'verifyFinished':
        verifyResults.value = {
          ...verifyResults.value,
          [archiveId]: { jobId: event.jobId, outcome: event.outcome, recorded: event.recorded },
        };
        toast(event.outcome === 'ok' ? 'success' : event.outcome === 'problems' ? 'warning' : 'info', VERIFY_OUTCOME[event.outcome]);
        break;
      case 'failed':
        stagingRemoved = event.stagingRemoved;
        if (event.error.code === 'wrongPassword' && current.kind === 'import' && current.backupId) {
          passwordPrompt.value = {
            backupId: current.backupId,
            archiveId,
            options: current.options ?? { ...DEFAULT_IMPORT_OPTIONS },
            error: event.error.message,
          };
        } else {
          setError(archiveId, { code: event.error.code, message: event.error.message, kind: current.kind });
        }
        break;
      case 'cancelled':
        stagingRemoved = event.stagingRemoved;
        toast(
          event.stagingRemoved ? 'info' : 'warning',
          current.kind === 'verify'
            ? 'Verification cancelled.'
            : event.stagingRemoved
              ? 'Import cancelled. Temporary files were removed. An import that had started can be continued.'
              : 'Import cancelled. Some temporary files could not be removed yet; they will be removed the next time OpenMessage Archive starts.',
        );
        break;
    }
    current.ended = true;
    current.waitingAtGate = false;
    job.value = null;
    clearActiveJob(archiveId);
    if (!stagingRemoved && event.kind !== 'cancelled') toast('warning', LEFTOVER_FILES_MESSAGE);
    void loadDetails(archiveId);
    void refresh();
    if (current.backupId) void useBackups().refresh();
  }

  return {
    listing,
    loading,
    selectedId,
    details,
    summaries,
    loadingDetails,
    lastError,
    job,
    importResults,
    verifyResults,
    createOpen,
    creating,
    createError,
    createThenImport,
    importDialog,
    passwordPrompt,
    confirming,
    gateError,
    entries,
    importable,
    selectedEntry,
    selectedDetails,
    selectedSummary,
    liveJob,
    jobRunning,
    gate,
    refresh,
    loadDetails,
    select,
    stopPolling,
    resumePolling,
    openCreate,
    closeCreate,
    create,
    openImport,
    closeImport,
    startImport,
    submitPassword,
    closePrompt,
    answerGate,
    verify,
    cancel,
  };
});

/** The toast for how an import ended. */
export function importToast(
  outcome: ImportOutcomeDto,
  stop: StopReasonDto | null,
  notImportable: NotImportableDto | null,
  gate: GateEndingDto | null,
): ['success' | 'info' | 'warning' | 'danger', string] {
  switch (outcome) {
    case 'completed':
      return ['success', OUTCOME_LABEL.completed];
    case 'stopped':
      return ['warning', `${OUTCOME_LABEL.stopped} ${STOP_REASON[stop ?? 'other']}`];
    case 'cancelledAtGate':
      return ['info', GATE_ENDING[gate ?? 'declined']];
    case 'notImportable':
      return ['danger', `${NOT_IMPORTABLE[notImportable ?? 'other']} Nothing was imported.`];
  }
}
