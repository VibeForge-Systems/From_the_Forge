// The details panel and the reply thread (M5 §3.6, §3.7, §7.3, §8.4),
// slice 5h3: values under their source column names, raw times with a UTC
// form at the unit's precision, captured versions and phone edits with
// withheld text revealed only on click, attachment observations as codes,
// reaction rows paged on request, raw chat flags "meaning unverified", no
// read, delivery or sent word, and the reply thread with its own ids.
import { beforeEach, describe, expect, it, vi } from 'vitest';
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
import MessageDetails from '@/components/viewer/MessageDetails.vue';
import ReplyThread from '@/components/viewer/ReplyThread.vue';
import ThreadItem from '@/components/viewer/ThreadItem.vue';
import { useViewer } from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import type {
  DecimalString,
  GetMessageDetailRequest,
  HandleDto,
  MessageDetailDto,
  ReactionRecordDto,
  SafeId,
} from '@/types';
import {
  ARCHIVE_ID,
  attachmentFacts,
  message,
  messageDetail,
  sid,
  snapshot,
  text,
  textPiece,
  threadPage,
  viewerSession,
} from './helpers';

const d = (s: string) => s as DecimalString;
const HANDLES = new Map<SafeId, HandleDto>([[sid(1), { id: sid(1), address: text('+1 555 0100'), service: 'iMessage' }]]);

function setup(): void {
  useSettings().snapshot = snapshot();
  const viewer = useViewer();
  viewer.session = viewerSession({
    imports: [{ session: sid(1), state: 'completed', startedMs: Date.UTC(2026, 2, 3), endedMs: null }],
  });
  viewer.archiveId = ARCHIVE_ID;
}

function detailRequests(): GetMessageDetailRequest[] {
  return fake.calls.filter(([n]) => n === 'getMessageDetail').map(([, a]) => a as GetMessageDetailRequest);
}

function row(id: number, adds = true): ReactionRecordDto {
  return {
    id: sid(id),
    kind: adds ? 'tapbackLove' : 'removedLove',
    adds,
    emoji: null,
    sender: { kind: 'handle', handle: sid(1) },
    time: { ms: Date.UTC(2026, 0, 2), raw: d('1'), unit: 'nanoseconds', state: null, before2007: false },
    targetPart: d('0'),
  };
}

function rich(): MessageDetailDto {
  const m = message(5, {
    date: { ms: Date.UTC(2026, 0, 1), raw: d('788918400'), unit: 'seconds', state: null, before2007: false },
  });
  return messageDetail(m, {
    versions: [
      {
        body: sid(9),
        current: false,
        firstSession: sid(1),
        lastSession: sid(1),
        text: { kind: 'withheld', reason: 'unsent' },
        textUnchanged: false,
        editLabel: 'unsentParts',
        edits: [
          {
            part: d('0'),
            ordinal: d('1'),
            at: { kind: 'integer', raw: { kind: 'integer', value: d('12345') }, ms: null },
            text: { kind: 'withheld', reason: 'unsent' },
          },
          { part: d('0'), ordinal: d('2'), at: { kind: null, raw: null, ms: null }, text: { kind: 'failed', error: 'truncated' } },
        ],
        moreEdits: 3,
        editTexts: 'decoded',
      },
      ...messageDetail(m).versions,
    ],
    account: text('synthetic@example.invalid'),
    chats: [
      {
        chat: sid(4),
        style: { value: d('43'), state: null },
        isFiltered: { value: null, state: 'absent' },
        isArchived: { value: d('0'), state: null },
        history: [],
      },
    ],
    attachments: [
      attachmentFacts(3, {
        card: { kind: 'notInBackup' },
        file: null,
        observations: [
          {
            session: sid(1),
            lookup: 'notListedAtSourcedKey',
            lookupRaw: d('40'),
            copy: null,
            copyRaw: d('99'),
            pathClass: 'smsAttachments',
            stored: false,
            sizeMatchedManifest: null,
          },
        ],
        hideAttachment: { value: d('1'), state: null },
      }),
    ],
    replyProbe: 'sharedByUnkeyed',
  });
}

beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = {};
});

