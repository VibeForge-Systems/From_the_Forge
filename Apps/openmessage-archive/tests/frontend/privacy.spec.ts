// The privacy controls (M5 §9.2, §9.3, §6.7, §13.8), slice 5j: the curtain
// unmounts the viewer, stops media, leaves fullscreen, closes the viewer's
// dialogs and moves focus to "Show"; Show returns to the same message; blur
// and idle hide; the page guards; the message menu's copy actions and the
// link guard. Dialogs get a minimal showModal/close, which jsdom lacks.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils';
import { defineComponent, h } from 'vue';
import { createMemoryHistory, createRouter } from 'vue-router';

const fake = vi.hoisted(() => ({
  calls: [] as [string, unknown][],
  handlers: {} as Record<string, (arg: unknown) => Promise<unknown>>,
  copied: [] as string[],
  copyOk: true,
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
vi.mock('@/services/features', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/features')>();
  return {
    ...actual,
    copyText: (text: string) => {
      fake.copied.push(text);
      return Promise.resolve(fake.copyOk);
    },
  };
});
import ViewerView from '@/views/ViewerView.vue';
import ConversationList from '@/components/viewer/ConversationList.vue';
import { useLayout } from '@/stores/layout';
import PrivacyCurtain from '@/components/PrivacyCurtain.vue';
import { guardedKey, installViewerGuards } from '@/services/guards';
import { matchShortcut } from '@/services/shortcuts';
import { useNotifications } from '@/stores/notifications';
import { IDLE_CHECK_MS, usePrivacy } from '@/stores/privacy';
import { useSettings } from '@/stores/settings';
import { useViewer } from '@/stores/viewer';
import type { GetThreadPageRequest, MessageDto, SegmentDto, ViewerPreferences } from '@/types';
import { ARCHIVE_ID, attachment, content, conversationRow, message, sid, snapshot, stubMatchMedia, threadPage, viewerSession } from './helpers';

const CHAT = 4;
const VIEWER = 5;

function seg(value: string, flags: Partial<Omit<SegmentDto, 'piece'>> = {}): SegmentDto {
  return {
    piece: { kind: 'text', value: content(value) },
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

/** Message 30 carries two links (the second with a hidden character) and a direction control. */
function item(id: number): MessageDto {
  const segments =
    id === 30
      ? [
          seg('see '),
          seg('https://one.invalid/a', { url: true }),
          seg(' or '),
          seg('https://tw​o.invalid', { url: true, urlHidden: true }),
          seg(' ‮done'),
        ]
      : [seg(`hello ${id}`)];
  const base = message(id);
  return {
    ...base,
    body: {
      ...(base.body as Extract<MessageDto['body'], { kind: 'text' }>),
      segments,
      bidiControls: id === 30,
    },
  };
}

function serve(): void {
  fake.handlers.getThreadPage = (arg) => {
    const r = arg as GetThreadPageRequest;
    const ids =
      r.anchor.kind === 'message' ? [r.anchor.message - 1, r.anchor.message, r.anchor.message + 1].filter((n) => n >= 1 && n <= 30) : [28, 29, 30];
    const focus = r.anchor.kind === 'message' ? { id: sid(r.anchor.message), reason: 'anchor' as const } : null;
    return Promise.resolve(threadPage(ids, { items: ids.map(item), hasOlder: false, focus }));
  };
}

const router = () =>
  createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'archive', component: { render: () => null } },
      { path: '/v/:chatId', name: 'viewer-chat', component: { render: () => null } },
    ],
  });

const Harness = defineComponent({
  setup: () => () => [h(ViewerView, { archiveId: ARCHIVE_ID, chatId: String(CHAT) }), h(PrivacyCurtain)],
});

async function mountViewer(): Promise<VueWrapper> {
  const w = mount(Harness, { attachTo: document.body, global: { plugins: [router()] } });
  await flushPromises();
  await flushPromises();
  return w;
}

function setViewerPrefs(change: Partial<ViewerPreferences>): void {
  const s = snapshot();
  s.settings.viewer = { ...s.settings.viewer, ...change };
  useSettings().snapshot = s;
}

