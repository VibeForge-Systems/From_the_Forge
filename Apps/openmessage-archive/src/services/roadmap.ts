// What the application can and cannot do yet, in one place. Controls for
// unfinished capabilities render disabled with a "Planned" badge sourced
// from here, and the About page lists the whole table, so the UI never
// offers a button that silently does nothing.

export type CapabilityStatus = 'available' | 'planned';

export interface Capability {
  id: CapabilityId;
  title: string;
  milestone: number;
  status: CapabilityStatus;
  summary: string;
}

export type CapabilityId =
  | 'shell'
  | 'backup-import'
  | 'extraction'
  | 'archive-database'
  | 'viewer'
  | 'search'
  | 'exports'
  | 'device-backup'
  | 'hardening';

export const CAPABILITIES: readonly Capability[] = [
  {
    id: 'shell',
    title: 'Desktop shell and settings',
    milestone: 1,
    status: 'available',
    summary: 'Application window, themes, keyboard shortcuts, and archive location selection.',
  },
  {
    id: 'backup-import',
    title: 'Find and check iPhone backups',
    milestone: 2,
    status: 'available',
    summary:
      'Discover local backups, show their details, unlock encrypted ones with your password, and validate the file index.',
  },
  {
    id: 'extraction',
    title: 'Check that messages can be read',
    milestone: 3,
    status: 'available',
    summary:
      'Locate the Messages database, detect its layout, and check that messages, chats, addresses and attachment references can be read. Shows counts only; nothing is saved until milestone 4.',
  },
  {
    id: 'archive-database',
    title: 'Archive database',
    milestone: 4,
    status: 'planned',
    summary:
      'Extract messages and copy attachments into a versioned SQLite archive, with transactional, resumable imports and FTS5 indexes.',
  },
  {
    id: 'viewer',
    title: 'Conversation viewer',
    milestone: 5,
    status: 'planned',
    summary: 'Conversation list, message threads, inline images, and attachment previews.',
  },
  {
    id: 'search',
    title: 'Search and filtering',
    milestone: 5,
    status: 'planned',
    summary: 'Full-text search across conversations with date, sender, and type filters.',
  },
  {
    id: 'exports',
    title: 'Exports',
    milestone: 6,
    status: 'planned',
    summary: 'TXT, CSV, JSON, HTML, PDF, and XLSX exports, plus attachment export.',
  },
  {
    id: 'device-backup',
    title: 'Back up a connected iPhone',
    milestone: 7,
    status: 'planned',
    summary: 'Detect a USB-connected iPhone and create a local backup where the platform allows.',
  },
  {
    id: 'hardening',
    title: 'Hardening and release',
    milestone: 8,
    status: 'planned',
    summary: 'Archive verification, recovery tests, accessibility review, and signed packages.',
  },
];

export function capability(id: CapabilityId): Capability {
  const found = CAPABILITIES.find((c) => c.id === id);
  if (!found) throw new Error(`Unknown capability: ${id}`);
  return found;
}