describe('MessageDetails', () => {
  it('lists the recorded values under their column names, with raw times and a UTC form at the unit precision', async () => {
    setup();
    fake.handlers.getMessageDetail = () => Promise.resolve(rich());
    const w = mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES } });
    await flushPromises();
    const t = w.text();
    expect(detailRequests()).toEqual([{ viewer: 1, messageId: 5 }]);
    expect(t).toContain('is_from_me');
    expect(t).toContain('Received by the backed-up phone');
    expect(t).toContain('+1 555 0100');
    expect(t).toContain('raw 788918400');
    expect(t).toContain('2026-01-01T00:00:00Z (UTC');
    expect(t).not.toContain('2026-01-01T00:00:00.');
    expect(t).toContain('date_edited');
    expect(t).toContain('No date recorded (NULL)');
    expect(t).toContain('destination_caller_id (Account, meaning unverified)');
    expect(t).toContain('synthetic@example.invalid');
    expect(t).toContain('Column not in this backup');
    expect(t).toContain('style (meaning unverified): 43');
    expect(t).toContain('is_filtered (meaning unverified): Column not in this backup');
    expect(t).toContain('Unkeyed messages share this identifier');
  });

  it('lists captured versions and phone edits, withheld until "Show earlier text"', async () => {
    setup();
    fake.handlers.getMessageDetail = () => Promise.resolve(rich());
    const w = mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES } });
    await flushPromises();
    const t = w.text();
    expect(t).toContain('Versions this archive captured');
    expect(t).toContain('Search covers captured versions only');
    expect(t).toContain('Earlier stored version');
    expect(t).toContain('First kept by the import of');
    expect(t).toContain('Earlier text was kept from an import before it was unsent.');
    expect(t).toContain('Versions recorded on the phone');
    expect(t).toContain('Part 0, entry 1');
    expect(t).toContain('time as recorded (whole number; unit assumed): 12345');
    expect(t).toContain('Edit text could not be decoded: The formatted text ends early');
    expect(t).toContain('3 more recorded edits not shown');
    expect(t).toContain('Synthetic text');
    expect(detailRequests().every((r) => r.reveal === undefined)).toBe(true);

    const revealed = rich();
    revealed.versions[0]!.text = { kind: 'text', pieces: [textPiece('Synthetic earlier words')] };
    fake.handlers.getMessageDetail = (arg) =>
      Promise.resolve((arg as GetMessageDetailRequest).reveal ? revealed : rich());
    await w.findAll('button').find((b) => b.text() === 'Show earlier text')!.trigger('click');
    await flushPromises();
    expect(detailRequests().filter((r) => r.reveal?.earlierText === true).length).toBeGreaterThanOrEqual(2);
    expect(w.text()).toContain('Synthetic earlier words');
  });

  it('lists attachment observations as codes with their raw values', async () => {
    setup();
    fake.handlers.getMessageDetail = () => Promise.resolve(rich());
    const w = mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES } });
    await flushPromises();
    const t = w.text();
    expect(t).toContain('transfer_name');
    expect(t).toContain('IMG_3.JPG');
    expect(t).toContain('hide_attachment (meaning unverified)');
    expect(t).toContain('This file was not in the backup');
    expect(t).toContain('Lookup: Not in the backup (code 40)');
    expect(t).toContain('Copy: code 99 (code 99)');
    expect(t).toContain('Recorded location: Messages attachments folder');
    expect(t).toContain('no file stored');
    expect(t).toContain('No file stored (20 KB declared)');
  });

  it('pages the reaction rows only on request, after the last row served', async () => {
    setup();
    fake.handlers.getMessageDetail = (arg) => {
      const r = arg as GetMessageDetailRequest;
      if (!r.reactionPage) return Promise.resolve(messageDetail(message(5)));
      const first = r.reactionPage.after === undefined;
      return Promise.resolve(
        messageDetail(message(5), {
          reactions: {
            target: sid(5),
            keyed: true,
            rows: first ? [row(20), row(21, false)] : [row(22)],
            total: { n: 3, capped: false },
            hasMore: first,
          },
        }),
      );
    };
    const w = mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES } });
    await flushPromises();
    expect(w.text()).toContain('Chips are derived from the recorded rows');
    expect(w.text()).not.toContain('Loved');
    await w.findAll('button').find((b) => b.text() === 'Show the recorded reaction rows')!.trigger('click');
    await flushPromises();
    expect(detailRequests().at(-1)?.reactionPage).toEqual({});
    expect(w.text()).toContain('3 rows recorded');
    expect(w.text()).toContain('Loved (add)');
    expect(w.text()).toContain('Removed a heart (removal)');
    expect(w.text()).toContain('part 0');
    await w.findAll('button').find((b) => b.text() === 'Show more rows')!.trigger('click');
    await flushPromises();
    expect(detailRequests().at(-1)?.reactionPage).toEqual({ after: 21 });
    expect(w.findAll('.oma-details-reactions li')).toHaveLength(3);
    expect(w.findAll('button').some((b) => b.text() === 'Show more rows')).toBe(false);
  });

  it('opens with the reaction rows when the bubble asked for them', async () => {
    setup();
    fake.handlers.getMessageDetail = () => Promise.resolve(messageDetail(message(5)));
    mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES, reactions: true } });
    await flushPromises();
    expect(detailRequests()[0]?.reactionPage).toEqual({});
  });

  it('shows no read, delivery or sent word', async () => {
    setup();
    fake.handlers.getMessageDetail = () => Promise.resolve(rich());
    const w = mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES } });
    await flushPromises();
    expect(w.text()).not.toMatch(/\bdeliver(?:ed|y)\b|\bread (?:by|at)\b|\bunread\b|\bis_read\b|\bis_sent\b/i);
  });

  it('says why it failed, with Retry', async () => {
    setup();
    const { IpcError } = await import('@/services/ipc');
    fake.handlers.getMessageDetail = () => Promise.reject(new IpcError('get_message_detail', 'messageNotFound', 'x'));
    const w = mount(MessageDetails, { props: { messageId: sid(5), handles: HANDLES } });
    await flushPromises();
    expect(w.text()).toContain('That message is no longer in the archive.');
    fake.handlers.getMessageDetail = () => Promise.resolve(messageDetail(message(5)));
    await w.findAll('button').find((b) => b.text() === 'Retry')!.trigger('click');
    await flushPromises();
    expect(w.text()).toContain('Synthetic text');
  });
});

