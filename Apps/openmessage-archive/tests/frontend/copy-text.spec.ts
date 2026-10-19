// The message menu's copy rules (M5 §9.3, D107): what each copy action puts
// on the clipboard, the link guard, and the menu's default item.
import { describe, expect, it } from 'vitest';
import {
  bodyText,
  linkRuns,
  menuItems,
  utcOffset,
  withSenderAndTime,
  withoutHiddenCharacters,
} from '@/services/copyText';
import type { BodyViewDto, MessageDto, SegmentDto } from '@/types';
import { attachment, content, message, sid, text, textPiece } from './helpers';

function seg(piece: SegmentDto['piece'], flags: Partial<Omit<SegmentDto, 'piece'>> = {}): SegmentDto {
  return {
    piece,
    bold: false,
    italic: false,
    underline: false,
    strikethrough: false,
    mention: false,
    transcription: false,
    oneTimeCode: false,
    url: false,
    urlHidden: false,
    ...flags,
  };
}

const words = (s: string) => seg({ kind: 'text', value: content(s) });

function withBody(segments: SegmentDto[], body: Partial<Extract<BodyViewDto, { kind: 'text' }>> = {}, m: Partial<MessageDto> = {}): MessageDto {
  const base = message(7, m);
  return {
    ...base,
    body: { ...(base.body as Extract<BodyViewDto, { kind: 'text' }>), segments, ...body },
  };
}

describe('bodyText', () => {
  it('copies the text as shown: codes masked, placeholders named, nothing withheld', () => {
    const m = withBody(
      [
        words('Your code is '),
        seg({ kind: 'withheld', mask: 'oneTimeCode' }, { oneTimeCode: true }),
        words('. See '),
        seg({ kind: 'attachment', attachment: sid(31) }),
        seg({ kind: 'inlineEmoji' }),
        seg({ kind: 'unmatchedPlaceholder' }),
      ],
      {},
      {
        attachments: [attachment(30), attachment(31)],
        edit: { ...message(7).edit, earlier: { kind: 'withheld', reason: 'unsent' } },
      },
    );
    expect(bodyText(m)).toBe(
      'Your code is [one-time code hidden]. See [attachment 2][inline emoji image not shown][attachment placeholder not matched to a file]',
    );
  });

  it('copies a revealed code, says when the text was cut, and prefers the full text once read', () => {
    const revealed = withBody([words('Code 123456')], { truncated: true });
    expect(bodyText(revealed)).toBe('Code 123456\n[the rest of this text was not loaded]');
    const full = { pieces: [textPiece('Code 123456 and the rest'), { kind: 'masked' as const, mask: 'oneTimeCode' as const }], end: 30, full: 30 };
    expect(bodyText(revealed, full)).toBe('Code 123456 and the rest[one-time code hidden]');
    expect(bodyText(revealed, { ...full, end: 20 })).toContain('[the rest of this text was not loaded]');
  });

  it('has nothing to copy without text', () => {
    expect(bodyText(message(1, { body: { kind: 'noBody' } }))).toBeNull();
  });
});

describe('withoutHiddenCharacters', () => {
  it('drops direction controls, zero-width and control characters', () => {
    expect(withoutHiddenCharacters('pay​pal ‮gnp.exe‬ a\u0007b')).toBe('paypal gnp.exe ab');
    expect(withoutHiddenCharacters('a⁦b⁩c﻿￼d⁠')).toBe('abcd');
  });

  it('keeps tabs, line breaks, emoji sequences, joining scripts and flag tags', () => {
    expect(withoutHiddenCharacters('a\tb\nc')).toBe('a\tb\nc');
    const family = '👩‍👩‍👧';
    expect(withoutHiddenCharacters(family)).toBe(family);
    const heart = '❤️‍🔥';
    expect(withoutHiddenCharacters(heart)).toBe(heart);
    const coder = '👩🏽‍💻';
    expect(withoutHiddenCharacters(coder)).toBe(coder);
    const persian = 'می‌خواهم';
    expect(withoutHiddenCharacters(persian)).toBe(persian);
    const england = '🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}';
    expect(withoutHiddenCharacters(england)).toBe(england);
  });

  it('drops joiners between ASCII letters, stray tags and long selector runs', () => {
    expect(withoutHiddenCharacters('ab‍cd‌e')).toBe('abcde');
    expect(withoutHiddenCharacters('x\u{E0067}y')).toBe('xy');
    expect(withoutHiddenCharacters(`a${'️'.repeat(6)}b`)).toBe('ab');
    expect(withoutHiddenCharacters('☺️')).toBe('☺️');
  });
});

