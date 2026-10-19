// The date-jump dialog (M5 §8.7, §10, §13.8, slice 5i), with a fake
// day-boundary builder: the 43 grid bounds Rust counts, days marked by
// text and shape, the APG grid keyboard, picking a day (count, then
// seek), "No messages on this day" with the nearest earlier and later
// items, and the typed ISO date.
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
import { IpcError } from '@/services/ipc';
import DateJumpDialog from '@/components/viewer/DateJumpDialog.vue';
import { DATE_JUMP_TEXT, fill } from '@/services/viewerText';
import type { DayBoundaryBuilder } from '@/services/time';
import { useViewer } from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import type { GetDayCountsRequest, SeekDateRequest } from '@/types';
import { ARCHIVE_ID, sid, snapshot, viewerSession } from './helpers';

/** A fake zone: day N after 2026-01-01 starts at N × 1,000 ms. */
const DAY = 1000;
const dayNo = (y: number, m: number, d: number) => Math.round((Date.UTC(y, m, d) - Date.UTC(2026, 0, 1)) / 86_400_000);
const build: DayBoundaryBuilder = (y, m, d) => dayNo(y, m, d) * DAY;

/** Visible items per fake day number. */
let items: Map<number, number>;
let seekAnswer: (r: SeekDateRequest) => number | null;

function countsCalls(): GetDayCountsRequest[] {
  return fake.calls.filter(([n]) => n === 'getDayCounts').map(([, a]) => a as GetDayCountsRequest);
}
function seekCalls(): SeekDateRequest[] {
  return fake.calls.filter(([n]) => n === 'seekDate').map(([, a]) => a as SeekDateRequest);
}

async function mountDialog() {
  await useViewer().open(ARCHIVE_ID);
  const w = mount(DateJumpDialog, {
    attachTo: document.body,
    // 2026-10-21 (a Wednesday), in UTC mode.
    props: { open: true, chatId: sid(4), initial: Date.UTC(2026, 9, 21, 12), build },
  });
  await flushPromises();
  return w;
}

function cell(w: Awaited<ReturnType<typeof mountDialog>>, day: number, inMonth = true) {
  return w
    .findAll('.oma-cal-day')
    .find((b) => b.find('span[aria-hidden="true"]').text() === String(day) && b.classes('oma-cal-outside') !== inMonth)!;
}

beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  items = new Map([
    [dayNo(2026, 9, 19), 3],
    [dayNo(2026, 9, 21), 1],
  ]);
  seekAnswer = (r) => (r.dir === 'onOrAfter' ? 77 : null);
  fake.handlers = {
    openViewer: () => Promise.resolve(viewerSession({ viewer: 5 })),
    getDayCounts: (arg) => {
      const r = arg as GetDayCountsRequest;
      const counts = r.boundsUtcMs.slice(0, -1).map((b) => items.get(b / DAY) ?? 0);
      return Promise.resolve({ counts });
    },
    seekDate: (arg) => Promise.resolve({ messageId: seekAnswer(arg as SeekDateRequest) }),
  };
  const s = snapshot();
  s.settings.viewer.timeZone = 'utc';
  useSettings().snapshot = s;
});

afterEach(() => {
  document.body.innerHTML = '';
});

