// The export's words against the viewer's (M6 spec §4.5, D129), slice 6d.
// `src/types/generated/export-words.json` is written by
// `crates/oma-export/tests/words.rs` from the Rust tables the export
// formats use. Every entry that names a viewer key must say exactly what
// `viewerText.ts` says there; export-only sentences (viewer null) are
// worded for a third-party reader and never say "You".
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import * as viewerText from '@/services/viewerText';

interface Entry {
  token: string;
  code: number;
  sentence: string;
  viewer: string | null;
}

interface Doc {
  format: string;
  tables: { name: string; registry: string; entries: Entry[] }[];
  phrases: { key: string; sentence: string; viewer: string | null }[];
}

// Relative to the app directory, where Vitest runs.
const doc = JSON.parse(readFileSync('src/types/generated/export-words.json', 'utf8')) as Doc;

/** `TABLE.key` or `CONSTANT` in viewerText.ts. */
function viewerString(path: string): unknown {
  const [name, key] = path.split('.') as [string, string | undefined];
  const value = (viewerText as Record<string, unknown>)[name];
  if (key === undefined) return value;
  return (value as Record<string, unknown> | undefined)?.[key];
}

const all = [
  ...doc.tables.flatMap((t) => t.entries.map((e) => [`${t.name}.${e.token}`, e.sentence, e.viewer] as const)),
  ...doc.phrases.map((p) => [`phrase.${p.key}`, p.sentence, p.viewer] as const),
];

describe('export words', () => {
  it('is the generated file', () => {
    expect(doc.format).toBe('oma-export-words');
    expect(doc.tables.length).toBeGreaterThanOrEqual(39);
  });

  it('matches viewerText.ts wherever it names a viewer key', () => {
    let compared = 0;
    for (const [where, sentence, viewer] of all) {
      if (viewer === null) continue;
      expect(viewerString(viewer), `${where} -> ${viewer}`).toBe(sentence);
      compared += 1;
    }
    expect(compared).toBeGreaterThan(200);
  });

  it('never says "You" and never calls a conversation a group', () => {
    for (const [where, sentence] of all) {
      expect(/\byou(r)?\b/i.test(sentence), where).toBe(false);
      const words = sentence.replace(/\bgroup_\w+/g, '');
      expect(/\bgroups?\b/i.test(words), where).toBe(false);
    }
  });

  it('keeps every code unique within its table', () => {
    for (const t of doc.tables) {
      const codes = t.entries.map((e) => e.code);
      expect(new Set(codes).size, t.name).toBe(codes.length);
    }
  });
});
