// The pure rules of attachment cards and the details panel (M5 §5.9,
// §8.4), slice 5h3. Components render what these functions decide; new
// card rules go here, with a test in `tests/frontend/attachment-view.spec.ts`.
//
// - The card is decided by Rust first (`CardStateDto`, from the lookup and
//   copy codes of the highest-ranked observation), then by the stored
//   file's sniffed content (`ContentKindDto`) from `inspect_attachment`.
//   The declared type decides nothing: it is a label.
// - An image shows a preview only when Rust minted a token for it after the
//   inspection and previews are not off for the session.
// - Audio and video show a Play card only when playback is not `off` and
//   the engine might play the sniffed container (`canPlayType`).
// - Sizes: the stored length beside the declared one, labelled "declared".
import type {
  AttachmentInspectionDto,
  CardStateDto,
  DecimalString,
  MediaPlayback,
  MediaToken,
  SniffedKindDto,
} from '@/types';
import {
  ATTACHMENT_TEXT,
  CARD_TEXT,
  CONTAINER_REFUSAL_TEXT,
  CONTENT_KIND_TEXT,
  FILE_PROBLEM_TEXT,
  MEDIA_UI_TEXT,
  PLAYBACK_TEXT,
  SNIFFED_KIND_TEXT,
  fill,
} from './viewerText';
import { engineMayPlay } from './media';

/** What an attachment's card shows. */
export type CardView =
  /** Not inspected yet: a fixed-size placeholder. */
  | { kind: 'pending' }
  /** A re-encoded preview of an image. */
  | { kind: 'image'; token: MediaToken; sniffed: SniffedKindDto; animated: boolean }
  /** Audio or video: a Play card. */
  | {
      kind: 'playable';
      token: MediaToken;
      sniffed: SniffedKindDto;
      media: 'audio' | 'video';
      structureChecked: boolean;
      /** Why there is no Play button (playback off, the engine cannot play it, refused). */
      blocked: string | null;
      /** Rust's verdict for Play, once asked. */
      ready: boolean;
    }
  /** A file card: the content's type and sentences, never a preview. */
  | { kind: 'file'; sniffed: SniffedKindDto | null; lines: string[] }
  /** No usable file: why. */
  | { kind: 'unavailable'; lines: string[] };

/** Sniffed kinds that play in an `<audio>` element; the others in `<video>`. */
const AUDIO_KINDS: ReadonlySet<SniffedKindDto> = new Set(['m4a', 'wav', 'aiff', 'ogg', 'mp3', 'caf']);

export function mediaElementFor(kind: SniffedKindDto): 'audio' | 'video' {
  return AUDIO_KINDS.has(kind) ? 'audio' : 'video';
}

/** The sentence of a card state that is not a usable file. */
export function cardSentence(card: CardStateDto): string {
  if (card.kind === 'unavailable') return fill(CARD_TEXT.unavailable, { lookup: card.lookup, copy: card.copy });
  return CARD_TEXT[card.kind];
}

/**
 * The card for one attachment. `inspection` is `inspect_attachment`'s
 * answer (null until it came); `playback` is the user's setting, and
 * `canPlay` asks the engine about a sniffed kind (`engineMayPlay`).
 */
