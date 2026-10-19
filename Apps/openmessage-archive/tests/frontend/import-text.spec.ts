import { describe, expect, it } from 'vitest';
import {
  NOT_RECORDED,
  PROVISIONAL,
  QUALIFIER,
  STAGING_NOTE,
  STATUS_LABEL,
  UNENCRYPTED_FROM_ENCRYPTED,
  UNENCRYPTED_NOTE,
  archiveName,
  keySentences,
  lineageEvidence,
  lineageLabel,
  numOrNotRecorded,
  orderSentences,
  privacySentences,
  qualifierSentences,
  recentlyDeletedSentences,
  resumeSentences,
  spaceSentences,
  statusSentence,
  verifyScope,
  verifySentences,
} from '@/services/importText';
import type { ArchiveStatusDto, LineageDto, QualifierCodeDto } from '@/types';
import { archiveEntry, archiveSummary, preflight, verifyCounts } from './helpers';

const lineage = (o: Partial<LineageDto> = {}): LineageDto => ({
  lineageId: 2,
  sessions: 3,
  fullSessions: 2,
  lastBackupMs: Date.UTC(2026, 8, 30, 9, 0, 0),
  modelDiffers: false,
  nameDiffers: false,
  deviceMatches: true,
  ...o,
});

describe('import text: continue or start over', () => {
  it('names the session and the pass when the import continues', () => {
    const s = resumeSentences(
      preflight({ resume: { kind: 'continue', session: 4, lineageId: 2, fromPass: 'attachments' } }),
    );
    expect(s[0]).toBe('Continuing import session 4 from attachments, if you choose import set 2 below.');
    expect(s[1]).toContain('starts over');
  });

  it('says that a changed backup or app sets the interrupted import aside (C3.18)', () => {
    const s = resumeSentences(preflight({ resume: { kind: 'fresh', supersedes: 1, reason: 'sourceOrBuildChanged' } }));
    expect(s).toEqual([
      'The backup has changed since the interrupted import (or the app was updated); that import will be set aside and everything re-read.',
    ]);
  });

  it('says that other options set the interrupted import aside, without blaming the backup', () => {
    const s = resumeSentences(preflight({ resume: { kind: 'fresh', supersedes: 1, reason: 'optionsDiffer' } }));
    expect(s).toEqual([
      'The import options differ from those of the interrupted import; that import will be set aside and everything re-read with these options.',
    ]);
    expect(s.join(' ')).not.toContain('backup has changed');
  });

  it('starts a new import, counting unfinished ones that will be set aside', () => {
    expect(resumeSentences(preflight())).toEqual(['This starts a new import.']);
    expect(
      resumeSentences(preflight({ resume: { kind: 'fresh', supersedes: 2, reason: 'nothingToContinue' } })),
    ).toEqual(['This starts a new import.', '2 unfinished imports will be set aside.']);
  });
});

describe('import text: lineage choice', () => {
  it('describes an import set with its imports and last backup date', () => {
    const label = lineageLabel(lineage());
    expect(label).toMatch(/^A newer backup of the same phone as import set 2 \(3 imports, last backup on .+\)$/);
    expect(lineageLabel(lineage({ sessions: 1, lastBackupMs: null }))).toBe(
      'A newer backup of the same phone as import set 2 (1 import, last backup date not recorded)',
    );
  });

  it('states the model and name evidence each as its own sentence, including "could not be compared"', () => {
    expect(lineageEvidence(lineage())).toEqual(['The phone model matches.', 'The device name matches.']);
    expect(lineageEvidence(lineage({ modelDiffers: true, nameDiffers: null }))).toEqual([
      'The phone model differs.',
      'The device name could not be compared.',
    ]);
  });
});