const curtain = () => document.querySelector<HTMLDialogElement>('dialog.oma-curtain')!;
const named = (n: string) => fake.calls.filter(([c]) => c === n).map(([, a]) => a);

let mounted: VueWrapper | null = null;

beforeEach(() => {
  setActivePinia(createPinia());
  stubMatchMedia(() => true);
  fake.calls.length = 0;
  fake.copied.length = 0;
  fake.copyOk = true;
  fake.handlers = {
    openViewer: () => Promise.resolve(viewerSession({ viewer: VIEWER })),
    getConversations: (arg) => {
      const [, wanted] = arg as [number, number[]];
      return Promise.resolve({ rows: wanted.map((id) => conversationRow(id)), skippedRows: 0 });
    },
    listConversations: () => Promise.resolve({ ids: [sid(CHAT)], total: 1, truncated: false }),
  };
  serve();
  setViewerPrefs({});
  HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement) {
    this.setAttribute('open', '');
  };
  HTMLDialogElement.prototype.close = function close(this: HTMLDialogElement) {
    if (!this.hasAttribute('open')) return;
    this.removeAttribute('open');
    this.dispatchEvent(new Event('close'));
  };
});

afterEach(() => {
  mounted?.unmount();
  mounted = null;
  vi.useRealTimers();
  document.body.innerHTML = '';
});

describe('the privacy curtain', () => {
  it('unmounts the viewer, stops media, leaves fullscreen, closes dialogs and focuses Show', async () => {
    mounted = await mountViewer();
    const viewer = useViewer();
    expect(mounted.find('[role="feed"]').exists()).toBe(true);
    viewer.dateJumpOpen = true;
    viewer.openImage(attachment(9));
    viewer.pendingPlay = attachment(10);
    const video = document.createElement('video');
    video.setAttribute('src', 'oma-media://localhost/v1/x/stream');
    document.body.append(video);
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => undefined);
    vi.spyOn(HTMLMediaElement.prototype, 'load').mockImplementation(() => undefined);
    let exited = 0;
    Object.defineProperty(document, 'fullscreenElement', { configurable: true, get: () => video });
    Object.defineProperty(document, 'exitFullscreen', {
      configurable: true,
      value: () => {
        exited += 1;
        return Promise.resolve();
      },
    });
    document.title = 'Viewer — OpenMessage Archive';

    await usePrivacy().hide('manual');
    await flushPromises();

    expect(mounted.find('[role="feed"]').exists()).toBe(false);
    expect(mounted.find('.oma-thread-toolbar').exists()).toBe(false);
    expect(document.querySelector('article')).toBeNull();
    expect(pause).toHaveBeenCalled();
    expect(video.hasAttribute('src')).toBe(false);
    expect(exited).toBe(1);
    expect(viewer.media).toBeNull();
    expect(viewer.pendingPlay).toBeNull();
    expect(viewer.dateJumpOpen).toBe(false);
    expect(named('hideViewer')).toEqual([VIEWER]);
    expect(curtain().open).toBe(true);
    expect(document.activeElement?.textContent?.trim()).toBe('Show');
    expect(curtain().textContent).toContain('it is not a lock');
    expect(document.title).toBe('Viewer — OpenMessage Archive');
    // The session stays open; nothing was closed in Rust.
    expect(named('closeViewer')).toEqual([]);
    delete (document as unknown as Record<string, unknown>).fullscreenElement;
    delete (document as unknown as Record<string, unknown>).exitFullscreen;
  });

  it('cannot be dismissed with Escape, and reopens if an engine closes it anyway', async () => {
    mounted = await mountViewer();
    await usePrivacy().hide('manual');
    await flushPromises();
    const cancel = new Event('cancel', { cancelable: true });
    curtain().dispatchEvent(cancel);
    expect(cancel.defaultPrevented).toBe(true);
    curtain().close();
    await flushPromises();
    expect(curtain().open).toBe(true);
    expect(usePrivacy().hidden).toBe(true);
  });

  it('shows the same message again and gives it focus back', async () => {
    mounted = await mountViewer();
    document.getElementById('oma-m-29')!.focus();
    expect(document.activeElement?.id).toBe('oma-m-29');
    fake.calls.length = 0;
    await usePrivacy().hide('manual');
    await flushPromises();
    curtain().querySelector('button')!.click();
    await flushPromises();
    await flushPromises();
    expect(curtain().open).toBe(false);
    const asked = named('getThreadPage') as GetThreadPageRequest[];
    expect(asked.at(-1)?.anchor).toMatchObject({ kind: 'message', message: 29 });
    expect(document.activeElement?.id).toBe('oma-m-29');
    expect(named('openViewer')).toEqual([]);
  });

  it('does nothing outside the viewer page', async () => {
    await usePrivacy().hide('manual');
    expect(usePrivacy().hidden).toBe(false);
  });
});

