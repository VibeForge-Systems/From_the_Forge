// @vitest-environment node
// The pure half of the thread's items (M5 §3.6, slice 5h2): shapes, edit
// and unsend sentences per part, reaction chips, sender runs and service
// changes. Nothing the archive does not record may come out of these.
import { describe, expect, it } from 'vitest';
import {
  associationSentences,
  editView,
  eventSentence,
  hasReactions,
  itemShape,
  reactionsView,
  replyCountText,
  runStarts,
  senderKey,
  serviceChanges,
} from '@/services/messageView';
import type {
  AssocKindDto,
  AssociationDto,
  DecimalString,
  EditFactsDto,
  EditLabelDto,
  ItemKindDto,
  ReactionChipDto,
  ReactionSummaryDto,
} from '@/types';
import { message, sid, text } from './helpers';

const d = (n: number) => String(n) as DecimalString;

function edit(overrides: Partial<EditFactsDto> = {}): EditFactsDto {
  return {
    label: null,
    editedParts: [],
    unsentParts: [],
    partCount: null,
    unsentWhole: false,
    storedVersions: 1,
    earlier: { kind: 'none' },
    ...overrides,
  };
}

function chip(overrides: Partial<ReactionChipDto> = {}): ReactionChipDto {
  return { kind: 'love', emoji: null, sender: { kind: 'me' }, rows: [sid(9)], state: 'shown', ...overrides };
}

function summary(overrides: Partial<ReactionSummaryDto> = {}): ReactionSummaryDto {
  return { rows: { n: 1, capped: false }, chips: [chip()], moreChips: 0, unpaired: [], ...overrides };
}

function assoc(overrides: Partial<AssociationDto> = {}): AssociationDto {
  return { visibility: { kind: 'notAReaction' }, kind: 'pollVote', shape: null, targetPart: null, emoji: null, ...overrides };
}

const ALL_LABELS: EditLabelDto[] = [
  'undeterminable',
  'notRecorded',
  'summaryUnreadable',
  'unsentPartsAndEditHistory',
  'unsentParts',
  'editHistory',
  'dateEditedNoBody',
  'dateEditedOnly',
  'noEditRecorded',
  'undetermined',
];

describe('itemShape and event sentences', () => {
  it('draws messages as bubbles, association rows and events as system rows', () => {
    expect(itemShape(message(1))).toBe('bubble');
    expect(itemShape(message(1, { association: assoc() }))).toBe('association');
    expect(itemShape(message(1, { item: 'participantAdded' }))).toBe('event');
    expect(itemShape(message(1, { item: 'unknown' }))).toBe('unknown');
    expect(itemShape(message(1, { item: null }))).toBe('unknown');
  });

  it('names the raw codes of an unrecognised item, and asserts no roles for events', () => {
    const unknown = message(1, {
      item: 'unknown',
      event: { handles: [], title: null, itemType: d(42), groupActionType: null },
    });
    expect(eventSentence(unknown)).toBe('Unrecognised item (item_type 42, group_action_type not recorded)');
    const kinds: ItemKindDto[] = ['participantAdded', 'participantRemoved', 'participantLeft', 'groupRenamed'];
    for (const item of kinds) {
      const s = eventSentence(message(1, { item }));
      expect(s).not.toMatch(/\b(?:by|added you|removed you)\b/i);
      expect(s.length).toBeGreaterThan(0);
    }
  });

  it('describes association rows that stay items, without parsing fallback text', () => {
    expect(associationSentences(assoc())).toEqual(['Poll vote (poll contents not decoded)']);
    expect(associationSentences(assoc({ kind: 'stickerLegacy' }))).toEqual(['Sticker placed on a message']);
    expect(
      associationSentences(assoc({ kind: 'tapbackLove', visibility: { kind: 'targetInAnotherChat', target: sid(3) } })),
    ).toEqual(['Loved', 'Reaction to a message in another conversation']);
    expect(associationSentences(assoc({ kind: null, visibility: { kind: 'noKind' } }))).toEqual([
      'Reaction kind not recorded',
    ]);
    expect(associationSentences(assoc({ kind: 'removedEmoji', visibility: { kind: 'targetUnreadable' } }))).toEqual([
      'Removal of an emoji reaction (emoji not recorded)',
      'Reaction target could not be read',
    ]);
    expect(
      associationSentences(assoc({ kind: 'tapbackLike', visibility: { kind: 'targetFolds', target: sid(2) } }))[1],
    ).toContain('Reaction to another reaction');
    const kinds: AssocKindDto[] = ['tapbackEmoji', 'removedSticker', 'unknown', 'noAssociation'];
    for (const kind of kinds) expect(associationSentences(assoc({ kind }))[0]!.length).toBeGreaterThan(0);
  });
});

