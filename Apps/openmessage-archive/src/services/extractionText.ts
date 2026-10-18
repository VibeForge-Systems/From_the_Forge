// Every sentence and label of the extraction check's report (M3 §17.4).
// The report carries only counts and closed kinds; the words are written
// here, from exhaustive tables, so a new kind fails `npm run typecheck`
// until it has a label.
import type {
  DamageReasonDto,
  ExtractionReportDto,
  FailureKindDto,
  IntegrityDto,
  KnownTableDto,
  OpenModeDto,
  QualifierDto,
  UnsupportedReasonDto,
  WalOutcomeDto,
} from '@/types';

const nf = new Intl.NumberFormat();

/** "1 table" / "3 tables". */
function plural(n: number, one: string, many: string): string {
  return `${nf.format(n)} ${n === 1 ? one : many}`;
}

/** Shown for a counter the database does not record. Never 0. */
export const NOT_RECORDED = 'not recorded by this database';

/** A count, or the "not recorded" text for `null`. */
export function count(n: number | null, saturated = false): string {
  if (n === null) return NOT_RECORDED;
  return saturated ? `${nf.format(n)} or more` : nf.format(n);
}

export const NEWEST_MAY_BE_MISSING = 'The newest messages may be missing.';

/** One sentence per qualifier (D43). */
export function qualifierSentence(q: QualifierDto): string {
  switch (q.kind) {
    case 'iosVersionUntested':
      return 'This iOS version has not been tested.';
    case 'unknownTables':
      return `${plural(q.count, 'table', 'tables')} this version does not understand.`;
    case 'unknownColumns':
      return `${plural(q.count, 'column', 'columns')} this version does not understand.`;
    case 'wrongCaseColumns':
      return `${plural(q.count, 'expected column has', 'expected columns have')} a name that differs only in capitalisation and ${q.count === 1 ? 'was' : 'were'} treated as missing.`;
    case 'walSkipped':
      return `The backup lists a pending-changes file (-wal) that could not be copied. ${NEWEST_MAY_BE_MISSING}`;
    case 'walStateUnknown':
      return `The database expects a pending-changes file (-wal), but the backup lists none. ${NEWEST_MAY_BE_MISSING}`;
    case 'walFramesInvalid':
      return `Part of the pending-changes file (-wal) is not valid. ${NEWEST_MAY_BE_MISSING}`;
    case 'sourceChanged':
      return 'The backup changed during the check; these counts may come from an inconsistent copy.';
    case 'aggregatesIncomplete':
      return 'Some summary counts could not be computed.';
    case 'valuesWithheld':
      return 'Some values were too large or unreadable and were not counted.';
    case 'nonUtf8Encoding':
      return 'The database stores text in UTF-16; SQLite converted it while reading.';
    case 'sizeMismatch':
      return "The copied database's size differs from the size the backup records.";
    case 'other':
      return 'Another limit applies that this version cannot describe.';
  }
}

/** Qualifiers after which the WAL sentence applies. */
export function newestMayBeMissing(r: ExtractionReportDto): boolean {
  return r.qualifiers.some(
    (q) => q.kind === 'walSkipped' || q.kind === 'walStateUnknown' || q.kind === 'walFramesInvalid',
  );
}

/** Our own names for the tables the check knows. */
export const TABLE_NAME: Record<KnownTableDto, string> = {
  message: 'message',
  handle: 'handle',
  chat: 'chat',
  chatMessageJoin: 'chat_message_join',
  chatHandleJoin: 'chat_handle_join',
  messageAttachmentJoin: 'message_attachment_join',
  attachment: 'attachment',
  chatRecoverableMessageJoin: 'chat_recoverable_message_join',
  recoverableMessagePart: 'recoverable_message_part',
  deletedMessages: 'deleted_messages',
  syncDeletedMessages: 'sync_deleted_messages',
  syncDeletedChats: 'sync_deleted_chats',
  syncDeletedAttachments: 'sync_deleted_attachments',
  unsyncedRemovedRecoverableMessages: 'unsynced_removed_recoverable_messages',
  sqliteDatabaseProperties: '_SqliteDatabaseProperties',
  other: 'a table this version cannot name',
};

export function unsupportedSentence(r: UnsupportedReasonDto): string {
  switch (r.kind) {
    case 'missingTable':
      return `The ${TABLE_NAME[r.table]} table is missing.`;
    case 'notOrdinaryTable':
      return `The name ${TABLE_NAME[r.table]} is used by something other than an ordinary table.`;
    case 'missingRequiredColumn':
      return `A required column is missing: ${r.column}.`;
    case 'rowidShape':
      return `The rows of the ${TABLE_NAME[r.table]} table cannot be read safely in order.`;
    case 'virtualTablePresent':
      return 'It contains a virtual table, which this version does not read.';
    case 'schemaExceedsLimits':
      return "Its layout is larger than this app's safety limits allow.";
    case 'other':
      return 'It has another layout problem this version cannot describe.';
  }
}

