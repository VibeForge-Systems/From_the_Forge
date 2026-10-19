// The pure half of the thread's items (M5 §3.6, §8.4, slice 5h2): which
// items are bubbles and which are system rows, the sentences for edits,
// unsends, reaction chips and conversation events, sender runs and
// service changes. Everything here works on closed codes and counts and
// returns words from `viewerText.ts`; content stays in the DTOs and is
// rendered by `SafeText` only.
//
// Rules (I5, D94, D115):
// - Nothing the archive does not record is shown: no read, delivery or
//   sent state; "Edited" and "Unsent" mark only the parts the record
//   lists, and a whole-bubble "Unsent" needs every part listed; codes 6
//   and 7 are never called unsent; codes 0, 2 and 9 never read "not
//   edited".
// - A chip state is a derivation and says so; a removal hides its chip.
// - Conversation events assert no actor or target roles.
import type {
  AssociationDto,
  EditFactsDto,
  ItemKindDto,
  MessageDto,
  PreviewBodyDto,
  ReactionChipDto,
  ReactionSummaryDto,
  SafeId,
  ServiceClassDto,
} from '@/types';
import {
  ASSOC_KIND_TEXT,
  ASSOC_VISIBILITY_TEXT,
  BUBBLE_TEXT,
  DISPLAY_NONE_TEXT,
  EDIT_LABEL_TEXT,
  ITEM_KIND_TEXT,
  PREVIEW_BODY_TEXT,
  fill,
} from './viewerText';

// ------------------------------------------------------------- item shape

/** How the thread draws an item. */
export type ItemShape =
  /** A message bubble, aligned by direction. */
  | 'bubble'
  /** A conversation event (`item_kind` 2–13), centred, no roles. */
  | 'event'
  /** `item_kind` 0, or a kind this build does not know. */
  | 'unknown'
  /** An association row that stays a timeline item (sticker, poll vote, a reaction that does not fold). */
  | 'association';

export function itemShape(m: Pick<MessageDto, 'item' | 'association'>): ItemShape {
  if (m.association !== null) return 'association';
  if (m.item === 'message') return 'bubble';
  if (m.item === null || m.item === 'unknown') return 'unknown';
  return 'event';
}

/** The sentence of an event or unknown item. */
export function eventSentence(m: Pick<MessageDto, 'item' | 'event'>): string {
  if (m.item === 'unknown') {
    const item = m.event?.itemType ?? null;
    const action = m.event?.groupActionType ?? null;
    return fill(BUBBLE_TEXT.unknownItem, {
      item: item ?? BUBBLE_TEXT.rawNotRecorded,
      action: action ?? BUBBLE_TEXT.rawNotRecorded,
    });
  }
  if (m.item === null) return BUBBLE_TEXT.itemKindNotRecognised;
  return ITEM_KIND_TEXT[m.item];
}

/**
 * The sentences of an association row that is a timeline item: its kind,
 * then why it is not shown on a message (a target in another conversation,
 * unreadable, not unique, undeterminable). Legacy stickers and poll votes
 * carry only their kind.
 */
export function associationSentences(a: AssociationDto): string[] {
  const out: string[] = [];
  if (a.kind === 'removedEmoji' && (a.emoji === null || a.emoji.kind === 'missing')) {
    out.push(BUBBLE_TEXT.emojiRemovalUnpaired);
  } else {
    out.push(a.kind === null ? BUBBLE_TEXT.reactionKindNotRecorded : ASSOC_KIND_TEXT[a.kind]);
  }
  const v = a.visibility.kind;
  if (v !== 'notAReaction' && !(v === 'noKind' && a.kind === null)) out.push(ASSOC_VISIBILITY_TEXT[v]);
  return out;
}

/** Whether an association row shows its emoji (a tapback emoji or its removal). */
export function associationEmoji(a: AssociationDto): boolean {
  return (a.kind === 'tapbackEmoji' || a.kind === 'removedEmoji') && a.emoji !== null && a.emoji.kind === 'text';
}

/** The sentence for a preview or a reply quote without text. */
export function previewSentence(body: PreviewBodyDto, item: ItemKindDto | null): string {
  switch (body.kind) {
    case 'noText':
      return body.reason ? DISPLAY_NONE_TEXT[body.reason] : PREVIEW_BODY_TEXT.noText;
    case 'reaction':
      return body.association ? ASSOC_KIND_TEXT[body.association] : PREVIEW_BODY_TEXT.reaction;
    case 'event':
      return item ? ITEM_KIND_TEXT[item] : PREVIEW_BODY_TEXT.event;
    default:
      return PREVIEW_BODY_TEXT[body.kind];
  }
}

// ------------------------------------------------------------- edits and unsends

/** What a bubble says about edits, unsends and stored versions. */
export interface EditView {
  /** "Edited", for the parts with recorded edit entries only. */
  edited: string | null;
  /** "Unsent by the sender", per part; whole only when every part is listed. */
  unsent: string | null;
  /** The label's own sentence when the lines above do not cover it. */
  label: string | null;
  /** "N stored versions", whenever more than one body row is stored. */
  versions: string | null;
}

function partList(parts: readonly string[]): string {
  return parts.join(', ');
}

