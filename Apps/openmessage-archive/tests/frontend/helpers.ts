import type {
  ArchiveDetailsDto,
  ArchiveEntryDto,
  ArchiveListingDto,
  ArchiveSummaryDto,
  EntityCountsDto,
  ImportCountsDto,
  ImportPreflightDto,
  LastImportDto,
  LinkCountsDto,
  VerifyCountsDto,
  VerifySummaryDto,
  ArchiveLocationStatus,
  BackupDetails,
  BackupList,
  BackupLocation,
  BackupSummary,
  ExtractionReportDto,
  LocationCheck,
  Settings,
  SettingsSnapshot,
  ValidationReportDto,
} from '@/types';

export function snapshot(overrides: Partial<Settings> = {}, readOnly = false): SettingsSnapshot {
  return {
    settings: {
      schemaVersion: 1,
      archiveDirectory: null,
      theme: 'system',
      layout: { leftPanelOpen: true, rightPanelOpen: true },
      backupFolders: [],
      viewer: {
        timeFormat: 'system',
        timeZone: 'system',
        showSeconds: false,
        motion: 'system',
        listPreviews: true,
        revealEarlierText: false,
        mediaPlayback: 'confirm',
        showHiddenCharacters: false,
        blurOnFocusLoss: false,
        idleHideMinutes: 0,
        captureProtection: false,
        uiZoom: 100,
      },
      ...overrides,
    },
    persistence: { file: '/config/settings.json', readOnly, notice: null },
  };
}

export function status(
  state: ArchiveLocationStatus['state'],
  path: string | null = '/data/archives',
  checks: LocationCheck[] = [],
): ArchiveLocationStatus {
  return { path: state === 'unset' ? null : path, state, checks };
}

/** jsdom has no matchMedia; tests choose what the "window" matches. */
export function stubMatchMedia(matching: (query: string) => boolean): void {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: (query: string) => ({
      matches: matching(query),
      media: query,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    }),
  });
}

export function location(overrides: Partial<BackupLocation> = {}): BackupLocation {
  return {
    id: 'loc-1',
    kind: 'addedParent',
    path: '/data/backups',
    viaLink: false,
    state: 'present',
    backupCount: 1,
    message: null,
    ...overrides,
  };
}

export function summary(overrides: Partial<BackupSummary> = {}): BackupSummary {
  return {
    id: 'b1',
    locationId: 'loc-1',
    viaLink: false,
    deviceName: { value: 'Synthetic Test Phone', source: 'infoPlist' },
    productType: { value: 'iPhone0,0', source: 'infoPlist' },
    productName: null,
    iosVersion: { value: '18.1', source: 'infoPlist' },
    lastBackupDate: '2026-10-01T12:00:00Z',
    encrypted: false,
    state: 'ready',
    stateReason: 'Ready to check.',
    activeJobId: null,
    lastReportImportable: null,
    ...overrides,
  };
}

export function backupList(
  backups: BackupSummary[] = [summary()],
  locations: BackupLocation[] = [location()],
): BackupList {
  return { locations, backups, budgetExhausted: false };
}

export function report(importable = true): ValidationReportDto {
  return {
    importable,
    checks: [
      { id: 'manifest-integrity', level: 'ok', message: 'The file index database passed SQLite\'s integrity check.' },
      importable
        ? { id: 'messages-db', level: 'ok', message: 'The Messages database (sms.db) is listed.' }
        : { id: 'messages-db', level: 'error', message: 'The Messages database (sms.db) is missing from the backup.' },
    ],
    stats: {
      totalRows: 8,
      files: 5,
      directories: 3,
      symlinks: 0,
      otherFlags: 0,
      declaredBytes: 4096,
      declaredBytesSaturated: false,
      missingBlobs: 0,
      sizeMismatches: 0,
      unsafeBlobs: 0,
      unreadableBlobs: 0,
      invalidRows: 0,
      invalidFileIds: 0,
      fileIdMismatches: 0,
      pathsUnusualForExport: 0,
      unreadableRecords: 0,
      filesWithoutKey: 0,
      filesWithUnavailableClass: 0,
      hasPropertiesTable: true,
      views: 0,
      triggers: 0,
      saturated: false,
    },
    smsDb: {
      listed: true,
      ambiguous: false,
      blobPresent: true,
      declaredSize: 4096,
      sizeConsistent: true,
      headerOk: importable,
      paddingOk: null,
      walListed: false,
      shmListed: false,
    },
    durationMs: 40,
    validatedAt: '2026-10-18T10:00:00Z',
  };
}

