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
}));
import { IpcError, ipc, toIpcError } from '@/services/ipc';

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
