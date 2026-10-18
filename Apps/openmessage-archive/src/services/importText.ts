// Every sentence and label of the Archive page (M4 §16): the archive list,
// the import gate (preflight), the import summary and the verify result.
// The archive DTOs carry counts, closed kinds, booleans and dates only;
// the words are written here, from exhaustive tables, so a new kind fails
// `npm run typecheck` until it has a label. No sentence shows message
// content, an address, a chat name or a file name: M4 shows none (§0.2).
import type {
  ArchiveEntryDto,
  ArchiveStatusDto,
  ArchiveSummaryDto,
  DamageKindDto,
  FreshReasonDto,
  GateEndingDto,
  ImportOutcomeDto,
  ImportPreflightDto,
  KeyCountDto,
  LineageDto,
  NotImportableDto,
  OrderSourceDto,
  PassDto,
  QualifierCodeDto,
  RefusalDto,
  SessionStateDto,
  StopReasonDto,
  VerifyCountsDto,
  VerifyDepthDto,
  VerifyOutcomeDto,
  CheckStateDto,
  SchemaCheckDto,
} from '@/types';

const nf = new Intl.NumberFormat();

/** Shown for a value the archive or the backup did not record. Never 0. */
export const NOT_RECORDED = 'not recorded';

/** A count; "N or more" when the counters were clamped. */
export function num(n: number, saturated = false): string {
  return saturated ? `${nf.format(n)} or more` : nf.format(n);
}

/** A count, or "not recorded" for `null`. */
export function numOrNotRecorded(n: number | null, saturated = false): string {
  return n === null ? NOT_RECORDED : num(n, saturated);
}

/** "1 message" / "3 messages" (with "or more" when clamped). */
export function plural(n: number, one: string, many: string, saturated = false): string {
  return `${num(n, saturated)} ${n === 1 && !saturated ? one : many}`;
}

/** A Unix-milliseconds date in the user's locale. */
export function formatMs(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return NOT_RECORDED;
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(d);
}

/** A Unix-milliseconds date, or "not recorded" for `null`. */
export function formatMsOrNotRecorded(ms: number | null): string {
  return ms === null ? NOT_RECORDED : formatMs(ms);
}

const MB = 1024 * 1024;
const GB = 1024 * MB;
const one = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });

/** A byte count as "12.3 MB" or "1.2 GB" (binary units, as the guards use). */
export function bytes(n: number, saturated = false): string {
  const more = saturated ? ' or more' : '';
  if (n >= GB) return `${one.format(n / GB)} GB${more}`;
  if (n >= MB) return `${one.format(n / MB)} MB${more}`;
  return `${one.format(n / 1024)} KB${more}`;
}

/** The sentence that M4 archives are plaintext (§13.1). */
export const UNENCRYPTED_NOTE =
  'The archive is stored unencrypted. Anyone who can open your archive folder can read the messages in it.';
export const UNENCRYPTED_FROM_ENCRYPTED =
  'The archive is stored unencrypted. Messages from an encrypted backup will be readable by anyone who can open this folder.';
export const STAGING_NOTE =
  'Until the import finishes, a temporary decrypted copy of the Messages database exists in the archive folder.';
export const RD_WARNING =
  'Recently Deleted messages, and their attachments, were deleted on the phone. Including them copies them into this archive, where they stay.';
export const PROVISIONAL =
  'This iOS version has not been tested. Derived details for this import are provisional.';

// ---------------------------------------------------------------------------
// The archive list
// ---------------------------------------------------------------------------

export const STATUS_LABEL: Record<ArchiveStatusDto, { text: string; cls: string }> = {
  ready: { text: 'Ready', cls: 'text-bg-success' },
  importing: { text: 'Importing', cls: 'text-bg-primary' },
  verifying: { text: 'Verifying', cls: 'text-bg-primary' },
  inUse: { text: 'In use', cls: 'text-bg-warning' },
  needsUpgrade: { text: 'Needs upgrade', cls: 'text-bg-warning' },
  newerVersion: { text: 'Newer version', cls: 'text-bg-secondary' },
  damaged: { text: 'Damaged', cls: 'text-bg-danger' },
  notAnArchive: { text: 'Not an archive', cls: 'text-bg-secondary' },
  refused: { text: 'Refused', cls: 'text-bg-danger' },
};

const DAMAGE: Record<DamageKindDto, string> = {
  schemaMismatch: "The archive's database layout is not the one this version wrote.",
  metaMissing: "The archive's own records are missing.",
  metaInconsistent: "The archive's own records contradict each other.",
  migrationFailed: 'An upgrade of this archive did not finish.',
  unreadable: "The archive's database could not be read.",
  workBudget: "The archive's database is too large or complex to check safely.",
  other: "The archive's database is damaged in a way this version cannot describe.",
};

