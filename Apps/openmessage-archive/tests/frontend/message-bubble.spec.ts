// Message bubbles and system rows (M5 §3.6, §8.4, §13.8), slice 5h2:
// every closed code renders words, no status mapping exists, one-time
// codes and withheld earlier text are revealed only on click, reply chips
// and their unresolved states, the "derived" and "cannot be determined"
// chips, per-part edits and unsends, and events that assert no roles.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount } from '@vue/test-utils';

const fake = vi.hoisted(() => ({
  calls: [] as [string, unknown][],
  handlers: {} as Record<string, (arg: unknown) => Promise<unknown>>,
}));
vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  const call =
    (name: string) =>
    (...args: unknown[]) => {
      const arg = args.length === 1 ? args[0] : args;
      fake.calls.push([name, arg]);
      const handler = fake.handlers[name];
      return handler ? handler(arg) : Promise.resolve(null);
    };
  return { ...actual, ipc: new Proxy({}, { get: (_t, name: string) => call(name) }) };
});
import ThreadItem from '@/components/viewer/ThreadItem.vue';
import { useViewer } from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import type {
  AssocKindDto,
  BodyViewDto,
  DecimalString,
  GetMessageDetailRequest,
  HandleDto,
  ItemKindDto,
  MessageDetailDto,
  MessageDto,
  SafeId,
  SegmentDto,
  SegmentPieceDto,
} from '@/types';
import { ARCHIVE_ID, content, message, sid, snapshot, text, textPiece, viewerSession } from './helpers';

const d = (n: number) => String(n) as DecimalString;

const HANDLES = new Map<SafeId, HandleDto>([
  [sid(1), { id: sid(1), address: text('+1 555 0100'), service: 'iMessage' }],
  [sid(2), { id: sid(2), address: text('synthetic@example.invalid'), service: 'iMessage' }],
]);

function seg(piece: SegmentPieceDto, flags: Partial<Omit<SegmentDto, 'piece'>> = {}): SegmentDto {
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

function textBody(segments: SegmentDto[], overrides: Partial<Extract<BodyViewDto, { kind: 'text' }>> = {}): BodyViewDto {
  return {
    kind: 'text',
    body: sid(50),
    segments,
    formatting: { kind: 'applied' },
    source: 'attributedBody',
    fullUtf16: 20,
    truncated: false,
    bidiControls: false,
    invisible: false,
    lossy: false,
    storedAsBlob: false,
    sourcesDiffer: null,
    ...overrides,
  };
}

const plain = (s: string) => seg({ kind: 'text', value: content(s) });

async function setup(): Promise<void> {
  const store = useViewer();
  await store.open(ARCHIVE_ID);
}

function mountItem(m: MessageDto, props: Record<string, unknown> = {}) {
  return mount(ThreadItem, {
    attachTo: document.body,
    props: { message: m, handles: HANDLES, dayLabel: null, ...props },
  });
}

beforeEach(async () => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = {
    openViewer: () =>
      Promise.resolve(
        viewerSession({
          viewer: 3,
          imports: [{ session: sid(1), state: 'completed', startedMs: Date.UTC(2026, 2, 1), endedMs: null }],
        }),
      ),
  };
  useSettings().snapshot = snapshot();
  await setup();
});

afterEach(() => {
  document.body.innerHTML = '';
});

const detailCalls = () =>
  fake.calls.filter(([n]) => n === 'getMessageDetail').map(([, a]) => a as GetMessageDetailRequest);