export function details(
  s: BackupSummary = summary(),
  overrides: Partial<BackupDetails> = {},
): BackupDetails {
  return {
    summary: s,
    path: '/data/backups/SYNTHETIC0-0000000000000000',
    device: {
      deviceName: s.deviceName,
      displayName: null,
      productType: s.productType,
      productName: s.productName,
      iosVersion: s.iosVersion,
      buildVersion: null,
    },
    status: {
      snapshotState: 'finished',
      isFullBackup: true,
      formatVersion: '3.3',
      date: null,
      backupState: null,
      snapshotFolderPresent: false,
    },
    encryption: { isEncrypted: s.encrypted, hasKeybag: true, hasManifestKey: s.encrypted === true },
    layout: { format: 'v3', manifestDbWalPresent: false, manifestDbShmPresent: false, nonRegularMarkers: 0 },
    notes: [],
    archiveSyncProvider: null,
    archiveConfigured: true,
    lastReport: null,
    lastExtractionReport: null,
    iosVersionUntested: true,
    lastCleanup: null,
    activeJob: null,
    ...overrides,
  };
}

/** A readable extraction report with every section filled in. */
export function extractionReport(overrides: Partial<ExtractionReportDto> = {}): ExtractionReportDto {
  return {
    jobId: 'job-1',
    outcome: 'readableWithQualifiers',
    qualifiers: [{ kind: 'iosVersionUntested' }, { kind: 'unknownColumns', count: 2 }],
    unsupportedReasons: [],
    damageReason: null,
    incompleteReason: null,
    sourceChanged: false,
    stagingRemoved: true,
    plaintextRemovedAfterRowPass: true,
    schema: {
      tables: [
        {
          table: 'message',
          present: true,
          rows: 40,
          missingColumns: ['message.dateRetracted', 'message.scheduleType'],
          unknownColumns: 2,
          generatedColumns: 0,
          wrongCaseColumns: 0,
        },
      ],
      missingColumns: 2,
      unknownTables: 0,
      unknownColumns: 2,
      wrongCaseColumns: 0,
      views: 0,
      triggers: 0,
      indexes: 3,
      statTables: 0,
      encoding: 'utf8',
      clientVersion: 17001,
      clientVersionParsed: true,
      clientVersionOutOfSafeRange: false,
      features: {
        attributedBody: true,
        tapbacks: true,
        emojiTapbacks: false,
        replies: true,
        editsAndUnsends: false,
        recentlyDeleted: true,
        scheduled: false,
        filtering: false,
        attachments: true,
        genmoji: false,
      },
    },
    database: {
      openMode: 'readOnly',
      walListed: false,
      walOutcome: 'notListed',
      walBytes: null,
      walHeaderSaysWal: false,
      walSizeMatches: null,
      walFrames: null,
      sizeMatches: true,
      integrity: 'ok',
      pageSize: 1024,
      stagedBytes: 65536,
    },
    counts: {
      messageRows: 40,
      associationRows: 5,
      groupActionRows: 2,
      recentlyDeletedRows: 1,
      otherRows: 32,
      chatRows: 4,
      handleRows: 6,
      attachmentRows: 3,
      messagesInNoChat: 1,
    },
    dates: {
      earliestMessageAt: '2020-01-02T03:04:05Z',
      latestMessageAt: '2026-10-01T12:00:00Z',
      excludedScheduled: 0,
      excludedBeforeFloor: 0,
      excludedAfterCheck: 0,
      excludedUnusable: 1,
    },
    attachments: {
      rows: 3,
      found: 2,
      notListedAtSourcedKey: 1,
      notFoundAtAssumedKey: 0,
      notLookedUp: 0,
      ambiguous: 0,
      invalidEntry: 0,
      filePresent: 1,
      fileMissing: 0,
      fileSizeMismatch: 0,
      fileNotReadable: 0,
      noKey: 0,
      keyDoesNotUnwrap: 1,
      classKeyUnavailable: 0,
      probeOther: 0,
      notProbed: 0,
      presentBlobBytes: 2048,
    },
    problems: [{ kind: 'tsTruncated', count: 1 }],
    checkedAt: '2026-10-18T10:00:00Z',
    durationMs: 120,
    saturated: false,
    ...overrides,
  };
}

