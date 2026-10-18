import { beforeEach, describe, expect, it, vi } from 'vitest';

// A hand-rolled fake rather than vi.fn(): vitest 4's mock bookkeeping
// reports a plain-object rejection returned from a vi.fn() as an unhandled
// error even when the caller handles it, and Tauri rejects with exactly such
// plain objects.
const fake = vi.hoisted(() => ({
  calls: [] as [string, unknown][],
  respond: (_command: string): Promise<unknown> => Promise.resolve(null),
}));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: unknown) => {
    fake.calls.push([command, args]);
    return fake.respond(command);
  },
  // Records the handler the wrapper installs, like Tauri's Channel.
  Channel: class {
    onmessage: (message: unknown) => void = () => undefined;
  },
}));
import { ERROR_CODES, IpcError, ipc, toIpcError } from '@/services/ipc';

const PASSWORD = 'synthetic-test-password-sentinel';

/** Every string anywhere in a value, recursively. */
function strings(value: unknown): string[] {
  if (typeof value === 'string') return [value];
  if (Array.isArray(value)) return value.flatMap(strings);
  if (value && typeof value === 'object') return Object.values(value).flatMap(strings);
  return [];
}

describe('toIpcError', () => {
  it('keeps the code and message of a structured command error', () => {
    const err = toIpcError('get_settings', { code: 'io', message: 'disk full' });
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe('io');
    expect(err.message).toBe('disk full');
    expect(err.command).toBe('get_settings');
  });

  it('treats an unknown code as internal rather than trusting it', () => {
    const err = toIpcError('get_settings', { code: 'rootAccess', message: 'x' });
    expect(err.code).toBe('internal');
  });

  it('reports Tauri string rejections (missing permission) as internal', () => {
    const err = toIpcError('get_settings', 'get_settings not allowed by ACL');
    expect(err.code).toBe('internal');
    expect(err.message).toContain('not allowed by ACL');
  });
});

describe('ipc wrappers', () => {
  beforeEach(() => {
    fake.calls.length = 0;
    fake.respond = () => Promise.resolve(null);
  });

  it('call the expected command names with typed arguments', async () => {
    await ipc.updateSettings({ theme: 'dark' });
    await ipc.chooseArchiveDirectory();
    expect(fake.calls).toEqual([
      ['update_settings', { update: { theme: 'dark' } }],
      ['choose_archive_directory', undefined],
    ]);
  });

  it('never send a path to the backend', async () => {
    await ipc.chooseArchiveDirectory();
    await ipc.openArchiveDirectory();
    expect(fake.calls).toHaveLength(2);
    for (const [, args] of fake.calls) {
      expect(args).toBeUndefined();
    }
  });

  it('reject with IpcError', async () => {
    // A plain object, as Tauri delivers a serialized command error.
    fake.respond = () => Promise.reject({ code: 'notConfigured', message: 'No archive location.' });
    const err = await ipc.openArchiveDirectory().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(IpcError);
    expect((err as IpcError).code).toBe('notConfigured');
    expect((err as IpcError).message).toBe('No archive location.');
  });
});

