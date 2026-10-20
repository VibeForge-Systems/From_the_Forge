// The pure half of `components/viewer/SafeText.vue` (M5 §8.1): it splits a
// content string into text runs and visible tokens for the characters that
// can hide or reorder text (Trojan Source, CVE-2021-42574). It only
// changes the display; the stored value is never altered.
//
// The classes mirror the Rust classifiers that set `ContentText.bidiControls`
// and `ContentText.invisible` (§3.8), plus C0 and C1 controls.

import type { ContentTextDto, UntrustedText } from '@/types';

/** One run of a rendered value: text, or a visible token for one character. */
export type SafeRun = { kind: 'text'; text: string } | { kind: 'token'; label: string };

export function isBidiControl(cp: number): boolean {
  return (
    cp === 0x061c ||
    cp === 0x200e ||
    cp === 0x200f ||
    (cp >= 0x202a && cp <= 0x202e) ||
    (cp >= 0x2066 && cp <= 0x2069)
  );
}

export function isInvisible(cp: number): boolean {
  return (
    (cp >= 0x200b && cp <= 0x200d) ||
    cp === 0x2060 ||
    cp === 0xfeff ||
    cp === 0xfffc ||
    (cp >= 0xe0000 && cp <= 0xe007f)
  );
}

export function isVariationSelector(cp: number): boolean {
  return (cp >= 0xfe00 && cp <= 0xfe0f) || (cp >= 0xe0100 && cp <= 0xe01ef);
}

/** C0 and C1 controls; tab and line breaks are kept in bodies. */
export function isControl(cp: number, keepLayout: boolean): boolean {
  if (keepLayout && (cp === 0x09 || cp === 0x0a || cp === 0x0d)) return false;
  return cp < 0x20 || (cp >= 0x7f && cp <= 0x9f);
}

/** "⟨U+202E⟩". */
export function tokenLabel(cp: number): string {
  return `⟨U+${cp.toString(16).toUpperCase().padStart(4, '0')}⟩`;
}

/**
 * Whether a run of variation selectors is long enough to be suspicious
 * (more than 4), as in the Rust classifier.
 */
export const VS_RUN_LIMIT = 4;

/**
 * Split `text` into runs, turning every bidi control, invisible character,
 * control character and over-long variation-selector run into a visible
 * token. `layout: 'body'` keeps tabs and line breaks as text.
 */
export function visibleRuns(text: string, layout: 'identifier' | 'body'): SafeRun[] {
  const runs: SafeRun[] = [];
  const chars = Array.from(text);
  let buffer = '';
  const flush = () => {
    if (buffer) runs.push({ kind: 'text', text: buffer });
    buffer = '';
  };
  for (let i = 0; i < chars.length; i += 1) {
    const ch = chars[i]!;
    const cp = ch.codePointAt(0)!;
    if (isVariationSelector(cp)) {
      let end = i;
      while (end < chars.length && isVariationSelector(chars[end]!.codePointAt(0)!)) end += 1;
      const run = chars.slice(i, end);
      if (run.length > VS_RUN_LIMIT) {
        flush();
        for (const vs of run) runs.push({ kind: 'token', label: tokenLabel(vs.codePointAt(0)!) });
      } else {
        buffer += run.join('');
      }
      i = end - 1;
      continue;
    }
    if (isBidiControl(cp) || isInvisible(cp) || isControl(cp, layout === 'body')) {
      flush();
      runs.push({ kind: 'token', label: tokenLabel(cp) });
      continue;
    }
    buffer += ch;
  }
  flush();
  return runs;
}

/** Whether the string holds anything `visibleRuns` would turn into a token. */
export function hasHiddenCharacters(text: string): boolean {
  return visibleRuns(text, 'body').some((r) => r.kind === 'token');
}

/**
 * A name Rust sent as bare `UntrustedText` (the export's `displayName`),
 * with the facts `SafeText` shows its badges from, computed here with the
 * same classifiers. Render it with `SafeText` in `identifier` mode.
 */
export function untrustedContent(text: UntrustedText): ContentTextDto {
  const cps = Array.from(text, (c) => c.codePointAt(0) ?? 0);
  return {
    text,
    lossy: false,
    storedAsBlob: false,
    truncated: false,
    fullUtf16: text.length,
    bidiControls: cps.some(isBidiControl),
    invisible: cps.some(isInvisible),
  };
}
