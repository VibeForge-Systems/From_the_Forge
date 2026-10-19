// The message menu's copy actions (M5 §9.3, D107), slice 5j: pure
// functions from a message as the bubble shows it to the plain text a copy
// puts on the clipboard. Nothing here copies, logs or stores anything; the
// menu calls `copyText` (services/features.ts) on an explicit choice only.
//
// - "Copy text": the current body as shown. A masked one-time code is
//   copied as "[one-time code hidden]" unless the message was revealed;
//   text withheld by Rust (earlier or unsent text, D115) is never part of
//   it, because only the current body is copied. A body cut at the page's
//   cap says so at the end.
// - "Copy text without invisible characters": the same, without bidi
//   controls, invisible and control characters. A zero-width joiner or
//   non-joiner stays only between two non-ASCII letters, marks or emoji
//   (emoji sequences and joining scripts keep their shape), and tag
//   characters stay only inside a flag's tag sequence.
// - "Copy with sender and time": a first line with the sender and the
//   time, and the time zone the time is shown in; then the bubble's
//   qualifiers in brackets ("Direction not recorded", "Unsent by the
//   sender", "… (derived)"); then the text.
// - "Copy link": URL runs only. A run with hidden characters is refused
//   (the menu says to use Copy text); one that is not complete here (a
//   masked or cut part) is refused too.
import type { HandleDto, MessageDto, SafeId, SegmentDto, TextPieceDto } from '@/types';
import { editView } from './messageView';
import {
  hasHiddenCharacters,
  isBidiControl,
  isControl,
  isInvisible,
  isVariationSelector,
  VS_RUN_LIMIT,
} from './safeText';
import { timeText, type TimePrefs } from './time';
import {
  BEFORE_2007_TEXT,
  COPY_ACTION_TEXT,
  DIRECTION_TEXT,
  MESSAGE_LABEL_TEXT,
  SENDER_TEXT,
  fill,
} from './viewerText';

/** The full text of a truncated body, as far as it was read. */
export interface LoadedText {
  pieces: readonly TextPieceDto[];
  end: number;
  full: number;
}

function segmentText(seg: SegmentDto, m: Pick<MessageDto, 'attachments'>): string {
  const p = seg.piece;
  switch (p.kind) {
    case 'text':
      return p.value.text;
    case 'withheld':
      return COPY_ACTION_TEXT.codeHidden;
    case 'inlineEmoji':
      return COPY_ACTION_TEXT.inlineEmoji;
    case 'attachment': {
      const i = m.attachments.findIndex((a) => a.id === p.attachment);
      return i < 0 ? COPY_ACTION_TEXT.attachmentUnlisted : fill(COPY_ACTION_TEXT.attachment, { n: i + 1 });
    }
    case 'unmatchedPlaceholder':
      return COPY_ACTION_TEXT.placeholder;
  }
}

function pieceText(p: TextPieceDto): string {
  return p.kind === 'text' ? p.value.text : COPY_ACTION_TEXT.codeHidden;
}

/**
 * The current body's text as the bubble shows it, or `null` when the
 * message has no text. `full` is the body's full text when it was read
 * ("Show the full text").
 */
export function bodyText(m: MessageDto, full: LoadedText | null = null): string | null {
  const b = m.body;
  if (b.kind !== 'text') return null;
  let text: string;
  let cut: boolean;
  if (full && full.pieces.length > 0) {
    text = full.pieces.map(pieceText).join('');
    cut = full.end < full.full;
  } else {
    text = b.segments.map((s) => segmentText(s, m)).join('');
    cut = b.truncated;
  }
  return cut ? `${text}\n${COPY_ACTION_TEXT.cut}` : text;
}

const ASCII_MAX = 0x7f;
const JOINABLE = /[\p{L}\p{M}\p{Extended_Pictographic}\p{Emoji_Modifier}]/u;
const BLACK_FLAG = 0x1f3f4;

function joinable(ch: string | undefined): boolean {
  return ch !== undefined && ch.codePointAt(0)! > ASCII_MAX && JOINABLE.test(ch);
}

const isTag = (cp: number) => cp >= 0xe0020 && cp <= 0xe007f;

/**
 * `text` without the characters that can hide or reorder it (the classes
 * `SafeText` shows as tokens), keeping what emoji and joining scripts need.
 */
export function withoutHiddenCharacters(text: string): string {
  const chars = Array.from(text);
  const out: string[] = [];
  for (let i = 0; i < chars.length; i += 1) {
    const ch = chars[i]!;
    const cp = ch.codePointAt(0)!;
    if (isVariationSelector(cp)) {
      let end = i;
      while (end < chars.length && isVariationSelector(chars[end]!.codePointAt(0)!)) end += 1;
      if (end - i <= VS_RUN_LIMIT) out.push(...chars.slice(i, end));
      i = end - 1;
      continue;
    }
    if (cp === 0x200c || cp === 0x200d) {
      if (joinable(out[out.length - 1]) && joinable(chars[i + 1])) out.push(ch);
      continue;
    }
    if (isTag(cp)) {
      const prev = out[out.length - 1]?.codePointAt(0);
      if (prev === BLACK_FLAG || (prev !== undefined && isTag(prev))) out.push(ch);
      continue;
    }
    if (isBidiControl(cp) || isInvisible(cp) || isControl(cp, true)) continue;
    out.push(ch);
  }
  return out.join('');
}