// ---------------------------------------------------------------- M4 archives

/** A synthetic archive id (`a` + 32 hex digits). */
export const ARCHIVE_ID = `a${'1'.repeat(32)}`;
export const OTHER_ARCHIVE_ID = `a${'2'.repeat(32)}`;

export function archiveSummary(overrides: Partial<ArchiveSummaryDto> = {}): ArchiveSummaryDto {
  return {
    archiveId: ARCHIVE_ID,
    label: 'Synthetic archive',
    labelUnreadable: false,
    schemaVersion: 1,
    createdAtMs: Date.UTC(2026, 9, 1, 12, 0, 0),
    stats: null,
    sessions: { running: 0, completed: 0, cancelled: 0, failed: 0, interrupted: 0, superseded: 0, stopped: 0 },
    resumableSession: null,
    resumableState: null,
    newerRules: false,
    statTables: false,
    ...overrides,
  };
}

export function archiveEntry(overrides: Partial<ArchiveEntryDto> = {}): ArchiveEntryDto {
  const status = overrides.status ?? 'ready';
  return {
    archiveId: ARCHIVE_ID,
    status,
    schemaVersion: null,
    damage: null,
    refusal: null,
    summary: status === 'ready' ? archiveSummary({ archiveId: overrides.archiveId ?? ARCHIVE_ID }) : null,
    ...overrides,
  };
}

export function archiveListing(entries: ArchiveEntryDto[] = [archiveEntry()]): ArchiveListingDto {
  return { entries, libraryState: 'ready', libraryError: null };
}

export function archiveDetails(overrides: Partial<ArchiveDetailsDto> = {}): ArchiveDetailsDto {
  return {
    summary: archiveSummary(),
    sessions: [],
    lastImport: null,
    lastVerify: null,
    activeJob: null,
    preflight: null,
    resumable: false,
    matchingBackupIds: [],
    resumeOptions: null,
    ...overrides,
  };
}

function entityCounts(seen: number): EntityCountsDto {
  return {
    seen,
    inserted: seen,
    alreadyPresent: 0,
    unkeyed: 0,
    cellsReplaced: 0,
    notApplied: 0,
    historyAdded: 0,
    valueIssueCells: 0,
    saturated: false,
  };
}

function linkCounts(seen: number): LinkCountsDto {
  return {
    seen,
    inserted: seen,
    alreadyPresent: 0,
    unresolved: 0,
    unresolvedFirst: 0,
    unresolvedSecond: 0,
    skippedRecentlyDeleted: 0,
    saturated: false,
  };
}

export function importCounts(): ImportCountsDto {
  return {
    handles: entityCounts(6),
    chats: entityCounts(4),
    messages: entityCounts(40),
    bodies: {
      added: 38,
      unchanged: 0,
      rederived: 0,
      currentMoved: 0,
      displayAttributedBody: 30,
      displayText: 8,
      displayTextLossy: 0,
      noneAttributedBodyUnusable: 0,
      noneTextEmpty: 1,
      noneNoSource: 1,
      sourcesDiffer: 0,
      decodeFailed: 0,
      runIssues: 0,
      saturated: false,
    },
    chatHandles: linkCounts(6),
    chatMessages: linkCounts(39),
    attachments: entityCounts(3),
    attachmentFiles: {
      copied: 2,
      replaced: 0,
      deduplicated: 0,
      adopted: 0,
      orphanReplaced: 0,
      alreadyArchived: 0,
      storeConflicts: 0,
      notRequested: 0,
      notLookedUp: 0,
      notInBackup: 1,
      blobUnavailable: 0,
      overLimit: 0,
      tooLargeForVolume: 0,
      readFailed: 0,
      writeFailed: 0,
      sizeMismatch: 0,
      partialsLeft: 0,
      bytesCopied: 2048,
      fsyncs: 3,
      blobsAdded: 2,
      currentMoved: 0,
      saturated: false,
    },
    messageAttachments: linkCounts(3),
    recentlyDeleted: {
      messages: 1,
      excluded: 1,
      notUpdated: 0,
      imported: 0,
      attachmentsExcluded: 0,
      marked: 0,
      notMarked: 0,
      saturated: false,
    },
    batches: 7,
    resumes: 0,
    saturated: false,
  };
}

