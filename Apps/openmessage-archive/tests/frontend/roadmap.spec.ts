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

  it('claims only milestone 1 as available in this build', () => {
    const available = CAPABILITIES.filter((c) => c.status === 'available').map((c) => c.id);
    expect(available).toEqual(['shell']);
  });

  it('throws on an unknown capability rather than rendering nothing', () => {
    // @ts-expect-error deliberately invalid id
    expect(() => capability('time-travel')).toThrow();
  });
});
