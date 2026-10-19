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
import type { SafeId } from '@/types';

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

describe('archive wrappers (M4)', () => {
  beforeEach(() => {
    fake.calls.length = 0;
    fake.respond = () => Promise.resolve(null);
  });

  const ID = `a${'0'.repeat(32)}`;
  const OPTIONS = { includeRecentlyDeleted: false, copyAttachments: true };

  it('call the six commands with requests wrapped as specified', async () => {
    await ipc.listArchives();
    await ipc.createArchive('Family phone');
    await ipc.getArchiveDetails(ID);
    await ipc.startImport('b1', ID, null, OPTIONS, () => undefined);
    await ipc.confirmImport('job-1', true, { kind: 'existing', lineageId: 2 });
    await ipc.confirmImport('job-1', false, null);
    await ipc.verifyArchive(ID, 'quick', () => undefined);
    expect(fake.calls.map(([c]) => c)).toEqual([
      'list_archives',
      'create_archive',
      'get_archive_details',
      'start_import',
      'confirm_import',
      'confirm_import',
      'verify_archive',
    ]);
    const args = fake.calls.map(([, a]) => a);
    expect(args[1]).toEqual({ request: { label: 'Family phone' } });
    expect(args[2]).toEqual({ request: { archiveId: ID } });
    const start = args[3] as { request: unknown; onEvent: unknown };
    expect(start.request).toEqual({ backupId: 'b1', archiveId: ID, password: null, options: OPTIONS });
    expect(start.onEvent).toBeDefined();
    expect(args[4]).toEqual({
      request: { jobId: 'job-1', proceed: true, lineage: { kind: 'existing', lineageId: 2 } },
    });
    expect(args[6]).toMatchObject({ request: { archiveId: ID, depth: 'quick' } });
  });

  it('put the password only at request.password, and never copy a raw start_import rejection', async () => {
    await ipc.startImport('b1', ID, PASSWORD, OPTIONS, () => undefined);
    const args = fake.calls.map(([, a]) => a)[0];
    expect(strings(args).filter((s) => s === PASSWORD)).toHaveLength(1);
    expect((args as { request: { password: string } }).request.password).toBe(PASSWORD);
    fake.respond = () => Promise.reject(`invalid args: ${PASSWORD}`);
    const err = await ipc.startImport('b1', ID, PASSWORD, OPTIONS, () => undefined).catch((e) => e);
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe('internal');
    expect(err.message).not.toContain(PASSWORD);
  });

  it('keep the new archive error codes', async () => {
    fake.respond = () => Promise.reject({ code: 'jobNotWaiting', message: 'Not waiting.' });
    const err = await ipc.confirmImport('job-1', false, null).catch((e) => e);
    expect(err.code).toBe('jobNotWaiting');
    for (const code of ['archiveLocked', 'insufficientSpace', 'invalidInput'] as const) {
      expect(ERROR_CODES).toContain(code);
    }
  });
});

describe('viewer wrappers (M5)', () => {
  beforeEach(() => {
    fake.calls.length = 0;
    fake.respond = () => Promise.resolve(null);
  });

  const ID = `a${'0'.repeat(32)}`;
  // Ids are SafeIds the backend gave; tests stand in for them.
  const sid = (n: number) => n as unknown as SafeId;
  const QUERY = 'synthetic-query-sentinel';

  it('call the 14 commands with requests wrapped as specified', async () => {
    await ipc.openViewer(ID);
    await ipc.listConversations(3, 'recent', null);
    await ipc.listConversations(3, 'name', QUERY);
    await ipc.getConversations(3, [sid(1), sid(2)]);
    await ipc.getThreadPage({ viewer: 3, chatId: sid(1), anchor: { kind: 'newest' }, direction: 'older', limit: 50 });
    await ipc.getReplyThread({ viewer: 3, messageId: sid(9) });
    await ipc.getMessageDetail({ viewer: 3, messageId: sid(9), reactionPage: 0 });
    await ipc.getMessageText({ viewer: 3, bodyId: sid(4), fromUtf16: 0 });
    await ipc.getDayCounts({ viewer: 3, chatId: sid(1), boundsUtcMs: [0, 86_400_000] });
    await ipc.seekDate({ viewer: 3, chatId: sid(1), utcMs: 0, dir: 'onOrAfter' });
    await ipc.searchMessages(
      { viewer: 3, query: QUERY, mode: 'auto', filters: {}, includeEarlier: false, includeEvents: false },
      () => undefined,
    );
    await ipc.getSearchResults({ viewer: 3, searchId: 1, offset: 0, limit: 50 });
    await ipc.cancelSearch({ viewer: 3, searchId: 1 });
    await ipc.inspectAttachment({ viewer: 3, attachmentId: sid(7), forPlayback: false });
    await ipc.closeViewer(3);
    expect(fake.calls.map(([c]) => c)).toEqual([
      'open_viewer',
      'list_conversations',
      'list_conversations',
      'get_conversations',
      'get_thread_page',
      'get_reply_thread',
      'get_message_detail',
      'get_message_text',
      'get_day_counts',
      'seek_date',
      'search_messages',
      'get_search_results',
      'cancel_search',
      'inspect_attachment',
      'close_viewer',
    ]);
    const args = fake.calls.map(([, a]) => a);
    expect(args[0]).toEqual({ request: { archiveId: ID } });
    expect(args[1]).toEqual({ request: { viewer: 3, sort: 'recent' } });
    expect(args[2]).toEqual({ request: { viewer: 3, sort: 'name', filter: QUERY } });
    const search = args[10] as { request: { query: string }; onProgress: unknown };
    expect(search.request.query).toBe(QUERY);
    expect(search.onProgress).toBeDefined();
    expect(args[14]).toEqual({ request: { viewer: 3 } });
    for (const a of args) {
      for (const s of strings(a)) {
        expect(s).not.toMatch(/^([A-Za-z]:[\\/]|\/)/);
      }
    }
  });

  it('never copy a raw rejection of the free-text commands', async () => {
    fake.respond = () => Promise.reject(`invalid args \`request\`: "${QUERY}" is wrong`);
    for (const call of [
      () => ipc.listConversations(3, 'name', QUERY),
      () =>
        ipc.searchMessages(
          { viewer: 3, query: QUERY, mode: 'exact', filters: {}, includeEarlier: false, includeEvents: false },
          () => undefined,
        ),
    ]) {
      const err = (await call().catch((e: unknown) => e)) as IpcError;
      expect(err).toBeInstanceOf(IpcError);
      expect(err.code).toBe('internal');
      expect(err.message).not.toContain(QUERY);
      expect(JSON.stringify(err)).not.toContain(QUERY);
    }
    fake.respond = () => Promise.reject({ code: 'searchSuperseded', message: 'A newer search replaced this one.' });
    const superseded = (await ipc
      .searchMessages(
        { viewer: 3, query: QUERY, mode: 'auto', filters: {}, includeEarlier: false, includeEvents: false },
        () => undefined,
      )
      .catch((e: unknown) => e)) as IpcError;
    expect(superseded.code).toBe('searchSuperseded');
  });

  it('know the viewer error codes', () => {
    for (const code of [
      'viewerClosed',
      'queryTooBroad',
      'invalidQuery',
      'searchUnavailable',
      'searchSuperseded',
      'messageNotFound',
      'anchorNotFound',
      'attachmentUnavailable',
    ] as const) {
      expect(ERROR_CODES).toContain(code);
    }
  });
});
