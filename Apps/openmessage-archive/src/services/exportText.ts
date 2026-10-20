// Every sentence and label of the Export dialog (M6 spec §10). The export
// DTOs carry counts, closed codes and booleans, plus one name
// (`displayName`, rendered by `SafeText`); the words are written here, in
// exhaustive `Record<…>` tables over the DTOs' codes, so a new code fails
// `npm run typecheck` until it has words. No sentence names a path, a
// conversation, a query or content. The job's phases are worded in
// `JobProgress.vue` (one table for every job), and the refusals' sentences
// come from Rust with their codes.
import type {
  AttachmentExclusionsDto,
  ClockInput,
  CsvProfileInput,
  ExportFormatInput,
  ExportOutcomeDto,
  ExportPreviewDto,
  ExportScopeInput,
  ExportSizeBucketDto,
  ExportSummaryDto,
  ExportWarningDto,
  FreeSpaceDto,
  LineEndingsInput,
  PackagingDto,
  PageSizeInput,
  SpaceVerdictDto,
  SyncProviderDto,
  ZoneChoiceInput,
} from '@/types';
import { bytes, num, plural } from './importText';

export type ScopeKind = Exclude<ExportScopeInput['kind'], 'attachment'>;
export type ExportStep = 'what' | 'format' | 'options' | 'review' | 'destination' | 'progress' | 'result';

export const DIALOG_TITLE = 'Export';

/** The steps' legends, which name each step for the reader. */
export const STEP_LEGEND: Record<ExportStep, string> = {
  what: 'What to export',
  format: 'Formats',
  options: 'Options',
  review: 'Review',
  destination: 'Where to save',
  progress: 'Exporting',
  result: 'Export finished',
};

export const SCOPE_LABEL: Record<ScopeKind, string> = {
  conversation: 'This conversation',
  searchResults: 'These search results',
  dateRange: 'A date range',
  archive: 'The whole archive',
};

/** Format order in the dialog (§10 step 2). */
export const FORMAT_ORDER: readonly ExportFormatInput[] = ['pdf', 'html', 'txt', 'csv', 'xlsx', 'json'];

export const FORMAT_TEXT: Record<ExportFormatInput, { name: string; bestFor: string }> = {
  pdf: { name: 'PDF', bestFor: 'Reading and printing.' },
  html: { name: 'HTML', bestFor: 'Browsing in a web browser.' },
  txt: { name: 'Plain text (TXT)', bestFor: 'Plain text anywhere.' },
  csv: { name: 'CSV', bestFor: 'Spreadsheets and data tools.' },
  xlsx: { name: 'Excel workbook (XLSX)', bestFor: 'Excel and LibreOffice.' },
  json: { name: 'JSON', bestFor: 'Other programs, with a documented schema; the most complete format.' },
};

/** Short names for sentences ("as HTML and JSON"). */
export const FORMAT_SHORT: Record<ExportFormatInput, string> = {
  pdf: 'PDF',
  html: 'HTML',
  txt: 'TXT',
  csv: 'CSV',
  xlsx: 'XLSX',
  json: 'JSON',
};

/**
 * Formats this version cannot write, and why. XLSX is refused by the app
 * itself (`exportUnavailable`); the dialog shows it disabled with this
 * reason rather than a control that seems to work.
 */
export const FORMAT_UNAVAILABLE: Partial<Record<ExportFormatInput, string>> = {
  xlsx: 'Not available in this version. Excel workbooks will come in a later update; CSV opens in Excel and LibreOffice meanwhile.',
};

export const EVENTS_ALWAYS =
  'Group and system events are always included in this version; leaving them out is not available yet.';

export const ZONE_CHOICE_LABEL: Record<ZoneChoiceInput, string> = {
  system: "This computer's zone",
  utc: 'UTC',
  named: 'Another zone',
};

export const CLOCK_LABEL: Record<ClockInput, string> = { h23: '24-hour', h12: '12-hour' };
export const LINE_ENDINGS_LABEL: Record<LineEndingsInput, string> = {
  lf: 'Linux and macOS (LF)',
  crlf: 'Windows (CRLF)',
};
export const CSV_PROFILE_LABEL: Record<CsvProfileInput, string> = {
  spreadsheet: 'Protect against formulas (for spreadsheets)',
  dataTools: 'Raw values (for data tools)',
};
export const CSV_DATA_TOOLS_WARNING =
  'Raw values are not protected: a cell that starts with = + - or @ can run as a formula if the file is opened in a spreadsheet.';
