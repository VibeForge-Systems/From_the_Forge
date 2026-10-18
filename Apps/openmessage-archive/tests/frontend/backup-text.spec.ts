import { describe, expect, it } from 'vitest';
import { isDefaultLocation, locationKindLabel } from '@/services/backupText';
import type { BackupLocationKind } from '@/types';
import { location } from './helpers';

describe('locationKindLabel', () => {
  it.each([
    ['appleDevices', 'Apple Devices / Microsoft Store iTunes'],
    ['itunesDesktop', 'iTunes (desktop)'],
    ['iMazing', 'iMazing'],
    ['addedParent', 'Added by you'],
    ['addedBackup', 'Added by you'],
  ] as [BackupLocationKind, string][])('labels %s as "%s"', (kind, label) => {
    expect(locationKindLabel(location({ kind }))).toBe(label);
  });

  it('marks a linked folder', () => {
    expect(locationKindLabel(location({ kind: 'iMazing', viaLink: true }))).toBe('iMazing (linked folder)');
  });
});

describe('isDefaultLocation', () => {
  it('treats the scanned locations as defaults and added folders as not', () => {
    const kinds: BackupLocationKind[] = ['appleDevices', 'itunesDesktop', 'iMazing', 'addedParent', 'addedBackup'];
    expect(kinds.filter((kind) => isDefaultLocation(location({ kind })))).toEqual([
      'appleDevices',
      'itunesDesktop',
      'iMazing',
    ]);
  });
});
