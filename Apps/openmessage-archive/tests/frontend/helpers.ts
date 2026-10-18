import type {
  ArchiveLocationStatus,
  BackupDetails,
  BackupList,
  BackupLocation,
  BackupSummary,
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
    lastCleanup: null,
    activeJob: null,
    ...overrides,
  };
}