export function cardView(
  inspection: AttachmentInspectionDto | null,
  playback: MediaPlayback,
  canPlay: (kind: SniffedKindDto) => boolean = (k) => engineMayPlay(k),
): CardView {
  if (inspection === null) return { kind: 'pending' };
  const card = inspection.facts.card;
  const content = inspection.content;
  if (card.kind !== 'file') {
    // Rust found no usable file for the highest-ranked observation.
    return { kind: 'unavailable', lines: [cardSentence(card)] };
  }
  if (content.kind === 'noFile') return { kind: 'unavailable', lines: [CONTENT_KIND_TEXT.noFile] };
  if (content.kind === 'unavailable') return { kind: 'unavailable', lines: [FILE_PROBLEM_TEXT[content.problem]] };
  const sniffed = content.sniffed;
  const token = inspection.token;
  switch (content.class.kind) {
    case 'image':
      if (token === null) return { kind: 'file', sniffed, lines: [] };
      if (inspection.previewsOff) return { kind: 'file', sniffed, lines: [MEDIA_UI_TEXT.previewsOff] };
      return { kind: 'image', token, sniffed, animated: sniffed === 'gif' };
    case 'playable': {
      if (token === null) return { kind: 'file', sniffed, lines: [] };
      const p = inspection.playback;
      let blocked: string | null = null;
      if (playback === 'off') blocked = PLAYBACK_TEXT.off;
      else if (!canPlay(sniffed)) blocked = MEDIA_UI_TEXT.cannotPlayHere;
      else if (p !== null && p.kind !== 'allowed') blocked = playbackSentence(p);
      return {
        kind: 'playable',
        token,
        sniffed,
        media: mediaElementFor(sniffed),
        structureChecked: content.class.structureChecked,
        blocked,
        ready: p !== null && p.kind === 'allowed',
      };
    }
    case 'file':
      return { kind: 'file', sniffed, lines: [] };
    case 'unrecognised':
      return { kind: 'file', sniffed: null, lines: [SNIFFED_KIND_TEXT.unknown] };
  }
}

/** The sentence for a playback verdict that is not "allowed". */
export function playbackSentence(p: NonNullable<AttachmentInspectionDto['playback']>): string {
  switch (p.kind) {
    case 'allowed':
      return PLAYBACK_TEXT.allowed;
    case 'refused':
      return `${PLAYBACK_TEXT.refused} (${CONTAINER_REFUSAL_TEXT[p.reason]})`;
    case 'unavailable':
      return FILE_PROBLEM_TEXT[p.problem];
    default:
      return PLAYBACK_TEXT[p.kind];
  }
}

const NF = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const KB = 1024;
const MB = KB * 1024;
const GB = MB * 1024;

/** A byte count for people: exact below 1 KB, else KB, MB or GB. */
export function bytesText(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '';
  if (n < KB) return n === 1 ? '1 byte' : `${n} bytes`;
  if (n < MB) return `${NF.format(n / KB)} KB`;
  if (n < GB) return `${NF.format(n / MB)} MB`;
  return `${NF.format(n / GB)} GB`;
}

/** A declared size (a raw integer as digits), or null when not usable. */
export function declaredBytes(raw: DecimalString | null): number | null {
  if (raw === null || !/^[0-9]{1,15}$/.test(raw)) return null;
  return Number(raw);
}

/**
 * "12 KB stored (12 KB declared)": the stored length beside the declared
 * one. Either may be missing; the sentence then says so.
 */
export function sizeLine(stored: number | null, declared: DecimalString | null): string {
  const d = declaredBytes(declared);
  const declaredText =
    d !== null
      ? fill(ATTACHMENT_TEXT.declaredSize, { size: bytesText(d) })
      : declared !== null
        ? fill(ATTACHMENT_TEXT.declaredRaw, { raw: declared })
        : ATTACHMENT_TEXT.declaredNone;
  if (stored === null) return `${ATTACHMENT_TEXT.noStoredFile} (${declaredText})`;
  return `${fill(ATTACHMENT_TEXT.storedSize, { size: bytesText(stored) })} (${declaredText})`;
}

/** The rendition label of the media viewer (§5.5). */
export function renditionLabel(
  original: { width: number; height: number } | null,
  stored: number | null,
  declared: DecimalString | null,
): string {
  const size = original
    ? fill(ATTACHMENT_TEXT.originalSize, { w: original.width, h: original.height })
    : ATTACHMENT_TEXT.originalLater;
  return `${ATTACHMENT_TEXT.renditionPrefix}; ${size}; ${sizeLine(stored, declared)}`;
}