export function lastImport(overrides: Partial<LastImportDto> = {}): LastImportDto {
  return {
    session: 1,
    lineageId: 1,
    state: 'completed',
    startedAtMs: Date.UTC(2026, 9, 18, 10, 0, 0),
    endedAtMs: Date.UTC(2026, 9, 18, 10, 5, 0),
    resumes: 0,
    stopReason: null,
    provisional: false,
    counts: importCounts(),
    ...overrides,
  };
}

/** A first-import preflight with nothing to warn about. */
export function preflight(overrides: Partial<ImportPreflightDto> = {}): ImportPreflightDto {
  const keys = (rows: number) => ({ rows, keyable: rows, duplicatedKeys: 0, unkeyed: 0 });
  return {
    resume: { kind: 'fresh', supersedes: 0, reason: 'nothingToContinue' },
    sameContentCompleted: false,
    device: 'firstImport',
    lineages: [],
    suggestedLineage: { kind: 'new' },
    keys: {
      handles: keys(6),
      chats: keys(4),
      messages: keys(40),
      attachments: keys(3),
      messagesAlreadyArchived: 0,
    },
    recentlyDeleted: { messages: 0, excluded: 0, alreadyArchived: 0, attachmentsExcluded: 0 },
    space: {
      stagedBytes: 65536,
      bytesToCopy: 2048,
      filesToCopy: 2,
      largestFile: 1024,
      reserve: 1073741824,
      walAllowance: 1048576,
      needed: 2 * 1073741824,
      available: 100 * 1073741824,
      warns: false,
    },
    order: { dated: true, source: 'statusPlistDate', outOfRange: false, sortsBeforeLatest: false, sourceDiffers: false },
    encryptedSource: false,
    syncFolderWarning: false,
    windowsAclInherited: false,
    qualifiers: [],
    provisional: false,
    saturated: false,
    ...overrides,
  };
}

export function verifyCounts(overrides: Partial<VerifyCountsDto> = {}): VerifyCountsDto {
  return {
    schema: 'ok',
    newerRules: false,
    rulesAboveHighWater: 0,
    statTables: true,
    ftsSecureDelete: 'ok',
    sqliteCheck: 'ok',
    sqliteCheckMessages: 0,
    foreignKeyViolations: 0,
    valueIssueOrphans: 0,
    valueHistoryOrphans: 0,
    labelUnreadable: false,
    sessionsRunning: 0,
    blobIntents: 0,
    ftsIntegrity: 'ok',
    blobs: 2,
    blobBadName: 0,
    blobMissing: 0,
    blobNotRegular: 0,
    blobLengthMismatch: 0,
    blobsHashed: 0,
    bytesHashed: 0,
    blobHashMismatch: 0,
    blobUnreadable: 0,
    storeEntries: 2,
    storeUnexpected: 0,
    storeOrphans: 0,
    storeLinks: 0,
    storeWalkTruncated: false,
    partialFiles: 0,
    provenanceFiles: 1,
    provenanceUnreadable: 0,
    provenanceTmpFiles: 0,
    importsScanTruncated: false,
    notPrivate: 0,
    checksNotRun: 0,
    hasProblems: false,
    saturated: false,
    ...overrides,
  };
}

export function verifySummary(overrides: Partial<VerifySummaryDto> = {}): VerifySummaryDto {
  return {
    run: 1,
    depth: 'quick',
    startedAtMs: Date.UTC(2026, 9, 18, 11, 0, 0),
    endedAtMs: Date.UTC(2026, 9, 18, 11, 0, 3),
    outcome: 'ok',
    counts: verifyCounts(),
    ...overrides,
  };
}