export function damageSentence(d: DamageReasonDto | null): string {
  if (d?.kind === 'rowsNotStreamed') {
    return `This Messages database is damaged: reading the ${TABLE_NAME[d.table]} table returned a different number of rows than it holds. No counts are shown.`;
  }
  return 'This Messages database is damaged (its integrity check failed). Nothing was read from it.';
}

export const OPEN_MODE_LABEL: Record<OpenModeDto, string> = {
  readOnly: 'Read-only',
  readWriteQueryOnly: 'Read-write, queries only (on the temporary copy)',
};

export const WAL_LABEL: Record<WalOutcomeDto, string> = {
  notListed: 'No pending-changes file (-wal) is listed',
  staged: 'The pending-changes file (-wal) was copied with the database',
  skipped: 'The pending-changes file (-wal) is listed but could not be copied',
  other: 'Not described by this version',
};

export const INTEGRITY_LABEL: Record<IntegrityDto, string> = {
  ok: "Passed SQLite's integrity check",
  problems: "Failed SQLite's integrity check",
  overBudget: 'Did not finish within its limit',
  other: 'Not described by this version',
};

/** One label per problem kind, as a count's noun phrase. */
export const FAILURE_LABEL: Record<FailureKindDto, string> = {
  tsUnsupportedHeader: 'Formatted bodies with an unsupported header',
  tsTruncated: 'Formatted bodies that end early',
  tsUnexpectedTag: 'Formatted bodies with an unexpected marker',
  tsBadReference: 'Formatted bodies with an invalid reference',
  tsUnsupportedType: 'Formatted bodies with an unsupported value type',
  tsTooDeep: 'Formatted bodies nested too deeply',
  tsBudget: 'Formatted bodies over a processing limit',
  tsMaterialisationBudget: 'Formatted bodies too large to decode',
  tsTrailingBytes: 'Formatted bodies with extra bytes at the end',
  tsNotAttributedString: 'Formatted bodies that are not formatted text',
  tsInvalidUtf8: 'Formatted bodies whose text is not valid UTF-8',
  tsRunLengthMismatch: 'Formatted bodies whose formatting ends before the text',
  tsRunOverflow: 'Formatted bodies whose formatting runs past the text',
  tsRunSplitsSurrogate: 'Formatted bodies whose formatting splits a character',
  tsBadRunIndex: 'Formatted bodies with unreadable formatting',
  summaryTooLarge: 'Edit and unsend records over the size limit',
  summaryPlistInvalid: 'Edit and unsend records that are not valid property lists',
  summaryPlistBudget: 'Edit and unsend records over a processing limit',
  summaryNotDictionary: 'Edit and unsend records of the wrong shape',
  summaryShape: 'Edit and unsend records with a field of the wrong type',
  summaryUnknownKeys: 'Unknown keys in edit and unsend records',
  valueOversized: 'Values over their size limit (not read)',
  typeMismatch: 'Values of an unexpected type',
  realInIntegerColumn: 'Decimal numbers where whole numbers were expected',
  boolOutOfRange: 'Yes/no values other than 0 or 1',
  invalidUtf8Text: 'Text values that are not valid UTF-8',
  blobInTextColumn: 'Binary values where text was expected',
  textInBlobColumn: 'Text values where binary data was expected',
  plistInvalid: 'Property lists that are not valid',
  plistOverBudget: 'Property lists over a processing limit',
  plistNotDictionary: 'Property lists of the wrong shape',
  dateAmbiguous: 'Dates in no known unit',
  dateNegative: 'Negative dates',
  assocGuidUnparsed: 'Reaction or sticker targets that could not be read',
  unknownAttributeKey: 'Unknown formatting attributes',
  unknownAttributeClass: 'Formatting values of an unknown kind',
  unreadableAttributeValue: 'Formatting values that could not be read',
  orphanJoinRow: 'Link rows that point to a missing row',
  attachmentAmbiguous: 'Attachments listed more than once in the file index',
  attachmentInvalidEntry: 'Attachments with an invalid file-index entry',
  attachmentUnusualName: 'Attachment locations too unusual to look up',
  attachmentBlobMissing: 'Attachments whose file is missing from the backup',
  attachmentBlobUnreadable: 'Attachments whose file could not be examined',
  attachmentKeyProblem: "Attachments whose key is missing or could not be unwrapped",
  aggregateOverBudget: 'Summary counts that ran over their limit',
};

/** The outcome's headline sentence. */
export function outcomeSentence(r: ExtractionReportDto): string {
  switch (r.outcome) {
    case 'readable':
      return 'Messages can be read from this backup.';
    case 'readableWithQualifiers':
      return 'Messages can be read from this backup, with these limits:';
    case 'damaged':
      return damageSentence(r.damageReason);
    case 'unsupported':
      return 'This Messages database has a layout this version does not support:';
    case 'incomplete':
      return 'The integrity check did not finish within its limit.';
    case 'skippedNotImportable':
      return 'Not run: validation found errors.';
  }
}