describe('import text: facts at the gate', () => {
  it('words space as an estimate, and says when free space could not be read', () => {
    const s = spaceSentences(preflight());
    expect(s[0]).toBe('This import needs about 2 GB of free space (an estimate).');
    expect(s[1]).toBe('About 100 GB is free.');
    expect(s[2]).toBe('2 attachment files (2 KB) will be copied.');
    const unknown = spaceSentences(
      preflight({ space: { ...preflight().space, available: null, warns: true, filesToCopy: 0 } }),
    );
    expect(unknown).toContain('The free space in your archive folder could not be read.');
    expect(unknown.at(-1)).toContain('stops safely if it runs out');
  });

  it('counts overlap, duplicated and unkeyed rows, and "already imported"', () => {
    const p = preflight({
      sameContentCompleted: true,
      lineages: [lineage()],
      keys: {
        ...preflight().keys,
        messages: { rows: 40, keyable: 37, duplicatedKeys: 1, unkeyed: 3 },
        handles: { rows: 6, keyable: 5, duplicatedKeys: 0, unkeyed: 1 },
        messagesAlreadyArchived: 30,
      },
    });
    const s = keySentences(p);
    expect(s).toContain('This backup holds 40 messages.');
    expect(s).toContain('This exact backup was already imported. Importing it again only records that it was seen.');
    expect(s).toContain('30 messages are already in this archive and will be merged.');
    expect(s).toContain(
      '3 messages have no usable or no unique identifier (1 identifier is used more than once), and will be imported without being merged.',
    );
    expect(s).toContain('1 address has no usable or no unique identifier, and will be imported without being merged.');
  });

  it('states zero overlap with a non-empty archive as possibly normal, not as an error', () => {
    const s = keySentences(preflight({ lineages: [lineage()] }));
    expect(s.join(' ')).toContain('That can be normal, for example when the phone keeps messages for only 30 days.');
    expect(keySentences(preflight()).join(' ')).not.toContain('30 days');
  });

  it('counts Recently Deleted messages, their excluded attachments and the already-archived ones', () => {
    expect(recentlyDeletedSentences(preflight())).toEqual(['No messages are in Recently Deleted.']);
    const s = recentlyDeletedSentences(
      preflight({ recentlyDeleted: { messages: 5, excluded: 4, alreadyArchived: 1, attachmentsExcluded: 2 } }),
    );
    expect(s).toEqual([
      '5 messages are in Recently Deleted.',
      '4 of them are not imported, nor 2 attachments linked only to them.',
      '1 of them is already in this archive: marked as Recently Deleted, but not changed.',
    ]);
    const included = recentlyDeletedSentences(
      preflight({ recentlyDeleted: { messages: 2, excluded: 0, alreadyArchived: 0, attachmentsExcluded: 0 } }),
    );
    expect(included).toContain('They will be imported with their attachments.');
  });

  it('says when the backup is older than the newest one in the archive, and names date problems', () => {
    expect(orderSentences(preflight())).toEqual(["The backup date comes from the backup's Status.plist."]);
    const s = orderSentences(
      preflight({
        order: { dated: false, source: null, outOfRange: true, sortsBeforeLatest: true, sourceDiffers: true },
      }),
    );
    expect(s[0]).toBe('This backup is older than the newest one in this archive.');
    expect(s).toContain('This backup has no usable date, so it ranks below every dated backup.');
    expect(s).toContain('A backup date outside the expected range was ignored.');
    expect(s.join(' ')).toContain('may not be comparable');
  });

  it('marks derived details provisional for an untested iOS version (D79), once', () => {
    expect(qualifierSentences(preflight({ qualifiers: ['iosVersionUntested'], provisional: true }))).toEqual([
      PROVISIONAL,
    ]);
    expect(qualifierSentences(preflight({ provisional: true }))).toEqual([PROVISIONAL]);
    expect(PROVISIONAL).toContain('Derived details for this import are provisional.');
  });

  it('has a sentence for every qualifier code', () => {
    const codes: QualifierCodeDto[] = [
      'backupDateOutOfRange',
      'walSkipped',
      'walStateUnknown',
      'walFramesInvalid',
      'nonUtf8Encoding',
      'sizeMismatch',
      'iosVersionUntested',
      'unknownTables',
      'unknownColumns',
      'wrongCaseColumns',
    ];
    expect(Object.keys(QUALIFIER).sort()).toEqual([...codes].sort());
    for (const c of codes) expect(QUALIFIER[c].endsWith('.')).toBe(true);
  });

  it('always states that the archive is unencrypted and that a decrypted copy exists during the import', () => {
    expect(privacySentences(preflight())).toEqual([UNENCRYPTED_NOTE, STAGING_NOTE]);
    const enc = privacySentences(preflight({ encryptedSource: true, syncFolderWarning: true, windowsAclInherited: true }));
    expect(enc[0]).toBe(UNENCRYPTED_FROM_ENCRYPTED);
    expect(enc[0]).toContain('Messages from an encrypted backup will be readable by anyone who can open this folder');
    expect(enc[1]).toBe(STAGING_NOTE);
    expect(enc).toHaveLength(4);
  });
});

