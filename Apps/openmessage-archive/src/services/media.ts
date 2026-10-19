// Media URLs and the media-element rules of the viewer (M5 §5.6, §6.3).
//
// `mediaUrl` is the only permitted source of a `src` binding under `src/`
// (the source rule in `tests/frontend/source-rules.spec.ts` fails on any
// other). It builds `<mediaBase>v1/<token>/<rendition>` from the base Rust
// sent in `ViewerSessionDto` and a token from a page DTO, and refuses
// anything else, so no URL is ever built from content, an id or a path.
import type { MediaBase, MediaPlayback, MediaToken, SniffedKindDto } from '@/types';

/** The renditions the `oma-media` protocol serves. */
export type Rendition = 'thumb' | 'display' | 'stream';

const RENDITIONS: readonly Rendition[] = ['thumb', 'display', 'stream'];
const TOKEN = /^[0-9a-f]{32}$/;
/** The two bases Rust can send (§6.2); anything else is refused. */
const BASES = new Set(['oma-media://localhost/', 'http://oma-media.localhost/']);

export function isMediaToken(value: unknown): value is MediaToken {
  return typeof value === 'string' && TOKEN.test(value);
}

/**
 * The URL of one rendition of a surfaced attachment, or `null` when the
 * base, the token or the rendition is not exactly what the protocol
 * accepts. A `null` renders the attachment's card, never a request.
 */
export function mediaUrl(base: MediaBase, token: MediaToken, rendition: Rendition): string | null {
  if (typeof base !== 'string' || !BASES.has(base)) return null;
  if (!isMediaToken(token)) return null;
  if (!RENDITIONS.includes(rendition)) return null;
  return `${base}v1/${token}/${rendition}`;
}

/**
 * Attributes every `<audio>` and `<video>` element carries (§5.6): no
 * download, picture-in-picture or remote playback, and nothing loads until
 * playback is enabled. They are constants, bound as a whole only by the
 * media components.
 */
export const MEDIA_ELEMENT_ATTRIBUTES = {
  controlsList: 'nodownload noremoteplayback',
  disablePictureInPicture: true,
  disableRemotePlayback: true,
  autoplay: false,
} as const;

/** `preload` before and after the user enabled playback of a file. */
export function preloadFor(enabled: boolean): 'none' | 'metadata' {
  return enabled ? 'metadata' : 'none';
}

/** The MIME type `canPlayType` is asked about for a sniffed container. */
const PLAYABLE_TYPE: Partial<Record<SniffedKindDto, string>> = {
  mp4: 'video/mp4',
  m4v: 'video/mp4',
  quickTime: 'video/quicktime',
  m4a: 'audio/mp4',
  webm: 'video/webm',
  wav: 'audio/wav',
  aiff: 'audio/aiff',
  ogg: 'audio/ogg',
  mp3: 'audio/mpeg',
  caf: 'audio/x-caf',
};

/** The type to ask `canPlayType` about, or `null` for a non-playable kind. */
export function playableType(kind: SniffedKindDto): string | null {
  return PLAYABLE_TYPE[kind] ?? null;
}

/** Whether a media element of this engine might play the sniffed kind. */
export function engineMayPlay(
  kind: SniffedKindDto,
  probe: (type: string) => string = defaultCanPlayType,
): boolean {
  const type = playableType(kind);
  if (type === null) return false;
  return probe(type) !== '';
}

function defaultCanPlayType(type: string): string {
  if (typeof document === 'undefined') return '';
  const el = document.createElement(type.startsWith('audio/') ? 'audio' : 'video');
  return typeof el.canPlayType === 'function' ? el.canPlayType(type) : '';
}

/**
 * What a Play button does under the user's `mediaPlayback` setting:
 * `none` (playback is off: no element is created), `confirm` (the
 * confirmation dialog first, once per file per session) or `play`.
 */
export function playGate(
  setting: MediaPlayback,
  confirmedThisSession: boolean,
): 'none' | 'confirm' | 'play' {
  if (setting === 'off') return 'none';
  if (setting === 'confirm' && !confirmedThisSession) return 'confirm';
  return 'play';
}

/**
 * The delay before retrying a failed rendition load: 0.5 s, 1 s, 2 s, then
 * `null` (show the card). Retries happen only while the item is visible.
 */
export function retryDelayMs(attempt: number): number | null {
  const delays = [500, 1000, 2000];
  return Number.isInteger(attempt) && attempt >= 0 && attempt < delays.length
    ? delays[attempt]!
    : null;
}

/** The neutral "now playing" metadata (§5.6): never a file name. */
export const NEUTRAL_MEDIA_TITLE = 'OpenMessage Archive';