const REFUSAL: Record<RefusalDto, string> = {
  link: 'This entry is a link, which archives may not be.',
  permissions: 'This folder can be changed by other users of this computer, or belongs to another user.',
  volume: 'This folder is on a drive that archives cannot use.',
  io: 'This folder could not be read.',
};

/** Why an archive in the list cannot be opened, or what it is doing. */
export function statusSentence(e: ArchiveEntryDto): string {
  switch (e.status) {
    case 'ready':
      return e.summary?.newerRules
        ? 'Written by a newer version of OpenMessage Archive: it can be read and verified, but not imported into.'
        : 'Ready.';
    case 'importing':
      return 'OpenMessage Archive is importing into this archive.';
    case 'verifying':
      return 'OpenMessage Archive is verifying this archive.';
    case 'inUse':
      return 'This archive is in use by another program, or by another copy of OpenMessage Archive.';
    case 'needsUpgrade':
      return `This archive was made by an older version (schema ${e.schemaVersion ?? NOT_RECORDED}) and must be upgraded. This version cannot upgrade it yet.`;
    case 'newerVersion':
      return `This archive was made by a newer version (schema ${e.schemaVersion ?? NOT_RECORDED}). Update the app to open it.`;
    case 'damaged':
      return `${e.damage ? DAMAGE[e.damage] : DAMAGE.other} It was left as it is.`;
    case 'notAnArchive':
      return 'This folder is not an OpenMessage Archive archive.';
    case 'refused':
      return e.refusal ? REFUSAL[e.refusal] : REFUSAL.io;
  }
}

/** The archive's name: its label as text, or a fixed stand-in. */
export function archiveName(s: ArchiveSummaryDto | null | undefined): string {
  if (!s) return 'Archive';
  if (s.label !== null) return s.label;
  return s.labelUnreadable ? 'Archive (name unreadable)' : 'Unnamed archive';
}

export const SESSION_STATE: Record<SessionStateDto, string> = {
  running: 'Running',
  completed: 'Completed',
  cancelled: 'Cancelled; can continue',
  failed: 'Failed',
  interrupted: 'Interrupted; can continue',
  superseded: 'Set aside',
  stopped: 'Stopped; can continue',
};

// ---------------------------------------------------------------------------
// The gate (preflight)
// ---------------------------------------------------------------------------

export const PASS_LABEL: Record<PassDto, string> = {
  handles: 'addresses',
  chats: 'chats',
  messages: 'messages',
  chatHandles: 'chat members',
  chatMessages: 'chat links',
  attachments: 'attachments',
  messageAttachments: 'attachment links',
  recentlyDeleted: 'Recently Deleted markers',
  finalize: 'the final step',
};

const FRESH: Record<FreshReasonDto, string> = {
  nothingToContinue: 'This starts a new import.',
  sourceOrBuildChanged:
    'The backup has changed since the interrupted import (or the app was updated); that import will be set aside and everything re-read.',
};

/** Continue or start over (C3.18). */
export function resumeSentences(p: ImportPreflightDto): string[] {
  const r = p.resume;
  if (r.kind === 'continue') {
    return [
      `Continuing import session ${nf.format(r.session)} from ${PASS_LABEL[r.fromPass]}, if you choose import set ${nf.format(r.lineageId)} below.`,
      'Choosing another import set sets the unfinished import aside and starts over.',
    ];
  }
  const out = [FRESH[r.reason]];
  if (r.reason === 'nothingToContinue' && r.supersedes > 0) {
    out.push(`${plural(r.supersedes, 'unfinished import', 'unfinished imports')} will be set aside.`);
  }
  return out;
}

function compared(differs: boolean | null, what: string): string {
  if (differs === null) return `The ${what} could not be compared.`;
  return differs ? `The ${what} differs.` : `The ${what} matches.`;
}

/** The radio label of an existing import set (D80). */
export function lineageLabel(l: LineageDto): string {
  const last = l.lastBackupMs === null ? 'last backup date not recorded' : `last backup on ${formatMs(l.lastBackupMs)}`;
  return `A newer backup of the same phone as import set ${nf.format(l.lineageId)} (${plural(l.sessions, 'import', 'imports')}, ${last})`;
}

/** The evidence for one import set: model and name, each its own sentence. */
export function lineageEvidence(l: LineageDto): string[] {
  return [compared(l.modelDiffers, 'phone model'), compared(l.nameDiffers, 'device name')];
}