describe('MessageBubble', () => {
  it('renders segments with closed styles, mentions and URLs as text, and nothing as a link', () => {
    const m = message(10, {
      body: textBody([
        plain('Hello '),
        seg({ kind: 'text', value: content('bold') }, { bold: true }),
        seg({ kind: 'text', value: content('@Someone') }, { mention: true }),
        seg({ kind: 'text', value: content('https://example.invalid/x') }, { url: true }),
        seg({ kind: 'inlineEmoji' }),
        seg({ kind: 'unmatchedPlaceholder' }),
      ]),
    });
    const w = mountItem(m);
    expect(w.find('.oma-seg-bold').text()).toBe('bold');
    expect(w.find('.oma-seg-mention').text()).toBe('@Someone');
    expect(w.find('.oma-seg-url').text()).toBe('https://example.invalid/x');
    expect(w.find('a').exists()).toBe(false);
    expect(w.text()).toContain('Inline emoji image (not shown)');
    expect(w.text()).toContain('Attachment placeholder not matched to a file');
  });

  it('renders hostile text as text', () => {
    const w = mountItem(message(10, { body: textBody([plain('<script>x</script><img src=x onerror=1>{{ 1+1 }}')]) }));
    expect(w.text()).toContain('<script>x</script><img src=x onerror=1>{{ 1+1 }}');
    expect(w.find('script').exists()).toBe(false);
    expect(w.find('.oma-bubble img').exists()).toBe(false);
  });

  it('masks a one-time code and sends the code reveal only on "Show code"', async () => {
    const m = message(11, {
      body: textBody([plain('Your code is '), seg({ kind: 'withheld', mask: 'oneTimeCode' }, { oneTimeCode: true })]),
    });
    fake.handlers.getMessageDetail = () =>
      Promise.resolve({
        message: message(11, {
          body: textBody([plain('Your code is '), seg({ kind: 'text', value: content('123456') }, { oneTimeCode: true })]),
        }),
      } as unknown as MessageDetailDto);
    const w = mountItem(m);
    expect(w.text()).toContain('One-time code hidden');
    expect(w.text()).not.toContain('123456');
    expect(detailCalls()).toHaveLength(0);
    await w.findAll('button').find((b) => b.text() === 'Show code')!.trigger('click');
    await flushPromises();
    expect(detailCalls()).toEqual([{ viewer: 3, messageId: 11, reveal: { oneTimeCode: true } }]);
    expect(w.get('.oma-seg-code').text()).toBe('123456');
    await w.findAll('button').find((b) => b.text() === 'Hide code')!.trigger('click');
    await flushPromises();
    expect(w.text()).not.toContain('123456');
    expect(w.text()).toContain('One-time code hidden');
  });

  it('withholds earlier text and sends the reveal only on click', async () => {
    const m = message(12, {
      body: { kind: 'noText', detail: { body: sid(5), reason: 'textEmpty', attributed: null, attributedError: null, textColumn: null } },
      edit: {
        label: 'unsentParts',
        editedParts: [],
        unsentParts: [d(0)],
        partCount: 1,
        unsentWhole: true,
        storedVersions: 2,
        earlier: { kind: 'withheld', reason: 'unsent' },
      },
    });
    fake.handlers.getMessageDetail = () =>
      Promise.resolve({
        message: { ...m, edit: { ...m.edit, earlier: { kind: 'revealed', reason: 'unsent', pieces: [textPiece('kept words')], session: sid(1) } } },
      } as unknown as MessageDetailDto);
    const w = mountItem(m);
    expect(w.text()).toContain('Earlier text was kept from an import before it was unsent.');
    expect(w.text()).toContain('Unsent by the sender');
    expect(w.text()).toContain('2 stored versions');
    expect(w.text()).not.toContain('kept words');
    // Page requests never carried a reveal, and nothing was asked yet.
    expect(detailCalls()).toHaveLength(0);
    await w.findAll('button').find((b) => b.text() === 'Show earlier text')!.trigger('click');
    await flushPromises();
    expect(detailCalls()).toEqual([{ viewer: 3, messageId: 12, reveal: { earlierText: true } }]);
    expect(w.text()).toContain('kept words');
    expect(w.text()).toContain('the phone no longer shows it');
    expect(w.text()).toContain('First kept by the import of');
    await w.findAll('button').find((b) => b.text() === 'Hide earlier text')!.trigger('click');
    await flushPromises();
    expect(w.text()).not.toContain('kept words');
  });

  it('shows a reveal failure as a sentence', async () => {
    const m = message(13, {
      edit: { label: 'dateEditedNoBody', editedParts: [], unsentParts: [], partCount: null, unsentWhole: false, storedVersions: 2, earlier: { kind: 'withheld', reason: 'earlierVersion' } },
    });
    const { IpcError } = await import('@/services/ipc');
    fake.handlers.getMessageDetail = () => Promise.reject(new IpcError('get_message_detail', 'messageNotFound', 'x'));
    const w = mountItem(m);
    expect(w.text()).toContain('An edit date is recorded; no edit history in this version');
    expect(w.text().toLowerCase()).not.toContain('unsent');
    await w.findAll('button').find((b) => b.text() === 'Show earlier text')!.trigger('click');
    await flushPromises();
    expect(w.get('[role="alert"]').text()).toContain('That message is no longer in the archive.');
  });

  it('marks edits and unsends per part', () => {
    const w = mountItem(
      message(14, {
        edit: { label: 'unsentPartsAndEditHistory', editedParts: [d(0)], unsentParts: [d(2)], partCount: 3, unsentWhole: false, storedVersions: 1, earlier: { kind: 'none' } },
      }),
    );
    expect(w.text()).toContain('Edited (recorded part 0)');
    expect(w.text()).toContain('1 of 3 parts unsent by the sender');
    expect(w.text()).not.toMatch(/^Unsent by the sender$/m);
  });

  it('gives a reason for a body without text, never an empty bubble', () => {
    const w = mountItem(
      message(15, {
        body: { kind: 'noText', detail: { body: sid(5), reason: 'attributedBodyUnusable', attributed: 'decodeFailed', attributedError: 'truncated', textColumn: 'oversized' } },
      }),
    );
    const t = w.text();
    expect(t).toContain('The formatted text could not be used');
    expect(t).toContain('The formatted text ends early');
    expect(t).toContain('Plain-text column: Too large to store');
    const none = mountItem(message(16, { body: { kind: 'noBody' } }));
    expect(none.text()).toContain('No text recorded');
  });

  it('notes lossy text, differing sources and unreadable formatting', () => {
    const w = mountItem(
      message(17, {
        body: textBody([plain('x')], {
          source: 'textLossy',
          sourcesDiffer: true,
          lossy: true,
          formatting: { kind: 'unreadable', error: 'badRunIndex' },
        }),
      }),
    );
    const t = w.text();
    expect(t).toContain('Some characters could not be decoded');
    expect(t).toContain('which one the phone showed is not known');
    expect(t).toContain('Formatting could not be read: A formatting range refers to a missing entry');
  });

  it('reads a truncated body with get_message_text on request', async () => {
    fake.handlers.getMessageText = () =>
      Promise.resolve({ body: sid(50), fromUtf16: 0, endUtf16: 9000, fullUtf16: 9000, text: { kind: 'text', pieces: [textPiece('the whole text')] } });
    const w = mountItem(message(18, { body: textBody([plain('the whole')], { truncated: true, fullUtf16: 9000 }) }));
    expect(fake.calls.some(([n]) => n === 'getMessageText')).toBe(false);
    expect(w.find('.oma-body-text').classes()).toContain('oma-collapsed');
    await w.findAll('button').find((b) => b.text() === 'Show the full text')!.trigger('click');
    await flushPromises();
    expect(fake.calls.find(([n]) => n === 'getMessageText')![1]).toEqual({ viewer: 3, bodyId: 50, fromUtf16: 0 });
    expect(w.text()).toContain('the whole text');
    expect(w.findAll('button').some((b) => b.text() === 'Show the full text')).toBe(false);
  });

  it('shows a resolved reply chip that jumps, and the unresolved states', async () => {
    const quote = { kind: 'text' as const, pieces: [textPiece('original words')] };
    const w = mountItem(message(19, { reply: { kind: 'resolved', originator: sid(7), quote } }));
    expect(w.text()).toContain('Reply to');
    expect(w.text()).toContain('original words');
    await w.findAll('button').find((b) => b.text() === 'Show the message this replies to')!.trigger('click');
    expect(w.emitted('jump')).toEqual([[7, null]]);

    const other = mountItem(message(20, { reply: { kind: 'inAnotherConversation', originator: sid(8), chat: sid(9), quote } }));
    expect(other.text()).toContain('Reply to a message in another conversation');
    await other.findAll('button').find((b) => b.text() === 'Open that conversation at the message')!.trigger('click');
    expect(other.emitted('jump')).toEqual([[8, 9]]);

    const noChat = mountItem(message(21, { reply: { kind: 'inAnotherConversation', originator: sid(8), chat: null, quote } }));
    expect(noChat.findAll('.oma-reply button')).toHaveLength(0);

    const miss = mountItem(message(22, { reply: { kind: 'notUniquelyIdentified' } }));
    expect(miss.text()).toContain('No uniquely identified message with this identifier is in the archive');
    expect(miss.findAll('.oma-reply button')).toHaveLength(0);
  });

  it('reads reaction chips as text with "derived" and "cannot be determined" states', () => {
    const w = mountItem(
      message(23, {
        reactions: {
          rows: { n: 5, capped: false },
          chips: [
            { kind: 'love', emoji: null, sender: { kind: 'handle', handle: sid(2) }, rows: [sid(30)], state: 'shown' },
            { kind: 'laugh', emoji: null, sender: { kind: 'me' }, rows: [sid(31), sid(32)], state: 'undeterminable' },
            { kind: 'emoji', emoji: text('🎉'), sender: { kind: 'notRecorded' }, rows: [sid(33)], state: 'added' },
            { kind: 'like', emoji: null, sender: { kind: 'me' }, rows: [sid(34)], state: 'hidden' },
          ],
          moreChips: 0,
          unpaired: [sid(35)],
        },
        replyCount: { n: 2, capped: false },
      }),
    );
    const chips = w.findAll('.oma-chip');
    expect(chips).toHaveLength(3);
    expect(chips[0]!.text()).toBe('Loved by synthetic@example.invalid, recorded (derived)');
    expect(chips[1]!.text()).toBe('Laughed by You, cannot be determined');
    expect(chips[2]!.text()).toContain('🎉');
    expect(chips[2]!.text()).toContain('sender not recorded');
    expect(w.text()).toContain('1 removed later (derived)');
    expect(w.text()).toContain('Removal of an emoji reaction (emoji not recorded)');
    expect(w.text()).toContain('2 replies across conversations');
    // Chips are not controls until the details panel lists their rows.
    expect(w.findAll('.oma-reactions button')).toHaveLength(0);
  });

  it('shows labels, Recently Deleted lines and the direction qualifiers', () => {
    const w = mountItem(
      message(24, {
        direction: 'notRecorded',
        sender: { kind: 'directionNotRecorded' },
        labels: [{ kind: 'scheduleType', value: d(2) }, { kind: 'unkeyed' }, { kind: 'provisional' }],
        recentlyDeleted: [{ session: sid(1), chat: null, notListedLater: [sid(99)] }],
        extras: false,
      }),
    );
    const t = w.text();
    expect(t).toContain('Direction not recorded');
    expect(t).toContain('schedule_type 2 recorded');
    expect(t).toContain('it may appear more than once');
    expect(t).toContain('Provisional (untested iOS version)');
    expect(t).toContain('Listed in Recently Deleted in the backup imported on');
    expect(t).toContain('Not listed in a later backup of the same set');
    expect(t).toContain('Reactions and replies could not be read for this message');
    expect(w.get('article').classes()).toContain('oma-dir-notRecorded');
  });

  it('shows the sender above a run only when asked, and keeps it in the accessible name', () => {
    const hidden = mountItem(message(25));
    const sender = hidden.get('.oma-item-sender');
    expect(sender.classes()).toContain('visually-hidden');
    expect(hidden.get('header').text()).toContain('+1 555 0100');
    const shown = mountItem(message(26), { showSender: true });
    expect(shown.get('.oma-item-sender').classes()).not.toContain('visually-hidden');
    // A sender that is a qualifier is always visible.
    const none = mountItem(message(27, { sender: { kind: 'noSender' } }));
    expect(none.get('.oma-item-sender').classes()).not.toContain('visually-hidden');
    expect(none.text()).toContain('No sender recorded');
  });

  it('shows a service divider where the service changes', () => {
    const w = mountItem(message(28, { service: 'sms' }), { serviceChange: 'sms' });
    expect(w.get('.oma-service-sep').text()).toBe('Service: SMS');
    expect(w.get('article').text()).toContain('Service: SMS');
    expect(mountItem(message(29)).find('.oma-service-sep').exists()).toBe(false);
  });

  it('never renders a read, delivery or sent word', () => {
    const w = mountItem(message(30, { direction: 'you', sender: { kind: 'fromMe' }, body: textBody([plain('hi')]) }));
    expect(w.text()).not.toMatch(/\b(?:Read|Delivered|Seen|Sent)\b/);
    expect(w.get('article').classes()).toContain('oma-dir-you');
  });
});

