// Attachment cards, the media viewer and the play confirmation (M5 §5.5,
// §5.6, §5.9, §8.4, §13.8), slice 5h3: inspections when a card comes into
// view (at most four at once), `src` only from `mediaUrl` and only while
// in view, retries with backoff, file and unavailable cards, and the Play
// gate: Rust's check, then the confirmation under `confirm`, none under
// `clickToPlay`, and no element at all under `off`.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount } from '@vue/test-utils';
import { defineComponent, h } from 'vue';

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
import AttachmentView from '@/components/viewer/AttachmentView.vue';
import MediaViewer from '@/components/viewer/MediaViewer.vue';
import PlayConfirmDialog from '@/components/viewer/PlayConfirmDialog.vue';
import { useViewer } from '@/stores/viewer';
import { useExports } from '@/stores/exports';
import { useSettings } from '@/stores/settings';
import { mediaUrl } from '@/services/media';
import { MEDIA_UI_TEXT, PLAYBACK_TEXT } from '@/services/viewerText';
import type {
  AttachmentDto,
  AttachmentInspectionDto,
  DecimalString,
  InspectAttachmentRequest,
  MediaPlayback,
  SniffedKindDto,
} from '@/types';
import { ARCHIVE_ID, MEDIA_BASE, TOKEN, attachment, inspection, snapshot, text, viewerSession } from './helpers';

const THUMB = mediaUrl(MEDIA_BASE, TOKEN, 'thumb');
const STREAM = mediaUrl(MEDIA_BASE, TOKEN, 'stream');
const DISPLAY = mediaUrl(MEDIA_BASE, TOKEN, 'display');

function setup(playback: MediaPlayback = 'confirm'): void {
  const settings = useSettings();
  const snap = snapshot();
  snap.settings.viewer = { ...snap.settings.viewer, mediaPlayback: playback };
  settings.snapshot = snap;
  const viewer = useViewer();
  viewer.session = viewerSession();
  viewer.archiveId = ARCHIVE_ID;
}

/** Answer `inspect_attachment` from a function of the request. */
function answer(fn: (r: InspectAttachmentRequest) => AttachmentInspectionDto): void {
  fake.handlers.inspectAttachment = (arg) => Promise.resolve(fn(arg as InspectAttachmentRequest));
}

function playableInspection(kind: SniffedKindDto, r: InspectAttachmentRequest): AttachmentInspectionDto {
  return inspection(r.attachmentId, {
    content: { kind: 'sniffed', sniffed: kind, class: { kind: 'playable', structureChecked: true }, storedBytes: 4096 },
    playback: r.forPlayback ? { kind: 'allowed', structureChecked: true } : null,
  });
}

const Host = defineComponent({
  props: { items: { type: Array as () => AttachmentDto[], required: true } },
  setup(props) {
    return () =>
      h('div', [
        ...props.items.map((a, i) => h(AttachmentView, { key: a.id, attachment: a, index: i + 1, idBase: `oma-m-${a.id}` })),
        h(MediaViewer),
        h(PlayConfirmDialog),
      ]);
  },
});

function mountHost(items: AttachmentDto[]) {
  return mount(Host, { props: { items }, attachTo: document.body });
}

const forPlayback = () =>
  fake.calls.filter(([n, a]) => n === 'inspectAttachment' && (a as InspectAttachmentRequest).forPlayback).length;

let canPlay: string;
beforeEach(() => {
  setActivePinia(createPinia());
  fake.calls.length = 0;
  fake.handlers = {};
  canPlay = 'maybe';
  vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockImplementation(() => canPlay as CanPlayTypeResult);
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  document.body.innerHTML = '';
});