export const PAGE_SIZE_LABEL: Record<PageSizeInput, string> = { a4: 'A4', letter: 'US Letter' };

// --- Option labels and notes -------------------------------------------------

export const OPTION_TEXT = {
  namesOnly: 'Names only',
  copyOriginals: 'Copy original files',
  previews: 'Image previews (HTML and PDF)',
  largerPreviews: 'Larger previews',
  keepExtensions: 'Keep original file extensions',
  keepExtensionsNote:
    'Off: a copied file keeps its extension only when it is a passive type that matches its content; every other file gets ".blocked" added. On: every file keeps its own extension, including files that can run programs.',
  history: 'Edit history',
  reactions: 'Reactions',
  events: 'Group and system events',
  revealCodes: 'Include one-time codes',
  revealEarlier: 'Include content captured before an unsend or edit',
  revealNote:
    'Off by default, and off again every time this dialog opens. The exact number of messages is shown in Review.',
  hiddenMarkers: 'Mark hidden characters',
  seconds: 'Show seconds',
  includeIdentifiers: 'Include archive and source identifiers',
  identifiersNote:
    'Archive ids reveal how many messages the archive holds and roughly when they were imported. Off: every id is a number counted within this export.',
  identifiersOnlyFor: 'Only for JSON and CSV.',
  previewsOnlyFor: 'Only for HTML and PDF.',
  keepExtensionsOnlyWithCopies: 'Only when original files are copied.',
  zone: 'Time zone',
  namedZone: 'Zone',
  noZoneList: 'This system does not list its time zones, so only this computer’s zone and UTC can be chosen.',
  noSystemZone: 'This computer’s zone is not known; times can be shown in UTC.',
  clock: 'Clock',
  lineEndings: 'Line endings (TXT)',
  csvProfile: 'CSV',
  pageSize: 'Page size (PDF)',
  change: 'Change…',
  hideOptions: 'Hide options',
} as const;

export const OPTION_GROUP = {
  attachments: 'Attachments',
  content: 'Content',
  sensitive: 'Sensitive content',
  time: 'Time',
  text: 'Text',
  spreadsheet: 'Spreadsheet',
  pdf: 'PDF',
  identifiers: 'Identifiers',
} as const;

// --- Review --------------------------------------------------------------------

export const COUNTING = 'Counting…';
export const PREPARING_ZONE = 'Preparing the time zone…';
export const DISCLOSURE =
  'An export is a plaintext copy. Once saved, the app cannot protect, track or delete it. Anyone who can open the files can read them. It includes your own account address where recorded. Original attachments keep their metadata, which can include location.';
export const KEEP_EXTENSIONS_CONFIRM = 'Files that can run programs will keep extensions that let them run';
export const USE_UTC = 'Use UTC instead';
export const ZONE_FAILED: Record<'timedOut' | 'tooManyTransitions' | 'unsupported', string> = {
  timedOut: 'The time zone information could not be prepared in time.',
  tooManyTransitions: 'This time zone changes its offset too often for the export to record it.',
  unsupported: 'This system cannot describe that time zone.',
};
/** The sentence about the export's zone (§10 step 4). */
export function zoneSentence(zone: ZoneChoiceInput, name: string | null): string {
  if (zone === 'utc' || name === null) return 'Times will be shown in UTC.';
  return `Times will be shown in ${name}. The zone name suggests where you are.`;
}

/**
 * The required confirmation of one reveal, against its exact count
 * (owner Q4, D147).
 */
export function revealCodesConfirm(n: number): string {
  return `This export will include one-time codes from ${plural(n, 'message', 'messages')} that the app normally hides.`;
}
export function revealEarlierConfirm(n: number): string {
  return `This export will include content from ${plural(n, 'message', 'messages')} that the app normally hides: text and attachments captured before the sender unsent or edited them. The phones involved no longer show unsent content.`;
}
export const REVEAL_NONE = 'No message in this export has content of this kind, so there is nothing to confirm.';

export const PACKAGING_TEXT: Record<PackagingDto, string> = {
  singleFile: 'One file.',
  bundle: 'A folder, with README.txt, manifest.json and SHA256SUMS describing it.',
};