describe('SystemRow', () => {
  const EVENTS: ItemKindDto[] = [
    'participantAdded',
    'ownNumberChanged',
    'participantRemoved',
    'groupRenamed',
    'participantLeft',
    'groupPhotoChanged',
    'groupPhotoRemoved',
    'backgroundChanged',
    'backgroundRemoved',
    'legacySharedLocation',
    'audioKept',
    'sharePlay',
  ];

  it('renders every event kind from closed sentences, listing addresses without roles', () => {
    for (const item of EVENTS) {
      const w = mountItem(
        message(40, {
          item,
          sender: { kind: 'resolved', handle: sid(1) },
          event: { handles: [sid(2), sid(77)], title: item === 'groupRenamed' ? text('New ‮name') : null, itemType: d(1), groupActionType: d(0) },
          body: { kind: 'noBody' },
        }),
      );
      expect(w.find('.oma-system').exists(), item).toBe(true);
      expect(w.find('.oma-bubble').exists(), item).toBe(false);
      const t = w.text();
      expect(t, item).toContain('Addresses recorded with this event:');
      expect(t, item).toContain('synthetic@example.invalid');
      expect(t, item).toContain('address not loaded with this page');
      // The row's own sender is never shown as the actor.
      expect(t, item).not.toContain('+1 555 0100');
      expect(t, item).not.toMatch(/\bby\b/);
      if (item === 'groupRenamed') expect(t).toContain('⟨U+202E⟩');
    }
  });

  it('names the raw codes of an unrecognised item, followed by its text', () => {
    const w = mountItem(
      message(41, {
        item: 'unknown',
        event: { handles: [], title: null, itemType: d(99), groupActionType: d(3) },
        body: textBody([plain('some recorded text')]),
      }),
    );
    expect(w.text()).toContain('Unrecognised item (item_type 99, group_action_type 3)');
    expect(w.text()).toContain('some recorded text');
  });

  it('shows association rows that stay items with their sender and recorded text', () => {
    const kinds: AssocKindDto[] = ['stickerLegacy', 'pollVote', 'tapbackLove', 'removedEmoji', 'unknown'];
    for (const kind of kinds) {
      const w = mountItem(
        message(42, {
          association: {
            visibility: kind === 'tapbackLove' ? { kind: 'targetInAnotherChat', target: sid(3) } : { kind: 'notAReaction' },
            kind,
            shape: 'part',
            targetPart: d(0),
            emoji: null,
          },
          body: textBody([plain('Loved “something”')]),
        }),
      );
      expect(w.find('.oma-system').exists(), kind).toBe(true);
      expect(w.text(), kind).toContain('+1 555 0100');
      expect(w.text(), kind).toContain('Text recorded with this item:');
    }
    const sticker = mountItem(
      message(43, { association: { visibility: { kind: 'notAReaction' }, kind: 'stickerLegacy', shape: null, targetPart: null, emoji: null } }),
    );
    expect(sticker.text()).toContain('Sticker placed on a message');
  });
});
