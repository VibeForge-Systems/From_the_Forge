import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';

const fake = vi.hoisted(() => ({
  updates: [] as unknown[],
  os: 'linux',
}));
vi.mock('@/services/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/services/ipc')>();
  return {
    ...actual,
    ipc: {
      updateSettings: (update: unknown) => {
        fake.updates.push(update);
        return Promise.resolve(null);
      },
      getSettings: () => Promise.resolve(null),
      getAppInfo: () => Promise.resolve({ os: fake.os }),
    },
  };
});
import ViewerSettings from '@/components/ViewerSettings.vue';
import { VIEWER_DEFAULTS, ZOOM_STEPS, useSettings } from '@/stores/settings';
import { snapshot } from './helpers';

beforeEach(() => {
  setActivePinia(createPinia());
  fake.updates.length = 0;
  fake.os = 'linux';
});

function mountWith(viewer = {}) {
  const settings = useSettings();
  const snap = snapshot();
  snap.settings.viewer = { ...snap.settings.viewer, ...viewer };
  settings.snapshot = snap;
  return mount(ViewerSettings);
}

describe('viewer settings', () => {
  it('match the backend defaults', () => {
    expect(VIEWER_DEFAULTS).toEqual(snapshot().settings.viewer);
    expect(ZOOM_STEPS[0]).toBe(100);
    expect(ZOOM_STEPS.at(-1)).toBe(400);
    expect(ZOOM_STEPS).toHaveLength(31);
  });

  it('send the whole viewer object with one field changed', async () => {
    const w = mountWith({ showSeconds: true });
    await w.find('#viewer-zoom').setValue('250');
    expect(fake.updates).toEqual([{ viewer: { ...snapshot().settings.viewer, showSeconds: true, uiZoom: 250 } }]);
  });

  it('enable the viewer and privacy-screen preferences now that both ship (5h3, 5j)', async () => {
    const w = mountWith();
    await flushPromises();
    for (const id of [
      'viewer-time-format',
      'viewer-seconds',
      'viewer-reveal-earlier',
      'viewer-playback-confirm',
      'viewer-blur',
      'viewer-idle',
    ]) {
      const el = w.find(`#${id}`).element as HTMLInputElement;
      expect(el.matches(':disabled'), id).toBe(false);
    }
    expect(w.text()).toContain('Always play without asking');
    expect(w.findAll('.oma-planned')).toHaveLength(0);
    await w.find('#viewer-blur').setValue(true);
    expect(fake.updates.at(-1)).toMatchObject({ viewer: { blurOnFocusLoss: true } });
    await w.find('#viewer-idle').setValue('5');
    expect(fake.updates.at(-1)).toMatchObject({ viewer: { idleHideMinutes: 5 } });
    expect(w.find('#viewer-zoom').element.matches(':disabled')).toBe(false);
    await w.find('#viewer-playback-off').setValue(true);
    expect(fake.updates.at(-1)).toMatchObject({ viewer: { mediaPlayback: 'off' } });
  });

  it('show capture protection only on Windows, and say it is not a security feature', async () => {
    const linux = mountWith();
    await flushPromises();
    expect(linux.find('#viewer-capture').exists()).toBe(false);
    fake.os = 'windows';
    const windows = mountWith();
    await flushPromises();
    expect(windows.find('#viewer-capture').attributes('disabled')).toBeUndefined();
    expect(windows.text()).toContain('not a security feature');
    expect(windows.text()).toContain('screen sharing');
    expect(windows.text()).toContain('has not been tested');
    await windows.find('#viewer-capture').setValue(true);
    expect(fake.updates.at(-1)).toMatchObject({ viewer: { captureProtection: true } });
  });

  it('name the media risk and the three playback choices', () => {
    const w = mountWith();
    expect(w.findAll('input[name="viewer-playback"]')).toHaveLength(3);
    expect((w.find('#viewer-playback-confirm').element as HTMLInputElement).checked).toBe(true);
    expect(w.text()).toContain('could exploit a flaw');
    expect(w.text()).toContain('It is not a lock');
  });
});