describe('import text: the archive list and verify', () => {
  it('labels every status and explains every non-ready one', () => {
    const statuses: ArchiveStatusDto[] = [
      'ready',
      'importing',
      'verifying',
      'exporting',
      'inUse',
      'needsUpgrade',
      'newerVersion',
      'damaged',
      'notAnArchive',
      'refused',
    ];
    expect(Object.keys(STATUS_LABEL).sort()).toEqual([...statuses].sort());
    for (const status of statuses) {
      expect(statusSentence(archiveEntry({ status, schemaVersion: 2 })).length).toBeGreaterThan(5);
    }
    expect(statusSentence(archiveEntry({ status: 'damaged', damage: 'metaMissing' }))).toContain(
      'It was left as it is.',
    );
    expect(statusSentence(archiveEntry({ status: 'needsUpgrade', schemaVersion: null }))).toContain(NOT_RECORDED);
  });

  it('names an archive by its label, or by a fixed stand-in', () => {
    expect(archiveName(archiveSummary())).toBe('Synthetic archive');
    expect(archiveName(archiveSummary({ label: null }))).toBe('Unnamed archive');
    expect(archiveName(archiveSummary({ label: null, labelUnreadable: true }))).toBe('Archive (name unreadable)');
    expect(numOrNotRecorded(null)).toBe(NOT_RECORDED);
    expect(numOrNotRecorded(0)).toBe('0');
  });

  it('writes no finding for a clean archive, and one sentence per finding kind with counts', () => {
    expect(verifySentences(verifyCounts())).toEqual([]);
    const s = verifySentences(
      verifyCounts({
        sqliteCheck: 'failed',
        sqliteCheckMessages: 2,
        blobMissing: 1,
        blobHashMismatch: 3,
        storeOrphans: 2,
        sessionsRunning: 1,
        storeWalkTruncated: true,
        notPrivate: 4,
        hasProblems: true,
      }),
    );
    expect(s).toEqual([
      "SQLite's integrity check failed with 2 findings.",
      '1 import was interrupted (still marked as running).',
      '1 attachment file is missing.',
      '3 attachment files do not match the recorded checksum.',
      '2 files in the attachment folder belong to no record.',
      'The attachment folder holds too many entries; the check stopped listing it.',
      '4 archive files or folders can be read or changed by other users of this computer.',
    ]);
  });

  it('says what a quick and a full verify looked at', () => {
    expect(verifyScope(verifyCounts(), 'quick')).toBe(
      '2 attachment files checked for presence and size. A full check also reads every file back.',
    );
    expect(verifyScope(verifyCounts({ blobsHashed: 2, bytesHashed: 4096 }), 'full')).toBe(
      '2 attachment files checked; 2 files (4 KB) read back and compared with the recorded checksums.',
    );
  });
});