describe('DateJumpDialog', () => {
  it('counts the month’s 6-week grid with 43 bounds from the injected builder', async () => {
    const w = await mountDialog();
    const [first] = countsCalls();
    expect(first!.viewer).toBe(5);
    expect(first!.chatId).toBe(4);
    expect(first!.boundsUtcMs).toHaveLength(43);
    // October 2026 starts on a Thursday: the grid starts on Monday 28 September.
    expect(first!.boundsUtcMs[0]).toBe(build(2026, 8, 28));
    expect(w.findAll('.oma-cal-day')).toHaveLength(42);
    expect(w.find('#oma-cal-title').text()).toContain('2026');
  });

  it('marks days with items by a dot and words, not colour alone', async () => {
    const w = await mountDialog();
    const busy = cell(w, 19);
    expect(busy.classes()).toContain('oma-cal-has');
    expect(busy.find('.oma-cal-dot').exists()).toBe(true);
    expect(busy.find('.visually-hidden').text()).toContain(fill(DATE_JUMP_TEXT.items, { n: 3 }));
    expect(cell(w, 21).find('.visually-hidden').text()).toContain(DATE_JUMP_TEXT.oneItem);
    const quiet = cell(w, 20);
    expect(quiet.find('.oma-cal-dot').exists()).toBe(false);
    expect(quiet.find('.visually-hidden').text()).toContain(DATE_JUMP_TEXT.noItems);
  });

  it('starts on the given day, focused, with one tab stop in the grid', async () => {
    const w = await mountDialog();
    const start = cell(w, 21);
    expect(start.attributes('tabindex')).toBe('0');
    expect(w.findAll('.oma-cal-day[tabindex="0"]')).toHaveLength(1);
    expect(document.activeElement).toBe(start.element);
    expect(start.element.closest('td')?.getAttribute('aria-selected')).toBe('true');
  });

  it('moves by day, week and month with the APG keys, recounting a new month', async () => {
    const w = await mountDialog();
    const grid = w.find('table[role="grid"]');
    await grid.trigger('keydown', { key: 'ArrowRight' });
    await flushPromises();
    expect(document.activeElement).toBe(cell(w, 22).element);
    await grid.trigger('keydown', { key: 'ArrowUp' });
    await flushPromises();
    expect(document.activeElement).toBe(cell(w, 15).element);
    await grid.trigger('keydown', { key: 'Home' });
    await flushPromises();
    expect(document.activeElement).toBe(cell(w, 12).element);
    expect(countsCalls()).toHaveLength(1);
    await grid.trigger('keydown', { key: 'PageDown' });
    await flushPromises();
    expect(countsCalls()).toHaveLength(2);
    expect(countsCalls()[1]!.boundsUtcMs[0]).toBe(build(2026, 9, 26)); // November's grid
    expect(document.activeElement).toBe(cell(w, 12).element);
    await grid.trigger('keydown', { key: 'PageUp', shiftKey: true });
    await flushPromises();
    expect(countsCalls().at(-1)!.boundsUtcMs[0]).toBe(build(2025, 9, 27)); // November 2025's grid
  });

  it('picks a day with items: counts it, seeks its first item and asks to jump there', async () => {
    const w = await mountDialog();
    await cell(w, 19).trigger('click');
    await flushPromises();
    expect(countsCalls().at(-1)!.boundsUtcMs).toEqual([build(2026, 9, 19), build(2026, 9, 20)]);
    expect(seekCalls()).toEqual([{ viewer: 5, chatId: 4, utcMs: build(2026, 9, 19), dir: 'onOrAfter' }]);
    expect(w.emitted('jump')).toEqual([[sid(77)]]);
  });

  it('says when a day has no messages, and offers the nearest earlier and later items', async () => {
    const w = await mountDialog();
    await cell(w, 20).trigger('click');
    await flushPromises();
    expect(seekCalls()).toHaveLength(0);
    expect(w.find('.oma-cal-outcome').text()).toContain(DATE_JUMP_TEXT.noMessages);
    const button = (t: string) => w.findAll('button').find((b) => b.text() === t)!;
    await button(DATE_JUMP_TEXT.earlier).trigger('click');
    await flushPromises();
    expect(seekCalls().at(-1)).toEqual({ viewer: 5, chatId: 4, utcMs: build(2026, 9, 20), dir: 'before' });
    expect(w.find('.oma-cal-outcome').text()).toContain(DATE_JUMP_TEXT.noEarlier);
    await button(DATE_JUMP_TEXT.later).trigger('click');
    await flushPromises();
    expect(seekCalls().at(-1)).toEqual({ viewer: 5, chatId: 4, utcMs: build(2026, 9, 21), dir: 'onOrAfter' });
    expect(w.emitted('jump')).toEqual([[sid(77)]]);
  });

  it('takes a typed ISO date, and refuses one that is not a date', async () => {
    const w = await mountDialog();
    const input = w.find('#oma-cal-typed');
    expect(input.attributes('spellcheck')).toBe('false');
    expect(input.attributes('autocomplete')).toBe('off');
    await input.setValue('2026-02-30');
    await w.find('form').trigger('submit');
    expect(w.find('[role="alert"]').text()).toBe(DATE_JUMP_TEXT.typedBad);
    expect(input.attributes('aria-invalid')).toBe('true');
    await input.setValue('2026-10-19');
    await w.find('form').trigger('submit');
    await flushPromises();
    expect(w.find('[role="alert"]').exists()).toBe(false);
    expect(w.emitted('jump')).toEqual([[sid(77)]]);
  });

  it('keeps the grid usable when counting fails, and reports a failed lookup', async () => {
    fake.handlers.getDayCounts = () => Promise.reject(new IpcError('get_day_counts', 'queryTooBroad', 'Too broad.'));
    const w = await mountDialog();
    expect(w.text()).toContain(DATE_JUMP_TEXT.countsFailed);
    expect(w.findAll('.oma-cal-has')).toHaveLength(0);
    await cell(w, 19).trigger('click');
    await flushPromises();
    expect(w.find('.oma-cal-outcome').text()).toContain(DATE_JUMP_TEXT.failed);
    expect(w.emitted('jump')).toBeUndefined();
  });
});