describe('editView', () => {
  it('marks only the parts the record lists as edited', () => {
    expect(editView(edit({ label: 'editHistory', editedParts: [d(0)], partCount: 1 })).edited).toBe('Edited');
    expect(editView(edit({ label: 'editHistory', editedParts: [d(1)], partCount: 3 })).edited).toBe(
      'Edited (recorded part 1)',
    );
    expect(editView(edit({ label: 'editHistory', editedParts: [d(0), d(2)] })).edited).toBe(
      'Edited (recorded part 0, 2)',
    );
    expect(editView(edit({ label: 'editHistory' }))).toMatchObject({
      edited: null,
      label: 'An edit is recorded; which parts is not recorded',
    });
  });

  it('says "1 of 3 parts unsent" and a whole-bubble label only when every part is listed', () => {
    const partial = editView(edit({ label: 'unsentParts', unsentParts: [d(1)], partCount: 3 }));
    expect(partial.unsent).toBe('1 of 3 parts unsent by the sender');
    const whole = editView(edit({ label: 'unsentParts', unsentParts: [d(0)], partCount: 1, unsentWhole: true }));
    expect(whole.unsent).toBe('Unsent by the sender');
    const unknownTotal = editView(edit({ label: 'unsentParts', unsentParts: [d(2)] }));
    expect(unknownTotal.unsent).toBe('Unsent by the sender: recorded part 2');
    expect(editView(edit({ label: 'unsentParts' })).label).toBe(
      'An unsend is recorded; which parts is not recorded',
    );
  });

  it('never calls codes 6 and 7 unsent, and never says "not edited"', () => {
    for (const label of ALL_LABELS) {
      const v = editView(edit({ label }));
      const all = [v.edited, v.unsent, v.label, v.versions].filter((x) => x !== null).join(' ').toLowerCase();
      expect(all, label).not.toContain('not edited');
      if (label === 'dateEditedNoBody' || label === 'dateEditedOnly') {
        expect(all).not.toContain('unsent');
        expect(v.label).toBe('An edit date is recorded; no edit history in this version');
      }
      if (label === 'undeterminable' || label === 'summaryUnreadable' || label === 'undetermined') {
        expect(v.label).toBe('Edit state could not be determined');
      }
      if (label === 'notRecorded' || label === 'noEditRecorded') expect(all).toBe('');
    }
  });

  it('shows "N stored versions" whenever more than one body row is stored', () => {
    expect(editView(edit({ storedVersions: 1 })).versions).toBeNull();
    expect(editView(edit({ storedVersions: 3, label: 'noEditRecorded' })).versions).toBe('3 stored versions');
  });
});

describe('reactionsView', () => {
  it('draws every derived state but hidden, and counts the hidden ones', () => {
    const v = reactionsView(
      summary({
        rows: { n: 5, capped: false },
        chips: [
          chip({ state: 'shown' }),
          chip({ state: 'hidden', kind: 'like' }),
          chip({ state: 'undeterminable', kind: 'laugh' }),
          chip({ state: 'added', sender: { kind: 'notRecorded' } }),
          chip({ state: 'removed', sender: { kind: 'notRecorded' } }),
        ],
      }),
    );
    expect(v.chips.map((c) => c.state)).toEqual(['shown', 'undeterminable', 'added', 'removed']);
    expect(v.removed).toBe('1 removed later (derived)');
  });

  it('reports the caps and unpaired emoji removals', () => {
    const over = reactionsView(summary({ rows: { n: 200, capped: true }, chips: [] }));
    expect(over.over).toBe('More than 200 reactions recorded; too many to show here');
    expect(reactionsView(summary({ moreChips: 4 })).more).toBe('4 more reactions not shown');
    expect(reactionsView(summary({ unpaired: [sid(1)] })).unpaired).toBe(
      'Removal of an emoji reaction (emoji not recorded)',
    );
    expect(reactionsView(summary({ unpaired: [sid(1), sid(2)] })).unpaired).toContain('2 rows');
    expect(hasReactions(null)).toBe(false);
    expect(hasReactions(summary({ rows: { n: 0, capped: false }, chips: [] }))).toBe(false);
    expect(hasReactions(summary({ chips: [chip({ emoji: text('👍') })] }))).toBe(true);
  });
});

describe('sender runs and service changes', () => {
  it('starts a run at a new sender, after a system row and on a new day; never fills a sender in', () => {
    const items = [
      message(1, { sender: { kind: 'resolved', handle: sid(1) } }),
      message(2, { sender: { kind: 'resolved', handle: sid(1) } }),
      message(3, { sender: { kind: 'resolved', handle: sid(2) } }),
      message(4, { item: 'participantAdded' }),
      message(5, { sender: { kind: 'resolved', handle: sid(2) } }),
      message(6, { sender: { kind: 'noSender' } }),
      message(7, { sender: { kind: 'noSender' } }),
      message(8, { sender: { kind: 'noSender' } }),
    ];
    const starts = runStarts(items, new Set([sid(8)]));
    expect([...starts]).toEqual([1, 3, 5, 6, 8]);
    expect(senderKey(items[5]!)).not.toBe(senderKey(items[0]!));
  });

  it('marks each place the service changes, not the first loaded item', () => {
    const items = [
      message(1, { service: 'iMessage' }),
      message(2, { service: 'iMessage' }),
      message(3, { service: 'sms' }),
      message(4, { service: null }),
    ];
    const changes = serviceChanges(items);
    expect([...changes.entries()]).toEqual([
      [3, 'sms'],
      [4, null],
    ]);
  });

  it('counts replies across conversations, capped', () => {
    expect(replyCountText(null)).toBeNull();
    expect(replyCountText({ n: 0, capped: false })).toBeNull();
    expect(replyCountText({ n: 1, capped: false })).toBe('1 reply across conversations');
    expect(replyCountText({ n: 1000, capped: true })).toBe('1000+ replies across conversations');
  });
});
