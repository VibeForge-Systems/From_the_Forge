import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      listBackups: vi.fn(),
      getBackupDetails: vi.fn(),
      cancelJob: vi.fn(),
      listArchives: vi.fn(),
      createArchive: vi.fn(),
      getArchiveDetails: vi.fn(),
      startImport: vi.fn(),
      confirmImport: vi.fn(),
      verifyArchive: vi.fn(),
    },
  };
});
import { IpcError, ipc } from '@/services/ipc';
import { LABEL_INVALID_MESSAGE, checkLabel, useArchives } from '@/stores/archives';
import { LEFTOVER_FILES_MESSAGE, useBackups } from '@/stores/backups';
import { useNotifications } from '@/stores/notifications';
import type { JobEvent } from '@/types';
import {
  ARCHIVE_ID,
  archiveDetails,
  archiveEntry,
  archiveListing,
  archiveSummary,
  backupList,
  preflight,
  summary,
} from './helpers';

const api = vi.mocked(ipc);
const PASSWORD = 'synthetic-test-password-sentinel';
const OPTIONS = { copyAttachments: true, includeRecentlyDeleted: false };

function importHandler(): (e: JobEvent) => void {
  const call = api.startImport.mock.calls.at(-1);
  if (!call) throw new Error('startImport was not called');
  return call[4];
}

function verifyHandler(): (e: JobEvent) => void {
  const call = api.verifyArchive.mock.calls.at(-1);
  if (!call) throw new Error('verifyArchive was not called');
  return call[2];
}

async function flush(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

function toasts(): string[] {
  return useNotifications().toasts.map((t) => `${t.level}: ${t.message}`);
}

const waiting = (jobId = 'job-1') =>
  archiveDetails({
    activeJob: { jobId, kind: 'import', phase: 'awaitingConfirmation', backupId: 'b1', waitingAtGate: true },
    preflight: preflight(),
  });

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  api.listBackups.mockResolvedValue(backupList([summary(), summary({ id: 'enc', state: 'needsPassword', encrypted: true })]));
  api.listArchives.mockResolvedValue(archiveListing());
  api.getArchiveDetails.mockResolvedValue(archiveDetails());
  api.startImport.mockResolvedValue({ jobId: 'job-1' });
  api.verifyArchive.mockResolvedValue({ jobId: 'job-2' });
  api.confirmImport.mockResolvedValue(null);
  api.cancelJob.mockResolvedValue(true);
});

afterEach(() => {
  useArchives().stopPolling();
  vi.useRealTimers();
});

describe('archive labels', () => {
  it('accepts an empty label as none and refuses long or control-character labels before IPC', () => {
    expect(checkLabel('')).toEqual({ ok: true, label: null });
    expect(checkLabel('Family phone')).toEqual({ ok: true, label: 'Family phone' });
    expect(checkLabel('x'.repeat(200))).toEqual({ ok: true, label: 'x'.repeat(200) });
    expect(checkLabel('x'.repeat(201))).toEqual({ ok: false, error: LABEL_INVALID_MESSAGE });
    // 67 three-byte characters are 201 bytes.
    expect(checkLabel('€'.repeat(67)).ok).toBe(false);
    expect(checkLabel('tab\there').ok).toBe(false);
    expect(checkLabel('rtl‮override').ok).toBe(false);
    expect(checkLabel('isolate⁧x').ok).toBe(false);
  });

  it('creates an archive, lists it and selects it; a refused label never reaches IPC', async () => {
    const a = useArchives();
    a.openCreate();
    expect(await a.create('bad\u0007label')).toBe(false);
    expect(a.createError).toBe(LABEL_INVALID_MESSAGE);
    expect(api.createArchive).not.toHaveBeenCalled();

    api.createArchive.mockResolvedValue(archiveSummary());
    expect(await a.create('')).toBe(true);
    expect(api.createArchive).toHaveBeenCalledWith(null);
    expect(a.createOpen).toBe(false);
    expect(a.selectedId).toBe(ARCHIVE_ID);
    expect(toasts()).toContain('success: Archive created.');
  });

  it('keeps the dialog open with the backend sentence when creation fails', async () => {
    const a = useArchives();
    a.openCreate();
    api.createArchive.mockRejectedValue(new IpcError('create_archive', 'insufficientSpace', 'Not enough space.'));
    expect(await a.create('x')).toBe(false);
    expect(a.createOpen).toBe(true);
    expect(a.createError).toBe('Not enough space.');
  });

  it('opens the import dialog with the backup after "create first"', async () => {
    const a = useArchives();
    a.openCreate('b1');
    expect(a.importDialog).toBeNull();
    api.createArchive.mockResolvedValue(archiveSummary());
    await a.create('');
    expect(a.importDialog).toEqual({ archiveId: ARCHIVE_ID, backupId: 'b1', continuing: false });
  });
});

