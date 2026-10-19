import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  applyFeatures,
  detectFeatures,
  engineFeatures,
  resetFeaturesForTests,
  type FeatureProbe,
} from '@/services/features';
import { devNoteText, devNote } from '@/services/devlog';
import { formatInstant, intlStyleIsSupported, setIntlStyleSupported } from '@/services/time';

const everything: FeatureProbe = {
  cssSupports: () => true,
  intlDateStyle: () => true,
  clipboardWrite: () => true,
  dialogModal: () => true,
};

afterEach(() => {
  resetFeaturesForTests();
  setIntlStyleSupported(true);
  document.documentElement.className = '';
});

describe('feature detection', () => {
  it('reports each feature from the probe', () => {
    const f = detectFeatures({
      ...everything,
      cssSupports: (p, v) => !(p === 'overflow' && v === 'clip') && p !== 'overflow-anchor',
      clipboardWrite: () => false,
    });
    expect(f).toEqual({
      overflowClip: false,
      bidiIsolate: true,
      intlDateStyle: true,
      clipboardWrite: false,
      dialogModal: true,
      overflowAnchor: false,
    });
  });

  it('applies the CSS fallbacks as classes on <html>', () => {
    const root = document.createElement('html');
    applyFeatures(detectFeatures({ ...everything, cssSupports: () => false }), root);
    expect(root.classList.contains('oma-no-overflow-clip')).toBe(true);
    expect(root.classList.contains('oma-no-bidi-isolate')).toBe(true);
    applyFeatures(detectFeatures(everything), root);
    expect(root.className).toBe('');
  });

  it('switches time formatting to the fixed format without Intl styles', () => {
    applyFeatures(detectFeatures({ ...everything, intlDateStyle: () => false }), document.createElement('html'));
    expect(intlStyleIsSupported()).toBe(false);
    expect(formatInstant(Date.UTC(2026, 0, 2, 3, 4), { timeFormat: 'h23', timeZone: 'utc', showSeconds: false })).toBe(
      '2026-01-02 03:04 UTC',
    );
  });

  it('remembers what was applied', () => {
    const f = applyFeatures(detectFeatures({ ...everything, dialogModal: () => false }), document.createElement('html'));
    expect(engineFeatures()).toBe(f);
    expect(f.dialogModal).toBe(false);
  });

  it('detects jsdom as it is (no CSS.supports, a dialog element without showModal)', () => {
    const f = detectFeatures();
    expect(typeof f.dialogModal).toBe('boolean');
    expect(f.overflowClip).toBe(typeof CSS !== 'undefined' && CSS.supports?.('overflow', 'clip') === true);
  });
});

describe('devNote', () => {
  it('writes a closed sentence and at most one finite number', () => {
    const lines: unknown[][] = [];
    const spy = vi.spyOn(console, 'debug').mockImplementation((...args: unknown[]) => {
      lines.push(args);
    });
    devNote('viewerPageMs', 12.5);
    devNote('jobIdMismatch');
    devNote('viewerPageMs', Number.NaN);
    expect(lines).toEqual([
      [`[oma] ${devNoteText('viewerPageMs')}`, 12.5],
      [`[oma] ${devNoteText('jobIdMismatch')}`],
      [`[oma] ${devNoteText('viewerPageMs')}`],
    ]);
    spy.mockRestore();
  });
});