export const NEW_LINEAGE_LABEL = 'A different phone or source';
export const FIRST_IMPORT = 'This is the first import into this archive.';
export const NO_IDENTITY_PROOF =
  'OpenMessage Archive cannot prove which phone a backup came from. Messages are merged only within the import set you choose.';

/** Needed and available space, worded as an estimate (§12). */
export function spaceSentences(p: ImportPreflightDto): string[] {
  const s = p.space;
  const sat = p.saturated;
  const out = [`This import needs about ${bytes(s.needed, sat)} of free space (an estimate).`];
  out.push(
    s.available === null
      ? 'The free space in your archive folder could not be read.'
      : `About ${bytes(s.available, sat)} is free.`,
  );
  if (s.filesToCopy > 0) {
    out.push(`${plural(s.filesToCopy, 'attachment file', 'attachment files', sat)} (${bytes(s.bytesToCopy, sat)}) will be copied.`);
  }
  if (s.warns) {
    out.push(
      'The estimate is more than the free space. The import checks the space as it goes and stops safely if it runs out; you can continue after freeing space.',
    );
  }
  return out;
}

function keyed(k: KeyCountDto, one: string, many: string, sat: boolean): string | null {
  if (k.unkeyed === 0) return null;
  const dup =
    k.duplicatedKeys > 0
      ? ` (${plural(k.duplicatedKeys, 'identifier is', 'identifiers are', sat)} used more than once)`
      : '';
  const verb = k.unkeyed === 1 && !sat ? 'has' : 'have';
  return `${plural(k.unkeyed, one, many, sat)} ${verb} no usable or no unique identifier${dup}, and will be imported without being merged.`;
}

/** Overlap with the archive, and duplicated and unkeyed counts. */
export function keySentences(p: ImportPreflightDto): string[] {
  const k = p.keys;
  const sat = p.saturated;
  const out = [`This backup holds ${plural(k.messages.rows, 'message', 'messages', sat)}.`];
  if (p.sameContentCompleted) {
    out.push('This exact backup was already imported. Importing it again only records that it was seen.');
  }
  if (k.messagesAlreadyArchived > 0) {
    out.push(`${plural(k.messagesAlreadyArchived, 'message is', 'messages are', sat)} already in this archive and will be merged.`);
  } else if (p.lineages.length > 0) {
    out.push(
      "None of this backup's messages are in this archive yet. That can be normal, for example when the phone keeps messages for only 30 days.",
    );
  }
  for (const [counts, one, many] of [
    [k.messages, 'message', 'messages'],
    [k.chats, 'chat', 'chats'],
    [k.handles, 'address', 'addresses'],
    [k.attachments, 'attachment', 'attachments'],
  ] as const) {
    const s = keyed(counts, one, many, sat);
    if (s) out.push(s);
  }
  return out;
}

/** Recently Deleted counts (D60). */
export function recentlyDeletedSentences(p: ImportPreflightDto): string[] {
  const r = p.recentlyDeleted;
  const sat = p.saturated;
  if (r.messages === 0) return ['No messages are in Recently Deleted.'];
  const out = [`${plural(r.messages, 'message is', 'messages are', sat)} in Recently Deleted.`];
  if (r.excluded > 0) {
    const att = r.attachmentsExcluded > 0 ? `, nor ${plural(r.attachmentsExcluded, 'attachment', 'attachments', sat)} linked only to them` : '';
    out.push(`${plural(r.excluded, 'of them is', 'of them are', sat)} not imported${att}.`);
  } else {
    out.push('They will be imported with their attachments.');
  }
  if (r.alreadyArchived > 0) {
    out.push(
      `${plural(r.alreadyArchived, 'of them is', 'of them are', sat)} already in this archive: marked as Recently Deleted, but not changed.`,
    );
  }
  return out;
}

const ORDER_SOURCE: Record<OrderSourceDto, string> = {
  statusPlistDate: "the backup's Status.plist",
  infoPlistLastBackupDate: "the backup's Info.plist",
  other: 'another source',
};

/** How the backup's date ranks it (§8.7). */
export function orderSentences(p: ImportPreflightDto): string[] {
  const o = p.order;
  const out: string[] = [];
  if (o.sortsBeforeLatest) out.push('This backup is older than the newest one in this archive.');
  if (!o.dated) {
    out.push('This backup has no usable date, so it ranks below every dated backup.');
  } else if (o.source) {
    out.push(`The backup date comes from ${ORDER_SOURCE[o.source]}.`);
  }
  if (o.outOfRange) out.push("A backup date outside the expected range was ignored.");
  if (o.sourceDiffers) {
    out.push('Its date comes from a different file than the newest import\'s date, so the two may not be comparable.');
  }
  return out;
}