describe('the archive list', () => {
  it('keeps the last summary of an archive that is busy, and drops archives no longer listed', async () => {
    const a = useArchives();
    await a.refresh();
    await a.select(ARCHIVE_ID);
    api.listArchives.mockResolvedValue(archiveListing([archiveEntry({ status: 'importing' })]));
    await a.refresh();
    expect(a.summaries[ARCHIVE_ID]?.label).toBe('Synthetic archive');
    expect(a.jobRunning).toBe(true);
    api.listArchives.mockResolvedValue(archiveListing([]));
    await a.refresh();
    expect(a.selectedId).toBeNull();
    expect(a.details[ARCHIVE_ID]).toBeUndefined();
  });

  it('drops an archive whose details say it is gone', async () => {
    const a = useArchives();
    api.getArchiveDetails.mockRejectedValue(new IpcError('get_archive_details', 'archiveNotFound', 'Gone.'));
    await a.select(ARCHIVE_ID);
    expect(a.selectedId).toBeNull();
    expect(toasts()).toContain('warning: Gone.');
  });
});

describe('the import job', () => {
  it('starts an unencrypted import at once, with the chosen options', async () => {
    const a = useArchives();
    await useBackups().refresh();
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    expect(api.startImport).toHaveBeenCalledWith('b1', ARCHIVE_ID, null, OPTIONS, expect.any(Function));
    expect(a.liveJob?.jobId).toBe('job-1');
    expect(a.liveJob?.kind).toBe('import');
  });

  it('asks an encrypted backup for its password, passes it once and keeps it nowhere', async () => {
    const a = useArchives();
    await useBackups().refresh();
    await a.startImport('enc', ARCHIVE_ID, { copyAttachments: false, includeRecentlyDeleted: true });
    expect(api.startImport).not.toHaveBeenCalled();
    expect(a.passwordPrompt).toEqual({
      backupId: 'enc',
      archiveId: ARCHIVE_ID,
      options: { copyAttachments: false, includeRecentlyDeleted: true },
      error: null,
    });
    await a.submitPassword(PASSWORD);
    expect(api.startImport).toHaveBeenCalledWith(
      'enc',
      ARCHIVE_ID,
      PASSWORD,
      { copyAttachments: false, includeRecentlyDeleted: true },
      expect.any(Function),
    );
    expect(a.passwordPrompt).toBeNull();
    expect(JSON.stringify(a.$state)).not.toContain(PASSWORD);
  });

  it('re-asks on a wrong password, whether refused at once or by a failed event', async () => {
    const a = useArchives();
    api.startImport.mockRejectedValueOnce(new IpcError('start_import', 'wrongPassword', 'Wrong password.'));
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    expect(a.passwordPrompt?.error).toBe('Wrong password.');
    await a.submitPassword(PASSWORD);
    importHandler()({
      kind: 'failed',
      jobId: 'job-1',
      error: { code: 'wrongPassword', message: 'Wrong password again.' },
      stagingRemoved: true,
    });
    expect(a.passwordPrompt).toEqual({ backupId: 'b1', archiveId: ARCHIVE_ID, options: OPTIONS, error: 'Wrong password again.' });
    expect(a.liveJob).toBeNull();
  });

  it('shows the gate once importPreflight arrives and the details carry the preflight', async () => {
    const a = useArchives();
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    api.getArchiveDetails.mockResolvedValue(waiting());
    importHandler()({ kind: 'importPreflight', jobId: 'job-1' });
    await flush();
    expect(a.gate?.jobId).toBe('job-1');
    expect(a.gate?.options).toEqual(OPTIONS);
    expect(a.liveJob?.phase).toBe('awaitingConfirmation');

    await a.answerGate(true, { kind: 'new' });
    expect(api.confirmImport).toHaveBeenCalledWith('job-1', true, { kind: 'new' });
    expect(a.gate).toBeNull();
  });

  it('declines with no lineage, and closes a gate that is no longer waiting', async () => {
    const a = useArchives();
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    api.getArchiveDetails.mockResolvedValue(waiting());
    importHandler()({ kind: 'importPreflight', jobId: 'job-1' });
    await flush();
    await a.answerGate(false, { kind: 'existing', lineageId: 1 });
    expect(api.confirmImport).toHaveBeenCalledWith('job-1', false, null);

    setActivePinia(createPinia());
    const b = useArchives();
    api.getArchiveDetails.mockResolvedValue(waiting('job-7'));
    await b.select(ARCHIVE_ID);
    api.confirmImport.mockRejectedValueOnce(new IpcError('confirm_import', 'jobNotWaiting', 'Not waiting.'));
    await b.answerGate(true, { kind: 'new' });
    expect(b.gate).toBeNull();
    expect(toasts()).toContain('info: Not waiting.');
  });

  it('keeps the gate open with the sentence when the lineage is refused', async () => {
    const a = useArchives();
    api.getArchiveDetails.mockResolvedValue(waiting());
    await a.select(ARCHIVE_ID);
    api.confirmImport.mockRejectedValueOnce(new IpcError('confirm_import', 'invalidInput', 'Choose again.'));
    await a.answerGate(true, { kind: 'existing', lineageId: 9 });
    expect(a.gate?.jobId).toBe('job-1');
    expect(a.gateError).toBe('Choose again.');
  });

  it('re-shows the gate after a reload from the details alone, and polls every 2 s', async () => {
    vi.useFakeTimers();
    const a = useArchives();
    api.getArchiveDetails.mockResolvedValue(waiting('job-5'));
    await a.select(ARCHIVE_ID);
    expect(a.liveJob).toBeNull();
    expect(a.gate).toEqual({ jobId: 'job-5', archiveId: ARCHIVE_ID, preflight: preflight(), options: null });
    expect(api.getArchiveDetails).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2000);
    expect(api.getArchiveDetails).toHaveBeenCalledTimes(2);
    api.getArchiveDetails.mockResolvedValue(archiveDetails());
    await vi.advanceTimersByTimeAsync(2000);
    expect(a.gate).toBeNull();
    await vi.advanceTimersByTimeAsync(6000);
    expect(api.getArchiveDetails).toHaveBeenCalledTimes(3);
  });

  it('records a stopped import as stoppable-and-continuable, with its reason', async () => {
    const a = useArchives();
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    importHandler()({
      kind: 'importFinished',
      jobId: 'job-1',
      outcome: 'stopped',
      stopReason: 'insufficientSpace',
      notImportable: null,
      gate: null,
      session: 3,
      notice: { code: 'insufficientSpace', message: 'Not enough space.' },
      stagingRemoved: false,
      plaintextRemoved: true,
    });
    expect(a.liveJob).toBeNull();
    expect(a.importResults[ARCHIVE_ID]?.outcome).toBe('stopped');
    const t = toasts();
    expect(t).toContain(
      'warning: Import stopped; can continue. There was not enough free space. Free some space, then continue the import.',
    );
    expect(t).toContain(`warning: ${LEFTOVER_FILES_MESSAGE}`);
    expect(t.join(' ')).not.toContain('failed');
  });

  it('says that nothing was written when the gate is declined or times out', async () => {
    const a = useArchives();
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    importHandler()({
      kind: 'importFinished',
      jobId: 'job-1',
      outcome: 'cancelledAtGate',
      stopReason: null,
      notImportable: null,
      gate: 'timedOut',
      session: null,
      notice: null,
      stagingRemoved: true,
      plaintextRemoved: null,
    });
    expect(toasts()).toEqual([
      'info: The import was not confirmed within two minutes, so it was not started. Nothing was written to the archive.',
    ]);
  });

  it('sends a Cancel clicked before the job had a name as soon as it has one', async () => {
    const a = useArchives();
    let resolve: (v: { jobId: string }) => void = () => undefined;
    api.startImport.mockReturnValue(new Promise((r) => (resolve = r)));
    const started = a.startImport('b1', ARCHIVE_ID, OPTIONS);
    await flush();
    await a.cancel();
    expect(api.cancelJob).not.toHaveBeenCalled();
    resolve({ jobId: 'job-1' });
    await started;
    expect(api.cancelJob).toHaveBeenCalledWith('job-1');
    importHandler()({ kind: 'cancelled', jobId: 'job-1', stagingRemoved: true });
    expect(toasts()[0]).toContain('Import cancelled.');
  });

  it('refuses a second job with the busy sentence and keeps other errors per archive', async () => {
    const a = useArchives();
    api.startImport.mockRejectedValueOnce(new IpcError('start_import', 'busy', 'Busy.'));
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    expect(toasts()).toContain('warning: Busy.');
    api.startImport.mockRejectedValueOnce(new IpcError('start_import', 'archiveNewerRules', 'Newer rules.'));
    await a.startImport('b1', ARCHIVE_ID, OPTIONS);
    expect(a.lastError[ARCHIVE_ID]).toEqual({ code: 'archiveNewerRules', message: 'Newer rules.', kind: 'import' });
  });

  it('pre-selects a listed backup from matchingBackupIds when continuing', async () => {
    const a = useArchives();
    await useBackups().refresh();
    api.getArchiveDetails.mockResolvedValue(
      archiveDetails({ resumable: true, matchingBackupIds: ['not-listed', 'enc'] }),
    );
    await a.select(ARCHIVE_ID);
    a.openImport({ archiveId: ARCHIVE_ID, continuing: true });
    expect(a.importDialog).toEqual({ archiveId: ARCHIVE_ID, backupId: 'enc', continuing: true });
    a.openImport({ archiveId: ARCHIVE_ID });
    expect(a.importDialog?.backupId).toBeNull();
  });
});

describe('the verify job', () => {
  it('runs a full verify and records its outcome', async () => {
    const a = useArchives();
    await a.verify(ARCHIVE_ID, 'full');
    expect(api.verifyArchive).toHaveBeenCalledWith(ARCHIVE_ID, 'full', expect.any(Function));
    expect(a.liveJob?.depth).toBe('full');
    verifyHandler()({ kind: 'progress', jobId: 'job-2', phase: 'verifyingFiles', done: 10, total: 100 });
    expect(a.liveJob?.done).toBe(10);
    verifyHandler()({ kind: 'verifyFinished', jobId: 'job-2', outcome: 'problems', recorded: true });
    expect(a.liveJob).toBeNull();
    expect(a.verifyResults[ARCHIVE_ID]).toEqual({ jobId: 'job-2', outcome: 'problems', recorded: true });
    expect(toasts()[0]).toContain('Problems were found. Nothing was changed');
  });

  it('ignores events of another job', async () => {
    const a = useArchives();
    await a.verify(ARCHIVE_ID, 'quick');
    verifyHandler()({ kind: 'verifyFinished', jobId: 'job-other', outcome: 'ok', recorded: true });
    expect(a.liveJob?.jobId).toBe('job-2');
  });
});
