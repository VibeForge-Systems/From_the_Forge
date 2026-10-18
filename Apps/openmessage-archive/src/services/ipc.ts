// The frontend's only door into the backend. Every command the Rust side
// exposes has exactly one typed wrapper here, so components never call
// `invoke` with a string they could misspell, and every failure arrives as
// an `IpcError` with a stable code.
import { Channel, invoke } from '@tauri-apps/api/core';
import type {
  AddBackupFolderResult,
  AppInfo,
  ArchiveLocationChoice,
  ArchiveLocationStatus,
  BackupDetails,
  BackupList,
  ErrorCode,
  ForgetBackupFolderResult,
  JobEvent,
  JobStarted,
  SettingsSnapshot,
  SettingsUpdate,
} from '@/types';

/**
 * Every error code the backend can send. `src/types/error-codes-exhaustive.ts`
 * makes `npm run typecheck` fail if a code is missing here.
 */
export const ERROR_CODES = [
  'settingsReadOnly',
  'io',
  'notConfigured',
  'platform',
  'internal',
  'backupNotFound',
  'notABackup',
  'unsupportedBackup',
  'incompleteBackup',
  'malformedBackup',
  'passwordRequired',
  'passwordTooLong',
  'wrongPassword',
  'busy',
] as const satisfies readonly ErrorCode[];

export class IpcError extends Error {
  readonly code: ErrorCode;
  readonly command: string;

  constructor(command: string, code: ErrorCode, message: string) {
    super(message);
    this.name = 'IpcError';
    this.command = command;
    this.code = code;
  }
}

function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === 'string' && (ERROR_CODES as readonly string[]).includes(value);
}

/** A structured `{ code, message }` rejection, or null. */
function structured(command: string, raw: unknown): IpcError | null {
  if (raw instanceof IpcError) return raw;
  if (typeof raw === 'object' && raw !== null) {
    const { code, message } = raw as { code?: unknown; message?: unknown };
    if (isErrorCode(code) && typeof message === 'string') {
      return new IpcError(command, code, message);
    }
  }
  return null;
}

/**
 * Convert whatever `invoke` rejected with into an IpcError. Commands reject
 * with `{ code, message }`; Tauri itself rejects with a plain string when a
 * command is missing or not permitted by the capability, which is reported
 * as an internal error because it can only be a build or wiring mistake.
 */
export function toIpcError(command: string, raw: unknown): IpcError {
  const known = structured(command, raw);
  if (known) return known;
  const detail = typeof raw === 'string' ? raw : raw instanceof Error ? raw.message : 'unknown';
  return new IpcError(command, 'internal', `The “${command}” request failed: ${detail}`);
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    throw toIpcError(command, raw);
  }
}

/**
 * Like `call`, for the one command that carries a secret: a rejection that
 * is not a structured IpcError never has its raw text copied anywhere,
 * because a deserialization error could echo the arguments.
 */
async function callRedacted<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    throw structured(command, raw) ?? new IpcError(command, 'internal', 'The check could not be started.');
  }
}

export const ipc = {
  getSettings: () => call<SettingsSnapshot>('get_settings'),
  updateSettings: (update: SettingsUpdate) =>
    call<SettingsSnapshot>('update_settings', { update }),
  chooseArchiveDirectory: () => call<ArchiveLocationChoice | null>('choose_archive_directory'),
  getArchiveLocationStatus: () => call<ArchiveLocationStatus>('get_archive_location_status'),
  clearArchiveDirectory: () => call<SettingsSnapshot>('clear_archive_directory'),
  openArchiveDirectory: () => call<null>('open_archive_directory'),
  getAppInfo: () => call<AppInfo>('get_app_info'),

  listBackups: () => call<BackupList>('list_backups'),
  addBackupFolder: () => call<AddBackupFolderResult | null>('add_backup_folder'),
  forgetBackupFolder: (locationId: string) =>
    call<ForgetBackupFolderResult>('forget_backup_folder', { request: { locationId } }),
  getBackupDetails: (backupId: string) =>
    call<BackupDetails>('get_backup_details', { request: { backupId } }),
  /** The only call that carries a secret. Rejections never echo raw text. */
  openBackup: (backupId: string, password: string | null, onEvent: (e: JobEvent) => void) => {
    const channel = new Channel<JobEvent>();
    channel.onmessage = onEvent;
    return callRedacted<JobStarted>('open_backup', {
      request: { backupId, password },
      onEvent: channel,
    });
  },
  cancelJob: (jobId: string) => call<boolean>('cancel_job', { request: { jobId } }),
};

export type Ipc = typeof ipc;
