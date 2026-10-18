import { describe, expect, it } from 'vitest';
import { CAPABILITIES, capability } from '@/services/roadmap';

describe('roadmap', () => {
  it('has unique ids and milestones within the published plan', () => {
    expect(new Set(CAPABILITIES.map((c) => c.id)).size).toBe(CAPABILITIES.length);
    for (const c of CAPABILITIES) {
      expect(c.milestone).toBeGreaterThanOrEqual(1);
      expect(c.milestone).toBeLessThanOrEqual(8);
    }
  });

  it('claims milestones 1 to 4 as available and everything later as planned', () => {
    const available = CAPABILITIES.filter((c) => c.status === 'available').map((c) => c.id);
    expect(available).toEqual(['shell', 'backup-import', 'extraction', 'archive-database']);
    for (const c of CAPABILITIES) {
      if (c.milestone >= 5) expect(c.status).toBe('planned');
    }
  });

  it('describes the extraction check as counts only, and the archive import as shipped', () => {
    const e = capability('extraction');
    expect(e.title).toBe('Check that messages can be read');
    expect(e.summary).toContain('Shows counts only; the check itself saves nothing.');
    expect(e.summary).not.toContain('milestone 4');
    expect(capability('backup-import').summary).not.toContain('milestone 3');
    const a = capability('archive-database');
    expect(a.milestone).toBe(4);
    expect(a.summary).toBe(
      'Import messages and copy attachments into a versioned SQLite archive, with transactional, resumable imports, an FTS5 index, and an integrity check.',
    );
  });

  it('moves basic verification to milestone 4 and leaves repair to milestone 8 (D69)', () => {
    const h = capability('hardening');
    expect(h.milestone).toBe(8);
    expect(h.status).toBe('planned');
    expect(h.summary).toBe(
      'Archive repair and recovery tests, accessibility review, the diagnostic log, and signed packages.',
    );
    expect(h.summary).not.toContain('verification');
  });

  it('throws on an unknown capability rather than rendering nothing', () => {
    // @ts-expect-error deliberately invalid id
    expect(() => capability('time-travel')).toThrow();
  });
});
