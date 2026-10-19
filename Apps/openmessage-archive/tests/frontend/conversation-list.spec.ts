// The conversation list (M5 §8.4, §10, §13.8): a listbox with
// aria-activedescendant over the id snapshot, rows loaded per visible
// window, the label precedence, and the filter's input attributes and
// debounce.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount } from '@vue/test-utils';

// A hand-written fake (see AGENTS.md: rejections from vi.fn() are reported
// as unhandled by vitest 4).
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
import ConversationList from '@/components/viewer/ConversationList.vue';
import ConversationLabel from '@/components/viewer/ConversationLabel.vue';
import { useViewer } from '@/stores/viewer';
import { useSettings } from '@/stores/settings';
import type { ChatLabelDto } from '@/types';
import { ARCHIVE_ID, content, conversationRow, sid, snapshot, text, textPiece, viewerSession } from './helpers';

const TOTAL = 300;
const ids = (n: number) => Array.from({ length: n }, (_, i) => sid(i + 1));

function serve(total = TOTAL): void {
  fake.handlers.openViewer = () => Promise.resolve(viewerSession({ viewer: 3 }));
  fake.handlers.listConversations = () =>
    Promise.resolve({ ids: ids(total), total, truncated: false, skippedRows: 0 });
  fake.handlers.getConversations = (arg) => {
    const [, wanted] = arg as [number, number[]];
    return Promise.resolve({ rows: wanted.map((id) => conversationRow(id)), skippedRows: 0 });
  };
}

async function mountList(selected: number | null = null) {
  const store = useViewer();
  await store.open(ARCHIVE_ID);
  const w = mount(ConversationList, { attachTo: document.body, props: { selected: selected === null ? null : sid(selected) } });
  await flushPromises();
  return w;
}

beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = {};
  serve();
});

afterEach(() => {
  vi.useRealTimers();
  document.body.innerHTML = '';
});

describe('ConversationList', () => {
  it('is a listbox whose active option is named by aria-activedescendant and is mounted', async () => {
    const w = await mountList();
    const box = w.get('[role="listbox"]');
    expect(box.attributes('tabindex')).toBe('0');
    expect(box.attributes('aria-label')).toBe('Conversations');
    const active = box.attributes('aria-activedescendant')!;
    expect(active).toBe('oma-conv-opt-0');
    const option = w.get(`#${active}`);
    expect(option.attributes('role')).toBe('option');
    expect(option.attributes('aria-posinset')).toBe('1');
    expect(option.attributes('aria-setsize')).toBe(String(TOTAL));
    expect(w.findAll('[role="option"]').length).toBeLessThan(TOTAL);
  });

  it('moves the active option with the keyboard and keeps it mounted', async () => {
    const w = await mountList();
    const box = w.get('[role="listbox"]');
    await box.trigger('keydown', { key: 'ArrowDown' });
    await box.trigger('keydown', { key: 'ArrowDown' });
    expect(box.attributes('aria-activedescendant')).toBe('oma-conv-opt-2');
    await box.trigger('keydown', { key: 'End' });
    await flushPromises();
    expect(box.attributes('aria-activedescendant')).toBe(`oma-conv-opt-${TOTAL - 1}`);
    expect(w.find(`#oma-conv-opt-${TOTAL - 1}`).exists()).toBe(true);
    expect(w.get(`#oma-conv-opt-${TOTAL - 1}`).attributes('aria-posinset')).toBe(String(TOTAL));
    await box.trigger('keydown', { key: 'PageUp' });
    expect(box.attributes('aria-activedescendant')).toBe(`oma-conv-opt-${TOTAL - 11}`);
    await box.trigger('keydown', { key: 'Home' });
    expect(box.attributes('aria-activedescendant')).toBe('oma-conv-opt-0');
    await box.trigger('keydown', { key: 'ArrowUp' });
    expect(box.attributes('aria-activedescendant')).toBe('oma-conv-opt-0');
  });

  it('opens the active option with Enter, and an option on click', async () => {
    const w = await mountList();
    const box = w.get('[role="listbox"]');
    await box.trigger('keydown', { key: 'ArrowDown' });
    await box.trigger('keydown', { key: 'Enter' });
    expect(w.emitted('open')).toEqual([[2]]);
    await w.get('#oma-conv-opt-3').trigger('click');
    expect(w.emitted('open')!.at(-1)).toEqual([4]);
    expect(w.get('[role="listbox"]').attributes('aria-activedescendant')).toBe('oma-conv-opt-3');
  });

  it('leaves modified keys to the shortcuts', async () => {
    const w = await mountList();
    const box = w.get('[role="listbox"]');
    await box.trigger('keydown', { key: 'ArrowDown', ctrlKey: true });
    expect(box.attributes('aria-activedescendant')).toBe('oma-conv-opt-0');
  });

  it('marks the open conversation selected and makes it active', async () => {
    const w = await mountList(5);
    expect(w.get('[role="listbox"]').attributes('aria-activedescendant')).toBe('oma-conv-opt-4');
    expect(w.get('#oma-conv-opt-4').attributes('aria-selected')).toBe('true');
    expect(w.get('#oma-conv-opt-0').attributes('aria-selected')).toBe('false');
  });

  it('loads the rows of the visible window and renders them', async () => {
    vi.useFakeTimers();
    const w = await mountList();
    await vi.advanceTimersByTimeAsync(100);
    await flushPromises();
    const loads = fake.calls.filter(([n]) => n === 'getConversations');
    expect(loads.length).toBeGreaterThan(0);
    const requested = (loads[0]![1] as [number, number[]])[1];
    expect(requested[0]).toBe(1);
    expect(requested.length).toBeLessThan(TOTAL);
    expect(w.get('#oma-conv-opt-0').text()).toContain('Synthetic chat 1');
    expect(w.get('#oma-conv-opt-0').text()).toContain('12 items recorded');
  });

  it('debounces the filter and sends it to Rust; the field has no spellcheck or autocomplete', async () => {
    vi.useFakeTimers();
    const w = await mountList();
    const input = w.get('input[type="search"]');
    expect(input.attributes('spellcheck')).toBe('false');
    expect(input.attributes('autocomplete')).toBe('off');
    expect(input.attributes('autocapitalize')).toBe('off');
    fake.calls.length = 0;
    await input.setValue('syn');
    await input.setValue('synth');
    await vi.advanceTimersByTimeAsync(100);
    expect(fake.calls.filter(([n]) => n === 'listConversations')).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(300);
    const lists = fake.calls.filter(([n]) => n === 'listConversations');
    expect(lists).toHaveLength(1);
    expect(lists[0]![1]).toEqual([3, 'recent', 'synth']);
  });

  it('says when nothing matches', async () => {
    fake.handlers.listConversations = () => Promise.resolve({ ids: [], total: 0, truncated: false, skippedRows: 0 });
    const w = await mountList();
    expect(w.text()).toContain('This archive has no conversations');
  });

  it('shows previews only when the setting is on, with masked pieces', async () => {
    vi.useFakeTimers();
    fake.handlers.getConversations = (arg) => {
      const [, wanted] = arg as [number, number[]];
      return Promise.resolve({
        rows: wanted.map((id) =>
          conversationRow(id, {
            preview: {
              message: sid(9),
              ms: 0,
              item: 'message',
              direction: 'you',
              body: { kind: 'text', pieces: [textPiece('see you <b>'), { kind: 'masked', mask: 'oneTimeCode' }] },
            },
          }),
        ),
        skippedRows: 0,
      });
    };
    const settings = useSettings();
    settings.snapshot = snapshot();
    const w = await mountList();
    await vi.advanceTimersByTimeAsync(100);
    await flushPromises();
    const first = w.get('#oma-conv-opt-0');
    expect(first.text()).toContain('You:');
    expect(first.text()).toContain('see you <b>');
    // "You:" and the text keep their space (Vue drops a whitespace-only
    // text node at the edge of an element; seen in the slice 5k smoke test).
    expect(first.get('.oma-conv-preview').element.textContent).toMatch(/You: \S/);
    expect(first.text()).toContain('One-time code hidden');
    expect(first.find('b').exists()).toBe(false);
    const off = snapshot();
    off.settings.viewer.listPreviews = false;
    settings.snapshot = off;
    await flushPromises();
    expect(w.get('#oma-conv-opt-0').text()).not.toContain('see you');
  });
});

