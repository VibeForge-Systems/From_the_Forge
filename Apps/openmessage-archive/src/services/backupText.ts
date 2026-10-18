// Display helpers for the Backups page. Values from a backup are already
// bounded and sanitised in Rust; these only format and label them.
import type { BackupLocation, BackupLocationKind, SourceFile, SourcedText } from '@/types';

const SOURCE_LABEL: Record<SourceFile, string> = {
  infoPlist: 'Info.plist',
  statusPlist: 'Status.plist',
  manifestPlist: 'Manifest.plist',
  manifestPlistLockdown: 'Manifest.plist (Lockdown)',
};

export function sourceHint(s: SourcedText | null): string | null {
  return s ? `from ${SOURCE_LABEL[s.source]}` : null;
}

const LOCATION_KIND_LABEL: Record<BackupLocationKind, string> = {
  appleDevices: 'Apple Devices / Microsoft Store iTunes',
  itunesDesktop: 'iTunes (desktop)',
  iMazing: 'iMazing',
  addedParent: 'Added by you',
  addedBackup: 'Added by you',
};

/** Kinds the app scans on its own (Windows only); the rest the user added. */
const DEFAULT_LOCATION_KINDS: ReadonlySet<BackupLocationKind> = new Set<BackupLocationKind>([
  'appleDevices',
  'itunesDesktop',
  'iMazing',
]);

export function isDefaultLocation(l: BackupLocation): boolean {
  return DEFAULT_LOCATION_KINDS.has(l.kind);
}

export function locationKindLabel(l: BackupLocation): string {
  const base = LOCATION_KIND_LABEL[l.kind];
  return l.viaLink ? `${base} (linked folder)` : base;
}

/** An RFC 3339 timestamp in the user's locale. */
export function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(d);
}

/** "just now", "5 minutes ago", … */
export function relativeTime(iso: string, now: Date = new Date()): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return iso;
  const seconds = Math.round((then - now.getTime()) / 1000);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
  const abs = Math.abs(seconds);
  if (abs < 60) return rtf.format(seconds, 'second');
  if (abs < 3600) return rtf.format(Math.round(seconds / 60), 'minute');
  if (abs < 86400) return rtf.format(Math.round(seconds / 3600), 'hour');
  return rtf.format(Math.round(seconds / 86400), 'day');
}