export function editView(e: EditFactsDto): EditView {
  let edited: string | null = null;
  if (e.editedParts.length > 0) {
    edited =
      e.partCount === 1
        ? BUBBLE_TEXT.edited
        : fill(BUBBLE_TEXT.editedParts, { parts: partList(e.editedParts) });
  }
  let unsent: string | null = null;
  if (e.unsentParts.length > 0) {
    if (e.unsentWhole) unsent = BUBBLE_TEXT.unsentWhole;
    else if (e.partCount !== null) {
      unsent = fill(BUBBLE_TEXT.partsUnsent, { n: e.unsentParts.length, total: e.partCount });
    } else unsent = fill(BUBBLE_TEXT.unsentParts, { parts: partList(e.unsentParts) });
  }
  let label: string | null = null;
  switch (e.label) {
    case 'editHistory':
      if (edited === null) label = BUBBLE_TEXT.editedUnknownParts;
      break;
    case 'unsentParts':
      if (unsent === null) label = BUBBLE_TEXT.unsentUnknownParts;
      break;
    case 'unsentPartsAndEditHistory':
      if (edited === null && unsent === null) label = EDIT_LABEL_TEXT.unsentPartsAndEditHistory;
      else if (edited === null) label = BUBBLE_TEXT.editedUnknownParts;
      else if (unsent === null) label = BUBBLE_TEXT.unsentUnknownParts;
      break;
    case 'dateEditedNoBody':
    case 'dateEditedOnly':
    case 'undeterminable':
    case 'summaryUnreadable':
    case 'undetermined':
      label = EDIT_LABEL_TEXT[e.label];
      break;
    // Codes 1 and 8 ("No edit information recorded", "No edit recorded")
    // are listed in the details panel, not on every bubble; neither ever
    // reads "not edited". A code this build does not know shows nothing.
    case 'notRecorded':
    case 'noEditRecorded':
    case null:
      break;
  }
  const versions = e.storedVersions > 1 ? fill(BUBBLE_TEXT.storedVersions, { n: e.storedVersions }) : null;
  return { edited, unsent, label, versions };
}

// ------------------------------------------------------------- reactions

/** What a bubble's reaction list shows. */
export interface ReactionsView {
  /** Chips to draw: every state but `hidden` (a removal hides its chip). */
  chips: ReactionChipDto[];
  /** "N removed later (derived)". */
  removed: string | null;
  /** "More than N reactions recorded", when the rows went over the cap. */
  over: string | null;
  /** "N more reactions not shown", for chips beyond the cap. */
  more: string | null;
  /** Unpaired removals of an emoji without its emoji. */
  unpaired: string | null;
}

export function reactionsView(r: ReactionSummaryDto): ReactionsView {
  const chips = r.chips.filter((c) => c.state !== 'hidden');
  const hidden = r.chips.length - chips.length;
  return {
    chips,
    removed:
      hidden === 0 ? null : hidden === 1 ? BUBBLE_TEXT.oneReactionRemoved : fill(BUBBLE_TEXT.reactionsRemoved, { n: hidden }),
    over: r.rows.capped && r.chips.length === 0 ? fill(BUBBLE_TEXT.reactionsOver, { n: r.rows.n }) : null,
    more: r.moreChips > 0 ? fill(BUBBLE_TEXT.reactionsMore, { n: r.moreChips }) : null,
    unpaired:
      r.unpaired.length === 0
        ? null
        : r.unpaired.length === 1
          ? BUBBLE_TEXT.emojiRemovalUnpaired
          : fill(BUBBLE_TEXT.emojiRemovalUnpairedMany, { n: r.unpaired.length }),
  };
}

/** Whether a reaction summary has anything to show. */
export function hasReactions(r: ReactionSummaryDto | null): r is ReactionSummaryDto {
  return r !== null && (r.chips.length > 0 || r.unpaired.length > 0 || r.rows.n > 0);
}

// ------------------------------------------------------------- runs and dividers

/**
 * A key for "the same sender" in a run of bubbles: the owner, one handle,
 * or one recorded reason. Two items without a resolved sender for the
 * same reason are one run; nothing is filled in from the participants.
 */
export function senderKey(m: Pick<MessageDto, 'sender' | 'direction'>): string {
  const s = m.sender;
  return s.kind === 'resolved' ? `h${s.handle}` : `${m.direction}:${s.kind}`;
}

/**
 * The bubbles that start a run of messages from one sender: the first
 * bubble, a bubble after a system row, after another sender, or on a new
 * local day (`dayStarts`).
 */
export function runStarts(
  items: readonly MessageDto[],
  dayStarts: ReadonlySet<SafeId> | ReadonlyMap<SafeId, unknown>,
): Set<SafeId> {
  const out = new Set<SafeId>();
  let previous: string | null = null;
  for (const m of items) {
    if (itemShape(m) !== 'bubble') {
      previous = null;
      continue;
    }
    const key = senderKey(m);
    if (key !== previous || dayStarts.has(m.id)) out.add(m.id);
    previous = key;
  }
  return out;
}

/**
 * The items where the service changes from the item before (§3.6): each
 * maps to its new service (`null`: no service was recorded for it, or the
 * cell could not be read; text this version does not recognise is
 * `other`, and empty text `empty`). The first loaded item has no divider,
 * since what precedes it is not loaded.
 */
export function serviceChanges(items: readonly MessageDto[]): Map<SafeId, ServiceClassDto | null> {
  const out = new Map<SafeId, ServiceClassDto | null>();
  for (let i = 1; i < items.length; i += 1) {
    const before = items[i - 1]!.service;
    const now = items[i]!.service;
    if (before !== now) out.set(items[i]!.id, now);
  }
  return out;
}

/** "1 reply…", "N replies…", "N+ replies…" across conversations. */
export function replyCountText(c: { n: number; capped: boolean } | null): string | null {
  if (c === null || c.n === 0) return null;
  if (c.capped) return fill(BUBBLE_TEXT.repliesCapped, { n: c.n });
  return c.n === 1 ? BUBBLE_TEXT.oneReply : fill(BUBBLE_TEXT.replies, { n: c.n });
}