/** One sentence per recorded qualifier (D43, D79). */
export const QUALIFIER: Record<QualifierCodeDto, string> = {
  backupDateOutOfRange: 'A backup date outside the expected range was ignored.',
  walSkipped: 'The backup lists a pending-changes file (-wal) that could not be copied. The newest messages may be missing.',
  walStateUnknown:
    'The Messages database is in WAL mode and the backup lists no pending-changes file (-wal); whether changes were pending is unknown.',
  walFramesInvalid: 'Part of the pending-changes file (-wal) is not valid. The newest messages may be missing.',
  nonUtf8Encoding: 'The Messages database stores text in UTF-16; SQLite converted it while reading.',
  sizeMismatch: "The copied Messages database's size differs from the size the backup records.",
  iosVersionUntested: PROVISIONAL,
  unknownTables: 'The Messages database has tables this version does not understand; they are not imported.',
  unknownColumns: 'The Messages database has columns this version does not understand; they are not imported.',
  wrongCaseColumns: 'Some expected columns have names that differ only in capitalisation and were treated as missing.',
};

/** The qualifiers' sentences, with the provisional one once. */
export function qualifierSentences(p: ImportPreflightDto): string[] {
  const out = p.qualifiers.map((q) => QUALIFIER[q]);
  if (p.provisional && !p.qualifiers.includes('iosVersionUntested')) out.push(PROVISIONAL);
  return out;
}

export const SYNC_FOLDER =
  'Your archive folder appears to be synchronized by a cloud service. It may upload the archive, and the temporary copy of the Messages database, while the import runs.';
export const WINDOWS_ACL =
  'On Windows, the archive takes its permissions from your archive folder. Anyone who can read that folder can read the archive.';

/** The privacy sentences: always the two, and the conditional ones. */
export function privacySentences(p: ImportPreflightDto): string[] {
  const out = [p.encryptedSource ? UNENCRYPTED_FROM_ENCRYPTED : UNENCRYPTED_NOTE, STAGING_NOTE];
  if (p.syncFolderWarning) out.push(SYNC_FOLDER);
  if (p.windowsAclInherited) out.push(WINDOWS_ACL);
  return out;
}

// ---------------------------------------------------------------------------
// How an import ended
// ---------------------------------------------------------------------------

export const STOP_REASON: Record<StopReasonDto, string> = {
  insufficientSpace: 'There was not enough free space. Free some space, then continue the import.',
  sourceChanged: 'The backup changed while it was being imported. Wait until no backup is in progress, then continue.',
  other: 'The import stopped for a reason this version cannot describe. You can continue it.',
};

export const NOT_IMPORTABLE: Record<NotImportableDto, string> = {
  backupNotImportable: 'This backup did not pass validation, so it was not imported.',
  schemaExceedsLimits: "The backup's Messages database is too large or complex to read safely.",
  unsupported: "The backup's Messages database uses a layout this version does not support.",
  damaged: "The backup's Messages database is damaged.",
  incomplete: "The backup's Messages database could not be read completely.",
  overKeyScanCap: 'The backup holds more than 4,194,304 records in one table, more than this version can import.',
  overRecentlyDeletedCap: 'The backup holds more than 4,194,304 Recently Deleted records, more than this version can import.',
  other: 'This backup cannot be imported by this version.',
};

export const GATE_ENDING: Record<GateEndingDto, string> = {
  declined: 'The import was not started. Nothing was written to the archive.',
  cancelled: 'The import was cancelled before it started. Nothing was written to the archive.',
  timedOut: 'The import was not confirmed within two minutes, so it was not started. Nothing was written to the archive.',
};

export const OUTCOME_LABEL: Record<ImportOutcomeDto, string> = {
  completed: 'Import completed.',
  stopped: 'Import stopped; can continue.',
  cancelledAtGate: 'Import not started.',
  notImportable: 'This backup cannot be imported. Nothing was imported.',
};

// ---------------------------------------------------------------------------
// Verify (one sentence per finding kind, §16)
// ---------------------------------------------------------------------------

export const VERIFY_OUTCOME: Record<VerifyOutcomeDto, string> = {
  ok: 'No problems were found.',
  problems: 'Problems were found. Nothing was changed; repair arrives in a later milestone.',
  cancelled: 'The check was cancelled before it finished.',
  incomplete: 'The check could not finish.',
};