describe('opening the panel', () => {
  it('a thread item opens its details, and a reply count opens the reply thread', async () => {
    setup();
    const m = message(5, { replyCount: { n: 2, capped: false } });
    const w = mount(ThreadItem, { props: { message: m, handles: HANDLES, dayLabel: null } });
    await w.findAll('button').find((b) => b.text() === 'Details')!.trigger('click');
    expect(useViewer().panel).toEqual({ kind: 'details', message: 5, reactions: false });
    await w.findAll('button').find((b) => b.text().includes('2 replies'))!.trigger('click');
    expect(useViewer().panel).toEqual({ kind: 'replies', message: 5 });
  });
});

describe('ReplyThread', () => {
  it('lists the replies with their own ids and no quote of the originator', async () => {
    setup();
    const page = threadPage([7, 8]);
    page.items = page.items.map((m) => ({ ...m, reply: { kind: 'resolved', originator: sid(5), quote: { kind: 'noBody' } } }));
    fake.handlers.getReplyThread = () => Promise.resolve(page);
    const w = mount(ReplyThread, { props: { messageId: sid(5) } });
    await flushPromises();
    expect(fake.calls.find(([n]) => n === 'getReplyThread')?.[1]).toEqual({ viewer: 1, messageId: 5 });
    const articles = w.findAll('article');
    expect(articles.map((a) => a.attributes('id'))).toEqual(['oma-r-7', 'oma-r-8']);
    expect(w.find('.oma-reply').exists()).toBe(false);
    expect(w.text()).toContain('across conversations');
  });

  it('says when there are none', async () => {
    setup();
    fake.handlers.getReplyThread = () => Promise.resolve(threadPage([], { hasOlder: false }));
    const w = mount(ReplyThread, { props: { messageId: sid(5) } });
    await flushPromises();
    expect(w.text()).toContain('No replies are recorded for this message.');
  });
});
