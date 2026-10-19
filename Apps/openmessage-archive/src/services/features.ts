// Engine feature detection at startup (M5 §8.10). Each feature has a
// degraded path; none is assumed. The result is applied as classes on
// <html> (`oma-no-<feature>`), which `assets/styles/viewer.css` uses for the
// CSS fallbacks, and is read by the services that need it.
import { setIntlStyleSupported } from './time';
import { devNote } from './devlog';

export interface EngineFeatures {
  /** `overflow: clip`; fallback `overflow: hidden`. */
  overflowClip: boolean;
  /** `unicode-bidi: isolate`; fallback: `<bdi>` alone. */
  bidiIsolate: boolean;
  /** `hourCycle`, `dateStyle`, `timeStyle`; fallback: fixed ISO-like format. */
  intlDateStyle: boolean;
  /** `navigator.clipboard.writeText`; fallback `execCommand('copy')`. */
  clipboardWrite: boolean;
  /** `HTMLDialogElement.showModal`; required (an "engine too old" page). */
  dialogModal: boolean;
  /** Native scroll anchoring (Chromium). The feed turns it off either way. */
  overflowAnchor: boolean;
}

/** What detection reads; injectable so tests can describe any engine. */
export interface FeatureProbe {
  cssSupports: (property: string, value: string) => boolean;
  intlDateStyle: () => boolean;
  clipboardWrite: () => boolean;
  dialogModal: () => boolean;
}

export const browserProbe: FeatureProbe = {
  cssSupports: (property, value) =>
    typeof CSS !== 'undefined' && typeof CSS.supports === 'function'
      ? CSS.supports(property, value)
      : false,
  intlDateStyle: () => {
    try {
      const resolved = new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
        hourCycle: 'h23',
      }).resolvedOptions() as Intl.ResolvedDateTimeFormatOptions & {
        dateStyle?: string;
        timeStyle?: string;
      };
      return resolved.dateStyle === 'medium' && resolved.timeStyle === 'short';
    } catch {
      return false;
    }
  },
  clipboardWrite: () =>
    typeof navigator !== 'undefined' &&
    typeof navigator.clipboard?.writeText === 'function',
  dialogModal: () =>
    typeof HTMLDialogElement !== 'undefined' &&
    typeof HTMLDialogElement.prototype.showModal === 'function',
};

export function detectFeatures(probe: FeatureProbe = browserProbe): EngineFeatures {
  return {
    overflowClip: probe.cssSupports('overflow', 'clip'),
    bidiIsolate: probe.cssSupports('unicode-bidi', 'isolate'),
    intlDateStyle: probe.intlDateStyle(),
    clipboardWrite: probe.clipboardWrite(),
    dialogModal: probe.dialogModal(),
    overflowAnchor: probe.cssSupports('overflow-anchor', 'auto'),
  };
}

/** The `<html>` class for each missing feature with a CSS fallback. */
const DEGRADED_CLASS: Partial<Record<keyof EngineFeatures, string>> = {
  overflowClip: 'oma-no-overflow-clip',
  bidiIsolate: 'oma-no-bidi-isolate',
};

let current: EngineFeatures | null = null;

/** The features detected at startup (detected now if not yet). */
export function engineFeatures(): EngineFeatures {
  current ??= detectFeatures();
  return current;
}

/**
 * Detect once at startup and apply the degraded paths. Returns the result,
 * so `main.ts` can show the "engine too old" page when `dialogModal` is
 * missing.
 */
export function applyFeatures(
  features: EngineFeatures = detectFeatures(),
  root: HTMLElement = document.documentElement,
): EngineFeatures {
  current = features;
  let missing = 0;
  for (const [key, cls] of Object.entries(DEGRADED_CLASS) as [keyof EngineFeatures, string][]) {
    root.classList.toggle(cls, !features[key]);
  }
  for (const key of [
    'overflowClip',
    'bidiIsolate',
    'intlDateStyle',
    'clipboardWrite',
    'dialogModal',
  ] as const) {
    if (!features[key]) missing += 1;
  }
  setIntlStyleSupported(features.intlDateStyle);
  if (missing > 0) devNote('featureDegraded', missing);
  return features;
}

/** For tests: forget the detected features. */
export function resetFeaturesForTests(): void {
  current = null;
}

/**
 * Copy plain text on an explicit user action, through the degraded path
 * when `navigator.clipboard` is missing. The text is never logged.
 */
export async function copyText(text: string): Promise<boolean> {
  if (engineFeatures().clipboardWrite) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Fall through to the degraded path.
    }
  }
  const area = document.createElement('textarea');
  area.value = text;
  area.readOnly = true;
  area.className = 'oma-copy-buffer';
  document.body.append(area);
  try {
    area.select();
    return document.execCommand('copy');
  } catch {
    return false;
  } finally {
    area.remove();
  }
}
