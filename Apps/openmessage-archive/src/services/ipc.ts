// The frontend's only door into the backend. Every command the Rust side
// exposes has exactly one typed wrapper here, so components never call
// `invoke` with a string they could misspell, and every failure arrives as
// an `IpcError` with a stable code.
import { Channel, invoke } from '@tauri-apps/api/core';
import type {
  AddBackupFolderResult,
  AppInfo,
  ArchiveDetailsDto,
  ArchiveListingDto,
  ArchiveSummaryDto,
  ImportOptionsInput,
  LineageInput,
  VerifyDepthDto,
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
  AttachmentInspectionDto,
  CancelSearchRequest,
  ConversationIndexDto,
  ConversationRowsDto,
  ConversationSortInput,
  DayCountsDto,
  GetDayCountsRequest,
  GetMessageDetailRequest,
  GetMessageTextRequest,
  GetReplyThreadRequest,
  GetSearchResultsRequest,
  GetThreadPageRequest,
  InspectAttachmentRequest,
  ListConversationsRequest,
  MessageDetailDto,
  SafeId,
  ScanProgressDto,
  SearchHitsDto,
  SearchMessagesRequest,
  SearchStartedDto,
  SeekDateRequest,
  SeekResultDto,
  TextChunkDto,
  ThreadPageDto,
  ViewerSessionDto,
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
  // Milestone 4: archives.
  'archiveLocked',
  'archiveNewer',
  'archiveNeedsUpgrade',
  'archiveNewerRules',
  'archiveDamaged',
  'notAnArchive',
  'archiveNotFound',
  'archivePermissions',
  'lockUnreliable',
  'networkVolume',
  'virtualisedVolume',
  'insufficientSpace',
  'sourceChanged',
  'sourceTooLarge',
  'jobNotWaiting',
  'invalidInput',
  // Milestone 5: the viewer.
  'viewerClosed',
  'queryTooBroad',
  'invalidQuery',
  'searchUnavailable',
  'searchSuperseded',
  'messageNotFound',
  'anchorNotFound',
  'attachmentUnavailable',
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
 * Like `call`, for the commands that carry a secret: a rejection that
 * is not a structured IpcError never has its raw text copied anywhere,
 * because a deserialization error could echo the arguments.
 */
async function callRedacted<T>(
  command: string,
  args: Record<string, unknown>,
  fallback = 'The check could not be started.',
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    throw structured(command, raw) ?? new IpcError(command, 'internal', fallback);
  }
}