export const VERIFY_DEPTH: Record<VerifyDepthDto, string> = {
  quick: 'Quick check',
  full: 'Full check',
};

const SCHEMA: Record<SchemaCheckDto, string | null> = {
  notRun: null,
  ok: null,
  notAnArchive: 'The database is not an OpenMessage Archive archive.',
  versionChanged: "The database's version changed while it was being checked.",
  mismatch: "The database's layout is not the one this version wrote.",
  metaMissing: "The archive's own records are missing.",
  metaInconsistent: "The archive's own records contradict each other.",
  unreadable: 'The database could not be read.',
};

function check(state: CheckStateDto, what: string): string | null {
  switch (state) {
    case 'failed':
      return `${what} failed.`;
    case 'cancelled':
      return `${what} was cancelled.`;
    default:
      return null;
  }
}

/** One sentence per finding kind, with counts. Empty when nothing was found. */
export function verifySentences(c: VerifyCountsDto): string[] {
  const s = c.saturated;
  const out: string[] = [];
  const add = (n: number, one: string, many: string, rest: string) => {
    if (n > 0) out.push(`${plural(n, one, many, s)} ${rest}`);
  };
  const schema = SCHEMA[c.schema];
  if (schema) out.push(schema);
  if (c.rulesAboveHighWater > 0) out.push('The archive records rules newer than this version.');
  const fts = check(c.ftsSecureDelete, "The search index's secure-delete setting check");
  if (fts) out.push(fts);
  const sqlite = check(c.sqliteCheck, "SQLite's integrity check");
  if (sqlite) {
    out.push(c.sqliteCheckMessages > 0 ? `${sqlite.slice(0, -1)} with ${plural(c.sqliteCheckMessages, 'finding', 'findings', s)}.` : sqlite);
  }
  add(c.foreignKeyViolations, 'link between records points', 'links between records point', 'at a missing record.');
  add(c.valueIssueOrphans + c.valueHistoryOrphans, 'value note belongs', 'value notes belong', 'to a missing record.');
  if (c.labelUnreadable) out.push("The archive's name is stored but cannot be read.");
  add(c.sessionsRunning, 'import was', 'imports were', 'interrupted (still marked as running).');
  add(c.blobIntents, 'attachment file placement was', 'attachment file placements were', 'interrupted.');
  const idx = check(c.ftsIntegrity, "The search index's integrity check");
  if (idx) out.push(idx);
  add(c.blobBadName, 'attachment record has', 'attachment records have', 'an invalid file name and were not opened.');
  add(c.blobMissing, 'attachment file is', 'attachment files are', 'missing.');
  add(c.blobNotRegular, 'attachment file is', 'attachment files are', 'not a regular file.');
  add(c.blobLengthMismatch, 'attachment file has', 'attachment files have', 'the wrong size.');
  add(c.blobHashMismatch, 'attachment file does', 'attachment files do', 'not match the recorded checksum.');
  add(c.blobUnreadable, 'attachment file could', 'attachment files could', 'not be read.');
  add(c.storeUnexpected, 'unexpected entry is', 'unexpected entries are', 'in the attachment folder.');
  add(c.storeOrphans, 'file in the attachment folder belongs', 'files in the attachment folder belong', 'to no record.');
  add(c.storeLinks, 'link was', 'links were', 'found in the attachment folder and not followed.');
  if (c.storeWalkTruncated) out.push('The attachment folder holds too many entries; the check stopped listing it.');
  add(c.partialFiles, 'unfinished file was', 'unfinished files were', 'left by an interrupted import.');
  add(c.provenanceUnreadable, 'import record file could', 'import record files could', 'not be read.');
  add(c.provenanceTmpFiles, 'unfinished import record file was', 'unfinished import record files were', 'found.');
  if (c.importsScanTruncated) out.push('The import record folder holds too many entries; the check stopped listing it.');
  add(c.notPrivate, 'archive file or folder can', 'archive files or folders can', 'be read or changed by other users of this computer.');
  add(c.checksNotRun, 'check was', 'checks were', 'not run.');
  return out;
}

/** What the verify looked at (not findings). */
export function verifyScope(c: VerifyCountsDto, depth: VerifyDepthDto): string {
  const s = c.saturated;
  const files = `${plural(c.blobs, 'attachment file', 'attachment files', s)} checked`;
  return depth === 'full'
    ? `${files}; ${plural(c.blobsHashed, 'file', 'files', s)} (${bytes(c.bytesHashed, s)}) read back and compared with the recorded checksums.`
    : `${files} for presence and size. A full check also reads every file back.`;
}