describe('overlays in a narrow window (review fix, WCAG 2.4.11)', () => {
  it('close the list overlay when a conversation is chosen, and the panel overlay with the panel', async () => {
    stubMatchMedia(() => false);
    mounted = await mountViewer();
    const layout = useLayout();
    layout.leftOverlay = true;
    await flushPromises();
    mounted.findComponent(ConversationList).vm.$emit('open', sid(CHAT));
    await flushPromises();
    expect(layout.leftOverlay).toBe(false);
    useViewer().openDetails(sid(30));
    await flushPromises();
    expect(layout.rightOverlay).toBe(true);
    useViewer().closePanel();
    await flushPromises();
    expect(layout.rightOverlay).toBe(false);
  });
});

describe('blur on focus loss and idle hide', () => {
  it('hide on window blur or a hidden document only when the setting is on', async () => {
    mounted = await mountViewer();
    window.dispatchEvent(new Event('blur'));
    await flushPromises();
    expect(usePrivacy().hidden).toBe(false);
    // A hidden document (minimised) leaves the screen alone with the setting off.
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
    await flushPromises();
    expect(usePrivacy().hidden).toBe(false);
    delete (document as unknown as Record<string, unknown>).visibilityState;
    setViewerPrefs({ blurOnFocusLoss: true });
    window.dispatchEvent(new Event('blur'));
    await flushPromises();
    expect(usePrivacy().hidden).toBe(true);
    expect(usePrivacy().reason).toBe('focusLoss');
    expect(curtain().textContent).toContain('Window previews may capture a frame');
    await usePrivacy().show();
    await flushPromises();
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
    await flushPromises();
    expect(usePrivacy().hidden).toBe(true);
    delete (document as unknown as Record<string, unknown>).visibilityState;
  });

  it('keep the curtain up when focus is lost while Show reopens an idle-closed session', async () => {
    setViewerPrefs({ blurOnFocusLoss: true, idleHideMinutes: 1 });
    mounted = await mountViewer();
    const privacy = usePrivacy();
    await privacy.hide('idle');
    await flushPromises();
    expect(privacy.closed).toBe(true);
    let release: () => void = () => undefined;
    fake.handlers.openViewer = () =>
      new Promise((resolve) => {
        release = () => resolve(viewerSession({ viewer: VIEWER }));
      });
    const showing = privacy.show();
    await flushPromises();
    // The user switches away while the archive opens again.
    window.dispatchEvent(new Event('blur'));
    release();
    await showing;
    await flushPromises();
    expect(privacy.hidden).toBe(true);
    expect(privacy.reason).toBe('focusLoss');
    expect(privacy.closed).toBe(false);
    expect(document.querySelector('.oma-thread')).toBeNull();
    // The next Show only lowers the curtain: the session is open.
    fake.calls.length = 0;
    await privacy.show();
    await flushPromises();
    expect(privacy.hidden).toBe(false);
    expect(named('openViewer')).toEqual([]);
  });

  it('close the session after the idle time, reset on activity, and Show reopens at the same place', async () => {
    setViewerPrefs({ idleHideMinutes: 1 });
    mounted = await mountViewer();
    document.getElementById('oma-m-30')!.focus();
    const privacy = usePrivacy();
    privacy.detach();
    vi.useFakeTimers();
    privacy.attach();
    vi.advanceTimersByTime(50_000);
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'a' }));
    vi.advanceTimersByTime(20_000);
    expect(privacy.hidden).toBe(false);
    vi.advanceTimersByTime(60_000 + IDLE_CHECK_MS);
    expect(privacy.hidden).toBe(true);
    vi.useRealTimers();
    await flushPromises();
    expect(named('closeViewer')).toEqual([VIEWER]);
    expect(useViewer().session).toBeNull();
    expect(useViewer().windows).toEqual([]);
    expect(curtain().textContent).toContain('closed after a period without activity');

    fake.calls.length = 0;
    curtain().querySelector('button')!.click();
    await flushPromises();
    await flushPromises();
    expect(named('openViewer')).toEqual([ARCHIVE_ID]);
    const asked = named('getThreadPage') as GetThreadPageRequest[];
    expect(asked.at(-1)?.anchor).toMatchObject({ kind: 'message', message: 30 });
    expect(document.activeElement?.id).toBe('oma-m-30');
  });
});