describe('withSenderAndTime', () => {
  const handles = new Map([[sid(1), { id: sid(1), address: text('+1 555 0100'), service: 'iMessage' as const }]]);

  it('names the sender, the time and the zone used, then the qualifiers and the text', () => {
    const m = withBody([words('hello')], {}, {
      edit: { ...message(7).edit, editedParts: ['0' as never], partCount: 1, label: 'editHistory' },
      labels: [{ kind: 'undated' }],
    });
    const utc = withSenderAndTime(m, 'hello', { handles, prefs: { timeFormat: 'h23', timeZone: 'utc', showSeconds: false } });
    const lines = utc.split('\n');
    expect(lines[0]).toMatch(/^\+1 555 0100 — .+ \(times in UTC\)$/);
    expect(lines).toContain('[Edited]');
    expect(lines).toContain('[Undated (derived): no usable date is recorded]');
    expect(lines.at(-1)).toBe('hello');
    const local = withSenderAndTime(m, 'hello', { handles, prefs: { timeFormat: 'h23', timeZone: 'system', showSeconds: false } });
    expect(local.split('\n')[0]).toContain(`(times in ${utcOffset(m.date.ms!)}, this computer's zone)`);
  });

  it('keeps the qualifiers that say what is not recorded', () => {
    const m = withBody([words('x')], {}, {
      sender: { kind: 'noSender' },
      direction: 'notRecorded',
      edit: { ...message(7).edit, unsentParts: ['0' as never], partCount: 1, unsentWhole: true, label: 'unsentParts' },
    });
    const copied = withSenderAndTime(m, 'x', { handles, prefs: { timeFormat: 'system', timeZone: 'utc', showSeconds: false } });
    expect(copied.split('\n')[0]).toMatch(/^No sender recorded — /);
    expect(copied).toContain('[Direction not recorded]');
    expect(copied).toContain('[Unsent by the sender]');
    const undated = withSenderAndTime(
      { ...m, date: { ms: null, raw: null, unit: null, state: 'null', before2007: false } },
      'x',
      { handles, prefs: { timeFormat: 'system', timeZone: 'utc', showSeconds: false } },
    );
    expect(undated.split('\n')[0]).not.toContain('times in');
  });

  it('formats this computer’s offset', () => {
    expect(utcOffset(Date.UTC(2026, 0, 1))).toMatch(/^UTC[+-]\d{2}:\d{2}$/);
  });
});

describe('links and the menu', () => {
  const url = (s: string, flags: Partial<Omit<SegmentDto, 'piece'>> = {}) =>
    seg({ kind: 'text', value: content(s) }, { url: true, ...flags });

  it('finds URL runs and refuses hidden or incomplete ones', () => {
    const m = withBody([
      words('see '),
      url('https://example.invalid/a'),
      words(' and '),
      url('https://exa​mple.invalid', { urlHidden: true }),
      words(' or '),
      url('https://cut.invalid/', { }),
    ], { truncated: true });
    const runs = linkRuns(m);
    expect(runs.map((r) => [r.text, r.hidden, r.complete])).toEqual([
      ['https://example.invalid/a', false, true],
      ['https://exa​mple.invalid', true, true],
      ['https://cut.invalid/', false, false],
    ]);
    // Hidden characters the flag missed are still caught.
    const sneaky = withBody([url('https://a‮.invalid')]);
    expect(linkRuns(sneaky)[0]!.hidden).toBe(true);
    // A masked part makes a run incomplete; consecutive URL segments are one run.
    const joined = withBody([url('https://a.invalid/'), url('b', { bold: true })]);
    expect(linkRuns(joined).map((r) => r.text)).toEqual(['https://a.invalid/b']);
    const masked = withBody([url('https://a.invalid/?c='), seg({ kind: 'withheld', mask: 'oneTimeCode' }, { url: true })]);
    expect(linkRuns(masked)[0]!.complete).toBe(false);
  });

  it('offers copy without invisible characters first when the body holds them', () => {
    const plain = menuItems(withBody([words('hi')])).map((i) => i.action.kind);
    expect(plain).toEqual(['copyText', 'copyClean', 'copyWithSender', 'details']);
    const hidden = menuItems(withBody([words('hi‮')], { bidiControls: true })).map((i) => i.action.kind);
    expect(hidden.slice(0, 2)).toEqual(['copyClean', 'copyText']);
    const invisible = menuItems(withBody([words('hi')], { invisible: true })).map((i) => i.label);
    expect(invisible[0]).toBe('Copy text without invisible characters');
  });

  it('lists one item per link and only Details without text', () => {
    const one = menuItems(withBody([url('https://a.invalid')])).map((i) => i.label);
    expect(one).toContain('Copy link…');
    const two = menuItems(withBody([url('https://a.invalid'), words(' '), url('https://b.invalid')])).map((i) => i.label);
    expect(two).toEqual(expect.arrayContaining(['Copy link 1 of 2…', 'Copy link 2 of 2…']));
    expect(menuItems(message(1, { body: { kind: 'noBody' } })).map((i) => i.action.kind)).toEqual(['details']);
  });
});