/** The counts of a finished preview, as short sentences. */
export function reviewLines(p: ExportPreviewDto, copies: boolean): string[] {
  const sat = p.clamped;
  const out: string[] = [];
  out.push(
    `${plural(p.timelineRows, 'item', 'items', sat)} (${plural(p.distinctMessages, 'message', 'messages', sat)}) from ${plural(p.conversations, 'conversation', 'conversations', sat)}.`,
  );
  if (p.unassigned > 0) {
    out.push(`${plural(p.unassigned, 'message belongs', 'messages belong', sat)} to no conversation and go to a "No conversation" file.`);
  }
  if (p.undatedExcluded > 0) {
    out.push(`${plural(p.undatedExcluded, 'undated item is', 'undated items are', sat)} left out of the date range.`);
  }
  if (p.attachments > 0) {
    const missing = p.attachments - p.attachmentsWithFile;
    let line = `${plural(p.attachments, 'attachment', 'attachments', sat)}`;
    if (copies) {
      line += `, ${plural(p.distinctFiles, 'file', 'files', sat)} to copy (${bytes(p.fileBytes, sat)})`;
    } else {
      line += ', names only';
    }
    if (missing > 0) line += `; ${num(missing, sat)} not available`;
    out.push(`${line}.`);
    if (p.attachmentsWithheldUnsent > 0) {
      out.push(
        `${plural(p.attachmentsWithheldUnsent, 'attachment belongs', 'attachments belong', sat)} to unsent parts and ${p.attachmentsWithheldUnsent === 1 ? 'is' : 'are'} left out unless content captured before an unsend is included.`,
      );
    }
  }
  out.push(`Estimated size: about ${bytes(p.estimatedBytes, sat)}.`);
  if (sat) out.push('Some counts are too large to show exactly.');
  return out;
}

/** The search scope's lines (§3.6, §10). */
export function searchLines(p: ExportPreviewDto): string[] {
  const s = p.search;
  if (!s) return [];
  const out = [`${plural(s.kept, 'match', 'matches', p.clamped)} will be exported.`];
  const hidden = s.excluded.oneTimeCodes + s.excluded.earlierContent + s.excluded.both;
  if (hidden > 0) {
    out.push(
      `${plural(hidden, 'match is', 'matches are', p.clamped)} left out because ${hidden === 1 ? 'it' : 'they'} matched only text this export does not include.`,
    );
  }
  if (s.excluded.notWritten > 0) {
    out.push(
      `${plural(s.excluded.notWritten, 'match is', 'matches are', p.clamped)} left out because ${s.excluded.notWritten === 1 ? 'it' : 'they'} matched only text no export option writes (an earlier version without edit history, or a message that no longer matches).`,
    );
  }
  if (s.capped) out.push('The search stopped at its limit, so more messages may match than are exported.');
  if (!s.coverageComplete) out.push('The search did not cover every message, so more messages may match.');
  return out;
}

/** "These search results: N matches in K conversations" (§10 step 1). */
export function searchScopeLine(matches: number, conversations: number): string {
  return `${SCOPE_LABEL.searchResults}: ${plural(matches, 'match', 'matches')} in ${plural(conversations, 'conversation', 'conversations')}`;
}
export const SEARCH_CAPPED = 'Only the first 10,000 matches are in this set.';
export const ALL_MATCHES = 'Include all matches (searches again during the export)';

// --- Destination ------------------------------------------------------------

export const CHOOSE_DESTINATION = 'Choose where to save…';
export const CHOOSE_ANOTHER = 'Choose another name';
export const CHOOSE_ANOTHER_FOLDER = 'Choose another folder';

export const SYNC_PROVIDER_LABEL: Record<SyncProviderDto, string> = {
  oneDrive: 'OneDrive',
  dropbox: 'Dropbox',
  iCloudDrive: 'iCloud Drive',
  googleDrive: 'Google Drive',
  pCloud: 'pCloud',
  nextcloud: 'Nextcloud',
  ownCloud: 'ownCloud',
  mega: 'MEGA',
  syncCom: 'Sync.com',
  other: 'a sync service',
};

/** Each destination warning (§8.2); `syncFolder` names the provider. */
export function warningText(w: ExportWarningDto, provider: SyncProviderDto | null): string {
  const TEXT: Record<ExportWarningDto, string> = {
    sharedFolder: 'Other accounts on this computer can change this folder while the export is written.',
    syncFolder: `Files in this folder will be uploaded by ${SYNC_PROVIDER_LABEL[provider ?? 'other']}. Cancelling cannot recall files already uploaded.`,
    networkOrVirtual: 'This export will be written to a network or virtual drive.',
    fatVolume: 'Files over 4 GiB cannot be written to this drive; any such attachment will be left out.',
    mayNotFit: 'The export may not fit in the free space on this drive.',
  };
  return TEXT[w];
}

