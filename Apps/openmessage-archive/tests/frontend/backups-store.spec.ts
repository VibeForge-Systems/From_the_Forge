import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      listBackups: vi.fn(),
      addBackupFolder: vi.fn(),
      forgetBackupFolder: vi.fn(),
      getBackupDetails: vi.fn(),
      openBackup: vi.fn(),
      cancelJob: vi.fn(),
    },
  };
});
import { IpcError, ipc } from '@/services/ipc';
import { LEFTOVER_FILES_MESSAGE, useBackups } from '@/stores/backups';
import { useNotifications } from '@/stores/notifications';
import type { JobEvent } from '@/types';
import { backupList, details, report, summary } from './helpers';

const api = vi.mocked(ipc);
const PASSWORD = 'synthetic-test-password-sentinel';

/** The handler the store passed to the last openBackup call. */
function lastHandler(): (e: JobEvent) => void {
  const call = api.openBackup.mock.calls.at(-1);
  if (!call) throw new Error('openBackup was not called');
  return call[2];
}

/** Let the store's fire-and-forget follow-ups (loadDetails, refresh) settle. */
async function flushMicrotasks(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

function toasts(): string[] {
  return useNotifications().toasts.map((t) => `${t.level}: ${t.message}`);
}

beforeEach(() => {
  setActivePinia(createPinia());
  vi.clearAllMocks();
  api.listBackups.mockResolvedValue(backupList());
  api.getBackupDetails.mockImplementation(async (id) =>
    details(summary({ id, state: id === 'enc' ? 'needsPassword' : 'ready' })),
  );
  api.cancelJob.mockResolvedValue(true);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('backups store: listing and folders', () => {
  it('refreshes, and ignores a refresh while one is loading', async () => {
    const store = useBackups();
    let resolve: (v: ReturnType<typeof backupList>) => void = () => undefined;
    api.listBackups.mockImplementationOnce(() => new Promise((r) => (resolve = r)));
    const first = store.refresh();
    expect(store.loading).toBe(true);
    await store.refresh();
    expect(api.listBackups).toHaveBeenCalledTimes(1);
    resolve(backupList());
    await first;
    expect(store.loading).toBe(false);
    expect(store.list?.backups).toHaveLength(1);
  });

  it('selects a backup and loads its details', async () => {
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    expect(store.selectedId).toBe('b1');
    expect(store.selectedDetails?.path).toContain('SYNTHETIC0');
    expect(api.getBackupDetails).toHaveBeenCalledWith('b1');
  });

  it.each([
    ['added', true, 'success: Backup folder added.'],
    ['addedAncestor', true, 'info: You chose a folder inside a backup. The backup folder itself was added.'],
    ['alreadyListed', false, 'info: That folder is already listed.'],
    ['refused', false, 'danger: No iPhone backup was found in this folder.'],
  ] as const)('adds a folder with outcome %s', async (outcome, saved, toast) => {
    api.addBackupFolder.mockResolvedValue({
      outcome,
      message: 'No iPhone backup was found in this folder.',
      saved,
      list: backupList(),
    });
    const store = useBackups();
    await store.addFolder();
    expect(store.list).not.toBeNull();
    expect(toasts()).toContain(toast);
  });

  it('warns when an added folder is kept for this session only', async () => {
    api.addBackupFolder.mockResolvedValue({ outcome: 'added', message: '', saved: false, list: backupList() });
    const store = useBackups();
    await store.addFolder();
    expect(toasts()).toContain('warning: Added for this session only. Settings are read-only.');
  });

  it('forgetting the folder of the selected backup clears the selection and its cached state', async () => {
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    store.lastError = { b1: { code: 'io', message: 'old' } };
    api.forgetBackupFolder.mockResolvedValue({ saved: true, list: backupList([summary({ id: 'b2' })]) });
    await store.forget('loc-1');
    expect(store.selectedId).toBeNull();
    expect(store.selectedSummary).toBeNull();
    expect(store.details.b1).toBeUndefined();
    expect(store.lastError.b1).toBeUndefined();
    expect(toasts()).toContain('info: Folder removed from the list. Nothing was deleted.');
  });

  it('warns when a forgotten folder is forgotten for this session only', async () => {
    api.forgetBackupFolder.mockResolvedValue({ saved: false, list: backupList([]) });
    const store = useBackups();
    await store.forget('loc-1');
    expect(store.list?.backups).toEqual([]);
    expect(toasts()).toContain('warning: Removed for this session only. Settings are read-only.');
  });

  it('clears a details error once the details load, but keeps a job error', async () => {
    const store = useBackups();
    await store.refresh();
    api.getBackupDetails.mockRejectedValueOnce(
      new IpcError('get_backup_details', 'io', 'The backup could not be read.'),
    );
    await store.select('b1');
    expect(store.lastError.b1?.message).toBe('The backup could not be read.');
    await store.select('b1');
    expect(store.lastError.b1).toBeUndefined();

    api.openBackup.mockResolvedValue({ jobId: 'job-9' });
    await store.check('b1');
    lastHandler()({
      kind: 'failed',
      jobId: 'job-9',
      error: { code: 'malformedBackup', message: 'Manifest.db could not be read.' },
      stagingRemoved: true,
    });
    await flushMicrotasks();
    expect(store.lastError.b1?.message).toBe('Manifest.db could not be read.');
  });

  it('does nothing when the picker is cancelled', async () => {
    api.addBackupFolder.mockResolvedValue(null);
    const store = useBackups();
    await store.addFolder();
    expect(store.list).toBeNull();
    expect(toasts()).toEqual([]);
  });
});

describe('backups store: jobs', () => {
  it('runs an unencrypted check: progress, then finished with a report', async () => {
    api.openBackup.mockResolvedValue({ jobId: 'job-1' });
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    await store.check('b1');
    expect(api.openBackup).toHaveBeenCalledWith('b1', null, expect.any(Function));
    expect(store.job?.jobId).toBe('job-1');
    const emit = lastHandler();
    emit({ kind: 'progress', jobId: 'job-1', phase: 'checkingFiles', done: 3, total: 8 });
    expect(store.job?.phase).toBe('checkingFiles');
    expect(store.job?.done).toBe(3);
    expect(store.jobRunning).toBe(true);
    emit({ kind: 'finished', jobId: 'job-1', report: report(), stagingRemoved: true });
    expect(store.job).toBeNull();
    expect(store.details.b1?.lastReport?.importable).toBe(true);
  });

  it('handles events that arrive before the command response', async () => {
    let respond: (v: { jobId: string }) => void = () => undefined;
    api.openBackup.mockImplementation(() => new Promise((r) => (respond = r)));
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    const pending = store.check('b1');
    expect(store.job?.jobId).toBeNull();
    const emit = lastHandler();
    emit({ kind: 'finished', jobId: 'job-7', report: report(), stagingRemoved: true });
    expect(store.job).toBeNull();
    respond({ jobId: 'job-7' });
    await pending;
    expect(store.job).toBeNull();
    expect(store.jobRunning).toBe(false);
  });

  it('opens the password prompt for an encrypted backup and reopens it on a wrong password', async () => {
    api.listBackups.mockResolvedValue(backupList([summary({ id: 'enc', state: 'needsPassword', encrypted: true })]));
    api.openBackup.mockResolvedValue({ jobId: 'job-2' });
    const store = useBackups();
    await store.refresh();
    await store.select('enc');
    await store.check('enc');
    expect(api.openBackup).not.toHaveBeenCalled();
    expect(store.passwordPrompt).toEqual({ backupId: 'enc', error: null });
    await store.submitPassword('enc', PASSWORD);
    expect(store.passwordPrompt).toBeNull();
    expect(api.openBackup).toHaveBeenCalledWith('enc', PASSWORD, expect.any(Function));
    lastHandler()({
      kind: 'failed',
      jobId: 'job-2',
      error: { code: 'wrongPassword', message: 'That password did not unlock the backup.' },
      stagingRemoved: true,
    });
    expect(store.passwordPrompt).toEqual({ backupId: 'enc', error: 'That password did not unlock the backup.' });
    expect(store.job).toBeNull();
    expect(JSON.stringify(store.$state)).not.toContain(PASSWORD);
  });

  it('refuses an over-long password without calling Rust', async () => {
    const store = useBackups();
    await store.submitPassword('enc', 'x'.repeat(1025));
    expect(api.openBackup).not.toHaveBeenCalled();
    expect(store.passwordPrompt?.error).toBe('Passwords longer than 1,024 bytes are not accepted.');
    // 342 three-byte characters are 1,026 bytes but only 342 UTF-16 units.
    await store.submitPassword('enc', '€'.repeat(342));
    expect(api.openBackup).not.toHaveBeenCalled();
  });

  it.each([
    [true, 'info: Check cancelled. Temporary files were removed.'],
    [
      false,
      'warning: Check cancelled. Some temporary files could not be removed yet; they will be removed the next time OpenMessage Archive starts.',
    ],
  ])('cancels (stagingRemoved %s)', async (removed, toast) => {
    api.openBackup.mockResolvedValue({ jobId: 'job-3' });
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    await store.check('b1');
    await store.cancel();
    expect(store.job?.cancelling).toBe(true);
    expect(api.cancelJob).toHaveBeenCalledWith('job-3');
    lastHandler()({ kind: 'cancelled', jobId: 'job-3', stagingRemoved: removed });
    expect(store.job).toBeNull();
    expect(toasts()).toContain(toast);
  });

  it('sends a cancel clicked before the job had a name once it has one', async () => {
    let respond: (v: { jobId: string }) => void = () => undefined;
    api.openBackup.mockImplementation(() => new Promise((r) => (respond = r)));
    const store = useBackups();
    await store.refresh();
    const pending = store.check('b1');
    await store.cancel();
    expect(api.cancelJob).not.toHaveBeenCalled();
    respond({ jobId: 'job-9' });
    await pending;
    expect(api.cancelJob).toHaveBeenCalledWith('job-9');
  });

  it('warns about leftover files after a finished job that could not clean up', async () => {
    api.openBackup.mockResolvedValue({ jobId: 'job-4' });
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    await store.check('b1');
    lastHandler()({ kind: 'finished', jobId: 'job-4', report: report(), stagingRemoved: false });
    expect(toasts()).toContain(`warning: ${LEFTOVER_FILES_MESSAGE}`);
  });

  it('shows busy as a toast and keeps no job', async () => {
    api.openBackup.mockRejectedValue(
      new IpcError('open_backup', 'busy', 'Another backup is being checked. Wait for it to finish or cancel it.'),
    );
    const store = useBackups();
    await store.refresh();
    await store.check('b1');
    expect(store.job).toBeNull();
    expect(toasts()).toContain('warning: Another backup is being checked. Wait for it to finish or cancel it.');
  });

  it('records other failures per backup', async () => {
    api.openBackup.mockResolvedValue({ jobId: 'job-5' });
    const store = useBackups();
    await store.refresh();
    await store.check('b1');
    lastHandler()({
      kind: 'failed',
      jobId: 'job-5',
      error: { code: 'malformedBackup', message: 'Manifest.db could not be read.' },
      stagingRemoved: true,
    });
    expect(store.lastError.b1).toEqual({ code: 'malformedBackup', message: 'Manifest.db could not be read.' });
  });

  it('polls a job it cannot hear until it ends', async () => {
    vi.useFakeTimers();
    let calls = 0;
    api.getBackupDetails.mockImplementation(async () => {
      calls += 1;
      const running = calls < 3;
      return details(summary(), {
        activeJob: running ? { jobId: 'job-6', backupId: 'b1', phase: 'checkingFiles' } : null,
        lastReport: running ? null : report(),
        lastCleanup: running ? null : { stagingRemoved: false },
      });
    });
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    expect(store.jobRunning).toBe(true);
    await vi.advanceTimersByTimeAsync(2000);
    expect(calls).toBe(2);
    await vi.advanceTimersByTimeAsync(2000);
    expect(calls).toBe(3);
    expect(store.selectedDetails?.lastReport?.importable).toBe(true);
    expect(store.jobRunning).toBe(false);
    expect(toasts()).toContain(`warning: ${LEFTOVER_FILES_MESSAGE}`);
    await vi.advanceTimersByTimeAsync(6000);
    expect(calls).toBe(3);
  });

  it('does not stay busy when a failed job is followed by its backup disappearing', async () => {
    api.openBackup.mockResolvedValue({ jobId: 'job-11' });
    api.getBackupDetails.mockResolvedValueOnce(
      details(summary(), { activeJob: { jobId: 'job-11', backupId: 'b1', phase: 'checkingFiles' } }),
    );
    const store = useBackups();
    await store.refresh();
    await store.check('b1');
    await store.select('b1');
    expect(store.jobRunning).toBe(true);
    // The drive is unplugged: the job fails and the backup is gone.
    api.getBackupDetails.mockRejectedValue(
      new IpcError('get_backup_details', 'backupNotFound', 'That backup is no longer available.'),
    );
    api.listBackups.mockResolvedValue(backupList([]));
    lastHandler()({
      kind: 'failed',
      jobId: 'job-11',
      error: { code: 'io', message: 'The backup could not be read.' },
      stagingRemoved: true,
    });
    await flushMicrotasks();
    expect(store.details.b1).toBeUndefined();
    expect(store.jobRunning).toBe(false);
  });

  it('resumes polling a cached running job after the page was left and reopened', async () => {
    vi.useFakeTimers();
    let running = true;
    api.getBackupDetails.mockImplementation(async () =>
      details(summary(), {
        activeJob: running ? { jobId: 'job-12', backupId: 'b1', phase: 'checkingFiles' } : null,
      }),
    );
    const store = useBackups();
    await store.refresh();
    await store.select('b1');
    expect(store.jobRunning).toBe(true);
    store.stopPolling(); // the page unmounted
    running = false; // the job ended meanwhile
    api.listBackups.mockResolvedValue(backupList());
    store.resumePolling(); // the page mounted again
    await store.refresh();
    await vi.advanceTimersByTimeAsync(2000);
    expect(store.jobRunning).toBe(false);
  });

  it('never keeps the password anywhere, even after a rejected invoke', async () => {
    api.listBackups.mockResolvedValue(backupList([summary({ id: 'enc', state: 'needsPassword', encrypted: true })]));
    api.openBackup.mockRejectedValue(new IpcError('open_backup', 'internal', 'The check could not be started.'));
    const store = useBackups();
    await store.refresh();
    await store.submitPassword('enc', PASSWORD);
    expect(JSON.stringify(store.$state)).not.toContain(PASSWORD);
    expect(JSON.stringify(store.lastError)).not.toContain(PASSWORD);
    expect(toasts().join('\n')).not.toContain(PASSWORD);
    expect(store.lastError.enc?.message).toBe('The check could not be started.');
  });
});