const VIEWER_FALLBACK = 'The viewer request could not be completed.';

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
  /** Carries a secret. Rejections never echo raw text. */
  openBackup: (backupId: string, password: string | null, onEvent: (e: JobEvent) => void) => {
    const channel = new Channel<JobEvent>();
    channel.onmessage = onEvent;
    return callRedacted<JobStarted>('open_backup', {
      request: { backupId, password },
      onEvent: channel,
    });
  },
  /**
   * Validate, then check that the Messages database can be read (counts
   * only). Carries a secret. The terminal event is a summary; the report is
   * fetched with getBackupDetails.
   */
  checkExtraction: (backupId: string, password: string | null, onEvent: (e: JobEvent) => void) => {
    const channel = new Channel<JobEvent>();
    channel.onmessage = onEvent;
    return callRedacted<JobStarted>('check_extraction', {
      request: { backupId, password },
      onEvent: channel,
    });
  },
  cancelJob: (jobId: string) => call<boolean>('cancel_job', { request: { jobId } }),

  // Milestone 4: archives. Counts and states only; the label is the one
  // free text and must be rendered with interpolation, never v-html.
  listArchives: () => call<ArchiveListingDto>('list_archives'),
  /** An empty or null label means none. The archive is stored unencrypted. */
  createArchive: (label: string | null) =>
    call<ArchiveSummaryDto>('create_archive', { request: { label } }),
  getArchiveDetails: (archiveId: string) =>
    call<ArchiveDetailsDto>('get_archive_details', { request: { archiveId } }),
  /**
   * Import a backup into an archive. Carries a secret. The job stops at the
   * gate (an `importPreflight` event, with the preflight in
   * getArchiveDetails) until confirmImport, a cancel or two minutes pass.
   */
  startImport: (
    backupId: string,
    archiveId: string,
    password: string | null,
    options: ImportOptionsInput,
    onEvent: (e: JobEvent) => void,
  ) => {
    const channel = new Channel<JobEvent>();
    channel.onmessage = onEvent;
    return callRedacted<JobStarted>('start_import', {
      request: { backupId, archiveId, password, options },
      onEvent: channel,
    });
  },
  /** Answer the gate. `lineage` is required when `proceed` is true. */
  confirmImport: (jobId: string, proceed: boolean, lineage: LineageInput | null) =>
    call<null>('confirm_import', { request: { jobId, proceed, lineage } }),
  /** Verify an archive (report only); the report comes with getArchiveDetails. */
  verifyArchive: (archiveId: string, depth: VerifyDepthDto, onEvent: (e: JobEvent) => void) => {
    const channel = new Channel<JobEvent>();
    channel.onmessage = onEvent;
    return call<JobStarted>('verify_archive', {
      request: { archiveId, depth },
      onEvent: channel,
    });
  },

  // Milestone 5: the viewer. Content arrives as `UntrustedText` and must be
  // rendered by interpolation only, never with v-html or in an attribute.
  // Ids are `SafeId`s the backend gave; every request names the session's
  // generation (`viewer`), and a stale one rejects with `viewerClosed`.
  /** Open an archive in the viewer (closes any open session first). */
  openViewer: (archiveId: string) =>
    call<ViewerSessionDto>('open_viewer', { request: { archiveId } }),
  closeViewer: (viewer: number) => call<null>('close_viewer', { request: { viewer } }),
  /** Carries free text (the filter): rejections never echo raw text. */
  listConversations: (viewer: number, sort: ConversationSortInput, filter: string | null) => {
    const request: ListConversationsRequest = filter ? { viewer, sort, filter } : { viewer, sort };
    return callRedacted<ConversationIndexDto>('list_conversations', { request }, VIEWER_FALLBACK);
  },
  /** At most 200 ids. */
  getConversations: (viewer: number, ids: SafeId[]) =>
    call<ConversationRowsDto>('get_conversations', { request: { viewer, ids } }),
  getThreadPage: (request: GetThreadPageRequest) =>
    call<ThreadPageDto>('get_thread_page', { request }),
  getReplyThread: (request: GetReplyThreadRequest) =>
    call<ThreadPageDto>('get_reply_thread', { request }),
  getMessageDetail: (request: GetMessageDetailRequest) =>
    call<MessageDetailDto>('get_message_detail', { request }),
  getMessageText: (request: GetMessageTextRequest) =>
    call<TextChunkDto>('get_message_text', { request }),
  getDayCounts: (request: GetDayCountsRequest) =>
    call<DayCountsDto>('get_day_counts', { request }),
  seekDate: (request: SeekDateRequest) => call<SeekResultDto>('seek_date', { request }),
  /**
   * Search. Carries free text (the query): rejections never echo raw text.
   * An exact-text scan reports progress through `onProgress`. A superseded
   * search rejects with `searchSuperseded`, which the UI drops silently.
   */
  searchMessages: (request: SearchMessagesRequest, onProgress: (p: ScanProgressDto) => void) => {
    const channel = new Channel<ScanProgressDto>();
    channel.onmessage = onProgress;
    return callRedacted<SearchStartedDto>(
      'search_messages',
      { request, onProgress: channel },
      VIEWER_FALLBACK,
    );
  },
  /** At most 50 hits. */
  getSearchResults: (request: GetSearchResultsRequest) =>
    call<SearchHitsDto>('get_search_results', { request }),
  cancelSearch: (request: CancelSearchRequest) => call<null>('cancel_search', { request }),
  /** Only for attachments a page or detail of this session showed. */
  inspectAttachment: (request: InspectAttachmentRequest) =>
    call<AttachmentInspectionDto>('inspect_attachment', { request }),
};

export type Ipc = typeof ipc;