/** Warnings shown but not confirmed (`fatVolume`; §8.2). */
export const INFORMATIONAL_WARNINGS: ReadonlySet<ExportWarningDto> = new Set<ExportWarningDto>(['fatVolume']);

export const FREE_SPACE_LABEL: Record<FreeSpaceDto, string> = {
  under100Mb: 'less than 100 MB',
  under1Gb: 'less than 1 GB',
  under10Gb: 'less than 10 GB',
  under100Gb: 'less than 100 GB',
  atLeast100Gb: '100 GB or more',
};

export const SPACE_VERDICT: Record<SpaceVerdictDto, string> = {
  yes: 'The export should fit.',
  no: 'The export may not fit.',
  unknown: 'Whether the export fits is not known.',
};

export function freeSpaceLine(free: FreeSpaceDto | null, enough: SpaceVerdictDto): string {
  const f = free === null ? 'Free space is not known.' : `Free space: ${FREE_SPACE_LABEL[free]}.`;
  return `${f} ${SPACE_VERDICT[enough]}`;
}

export function leftoversLine(n: number): string {
  return `Remove ${plural(n, 'unfinished export', 'unfinished exports')} found in this folder (older than five minutes and not in use)`;
}

export const REPLACE_PROMPT_PREFIX = 'Replace “';
export const REPLACE_PROMPT_SUFFIX = '”? This cannot be undone.';
export const REPLACE = 'Replace';
export const REPLACE_CONFIRMED = 'The existing file will be replaced.';
export const START = 'Start export';
export const SAVES_AS: Record<PackagingDto, string> = {
  singleFile: 'The export will be saved as',
  bundle: 'The export will be saved in a new folder named',
};

// --- Progress and result --------------------------------------------------------

export const CANCEL_EXPORT = 'Cancel export';
/** Stops the count and discards the draft (§10 step 4). */
export const CANCEL_COUNT = 'Cancel';
export const CANCEL_NOTE = 'Cancelling removes the unfinished export.';
export const ASK_CANCEL = 'The export is still running. Cancel it? Cancelling removes the unfinished export.';
export const KEEP_EXPORTING = 'Keep exporting';
export const SHOW_IN_FOLDER = 'Show in folder';
export const DONE = 'Done';
export const REVIEW_AGAIN = 'Review again';
export const STALE_NOTICE = 'The export changed since it was reviewed. Its counts were taken again; review them before saving.';

export const OUTCOME_TEXT: Record<ExportOutcomeDto, string> = {
  completed: 'Export finished.',
  completedWithExclusions: 'Export finished, with some things left out.',
};

export const SIZE_LABEL: Record<ExportSizeBucketDto, string> = {
  under1Mb: 'under 1 MB',
  under10Mb: 'under 10 MB',
  under100Mb: 'under 100 MB',
  under1Gb: 'under 1 GB',
  under10Gb: 'under 10 GB',
  atLeast10Gb: '10 GB or more',
};

export const EXCLUSION_TEXT: Record<keyof AttachmentExclusionsDto, (n: number) => string> = {
  withheldUnsent: (n) => `${plural(n, 'attachment of an unsent part was', 'attachments of unsent parts were')} left out.`,
  noStoredFile: (n) => `${plural(n, 'attachment has', 'attachments have')} no file in the archive.`,
  checksumMismatch: (n) => `${plural(n, 'attachment file did', 'attachment files did')} not match the archive's checksum and ${n === 1 ? 'was' : 'were'} left out.`,
  tooLargeForVolume: (n) => `${plural(n, 'attachment was', 'attachments were')} too large for this drive.`,
  writeFailed: (n) => `${plural(n, 'attachment file', 'attachment files')} could not be written.`,
};

