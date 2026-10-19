// The pure card rules of slice 5h3 (M5 §5.9, §5.5, §5.6): the card comes
// from Rust's card state first, then the sniffed content, never from the
// declared type; previews need a token; Play needs playback on and an
// engine type; sizes show stored beside declared.
import { describe, expect, it } from 'vitest';
import {
  bytesText,
  cardSentence,
  cardView,
  declaredBytes,
  mediaElementFor,
  playbackSentence,
  renditionLabel,
  sizeLine,
} from '@/services/attachmentView';
import type { CardStateDto, DecimalString, SniffedKindDto } from '@/types';
import { CARD_TEXT, FILE_PROBLEM_TEXT, MEDIA_UI_TEXT, PLAYBACK_TEXT, SNIFFED_KIND_TEXT } from '@/services/viewerText';
import { TOKEN, attachmentFacts, inspection } from './helpers';

const yes = () => true;
const no = () => false;
const d = (s: string) => s as DecimalString;

function playable(kind: SniffedKindDto, playback: Parameters<typeof inspection>[1] = {}) {
  return inspection(1, {
    content: { kind: 'sniffed', sniffed: kind, class: { kind: 'playable', structureChecked: true }, storedBytes: 100 },
    ...playback,
  });
}

describe('cardView', () => {
  it('is pending until inspected', () => {
    expect(cardView(null, 'confirm', yes)).toEqual({ kind: 'pending' });
  });

  it('takes the card state from Rust first, whatever the content', () => {
    const states: CardStateDto[] = [
      { kind: 'noObservation' },
      { kind: 'notInBackup' },
      { kind: 'notFoundAtAssumedKey' },
      { kind: 'notCopied' },
      { kind: 'classNotLookedUp' },
      { kind: 'locationUnusable', lookup: 'notAttemptedNull' },
      { kind: 'notDecrypted' },
      { kind: 'couldNotBeRead' },
      { kind: 'tooLarge' },
    ];
    for (const card of states) {
      const v = cardView(inspection(1, { facts: attachmentFacts(1, { card }) }), 'confirm', yes);
      expect(v).toEqual({ kind: 'unavailable', lines: [CARD_TEXT[card.kind]] });
    }
    expect(cardSentence({ kind: 'unavailable', lookup: d('41'), copy: d('7') })).toBe(
      'Not available (lookup code 41, copy code 7)',
    );
  });

  it('says why a stored file cannot be used, including a checksum mismatch', () => {
    expect(cardView(inspection(1, { content: { kind: 'noFile' } }), 'confirm', yes)).toEqual({
      kind: 'unavailable',
      lines: ['No file is stored for this attachment'],
    });
    const v = cardView(inspection(1, { content: { kind: 'unavailable', problem: 'digestMismatch' } }), 'confirm', yes);
    expect(v).toEqual({ kind: 'unavailable', lines: [FILE_PROBLEM_TEXT.digestMismatch] });
    expect(FILE_PROBLEM_TEXT.digestMismatch).toBe('Preview unavailable: the file does not match its recorded checksum');
  });

  it('previews an image only with a token and previews on; a GIF is marked animated', () => {
    expect(cardView(inspection(1), 'confirm', yes)).toEqual({ kind: 'image', token: TOKEN, sniffed: 'jpeg', animated: false });
    expect(cardView(inspection(1, { token: null }), 'confirm', yes)).toEqual({ kind: 'file', sniffed: 'jpeg', lines: [] });
    expect(cardView(inspection(1, { previewsOff: true }), 'confirm', yes)).toEqual({
      kind: 'file',
      sniffed: 'jpeg',
      lines: [MEDIA_UI_TEXT.previewsOff],
    });
    const gif = inspection(1, { content: { kind: 'sniffed', sniffed: 'gif', class: { kind: 'image' }, storedBytes: 9 } });
    expect(cardView(gif, 'confirm', yes)).toMatchObject({ kind: 'image', animated: true });
  });

  it('shows HEIC, PDF and other kinds as file cards, and unknown bytes as unrecognised', () => {
    const heic = inspection(1, { content: { kind: 'sniffed', sniffed: 'heif', class: { kind: 'file' }, storedBytes: 9 } });
    expect(cardView(heic, 'confirm', yes)).toEqual({ kind: 'file', sniffed: 'heif', lines: [] });
    expect(SNIFFED_KIND_TEXT.heif).toBe('HEIC image: preview not available in this version');
    const odd = inspection(1, { content: { kind: 'sniffed', sniffed: 'unknown', class: { kind: 'unrecognised' }, storedBytes: 9 } });
    expect(cardView(odd, 'confirm', yes)).toEqual({ kind: 'file', sniffed: null, lines: ['Unrecognised content'] });
  });

  it('offers Play only when playback is on and the engine might play the type', () => {
    expect(cardView(playable('mp4'), 'confirm', yes)).toMatchObject({
      kind: 'playable',
      media: 'video',
      blocked: null,
      ready: false,
      structureChecked: true,
    });
    expect(cardView(playable('mp4'), 'off', yes)).toMatchObject({ blocked: PLAYBACK_TEXT.off });
    expect(cardView(playable('caf'), 'confirm', no)).toMatchObject({ media: 'audio', blocked: MEDIA_UI_TEXT.cannotPlayHere });
    const refused = playable('quickTime', { playback: { kind: 'refused', reason: 'referenceMovie' } });
    expect(cardView(refused, 'clickToPlay', yes)).toMatchObject({
      blocked: "This file can't be played in the app: its structure failed a safety check (it refers to another file or address)",
    });
    const allowed = playable('m4a', { playback: { kind: 'allowed', structureChecked: true } });
    expect(cardView(allowed, 'confirm', yes)).toMatchObject({ blocked: null, ready: true, media: 'audio' });
  });

  it('puts audio kinds in <audio> and the rest in <video>', () => {
    for (const k of ['m4a', 'wav', 'aiff', 'ogg', 'mp3', 'caf'] as const) expect(mediaElementFor(k)).toBe('audio');
    for (const k of ['mp4', 'm4v', 'quickTime', 'webm'] as const) expect(mediaElementFor(k)).toBe('video');
  });

  it('words every playback verdict', () => {
    expect(playbackSentence({ kind: 'off' })).toBe(PLAYBACK_TEXT.off);
    expect(playbackSentence({ kind: 'notPlayable' })).toBe(PLAYBACK_TEXT.notPlayable);
    expect(playbackSentence({ kind: 'unavailable', problem: 'changed' })).toBe(FILE_PROBLEM_TEXT.changed);
  });
});

describe('sizes and labels', () => {
  it('formats byte counts, exact below 1 KB', () => {
    expect(bytesText(1)).toBe('1 byte');
    expect(bytesText(900)).toBe('900 bytes');
    expect(bytesText(2048)).toMatch(/^2 KB$/);
    expect(bytesText(-1)).toBe('');
  });

  it('shows the stored size beside the declared one, and says when either is missing', () => {
    expect(sizeLine(2048, d('2048'))).toBe('2 KB stored (2 KB declared)');
    expect(sizeLine(null, d('2048'))).toBe('No file stored (2 KB declared)');
    expect(sizeLine(10, null)).toBe('10 bytes stored (no size declared)');
    expect(sizeLine(10, d('-5'))).toBe('10 bytes stored (declared value -5)');
    expect(declaredBytes(d('9999999999999999'))).toBeNull();
  });

  it('labels the preview as re-encoded, with the original size once known', () => {
    expect(renditionLabel(null, 20480, d('20480'))).toBe(
      'Preview, re-encoded and reduced to 2048 px; original size shown after the preview loads; 20 KB stored (20 KB declared)',
    );
    expect(renditionLabel({ width: 4032, height: 3024 }, 20480, d('20480'))).toContain('original 4032×3024');
  });
});
