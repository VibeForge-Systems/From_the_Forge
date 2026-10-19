import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import SafeText from '@/components/viewer/SafeText.vue';
import { hasHiddenCharacters, tokenLabel, visibleRuns } from '@/services/safeText';
import { CONTENT_BADGE_TEXT, MASK_TEXT, VALUE_STATE_TEXT } from '@/services/viewerText';
import type { OptContentDto, ValueStateDto } from '@/types';
import { content, missing, textPiece } from './helpers';

const RLO = '‮';
const LRI = '⁦';
const RLI = '⁧';
const PDI = '⁩';
const ZWJ = '‍';
const ZWSP = '​';
const OBJ = '￼';

describe('visibleRuns', () => {
  it('turns direction controls and invisible characters into tokens', () => {
    const runs = visibleRuns(`a${RLO}b${LRI}c${RLI}d${PDI}e${ZWJ}f${ZWSP}g${OBJ}`, 'identifier');
    expect(runs.filter((r) => r.kind === 'token').map((r) => r.kind === 'token' && r.label)).toEqual([
      '⟨U+202E⟩',
      '⟨U+2066⟩',
      '⟨U+2067⟩',
      '⟨U+2069⟩',
      '⟨U+200D⟩',
      '⟨U+200B⟩',
      '⟨U+FFFC⟩',
    ]);
    expect(runs.filter((r) => r.kind === 'text').map((r) => r.kind === 'text' && r.text).join('')).toBe(
      'abcdefg',
    );
  });

  it('keeps line breaks in bodies but not in identifiers', () => {
    expect(visibleRuns('a\nb', 'body')).toEqual([{ kind: 'text', text: 'a\nb' }]);
    expect(visibleRuns('a\nb', 'identifier')).toContainEqual({ kind: 'token', label: '⟨U+000A⟩' });
  });

  it('keeps short variation-selector runs and tokenises long ones', () => {
    expect(visibleRuns('❤️', 'identifier')).toEqual([{ kind: 'text', text: '❤️' }]);
    const long = visibleRuns(`x${'️'.repeat(5)}`, 'identifier');
    expect(long.filter((r) => r.kind === 'token')).toHaveLength(5);
  });

  it('tokenises tag characters and astral code points correctly', () => {
    expect(tokenLabel(0xe0041)).toBe('⟨U+E0041⟩');
    expect(hasHiddenCharacters('a\u{E0041}')).toBe(true);
    expect(hasHiddenCharacters('plain 😀 text')).toBe(false);
  });
});

describe('SafeText', () => {
  it('renders markup as text and creates no element', () => {
    for (const hostile of ['<script>alert(1)</script>', '<img src=x onerror=alert(1)>', '{{ 7*7 }}']) {
      const w = mount(SafeText, { props: { value: content(hostile) } });
      expect(w.text()).toContain(hostile);
      expect(w.find('script').exists()).toBe(false);
      expect(w.find('img').exists()).toBe(false);
      expect(w.text()).not.toContain('49');
      expect(w.findAll('bdi')).toHaveLength(1);
      expect(w.find('bdi').element.children).toHaveLength(0);
    }
  });

  it('never puts content into an attribute', () => {
    const value = 'sentinel-attr-value';
    const w = mount(SafeText, { props: { value: content(value), mode: 'identifier', ltr: true } });
    for (const el of [w.element, ...w.element.querySelectorAll('*')]) {
      for (const attr of Array.from((el as Element).attributes ?? [])) {
        expect(attr.value).not.toContain(value);
      }
    }
  });

  it('shows hidden characters as tokens in identifier mode, each in its own span', () => {
    const w = mount(SafeText, {
      props: { value: content(`ab${RLO}cd${ZWJ}`, { bidiControls: true, invisible: true }), mode: 'identifier' },
    });
    const tokens = w.findAll('.oma-cp').map((t) => t.text());
    expect(tokens).toEqual(['⟨U+202E⟩', '⟨U+200D⟩']);
    expect(w.find('bdi').text()).not.toContain(RLO);
    expect(w.text()).toContain(CONTENT_BADGE_TEXT.hidden);
  });

  it('forces addresses left-to-right and lets other values choose', () => {
    expect(mount(SafeText, { props: { value: content('a'), ltr: true } }).find('bdi').attributes('dir')).toBe(
      'ltr',
    );
    expect(mount(SafeText, { props: { value: content('a') } }).find('bdi').attributes('dir')).toBe('auto');
  });

  it('keeps the characters inside one bdi in body mode, with the badge', () => {
    const text = `x${RLO}y👩${ZWJ}💻`;
    const w = mount(SafeText, { props: { value: content(text, { bidiControls: true, invisible: true }) } });
    expect(w.findAll('bdi')).toHaveLength(1);
    expect(w.find('bdi').text()).toBe(text);
    expect(w.find('.oma-cp').exists()).toBe(false);
    expect(w.text()).toContain(CONTENT_BADGE_TEXT.hidden);
  });

  it('switches bodies to visible tokens when showHidden is set', () => {
    const w = mount(SafeText, { props: { value: content(`x${RLO}y`, { bidiControls: true }), showHidden: true } });
    expect(w.find('.oma-cp').text()).toBe('⟨U+202E⟩');
  });

  it('shows the truncation marker, and the lossy badge', () => {
    const w = mount(SafeText, {
      props: { value: content('long address', { truncated: true, lossy: true }), mode: 'identifier' },
    });
    expect(w.text()).toContain(CONTENT_BADGE_TEXT.truncated);
    expect(w.text()).toContain(CONTENT_BADGE_TEXT.lossy);
  });

  it('renders each missing state as its sentence', () => {
    for (const state of ['null', 'absent', 'oversized', 'unreadable'] as ValueStateDto[]) {
      const value: OptContentDto = missing(state);
      const w = mount(SafeText, { props: { value } });
      expect(w.find('.oma-missing').text()).toBe(VALUE_STATE_TEXT[state]);
      expect(w.find('bdi').exists()).toBe(false);
    }
  });

  it('renders masked pieces as their sentence, never the text', () => {
    const w = mount(SafeText, {
      props: { pieces: [textPiece('Your code is '), { kind: 'masked', mask: 'oneTimeCode' }] },
    });
    expect(w.text()).toContain('Your code is');
    expect(w.find('.oma-masked').text()).toBe(MASK_TEXT.oneTimeCode);
  });

  it('contains zalgo text and wraps a 100,000-character token', () => {
    const zalgo = `Z${'̶̵̴'.repeat(200)}`;
    const w = mount(SafeText, { props: { value: content(zalgo) } });
    expect(w.find('bdi').text()).toBe(zalgo);
    const long = 'x'.repeat(100_000);
    const l = mount(SafeText, { props: { value: content(long) } });
    expect(l.find('bdi').text()).toHaveLength(100_000);
    expect(l.classes()).toContain('oma-safe-text');
    // The wrapping and containment rules live in viewer.css.
    const css = readFileSync('src/assets/styles/viewer.css', 'utf8');
    expect(css).toMatch(/\.oma-safe-text\s*\{[^}]*white-space:\s*pre-wrap;[^}]*overflow-wrap:\s*anywhere;/);
    expect(css).toMatch(/\.oma-contained\s*\{[^}]*contain:\s*paint;[^}]*overflow:\s*clip;/);
    expect(css).toMatch(/\.oma-safe-text bdi\s*\{[^}]*unicode-bidi:\s*isolate;/);
  });

  it('renders nothing for a null value', () => {
    const w = mount(SafeText, { props: { value: null } });
    expect(w.text()).toBe('');
  });
});
