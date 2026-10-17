// The frontend's only door into the backend. Every command the Rust side
// exposes has exactly one typed wrapper here, so components never call
// `invoke` with a string they could misspell, and every failure arrives as
// an `IpcError` with a stable code.
import { invoke } from '@tauri-apps/api/core';
import type {
  AppInfo,
  ArchiveLocationChoice,
  ArchiveLocationStatus,
  ErrorCode,
  SettingsSnapshot,
  SettingsUpdate,
} from '@/types';

const ERROR_CODES: readonly ErrorCode[] = [
  'settingsReadOnly',
  'io',
  'notConfigured',
  'platform',
  'internal',
];

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

/**
 * Convert whatever `invoke` rejected with into an IpcError. Commands reject
 * with `{ code, message }`; Tauri itself rejects with a plain string when a
 * command is missing or not permitted by the capability, which is reported
 * as an internal error because it can only be a build or wiring mistake.
 */
export function toIpcError(command: string, raw: unknown): IpcError {
  if (raw instanceof IpcError) return raw;
  if (typeof raw === 'object' && raw !== null) {
    const { code, message } = raw as { code?: unknown; message?: unknown };
    if (isErrorCode(code) && typeof message === 'string') {
      return new IpcError(command, code, message);
    }
  }
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

export const ipc = {
  getSettings: () => call<SettingsSnapshot>('get_settings'),
  updateSettings: (update: SettingsUpdate) =>
    call<SettingsSnapshot>('update_settings', { update }),
  chooseArchiveDirectory: () => call<ArchiveLocationChoice | null>('choose_archive_directory'),
  getArchiveLocationStatus: () => call<ArchiveLocationStatus>('get_archive_location_status'),
  clearArchiveDirectory: () => call<SettingsSnapshot>('clear_archive_directory'),
  openArchiveDirectory: () => call<null>('open_archive_directory'),
  getAppInfo: () => call<AppInfo>('get_app_info'),
};

export type Ipc = typeof ipc;
