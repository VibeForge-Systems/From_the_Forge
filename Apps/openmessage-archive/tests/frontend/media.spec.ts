import { describe, expect, it } from 'vitest';
import {
  MEDIA_ELEMENT_ATTRIBUTES,
  NEUTRAL_MEDIA_TITLE,
  engineMayPlay,
  isMediaToken,
  mediaUrl,
  playGate,
  playableType,
  preloadFor,
  retryDelayMs,
} from '@/services/media';
import type { MediaBase, MediaToken } from '@/types';
import { MEDIA_BASE, TOKEN } from './helpers';

const base = (s: string) => s as MediaBase;
const token = (s: string) => s as MediaToken;

describe('mediaUrl', () => {
  it('builds the protocol URL on both platforms', () => {
    expect(mediaUrl(MEDIA_BASE, TOKEN, 'thumb')).toBe(`oma-media://localhost/v1/${'a'.repeat(32)}/thumb`);
    expect(mediaUrl(base('http://oma-media.localhost/'), TOKEN, 'stream')).toBe(
      `http://oma-media.localhost/v1/${'a'.repeat(32)}/stream`,
    );
  });

  it('rejects bad tokens', () => {
    for (const bad of ['', 'A'.repeat(32), 'a'.repeat(31), 'a'.repeat(33), `${'a'.repeat(31)}/`, '../x', 'g'.repeat(32)]) {
      expect(isMediaToken(bad)).toBe(false);
      expect(mediaUrl(MEDIA_BASE, token(bad), 'thumb')).toBeNull();
    }
  });

  it('rejects any other base or rendition', () => {
    for (const b of ['https://example.com/', 'oma-media://localhost', 'file:///', 'asset://localhost/']) {
      expect(mediaUrl(base(b), TOKEN, 'thumb')).toBeNull();
    }
    expect(mediaUrl(MEDIA_BASE, TOKEN, 'original' as 'thumb')).toBeNull();
  });
});

describe('media elements', () => {
  it('never autoplay, never download, never go picture-in-picture or remote', () => {
    expect(MEDIA_ELEMENT_ATTRIBUTES).toEqual({
      controlsList: 'nodownload noremoteplayback',
      disablePictureInPicture: true,
      disableRemotePlayback: true,
      autoplay: false,
    });
    expect(preloadFor(false)).toBe('none');
    expect(preloadFor(true)).toBe('metadata');
    expect(NEUTRAL_MEDIA_TITLE).toBe('OpenMessage Archive');
  });

  it('asks canPlayType about the sniffed container, and shows a card for ""', () => {
    expect(playableType('quickTime')).toBe('video/quicktime');
    expect(playableType('pdf')).toBeNull();
    const asked: string[] = [];
    const probe = (t: string) => {
      asked.push(t);
      return t === 'video/quicktime' ? 'maybe' : '';
    };
    expect(engineMayPlay('quickTime', probe)).toBe(true);
    expect(engineMayPlay('caf', probe)).toBe(false);
    expect(engineMayPlay('png', probe)).toBe(false);
    expect(asked).toEqual(['video/quicktime', 'audio/x-caf']);
  });

  it('gates Play by the setting: no element when off, a dialog under confirm', () => {
    expect(playGate('off', false)).toBe('none');
    expect(playGate('off', true)).toBe('none');
    expect(playGate('confirm', false)).toBe('confirm');
    expect(playGate('confirm', true)).toBe('play');
    expect(playGate('clickToPlay', false)).toBe('play');
  });

  it('backs off retries, then gives up', () => {
    expect([0, 1, 2, 3, -1, 1.5].map(retryDelayMs)).toEqual([500, 1000, 2000, null, null, null]);
  });
});
