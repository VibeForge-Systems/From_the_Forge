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

  it('claims milestones 1 and 2 and the milestone 3 extraction check as available', () => {
    const available = CAPABILITIES.filter((c) => c.status === 'available').map((c) => c.id);
    expect(available).toEqual(['shell', 'backup-import', 'extraction']);
    for (const c of CAPABILITIES) {
      if (c.milestone >= 4) expect(c.status).toBe('planned');
    }
  });

  it('describes the extraction check as counts only, with importing in milestone 4', () => {
    const e = capability('extraction');
    expect(e.title).toBe('Check that messages can be read');
    expect(e.summary).toContain('Shows counts only; nothing is saved until milestone 4.');
    expect(capability('backup-import').summary).not.toContain('milestone 3');
    expect(capability('archive-database').milestone).toBe(4);
    expect(capability('archive-database').summary).toContain('copy attachments');
  });

  it('throws on an unknown capability rather than rendering nothing', () => {
    // @ts-expect-error deliberately invalid id
    expect(() => capability('time-travel')).toThrow();
  });
});