describe('backup wrappers', () => {
  beforeEach(() => {
    fake.calls.length = 0;
    fake.respond = () => Promise.resolve(null);
  });

  it('call the right commands with requests wrapped as specified', async () => {
    const onEvent = () => undefined;
    await ipc.listBackups();
    await ipc.addBackupFolder();
    await ipc.forgetBackupFolder('loc-1');
    await ipc.getBackupDetails('b1');
    await ipc.openBackup('b1', null, onEvent);
    await ipc.cancelJob('job-1');
    await ipc.checkExtraction('b2', null, onEvent);
    expect(fake.calls.map(([c]) => c)).toEqual([
      'list_backups',
      'add_backup_folder',
      'forget_backup_folder',
      'get_backup_details',
      'open_backup',
      'cancel_job',
      'check_extraction',
    ]);
    expect(fake.calls[0]?.[1]).toBeUndefined();
    expect(fake.calls[1]?.[1]).toBeUndefined();
    expect(fake.calls[2]?.[1]).toEqual({ request: { locationId: 'loc-1' } });
    expect(fake.calls[3]?.[1]).toEqual({ request: { backupId: 'b1' } });
    expect(fake.calls[5]?.[1]).toEqual({ request: { jobId: 'job-1' } });
    const open = fake.calls[4]?.[1] as { request: unknown; onEvent: { onmessage: unknown } };
    expect(open.request).toEqual({ backupId: 'b1', password: null });
    expect(open.onEvent.onmessage).toBe(onEvent);
    const ext = fake.calls[6]?.[1] as { request: unknown; onEvent: { onmessage: unknown } };
    expect(ext.request).toEqual({ backupId: 'b2', password: null });
    expect(ext.onEvent.onmessage).toBe(onEvent);
  });

  it('never send a path', async () => {
    await ipc.listBackups();
    await ipc.addBackupFolder();
    await ipc.forgetBackupFolder('0123abcd');
    await ipc.getBackupDetails('4567ef01');
    await ipc.openBackup('4567ef01', PASSWORD, () => undefined);
    await ipc.cancelJob('job-3');
    await ipc.checkExtraction('4567ef01', PASSWORD, () => undefined);
    expect(fake.calls).toHaveLength(7);
    for (const [, args] of fake.calls) {
      for (const s of strings(args)) {
        expect(s).not.toMatch(/^([A-Za-z]:[\\/]|\/)/);
      }
    }
  });

  it('put the password only at request.password', async () => {
    await ipc.openBackup('b1', PASSWORD, () => undefined);
    await ipc.checkExtraction('b1', PASSWORD, () => undefined);
    expect(fake.calls.map(([c]) => c)).toEqual(['open_backup', 'check_extraction']);
    for (const [, raw] of fake.calls) {
      const args = raw as { request: { password: string; backupId: string } };
      expect(args.request.password).toBe(PASSWORD);
      const { password: _password, ...rest } = args.request;
      expect(strings(rest)).not.toContain(PASSWORD);
      expect(Object.keys(args).sort()).toEqual(['onEvent', 'request']);
      expect(Object.keys(args.request).sort()).toEqual(['backupId', 'password']);
    }
  });

  it('never copy a raw check_extraction rejection, which could echo the password', async () => {
    fake.respond = () => Promise.reject(`invalid args \`request\`: "${PASSWORD}" is wrong`);
    const err = (await ipc.checkExtraction('b1', PASSWORD, () => undefined).catch((e: unknown) => e)) as IpcError;
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe('internal');
    expect(err.command).toBe('check_extraction');
    expect(JSON.stringify(err)).not.toContain(PASSWORD);
    expect(strings({ ...err, message: err.message, stack: '' })).not.toContain(PASSWORD);
    expect(err.message).not.toContain(PASSWORD);
    // A structured error keeps its code and sentence.
    fake.respond = () => Promise.reject({ code: 'wrongPassword', message: 'That password did not unlock the backup.' });
    const wrong = (await ipc.checkExtraction('b1', PASSWORD, () => undefined).catch((e: unknown) => e)) as IpcError;
    expect(wrong.code).toBe('wrongPassword');
  });

  it('never copy a raw open_backup rejection, which could echo the password', async () => {
    fake.respond = () => Promise.reject(`invalid args \`request\`: "${PASSWORD}" is wrong`);
    const err = (await ipc.openBackup('b1', PASSWORD, () => undefined).catch((e: unknown) => e)) as IpcError;
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe('internal');
    expect(err.message).toBe('The check could not be started.');
    expect(JSON.stringify(err)).not.toContain(PASSWORD);
    expect(strings({ ...err, message: err.message, stack: '' })).not.toContain(PASSWORD);
    expect(err.message).not.toContain(PASSWORD);
  });

  it('keep structured open_backup errors, and treat unknown codes as internal', async () => {
    fake.respond = () => Promise.reject({ code: 'wrongPassword', message: 'That password did not unlock the backup.' });
    const err = (await ipc.openBackup('b1', PASSWORD, () => undefined).catch((e: unknown) => e)) as IpcError;
    expect(err.code).toBe('wrongPassword');
    fake.respond = () => Promise.reject({ code: 'selfDestruct', message: 'x' });
    const unknown = (await ipc.getBackupDetails('b1').catch((e: unknown) => e)) as IpcError;
    expect(unknown.code).toBe('internal');
  });

  it('list every error code once', () => {
    expect(new Set(ERROR_CODES).size).toBe(ERROR_CODES.length);
    expect(ERROR_CODES).toContain('busy');
  });
});