/** "UTC+01:00": this computer's offset at an instant. */
export function utcOffset(ms: number): string {
  const minutes = -new Date(ms).getTimezoneOffset();
  const sign = minutes < 0 ? '-' : '+';
  const abs = Math.abs(minutes);
  const h = String(Math.floor(abs / 60)).padStart(2, '0');
  const m = String(abs % 60).padStart(2, '0');
  return `UTC${sign}${h}:${m}`;
}

export interface SenderTimeContext {
  handles: ReadonlyMap<SafeId, HandleDto>;
  prefs: TimePrefs;
}

/** The sender as the bubble names it: the address as recorded, or why there is none. */
export function senderName(m: Pick<MessageDto, 'sender'>, handles: ReadonlyMap<SafeId, HandleDto>): string {
  const s = m.sender;
  if (s.kind !== 'resolved') return SENDER_TEXT[s.kind];
  const h = handles.get(s.handle);
  if (!h) return COPY_ACTION_TEXT.senderNotLoaded;
  return h.address.kind === 'text' ? h.address.value.text : SENDER_TEXT.resolved;
}

/** The bubble's qualifiers, each a fixed sentence. */
export function qualifiers(m: MessageDto, prefs: TimePrefs): string[] {
  const out: string[] = [];
  if (m.direction === 'notRecorded' && m.sender.kind !== 'directionNotRecorded') out.push(DIRECTION_TEXT.notRecorded);
  const t = timeText(m.date, prefs);
  if (t.kind === 'instant' && t.before2007) out.push(BEFORE_2007_TEXT);
  const e = editView(m.edit);
  for (const line of [e.edited, e.unsent, e.label]) if (line) out.push(line);
  for (const l of m.labels) {
    out.push(l.kind === 'scheduleType' ? fill(MESSAGE_LABEL_TEXT.scheduleType, { value: l.value }) : MESSAGE_LABEL_TEXT[l.kind]);
  }
  return out;
}

/** "Sender — time (times in UTC+01:00, this computer's zone)". */
export function senderTimeLine(m: MessageDto, ctx: SenderTimeContext): string {
  const t = timeText(m.date, ctx.prefs);
  const zone =
    t.kind !== 'instant'
      ? ''
      : ` (${ctx.prefs.timeZone === 'utc' ? COPY_ACTION_TEXT.zoneUtc : fill(COPY_ACTION_TEXT.zoneSystem, { offset: utcOffset(t.ms) })})`;
  return `${senderName(m, ctx.handles)} — ${t.text}${zone}`;
}

/** "Copy with sender and time". */
export function withSenderAndTime(m: MessageDto, text: string, ctx: SenderTimeContext): string {
  const lines = [senderTimeLine(m, ctx), ...qualifiers(m, ctx.prefs).map((q) => `[${q}]`), text];
  return lines.join('\n');
}

/** One URL run of a body (§3.6), as one link. */
export interface LinkRun {
  /** Its pieces, for showing the exact string in identifier mode. */
  pieces: TextPieceDto[];
  text: string;
  /** It holds bidi controls or invisible characters: never copied as a link. */
  hidden: boolean;
  /** No part of it is masked, an inline object or cut at a cap. */
  complete: boolean;
}

/** The body's URL runs, in order (consecutive URL segments form one run). */
export function linkRuns(m: MessageDto): LinkRun[] {
  const b = m.body;
  if (b.kind !== 'text') return [];
  const out: LinkRun[] = [];
  let run: LinkRun | null = null;
  b.segments.forEach((seg, i) => {
    if (!seg.url) {
      run = null;
      return;
    }
    if (run === null) {
      run = { pieces: [], text: '', hidden: false, complete: true };
      out.push(run);
    }
    if (seg.urlHidden) run.hidden = true;
    if (seg.piece.kind === 'text') {
      const v = seg.piece.value;
      run.pieces.push({ kind: 'text', value: v });
      run.text += v.text;
      if (v.bidiControls || v.invisible || hasHiddenCharacters(v.text)) run.hidden = true;
      if (v.truncated || (b.truncated && i === b.segments.length - 1)) run.complete = false;
    } else {
      run.complete = false;
    }
  });
  return out;
}

/** What the message menu offers. */
export type MenuAction =
  | { kind: 'copyText' }
  | { kind: 'copyClean' }
  | { kind: 'copyWithSender' }
  | { kind: 'copyLink'; index: number }
  | { kind: 'details' };

export interface MenuItem {
  action: MenuAction;
  label: string;
}

/** At most this many "Copy link" items. */
export const MAX_LINK_ITEMS = 10;

/**
 * The menu for one message. When the body holds bidi controls or invisible
 * characters, the first (default) item copies without them (§9.3).
 */
export function menuItems(m: MessageDto): MenuItem[] {
  const items: MenuItem[] = [];
  const b = m.body;
  if (b.kind === 'text') {
    const plain: MenuItem = { action: { kind: 'copyText' }, label: COPY_ACTION_TEXT.copyText };
    const clean: MenuItem = { action: { kind: 'copyClean' }, label: COPY_ACTION_TEXT.copyClean };
    if (b.bidiControls || b.invisible) items.push(clean, plain);
    else items.push(plain, clean);
    items.push({ action: { kind: 'copyWithSender' }, label: COPY_ACTION_TEXT.copyWithSender });
    const links = linkRuns(m).slice(0, MAX_LINK_ITEMS);
    links.forEach((_, index) =>
      items.push({
        action: { kind: 'copyLink', index },
        label:
          links.length === 1
            ? COPY_ACTION_TEXT.copyLink
            : fill(COPY_ACTION_TEXT.copyLinkN, { n: index + 1, total: links.length }),
      }),
    );
  }
  items.push({ action: { kind: 'details' }, label: COPY_ACTION_TEXT.details });
  return items;
}