describe('the page guards', () => {
  it('stop print, save and reload, also in text fields', () => {
    const remove = installViewerGuards(window);
    const input = document.createElement('input');
    document.body.append(input);
    const keys: [KeyboardEventInit, boolean][] = [
      [{ key: 'p', ctrlKey: true }, true],
      [{ key: 'P', ctrlKey: true, shiftKey: true }, true],
      [{ key: 's', ctrlKey: true }, true],
      [{ key: 'F5' }, true],
      [{ key: 'r', ctrlKey: true }, true],
      [{ key: 'R', ctrlKey: true, shiftKey: true }, true],
      [{ key: 'a', ctrlKey: true }, false],
      [{ key: 'p' }, false],
    ];
    for (const [init, stopped] of keys) {
      for (const target of [document.body, input]) {
        const e = new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true });
        target.dispatchEvent(e);
        expect(e.defaultPrevented, JSON.stringify(init)).toBe(stopped);
      }
    }
    expect(guardedKey(new KeyboardEvent('keydown', { key: 'p', metaKey: true }), true)).toBe('print');
    expect(document.documentElement.classList.contains('oma-viewer-open')).toBe(true);
    remove();
    expect(document.documentElement.classList.contains('oma-viewer-open')).toBe(false);
    const after = new KeyboardEvent('keydown', { key: 'p', ctrlKey: true, bubbles: true, cancelable: true });
    document.body.dispatchEvent(after);
    expect(after.defaultPrevented).toBe(false);
  });

  it('stop the engine context menu and drag-out except in text fields', () => {
    const remove = installViewerGuards(window);
    const div = document.createElement('div');
    const input = document.createElement('input');
    document.body.append(div, input);
    for (const type of ['contextmenu', 'dragstart']) {
      const onDiv = new MouseEvent(type, { bubbles: true, cancelable: true });
      div.dispatchEvent(onDiv);
      expect(onDiv.defaultPrevented, type).toBe(true);
      const onInput = new MouseEvent(type, { bubbles: true, cancelable: true });
      input.dispatchEvent(onInput);
      expect(onInput.defaultPrevented, type).toBe(false);
    }
    remove();
  });

  it('are installed by the viewer page and removed when it goes', async () => {
    mounted = await mountViewer();
    const e = new KeyboardEvent('keydown', { key: 'p', ctrlKey: true, bubbles: true, cancelable: true });
    document.body.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(true);
    const articles = document.querySelectorAll('article.oma-thread-item');
    expect(articles.length).toBeGreaterThan(0);
    for (const a of articles) expect(a.getAttribute('draggable')).toBe('false');
    mounted.unmount();
    mounted = null;
    expect(document.documentElement.classList.contains('oma-viewer-open')).toBe(false);
  });

  it('declare Ctrl+Shift+H (also in text fields) and Shift+F10 on a message', () => {
    const input = document.createElement('input');
    document.body.append(input);
    const e = new KeyboardEvent('keydown', { key: 'H', ctrlKey: true, shiftKey: true });
    Object.defineProperty(e, 'target', { value: input });
    expect(matchShortcut(e, false)?.id).toBe('viewer-privacy-screen');
    const menuKey = new KeyboardEvent('keydown', { key: 'F10', shiftKey: true });
    expect(matchShortcut(menuKey, false, 'message')?.id).toBe('viewer-message-menu');
    expect(matchShortcut(menuKey, false)).toBeUndefined();
  });
});

