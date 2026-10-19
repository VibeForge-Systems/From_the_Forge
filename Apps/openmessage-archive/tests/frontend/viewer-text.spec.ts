// @vitest-environment node
import { readFileSync, readdirSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import * as words from '@/services/viewerText';

/** Every exported table (a plain object of strings). */
const TABLES = Object.entries(words).filter(
  (e): e is [string, Record<string, string>] =>
    typeof e[1] === 'object' && e[1] !== null && Object.values(e[1]).every((v) => typeof v === 'string'),
);

/** Values that are deliberately empty: the label is the content itself. */
const EMPTY_ON_PURPOSE = new Set([
  'PREVIEW_BODY_TEXT.text',
  'CHAT_LABEL_TEXT.name',
  'CHAT_LABEL_TEXT.identifier',
  'VIEWER_ERROR_TEXT.searchSuperseded',
]);

/** Read, delivery and sent state (D94), as keys. */
const STATUS_KEY =
  /^(is)?(read|delivered|sent|played)$|isRead|isDelivered|isSent|dateRead|dateDelivered|datePlayed|lastRead|receipt|^status$/i;
/** ... and as sentences. */
const STATUS_WORDS = [
  /\bdeliver(?:ed|y)\b/i,
  /\bread (?:by|at|receipt)/i,
  /\b(?:is|was|been|were) (?:not )?(?:read|sent)\b/i,
  /\bunread\b/i,
  // "Not seen by the newest import" is about backups, not people.
  /\b(?:seen|opened) by (?:the )?(?:recipient|sender|you|them)\b/i,
  /\b(?:was|been) played\b/i,
  /\bfailed to send\b/i,
];

describe('viewerText', () => {
  it('exports the code tables', () => {
    expect(TABLES.length).toBeGreaterThan(40);
  });

  it('gives every code non-empty words, except the content-labelled ones', () => {
    for (const [name, table] of TABLES) {
      for (const [key, value] of Object.entries(table)) {
        if (EMPTY_ON_PURPOSE.has(`${name}.${key}`)) continue;
        expect(value.trim(), `${name}.${key}`).not.toBe('');
      }
    }
  });

  it('maps no read, delivery or sent state, by key or by sentence (D94)', () => {
    for (const [name, table] of TABLES) {
      for (const [key, value] of Object.entries(table)) {
        expect(STATUS_KEY.test(key), `${name}.${key}`).toBe(false);
        for (const pattern of STATUS_WORDS) {
          expect(pattern.test(value), `${name}.${key}: ${value}`).toBe(false);
        }
      }
    }
  });

  it('never calls an edit code "not edited" and never calls codes 6 and 7 "unsent"', () => {
    for (const value of Object.values(words.EDIT_LABEL_TEXT)) {
      expect(value.toLowerCase()).not.toContain('not edited');
    }
    expect(words.EDIT_LABEL_TEXT.dateEditedNoBody.toLowerCase()).not.toContain('unsent');
    expect(words.EDIT_LABEL_TEXT.dateEditedOnly.toLowerCase()).not.toContain('unsent');
  });

  it('marks derived labels as derived', () => {
    expect(words.CHIP_STATE_TEXT.shown).toContain('derived');
    expect(words.EDIT_LABEL_TEXT.editHistory).toContain('derived');
    expect(words.MESSAGE_LABEL_TEXT.notInLatest).toContain('derived');
    expect(words.CHIP_STATE_TEXT.undeterminable).toBe('cannot be determined');
  });

  it('fills placeholders with closed values only', () => {
    expect(words.fill(words.TIME_UNIT_TEXT.negative, { raw: '-5' })).toBe('Negative date (raw -5)');
    expect(words.fill('{a} and {b}', { a: 1 })).toBe('1 and {b}');
  });
});

describe('the viewer DTO types', () => {
  it('have no read, delivery or sent field (secondary to the Rust guard)', () => {
    const index = readFileSync('src/types/index.ts', 'utf8');
    const m5 = index.slice(index.indexOf('// Milestone 5'));
    const names = [...m5.matchAll(/generated\/(\w+)'/g)].map((m) => m[1]!);
    expect(names.length).toBeGreaterThan(100);
    const available = new Set(readdirSync('src/types/generated').map((f) => f.replace(/\.ts$/, '')));
    for (const name of names) {
      expect(available.has(name), name).toBe(true);
      const source = readFileSync(`src/types/generated/${name}.ts`, 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
      const keys = [...source.matchAll(/(?:[{,]\s*)"?(\w+)"?\??:/g)].map((k) => k[1]!);
      for (const key of keys) expect(STATUS_KEY.test(key), `${name}.${key}`).toBe(false);
    }
  });
});

describe('viewerText (review fixes)', () => {
  it('never calls a conversation a group', () => {
    for (const [name, table] of TABLES) {
      for (const [key, value] of Object.entries(table)) {
        // Raw column names (group_title, group_action_type) are not words.
        const words = value.replace(/\bgroup_\w+/g, '');
        expect(/\bgroups?\b/i.test(words), `${name}.${key}: ${value}`).toBe(false);
      }
    }
  });

  it('words the edit codes neutrally: never a whole unsend, never "partly"', () => {
    for (const value of Object.values(words.EDIT_LABEL_TEXT)) {
      expect(value).not.toBe(words.BUBBLE_TEXT.unsentWhole);
      expect(value.toLowerCase()).not.toContain('partly');
      expect(value.startsWith(words.BUBBLE_TEXT.unsentWhole)).toBe(false);
    }
  });

  it('never says a NULL service is unrecognised', () => {
    expect(words.BUBBLE_TEXT.serviceNotRecorded).toBe('Service not recorded or unreadable');
    expect(Object.values(words.BUBBLE_TEXT).join(' ')).not.toContain('not recognised by this version');
  });
});