/** "as HTML and JSON". */
export function formatList(formats: readonly ExportFormatInput[]): string {
  const names = FORMAT_ORDER.filter((f) => formats.includes(f)).map((f) => FORMAT_SHORT[f]);
  if (names.length <= 1) return names.join('');
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/** The result's main sentence (§10 step 7). */
export function resultSentence(s: ExportSummaryDto, formats: readonly ExportFormatInput[]): string {
  const sat = s.clamped;
  const excluded = Object.values(s.attachmentsExcluded).reduce((a, b) => a + b, 0);
  let line = `Exported ${plural(s.items, 'item', 'items', sat)} from ${plural(s.conversations, 'conversation', 'conversations', sat)} as ${formatList(formats)}.`;
  if (s.attachmentsCopied > 0 || excluded > 0) {
    line += ` ${plural(s.attachmentsCopied, 'attachment file', 'attachment files', sat)} copied`;
    if (excluded > 0) {
      line += `; ${num(excluded, sat)} not included${s.bundle ? ' (reasons in manifest.json)' : ''}`;
    }
    line += '.';
  }
  return line;
}

/** The result's short list of exclusions and notes. */
export function resultNotes(s: ExportSummaryDto): string[] {
  const sat = s.clamped;
  const out: string[] = [];
  for (const key of Object.keys(EXCLUSION_TEXT) as (keyof AttachmentExclusionsDto)[]) {
    const n = s.attachmentsExcluded[key];
    if (n > 0) out.push(EXCLUSION_TEXT[key](n));
  }
  if (s.attachmentsBlocked > 0) {
    out.push(`${plural(s.attachmentsBlocked, 'copied file has', 'copied files have')} ".blocked" added to ${s.attachmentsBlocked === 1 ? 'its' : 'their'} name.`);
  }
  if (s.oneTimeCodesMasked > 0) out.push(`${plural(s.oneTimeCodesMasked, 'one-time code was', 'one-time codes were', sat)} hidden.`);
  const withheld = s.earlierTextWithheld + s.editTextWithheld + s.alternativeTextWithheld;
  if (withheld > 0) out.push(`${plural(withheld, 'earlier text was', 'earlier texts were', sat)} left out.`);
  if (s.neutralisedCells > 0) out.push(`${plural(s.neutralisedCells, 'spreadsheet cell was', 'spreadsheet cells were', sat)} protected against formulas.`);
  if (s.timesOutsideZoneWindow > 0) {
    out.push(`${plural(s.timesOutsideZoneWindow, 'time is', 'times are', sat)} shown in UTC because ${s.timesOutsideZoneWindow === 1 ? 'it lies' : 'they lie'} outside the time zone's range.`);
  }
  if (s.undatedExcluded > 0) out.push(`${plural(s.undatedExcluded, 'undated item was', 'undated items were', sat)} left out of the date range.`);
  if (s.searchHitsExcluded !== null && s.searchHitsExcluded > 0) {
    out.push(`${plural(s.searchHitsExcluded, 'search match was', 'search matches were', sat)} left out because ${s.searchHitsExcluded === 1 ? 'it' : 'they'} matched only text this export does not include.`);
  }
  out.push(`${s.bundle ? 'A folder' : 'One file'}, ${SIZE_LABEL[s.size]}.`);
  return out;
}

export function cancelledSentence(stagingRemoved: boolean): string {
  return stagingRemoved
    ? 'Export cancelled. The unfinished export was removed.'
    : 'Export cancelled. Part of the unfinished export could not be removed; the next export into that folder offers to remove it.';
}

export function failedNote(stagingRemoved: boolean): string {
  return stagingRemoved
    ? 'Nothing was saved.'
    : 'Part of the unfinished export could not be removed; the next export into that folder offers to remove it.';
}

/** The options' one-line summary (§10 step 3). */
export function optionsSummary(o: {
  revealCodes: boolean;
  revealEarlier: boolean;
  copies: boolean;
  zone: ZoneChoiceInput;
  clock: ClockInput;
}): string {
  return [
    o.revealCodes ? 'Codes included' : 'Codes hidden',
    o.revealEarlier ? 'earlier content included' : 'earlier content left out',
    o.copies ? 'attachments copied' : 'attachment names only',
    `${o.zone === 'system' ? "this computer's zone" : o.zone === 'utc' ? 'UTC' : 'another zone'}, ${CLOCK_LABEL[o.clock]}`,
  ].join(' · ');
}

export const DATE_FROM = 'From';
export const DATE_THROUGH = 'Through';
export const DATE_HELP = 'Local dates, in the export’s time zone; both days are included.';
export const DATE_INVALID = 'Choose dates from 1900 to 9999, with the end on or after the start.';
export const DATE_ALL = 'All conversations';
export const DATE_THIS = 'This conversation';
export const FORMAT_NEEDED = 'Choose at least one format.';
export const NEXT = 'Next';
export const BACK = 'Back';
export const CLOSE = 'Close';