describe('ConversationLabel precedence', () => {
  const render = (label: ChatLabelDto) =>
    mount(ConversationLabel, { props: { label, chatId: sid(42) } }).text();

  it('uses the recorded name', () => {
    expect(render({ kind: 'name', value: content('Family') })).toBe('Family');
  });

  it('does not fall through when the name is unreadable', () => {
    expect(render({ kind: 'nameUnreadable', state: 'oversized' })).toBe(
      'Name recorded but too large or unreadable',
    );
  });

  it('lists the participants as recorded and says so', () => {
    const label: ChatLabelDto = {
      kind: 'participants',
      first: [
        { id: sid(1), address: text('+1 555 0100'), service: 'sms' },
        { id: sid(2), address: text('b@example.invalid'), service: 'iMessage' },
      ],
      more: 3,
    };
    const shown = render(label);
    expect(shown).toContain('+1 555 0100, b@example.invalid');
    // Spaces between the addresses, the count and the note survive Vue's
    // whitespace condensing (they were lost before the slice 5k smoke test).
    expect(shown).toMatch(/b@example\.invalid\S* \+3 \(from participants\)$/);
  });

  it('falls back to the chat identifier, then to a sentence with the id', () => {
    expect(render({ kind: 'identifier', value: content('chat000000000001') })).toBe('chat000000000001');
    expect(render({ kind: 'unnamed' })).toBe('Conversation without a name or participants (#42)');
  });

  it('shows direction controls in a name as visible tokens', () => {
    expect(render({ kind: 'name', value: content('a‮b', { bidiControls: true }) })).toContain('⟨U+202E⟩');
  });
});

describe('ConversationList (review fixes)', () => {
  it('says how many addresses are recorded, never "Group", and marks a last date before 2007', async () => {
    vi.useFakeTimers();
    fake.handlers.getConversations = (arg) => {
      const [, wanted] = arg as [number, number[]];
      return Promise.resolve({
        rows: wanted.map((id) =>
          conversationRow(id, { addresses: id === 1 ? 2 : 1, group: true, lastDatedMs: id === 2 ? Date.UTC(2001, 0, 1) : Date.UTC(2026, 0, 1) }),
        ),
        skippedRows: 0,
      });
    };
    const w = await mountList();
    await vi.advanceTimersByTimeAsync(100);
    await flushPromises();
    const first = w.get('#oma-conv-opt-0').text();
    expect(first).toContain('2 addresses recorded');
    expect(first).not.toMatch(/\bGroup\b/);
    expect(w.get('#oma-conv-opt-1').text()).not.toContain('addresses recorded');
    expect(w.get('#oma-conv-opt-1').text()).toContain('Before 2007');
    expect(first).not.toContain('Before 2007');
  });
});