describe('attachment cards', () => {
  it('inspect when they come into view, at most four at once, once per session', async () => {
    setup();
    const pending: ((v: AttachmentInspectionDto) => void)[] = [];
    fake.handlers.inspectAttachment = (arg) =>
      new Promise((resolve) => pending.push(() => resolve(inspection((arg as InspectAttachmentRequest).attachmentId))));
    const w = mountHost([1, 2, 3, 4, 5, 6].map((i) => attachment(i)));
    await flushPromises();
    expect(fake.calls.filter(([n]) => n === 'inspectAttachment')).toHaveLength(4);
    pending.splice(0).forEach((r) => r(inspection(1)));
    await flushPromises();
    expect(fake.calls.filter(([n]) => n === 'inspectAttachment')).toHaveLength(6);
    pending.splice(0).forEach((r) => r(inspection(1)));
    await flushPromises();
    expect(w.findAll('img')).toHaveLength(6);
    // A remount uses the session's cache.
    w.unmount();
    mountHost([attachment(1)]);
    await flushPromises();
    expect(fake.calls.filter(([n]) => n === 'inspectAttachment')).toHaveLength(6);
  });

  it('take src only from mediaUrl, with alt="" and the caption as the accessible name', async () => {
    setup();
    answer((r) => inspection(r.attachmentId));
    const hostile = attachment(7, { name: text('<img src=x onerror=alert(1)>.jpg') });
    const w = mountHost([hostile]);
    await flushPromises();
    const img = w.find('figure img');
    expect(img.attributes('src')).toBe(THUMB);
    expect(img.attributes('alt')).toBe('');
    const button = w.find('button.oma-media-btn');
    const caption = w.find(`#${button.attributes('aria-labelledby')}`);
    expect(caption.text()).toContain('Image,');
    expect(caption.text()).toContain('<img src=x onerror=alert(1)>.jpg');
    expect(w.findAll('img')).toHaveLength(1);
    expect(caption.text()).toContain('20 KB stored (20 KB declared)');
    await button.trigger('click');
    expect(useViewer().media).toEqual({ attachment: hostile, mode: 'image', returnTo: 'oma-m-7-a1-open' });
  });

  it('set src only while in view', async () => {
    setup();
    answer((r) => inspection(r.attachmentId));
    const observers: ((entries: { isIntersecting: boolean }[]) => void)[] = [];
    vi.stubGlobal(
      'IntersectionObserver',
      class {
        constructor(cb: (entries: { isIntersecting: boolean }[]) => void) {
          observers.push(cb);
        }
        observe() {}
        disconnect() {}
      },
    );
    const w = mountHost([attachment(1)]);
    await flushPromises();
    expect(fake.calls).toHaveLength(0);
    expect(w.find('img').exists()).toBe(false);
    observers[0]!([{ isIntersecting: true }]);
    await flushPromises();
    expect(w.find('img').attributes('src')).toBe(THUMB);
    observers[0]!([{ isIntersecting: false }]);
    await flushPromises();
    expect(w.find('img').exists()).toBe(false);
    expect(w.find('.oma-media-placeholder').exists()).toBe(true);
  });

  it('retry a failed preview after 0.5 s, 1 s and 2 s, then offer Retry', async () => {
    setup();
    answer((r) => inspection(r.attachmentId));
    const w = mountHost([attachment(1)]);
    await flushPromises();
    vi.useFakeTimers();
    for (const delay of [500, 1000, 2000]) {
      await w.find('img').trigger('error');
      expect(w.find('img').exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(delay - 1);
      expect(w.find('img').exists()).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      expect(w.find('img').attributes('src')).toBe(THUMB);
    }
    await w.find('img').trigger('error');
    expect(w.find('img').exists()).toBe(false);
    expect(w.text()).toContain('Preview could not be loaded.');
    await w.findAll('button').find((b) => b.text() === 'Retry')!.trigger('click');
    expect(w.find('img').attributes('src')).toBe(THUMB);
  });

  it('show file cards and unavailable files as sentences, never as previews', async () => {
    setup();
    answer((r) =>
      r.attachmentId === 1
        ? inspection(1, { content: { kind: 'sniffed', sniffed: 'heif', class: { kind: 'file' }, storedBytes: 9 } })
        : inspection(2, { content: { kind: 'unavailable', problem: 'digestMismatch' } }),
    );
    const w = mountHost([attachment(1, { declaredType: text('image/heic') }), attachment(2)]);
    await flushPromises();
    expect(w.find('img').exists()).toBe(false);
    expect(w.text()).toContain('HEIC image: preview not available in this version');
    expect(w.text()).toContain('Type recorded: image/heic; content: HEIC image');
    expect(w.text()).toContain('Preview unavailable: the file does not match its recorded checksum');
  });

  it('say when the engine cannot play a format, and offer no Play button', async () => {
    setup();
    canPlay = '';
    answer((r) => playableInspection('caf', r));
    const w = mountHost([attachment(1)]);
    await flushPromises();
    expect(w.text()).toContain(MEDIA_UI_TEXT.cannotPlayHere);
    expect(w.findAll('figure button').some((b) => b.text().includes('Play'))).toBe(false);
  });
});

describe('playback', () => {
  async function play(w: ReturnType<typeof mountHost>): Promise<void> {
    await w.findAll('figure button').find((b) => b.text().includes('Play'))!.trigger('click');
    await flushPromises();
  }

  it('under confirm: checks in Rust, asks, and only then creates the element', async () => {
    setup('confirm');
    answer((r) => playableInspection('mp4', r));
    const w = mountHost([attachment(1)]);
    await flushPromises();
    expect(w.find('video').exists()).toBe(false);
    await play(w);
    expect(forPlayback()).toBe(1);
    expect(useViewer().pendingPlay?.id).toBe(1);
    expect(w.text()).toContain(MEDIA_UI_TEXT.confirm);
    expect(w.find('video').exists()).toBe(false);

    await w.findAll('.modal-footer button').find((b) => b.text() === 'Play')!.trigger('click');
    await flushPromises();
    const video = w.find('video');
    expect(video.attributes('src')).toBe(STREAM);
    expect(video.attributes('preload')).toBe('metadata');
    expect(video.attributes('autoplay')).toBeUndefined();
    expect(video.attributes('controls')).toBeDefined();
    expect(video.attributes('controlslist')).toBe('nodownload noremoteplayback');
    expect(video.attributes('disablepictureinpicture')).toBeDefined();
    expect(video.attributes('disableremoteplayback')).toBeDefined();

    // Closing stops it; the second Play of the same file does not ask again.
    useViewer().closeMedia();
    await flushPromises();
    expect(w.find('video').exists()).toBe(false);
    await play(w);
    expect(useViewer().pendingPlay).toBeNull();
    expect(w.find('video').exists()).toBe(true);
  });

  it('Cancel plays nothing; "Always play without asking" switches the setting to clickToPlay', async () => {
    setup('confirm');
    fake.handlers.updateSettings = () => Promise.resolve(snapshot());
    answer((r) => playableInspection('m4a', r));
    const w = mountHost([attachment(1), attachment(2)]);
    await flushPromises();
    await play(w);
    await w.findAll('.modal-footer button').find((b) => b.text() === 'Cancel')!.trigger('click');
    await flushPromises();
    expect(w.find('audio').exists()).toBe(false);
    expect(useViewer().media).toBeNull();

    await play(w);
    await w.findAll('.modal-footer button').find((b) => b.text() === 'Always play without asking')!.trigger('click');
    await flushPromises();
    expect(fake.calls.find(([n]) => n === 'updateSettings')?.[1]).toMatchObject({ viewer: { mediaPlayback: 'clickToPlay' } });
    expect(w.find('audio').attributes('src')).toBe(STREAM);
    expect(w.find('video').exists()).toBe(false);
  });

  it('under clickToPlay: no confirmation', async () => {
    setup('clickToPlay');
    answer((r) => playableInspection('webm', r));
    const w = mountHost([attachment(1)]);
    await flushPromises();
    await play(w);
    expect(useViewer().pendingPlay).toBeNull();
    expect(w.find('video').attributes('src')).toBe(STREAM);
  });

  it('under off: no Play button, no check and no element', async () => {
    setup('off');
    answer((r) => playableInspection('mp4', r));
    const w = mountHost([attachment(1)]);
    await flushPromises();
    expect(w.text()).toContain(PLAYBACK_TEXT.off);
    expect(w.findAll('figure button').some((b) => b.text().includes('Play'))).toBe(false);
    await useViewer().requestPlay(attachment(1));
    expect(forPlayback()).toBe(0);
    expect(w.find('video').exists()).toBe(false);
    expect(w.find('audio').exists()).toBe(false);
  });

  it('a file refused by the structural check never asks and never plays', async () => {
    setup('confirm');
    answer((r) =>
      inspection(r.attachmentId, {
        content: { kind: 'sniffed', sniffed: 'quickTime', class: { kind: 'playable', structureChecked: true }, storedBytes: 9 },
        playback: r.forPlayback ? { kind: 'refused', reason: 'referenceMovie' } : null,
      }),
    );
    const w = mountHost([attachment(1)]);
    await flushPromises();
    await play(w);
    expect(useViewer().pendingPlay).toBeNull();
    expect(w.find('video').exists()).toBe(false);
    expect(w.text()).toContain("This file can't be played in the app: its structure failed a safety check");
  });

  it('sets a neutral media-session title while playing, never a file name', async () => {
    setup('clickToPlay');
    const session = { metadata: null as unknown };
    vi.stubGlobal('navigator', { ...navigator, mediaSession: session });
    vi.stubGlobal(
      'MediaMetadata',
      class {
        title: string;
        constructor(init: { title: string }) {
          this.title = init.title;
        }
      },
    );
    answer((r) => playableInspection('mp4', r));
    const w = mountHost([attachment(1, { name: text('secret-holiday.mov') })]);
    await flushPromises();
    await play(w);
    await w.find('video').trigger('play');
    expect((session.metadata as { title: string }).title).toBe('OpenMessage Archive');
  });
});

describe('the media viewer', () => {
  it('shows the display rendition labelled as a re-encoded preview, and asks for the original size after it loads', async () => {
    setup();
    let fresh = 0;
    answer((r) => {
      fresh += 1;
      return inspection(r.attachmentId, fresh > 1 ? { originalSize: { width: 4032, height: 3024 } } : {});
    });
    const w = mountHost([attachment(1)]);
    await flushPromises();
    await w.find('button.oma-media-btn').trigger('click');
    await flushPromises();
    const full = w.find('img.oma-media-full');
    expect(full.attributes('src')).toBe(DISPLAY);
    expect(full.attributes('alt')).toBe('');
    expect(w.find('.oma-dialog').text()).toContain('original size shown after the preview loads');
    expect(w.find('.oma-dialog').text()).toContain('Type recorded: image/jpeg; content: JPEG image');
    await full.trigger('load');
    await flushPromises();
    expect(w.find('.oma-dialog').text()).toContain('Preview, re-encoded, at most 2048 px; original 4032×3024');
    // No download, open or share control; the one way to keep the file
    // is "Save a copy…" (M6 §6.6), which closes this viewer and hands the
    // attachment to the Export dialog's copy flow (Rust checks it again).
    const labels = w.find('.oma-dialog').findAll('button').map((b) => b.text().toLowerCase());
    for (const l of labels.filter((x) => x !== 'save a copy…')) expect(l).not.toMatch(/download|save|open|share/);
    expect(labels).toContain('save a copy…');
    const save = w.find('.oma-dialog').findAll('button').find((b) => b.text() === 'Save a copy…');
    await save!.trigger('click');
    await flushPromises();
    expect(useViewer().media).toBeNull();
    const x = useExports();
    expect(x.open).toBe(true);
    expect(x.copyTarget).toEqual({ archiveId: ARCHIVE_ID, viewer: viewerSession().viewer, attachmentId: 1 });
    const preview = fake.calls.find(([n]) => n === 'previewExport')?.[1] as unknown[];
    expect(preview[0]).toMatchObject({
      scope: { kind: 'attachment', viewer: viewerSession().viewer, attachmentId: 1 },
      formats: [],
    });
  });

  it('offers "Save a copy…" on a card only for a stored file the inspection read', async () => {
    setup();
    answer((r) =>
      r.attachmentId === 2
        ? inspection(r.attachmentId, { content: { kind: 'noFile' } })
        : inspection(r.attachmentId),
    );
    const w = mountHost([attachment(1), attachment(2)]);
    await flushPromises();
    const saves = w.findAll('figure').map((f) => f.findAll('button').some((b) => b.text() === 'Save a copy…'));
    expect(saves).toEqual([true, false]);
  });
});

describe('attachment cards (review fixes)', () => {
  it('keep Play focusable while Rust checks the file, and ignore a second press', async () => {
    setup('confirm');
    let release: () => void = () => undefined;
    fake.handlers.inspectAttachment = (arg) => {
      const r = arg as InspectAttachmentRequest;
      if (!r.forPlayback) return Promise.resolve(playableInspection('mp4', r));
      return new Promise((resolve) => {
        release = () => resolve(playableInspection('mp4', r));
      });
    };
    const w = mountHost([attachment(1)]);
    await flushPromises();
    const button = w.get('#oma-m-1-a1-play');
    (button.element as HTMLButtonElement).focus();
    await button.trigger('click');
    await flushPromises();
    expect(button.attributes('disabled')).toBeUndefined();
    expect(button.attributes('aria-disabled')).toBe('true');
    expect(document.activeElement).toBe(button.element);
    await button.trigger('click');
    expect(forPlayback()).toBe(1);
    release();
    await flushPromises();
    expect(useViewer().pendingPlay?.id).toBe(1);
  });

  it('say on the card when the inspection failed, with Retry', async () => {
    setup();
    let fail = true;
    fake.handlers.inspectAttachment = (arg) =>
      fail
        ? Promise.reject(new Error('could not read'))
        : Promise.resolve(inspection((arg as InspectAttachmentRequest).attachmentId));
    const w = mountHost([attachment(1)]);
    await flushPromises();
    expect(w.text()).toContain('This attachment could not be inspected.');
    fail = false;
    await w.findAll('button').find((b) => b.text() === 'Retry')!.trigger('click');
    await flushPromises();
    expect(w.text()).not.toContain('could not be inspected');
    expect(w.find('img').exists()).toBe(true);
  });

  it('note a file shown from an older backup and a verify problem on an image card', async () => {
    setup();
    answer((r) =>
      inspection(r.attachmentId, {
        facts: {
          ...inspection(r.attachmentId).facts,
          card: { kind: 'file', olderBackup: { heldBy: r.attachmentId, newest: r.attachmentId } },
          verify: { state: 'lengthMismatch', raw: '3' as DecimalString, verifiedMs: null, mayBeStale: false },
        },
      }),
    );
    const w = mountHost([attachment(1)]);
    await flushPromises();
    expect(w.find('img').exists()).toBe(true);
    const notes = w.findAll('.oma-card-note').map((n) => n.text());
    expect(notes[0]).toMatch(/^Showing the file from/);
    expect(notes[1]).toBe('The full verify reported: wrong size');
  });
});
