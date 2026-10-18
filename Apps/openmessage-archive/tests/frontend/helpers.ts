import type {
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