describe('the message menu', () => {
  const menu = () => document.querySelector<HTMLDialogElement>('dialog.oma-menu')!;
  const items = () => Array.from(menu().querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
  const item = (label: string) => items().find((b) => b.textContent?.trim() === label)!;

  it('replaces the engine menu with the app’s, and copies the text as shown', async () => {
    mounted = await mountViewer();
    const article = document.getElementById('oma-m-29')!;
    const e = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 50 });
    article.dispatchEvent(e);
    await flushPromises();
    expect(e.defaultPrevented).toBe(true);
    expect(menu().open).toBe(true);
    expect(menu().getAttribute('aria-label')).toBe('Message actions');
    expect(items().map((b) => b.textContent?.trim())).toEqual([
      'Copy text',
      'Copy text without invisible characters',
      'Copy with sender and time',
      'Details',
    ]);
    expect(document.activeElement).toBe(items()[0]);
    // Arrow keys move within the menu.
    items()[0]!.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }));
    expect(document.activeElement).toBe(items().at(-1));
    item('Copy text').click();
    await flushPromises();
    expect(fake.copied).toEqual(['hello 29']);
    expect(useNotifications().toasts.at(-1)).toMatchObject({ level: 'success', message: 'Copied to the clipboard.' });
    expect(useNotifications().toasts.at(-1)!.message).not.toContain('hello');
    expect(menu().open).toBe(false);
    expect(document.activeElement?.id).toBe('oma-m-29');
  });

  it('opens on Shift+F10 and puts copy without invisible characters first when the text has them', async () => {
    mounted = await mountViewer();
    const article = document.getElementById('oma-m-30')!;
    article.focus();
    article.dispatchEvent(new KeyboardEvent('keydown', { key: 'F10', shiftKey: true, bubbles: true, cancelable: true }));
    await flushPromises();
    expect(menu().open).toBe(true);
    expect(items()[0]!.textContent?.trim()).toBe('Copy text without invisible characters');
    items()[0]!.click();
    await flushPromises();
    expect(fake.copied.at(-1)).toBe('see https://one.invalid/a or https://two.invalid done');
  });

  it('refuses a link with hidden characters and shows a clean one exactly before copying', async () => {
    mounted = await mountViewer();
    const article = document.getElementById('oma-m-30')!;
    article.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 5, clientY: 5 }));
    await flushPromises();
    item('Copy link 2 of 2…').click();
    await flushPromises();
    const dialog = () => Array.from(document.querySelectorAll('dialog.oma-dialog')).find((d) => d.textContent?.includes('Copy link'))!;
    expect(dialog().textContent).toContain("can't be copied as a link; use Copy text");
    expect(Array.from(dialog().querySelectorAll('button')).map((b) => b.textContent?.trim())).not.toContain('Copy');
    expect(fake.copied).toEqual([]);
    Array.from(dialog().querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Close')!.click();
    await flushPromises();

    article.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 5, clientY: 5 }));
    await flushPromises();
    item('Copy link 1 of 2…').click();
    await flushPromises();
    expect(dialog().textContent).toContain('https://one.invalid/a');
    expect(fake.copied).toEqual([]);
    Array.from(dialog().querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Copy')!.click();
    await flushPromises();
    expect(fake.copied).toEqual(['https://one.invalid/a']);
  });

  it('says so when the copy failed, without the text', async () => {
    fake.copyOk = false;
    mounted = await mountViewer();
    document.getElementById('oma-m-28')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    await flushPromises();
    item('Copy with sender and time').click();
    await flushPromises();
    expect(fake.copied.at(-1)).toMatch(/^\+1 555 0100 — .+\nhello 28$/);
    expect(useNotifications().toasts.at(-1)).toMatchObject({ level: 'danger', message: 'The text could not be copied.' });
  });

  it('is closed by the privacy screen', async () => {
    mounted = await mountViewer();
    document.getElementById('oma-m-28')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    await flushPromises();
    expect(useViewer().menu).not.toBeNull();
    await usePrivacy().hide('manual');
    await flushPromises();
    expect(useViewer().menu).toBeNull();
    expect(document.querySelector('dialog.oma-menu')).toBeNull();
  });
});
